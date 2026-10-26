use minidb::layout::Layout;
use minidb::record::{self, Row};
use minidb::table::Table;
use minidb::{codegen, Column, DataType, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "minidb-review-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn schema() -> Schema {
    Schema::new(
        "items",
        vec![
            Column::new("id", DataType::INT),
            Column::new("name", DataType::STRING),
        ],
        0,
    )
}
fn values(id: i64) -> [Value<'static>; 2] {
    [Value::Int(id), Value::String("body")]
}
fn added() -> Column {
    Column::new(
        "score",
        DataType {
            nullable: true,
            ..DataType::FLOAT
        },
    )
}

#[test]
fn schema_metadata_roundtrips_with_scientific_extremes_and_rejects_bad_tails() -> Result<()> {
    let initial = schema();
    let evolved = initial.with_added_column(added())?;
    let text = evolved.to_text();
    assert!(text.contains("-1.7976931348623157e308"));
    assert!(text.contains("1.7976931348623157e308"));
    assert_eq!(Schema::from_text(&text)?, evolved);
    let create = text.split("VERSION 2").next().unwrap();
    let reordered =
        format!("{create}COLUMNS_AT_VERSION 2 = 3\nVERSION 2\nCOLUMNS_AT_VERSION 1 = 2\n");
    assert_eq!(Schema::from_text(&reordered)?, evolved);
    for tail in [
        "VERSION 2\n",
        "VERSION nonsense\n",
        "COLUMNS_AT_VERSION 1 = 2\n",
        "garbage\n",
    ] {
        assert!(Schema::from_text(&(text.clone() + tail)).is_err(), "{tail}");
    }
    for (bad, message) in [
        ("VERSION nonsense", "VERSION: ожидается u32"),
        (
            "COLUMNS_AT_VERSION nope = 2",
            "COLUMNS_AT_VERSION: ожидается версия u32",
        ),
        (
            "COLUMNS_AT_VERSION 1 = nope",
            "COLUMNS_AT_VERSION: ожидается число колонок",
        ),
    ] {
        assert!(Schema::from_text(&format!("{create}{bad}"))
            .unwrap_err()
            .to_string()
            .contains(message));
    }
    Ok(())
}

#[test]
fn reader_rejects_rolled_back_schema_but_defaults_old_physical_rows() -> Result<()> {
    let old = schema();
    let current = old.with_added_column(added())?;
    let old_layout = Layout::from_schema(&old)?;
    let columns = [("id", 8, 0), ("name", 66, 2), ("score", 8, 1)];
    let mut bytes = vec![0; old_layout.row_size];
    record::encode(&old_layout, &old, &values(1), &mut bytes)?;
    let row = Row {
        bytes: &bytes,
        layout: &old_layout,
        schema: &old,
    };
    assert!(codegen::check_reader(row, "items", 2, &columns)
        .unwrap_err()
        .to_string()
        .contains("более новую VERSION"));
    assert!(codegen::check_schema_once(&old, "items", 2, &columns).is_err());
    let current_row = Row {
        schema: &current,
        ..row
    };
    codegen::check_schema_once(&current, "items", 2, &columns)?;
    codegen::check_reader(current_row, "items", 2, &columns)?;
    assert_eq!(current_row.at(2)?, Value::Null);
    codegen::check_reader(current_row, "items", 1, &columns[..2])?;
    let mut invalid = old.clone();
    invalid.version = 0;
    assert!(codegen::generate(&invalid).is_err());
    let ambiguous = Schema::new(
        "test",
        vec![
            Column::new("self", DataType::INT),
            Column::new("self_", DataType::STRING),
        ],
        0,
    );
    assert!(codegen::generate(&ambiguous).is_err());
    Ok(())
}

#[test]
fn appends_and_deletes_keep_schema_unchanged_until_checkpoint_is_needed() -> Result<()> {
    let temp = Temp::new()?;
    let mut table = Table::create(&temp.0, schema())?;
    let path = temp.0.join("items.schema.sql");
    let initial = fs::read(&path)?;
    table.insert(&values(100))?;
    assert_eq!(fs::read(&path)?, initial);
    table.remove(100)?;
    assert_eq!(fs::read(&path)?, initial);
    table.insert(&values(1))?;
    let checkpoint = fs::read_to_string(&path)?;
    assert!(checkpoint.contains("-- minidb-highest-id: 100"));
    assert_eq!(table.get(1)?.unwrap().get("name")?, Value::String("body"));
    table.insert(&values(200))?;
    assert_eq!(fs::read_to_string(&path)?, checkpoint);
    table.update(1, &[Value::Int(1), Value::String("unchanged PK")])?;
    assert_eq!(fs::read_to_string(&path)?, checkpoint);
    table.update(200, &values(2))?;
    assert!(fs::read_to_string(&path)?.contains("-- minidb-highest-id: 200"));
    table.insert(&[Value::Null, Value::String("auto")])?;
    assert!(table.get(201)?.is_some());
    assert!(!fs::read_to_string(&path)?.contains("-- minidb-highest-id: 201"));
    table.sync()?;
    assert!(fs::read_to_string(&path)?.contains("-- minidb-highest-id: 201"));
    assert_eq!(table.segments()[0].path(), temp.0.join("items"));
    assert_eq!(table.segments()[0].slot_count(), 3);
    assert_eq!(table.segments()[0].free_slot_count(), 0);
    Ok(())
}

// Вызывается отдельным процессом; exit не запускает Drop/сохраняющий sync.
#[test]
fn crash_writer_child() -> Result<()> {
    let Some(root) = std::env::var_os("MINIDB_CRASH_TEST_ROOT") else {
        return Ok(());
    };
    let mode = std::env::var("MINIDB_CRASH_TEST_MODE")?;
    let root = PathBuf::from(root);
    let mut table = Table::create(&root, schema())?;
    table.insert(&values(100))?;
    match mode.as_str() {
        "append" => (),
        "delete" => {
            table.remove(100)?;
        }
        "reuse" | "free" => {
            table.remove(100)?;
            table.insert(&values(1))?;
            if mode == "free" {
                // Состояние после тела FREE, но до коммита LIVE при reuse.
                fs::write(
                    root.join("items"),
                    "# MiniDB text; VERSION 1\n# state\tid\tname\nFREE\n",
                )?;
            }
        }
        "lower" => table.update(100, &values(1))?,
        _ => return Err("unknown child mode".into()),
    }
    std::process::exit(0);
}

#[test]
fn auto_id_survives_process_exit_without_drop_or_sync() -> Result<()> {
    let temp = Temp::new()?;
    for mode in ["append", "delete", "reuse", "free", "lower"] {
        let root = temp.0.join(mode);
        let out = Command::new(std::env::current_exe()?)
            .args(["--exact", "crash_writer_child", "--nocapture"])
            .env("MINIDB_CRASH_TEST_ROOT", &root)
            .env("MINIDB_CRASH_TEST_MODE", mode)
            .output()?;
        assert!(
            out.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = fs::read_to_string(root.join("items.schema.sql"))?;
        if mode == "append" || mode == "delete" {
            assert!(!text.contains("minidb-highest-id"));
        }
        let mut reopened = Table::open(&root, "items")?;
        reopened.insert(&[Value::Null, Value::String("after restart")])?;
        assert!(reopened.get(101)?.is_some(), "{mode}");
    }
    Ok(())
}

#[test]
fn unchanged_update_has_the_same_count_and_preserves_files_in_all_segments() -> Result<()> {
    let temp = Temp::new()?;
    let mut db = minidb::Database::open(&temp.0)?;
    db.create(schema())?;
    db.insert("items", &[Value::Int(1), Value::String("running")])?;
    db.add_column("items", added())?;
    db.insert(
        "items",
        &[Value::Int(2), Value::String("running"), Value::Null],
    )?;
    let before1 = fs::read(temp.0.join("items"))?;
    let before2 = fs::read(temp.0.join("v2/items"))?;
    assert_eq!(
        db.update(
            "items",
            &minidb::Expr::All,
            &[("name", Value::String("running"))]
        )?,
        0
    );
    assert_eq!(
        db.update("items", &minidb::Expr::All, &[("score", Value::Null)])?,
        0
    );
    assert_eq!(fs::read(temp.0.join("items"))?, before1);
    assert_eq!(fs::read(temp.0.join("v2/items"))?, before2);
    assert_eq!(
        db.update(
            "items",
            &minidb::Expr::All,
            &[("name", Value::String("done"))]
        )?,
        2
    );
    assert_eq!(
        db.update(
            "items",
            &minidb::Expr::All,
            &[("name", Value::String("done"))]
        )?,
        0
    );
    Ok(())
}

#[test]
fn sql_not_uses_three_valued_logic_and_reports_plain_tokens() -> Result<()> {
    let temp = Temp::new()?;
    let mut db = minidb::Database::open(&temp.0)?;
    let mut schema = schema();
    schema.columns[1].data_type.nullable = true;
    db.create(schema)?;
    for (id, name) in [
        (1, Value::Null),
        (2, Value::String("A")),
        (3, Value::String("B")),
    ] {
        db.insert("items", &[Value::Int(id), name])?;
    }
    for (condition, expected) in [
        ("name IS NOT NULL", vec![2, 3]),
        ("NOT name IS NULL", vec![2, 3]),
        ("NOT (name = 'A')", vec![3]),
        ("NOT NOT id = 1", vec![1]),
        ("name != NULL", vec![]),
        ("NOT (name != NULL)", vec![]),
        ("NOT (name = 'A' AND id = 3)", vec![1, 2, 3]),
        ("NOT (name = 'A' OR id = 3)", vec![]),
        ("NOT name = 'A' AND id > 1 OR name IS NULL", vec![1, 3]),
    ] {
        let mut commands = minidb::sql::parse(&format!("SELECT id FROM items WHERE {condition}"))?;
        let minidb::sql::Command::Select {
            condition: parsed, ..
        } = commands.remove(0)
        else {
            unreachable!()
        };
        let schema = db.schema("items")?.clone();
        let filter = parsed.expression(&schema)?;
        let mut ids = Vec::new();
        db.scan("items", &filter, |row| {
            ids.push(row.at(0)?.int()?);
            Ok(())
        })?;
        assert_eq!(ids, expected, "{condition}");
    }
    let bad = minidb::sql::parse("SELECT id FORM items")
        .err()
        .unwrap()
        .to_string();
    assert!(bad.contains("получено FORM"), "{bad}");
    assert!(!bad.contains("Some("));
    assert!(!bad.contains("Word("));
    let eof = minidb::sql::parse("SELECT id FROM items WHERE name IS")
        .err()
        .unwrap()
        .to_string();
    assert!(eof.contains("конец команды"));
    assert!(minidb::sql::parse(&format!(
        "SELECT id FROM items WHERE {}id = 1",
        "NOT ".repeat(66)
    ))
    .is_err());
    Ok(())
}
