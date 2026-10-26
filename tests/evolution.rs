use minidb::layout::Layout;
use minidb::record::{self, Row};
use minidb::schema::check_evolution;
use minidb::table::Table;
use minidb::{codegen, Column, Compare, DataType, Database, Expr, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "minidb-evolution-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn nullable(mut ty: DataType) -> DataType {
    ty.nullable = true;
    ty
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

#[test]
fn history_and_evolution_reject_destructive_changes() -> Result<()> {
    let old = schema();
    let new = old.with_added_column(Column::new("score", nullable(DataType::FLOAT)))?;
    assert_eq!(Schema::from_text(&new.to_text())?, new);
    assert!(new
        .to_text()
        .contains("VERSION 2\nCOLUMNS_AT_VERSION 1 = 2\nCOLUMNS_AT_VERSION 2 = 3"));
    check_evolution(&old, &new)?;
    for variant in 0..9 {
        let mut bad = new.clone();
        match variant {
            0 => {
                bad.columns.remove(0);
            }
            1 => bad.columns[0].name = "renamed".into(),
            2 => bad.columns[1].data_type = DataType::INT,
            3 => bad.columns.swap(0, 1),
            4 => bad.columns[2].data_type.nullable = false,
            5 => bad.version = 1,
            6 => {
                bad.columns_at_version.insert(1, 1);
            }
            7 => bad.primary_key = 2,
            _ => {
                bad.columns[0].data_type = DataType {
                    kind: minidb::Kind::Int { min: 0, max: 10 },
                    nullable: false,
                }
            }
        }
        assert!(check_evolution(&old, &bad).is_err(), "variant {variant}");
    }
    assert!(old
        .with_added_column(Column::new("required", DataType::INT))
        .is_err());
    for text in [
        new.to_text().replace("VERSION 2", "VERSION 0"),
        new.to_text().replace("COLUMNS_AT_VERSION 1 = 2\n", ""),
        new.to_text()
            .replace("COLUMNS_AT_VERSION 2 = 3", "COLUMNS_AT_VERSION 2 = 2"),
        new.to_text() + "COLUMNS_AT_VERSION 1 = 2\n",
    ] {
        assert!(Schema::from_text(&text).is_err());
    }
    for sql in [
        "ALTER TABLE items ADD COLUMN null INT;",
        "ALTER TABLE items ADD COLUMN parent INT REFERENCES null(id);",
    ] {
        assert!(minidb::sql::parse(sql).is_err());
    }
    let legacy = "CREATE TABLE items (id INT PRIMARY KEY, name STRING NOT NULL);";
    assert_eq!(Schema::from_text(legacy)?.version, 1);
    Ok(())
}

#[test]
fn old_rows_default_null_and_bitmap_growth_uses_segment_offsets() -> Result<()> {
    let mut old = schema();
    for i in 2..8 {
        old.columns
            .push(Column::new(&format!("c{i}"), DataType::INT));
    }
    let new = old.with_added_column(Column::new("accuracy", nullable(DataType::FLOAT)))?;
    let old_layout = Layout::from_schema(&old)?;
    let new_layout = Layout::from_schema(&new)?;
    assert_eq!((old_layout.header, new_layout.header), (2, 3));
    old_layout.validate(&new)?;
    let values = [
        Value::Int(1),
        Value::String("old"),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
        Value::Int(5),
        Value::Int(6),
        Value::Int(7),
    ];
    let mut bytes = vec![0; old_layout.row_size];
    record::encode(&old_layout, &old, &values, &mut bytes)?;
    let row = Row {
        bytes: &bytes,
        layout: &old_layout,
        schema: &new,
    };
    assert_eq!(row.at(8)?, Value::Null);
    assert!(row.at(9).is_err());
    assert_eq!(row.at(7)?, Value::Int(7));
    row.validate()?;
    // Префикс разрешён для чтения; кодирование требует полного текущего Layout.
    let before = bytes.clone();
    let mut all = values.to_vec();
    all.push(Value::Null);
    assert!(record::encode(&old_layout, &new, &all, &mut bytes).is_err());
    assert_eq!(bytes, before);
    Ok(())
}

#[test]
fn migration_scan_update_delete_reopen_and_backup_use_all_segments() -> Result<()> {
    let temp = Temp::new();
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    db.create(schema())?;
    db.insert("items", &[Value::Int(1), Value::String("first")])?;
    db.insert("items", &[Value::Int(2), Value::String("second")])?;
    db.insert("items", &[Value::Int(3), Value::String("deleted")])?;
    assert_eq!(
        db.delete("items", &Expr::Compare("id", Compare::Eq, Value::Int(3)))?,
        1
    );
    let v1 = fs::read(root.join("items"))?;
    db.add_column("items", Column::new("score", nullable(DataType::FLOAT)))?;
    assert_eq!(fs::read(root.join("items"))?, v1);
    db.insert_versioned(
        "items",
        2,
        &[Value::Int(4), Value::String("new"), Value::Float(0.5)],
    )?;
    assert_eq!(fs::read(root.join("items"))?, v1); // Старые free-слоты не используются.
    assert!(db
        .insert_versioned("items", 1, &[Value::Int(5), Value::String("stale")])
        .is_err());
    assert_eq!(db.scan("items", &Expr::IsNull("score"), |_| Ok(()))?, 2);
    assert_eq!(
        db.update("items", &Expr::All, &[("score", Value::Float(0.75))])?,
        3
    );
    let dead = fs::read_to_string(root.join("items"))?;
    let rows: Vec<_> = dead.lines().filter(|line| !line.starts_with('#')).collect();
    assert!(rows.iter().all(|line| line.starts_with("DEAD\t")));
    let old = std::str::from_utf8(&v1)?;
    for (old, new) in old.lines().filter(|line| !line.starts_with('#')).zip(rows) {
        assert_eq!(
            old.split_once('\t').unwrap().1,
            new.split_once('\t').unwrap().1
        );
    }
    db.update("items", &Expr::All, &[("score", Value::Float(1.0))])?;
    assert_eq!(
        fs::read_to_string(root.join("v2/items"))?
            .lines()
            .filter(|line| line.starts_with("LIVE\t"))
            .count(),
        3
    );
    db.add_column("items", Column::new("note", nullable(DataType::STRING)))?;
    db.insert(
        "items",
        &[
            Value::Int(6),
            Value::String("last"),
            Value::Null,
            Value::Null,
        ],
    )?;
    assert_eq!(db.scan("items", &Expr::IsNull("note"), |_| Ok(()))?, 4);
    db.backup("snapshot")?;
    drop(db);
    let mut table = Table::open(&root, "items")?;
    assert_eq!(table.segments().len(), 3);
    table.rebuild_index()?;
    assert_eq!(table.check()?, 4);
    assert!(table.remove(1)?);
    assert!(table.get(1)?.is_none());
    drop(table);
    let mut restored = Database::restore(
        temp.0.join("db_backups/snapshot"),
        temp.0.join("restored"),
        &[],
    )?;
    assert_eq!(restored.scan("items", &Expr::All, |r| r.validate())?, 4);
    assert_eq!(
        restored.get("items", 1, |r| Ok(
            r.at(2)? == Value::Float(1.0) && r.at(3)? == Value::Null
        ))?,
        Some(true)
    );
    Ok(())
}

#[test]
fn compiled_old_and_new_consumers_share_bytes_and_reject_stale_writes() -> Result<()> {
    let temp = Temp::new();
    let source = temp.0.join("consumer");
    fs::create_dir_all(source.join("src/bin"))?;
    let mut old = schema();
    for i in 2..8 {
        old.columns
            .push(Column::new(&format!("c{i}"), DataType::INT));
    }
    let new = old.with_added_column(Column::new("accuracy", nullable(DataType::FLOAT)))?;
    fs::write(source.join("Cargo.toml"), format!("[package]\nname = \"compat-consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nminidb = {{ path = {:?} }}\n", env!("CARGO_MANIFEST_DIR")))?;
    let common = r#"
fn main() -> minidb::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut db = minidb::Database::open(args.next().unwrap())?;
    let old = db.get("items", 1, Item::from_row)?.unwrap();
    assert_eq!(old.name, "old");
    assert_eq!(old.c7, 7);
    if args.next().as_deref() == Some("before") { return Ok(()); }
    let new = db.get("items", 2, Item::from_row)?.unwrap();
    assert_eq!(new.name, "new");
"#;
    fs::write(source.join("src/bin/old.rs"), format!("{}\n{common}\nassert!(new.insert(&mut db).unwrap_err().to_string().contains(\"VERSION\"));\nassert!(new.update(&mut db, 2).unwrap_err().to_string().contains(\"VERSION\"));\nOk(())\n}}", codegen::generate(&old)?))?;
    fs::write(source.join("src/bin/new.rs"), format!("{}\n{common}\nassert_eq!(old.accuracy, None);\nassert_eq!(new.accuracy, Some(0.75));\nlet mut visited = 0;\nassert_eq!(Item::scan(&mut db, &minidb::Expr::All, |item| {{ if item.id == 1 {{ assert_eq!(item.accuracy, None); }} visited += 1; Ok(()) }})?, 2);\nassert_eq!(visited, 2);\nlet copy = Item {{ id: 3, ..new }};\ncopy.insert(&mut db)?;\nassert_eq!(db.get(\"items\", 3, Item::from_row)?.unwrap().accuracy, Some(0.75));\nlet changed = Item {{ id: 0, name: \"updated\".into(), ..copy }};\nassert_eq!(changed.update(&mut db, 3)?, 1);\nlet saved = db.get(\"items\", 3, Item::from_row)?.unwrap();\nassert_eq!(saved.id, 3);\nassert_eq!(saved.name, \"updated\");\nassert!(db.get(\"items\", 0, Item::from_row)?.is_none());\nOk(())\n}}", codegen::generate(&new)?))?;
    // Rust-ключевые слова и имена стандартных типов не ломают генерацию.
    let names = source.join("schemas");
    fs::create_dir(&names)?;
    for table in [
        "strings", "oks", "somes", "self", "_1rows", "status", "address", "progress",
    ] {
        let schema = Schema::new(
            table,
            vec![
                Column::new("type", DataType::INT),
                Column::new("self", nullable(DataType::STRING)),
                Column::new("value", nullable(DataType::INT)),
                Column::new("score", nullable(DataType::FLOAT)),
            ],
            0,
        );
        fs::write(names.join(format!("{table}.schema.sql")), schema.to_text())?;
    }
    let generated = codegen::generate_dir(&names)?;
    assert_eq!(generated, codegen::generate_dir(&names)?);
    assert_eq!(fs::read_dir(&names)?.count(), 8); // Генерация не открывает/создаёт БД.
    fs::write(
        source.join("src/bin/names.rs"),
        format!("{generated}\nfn main() {{ let _: status::Status = Default::default(); let _: address::Address = Default::default(); let _: progress::Progress = Default::default(); }}"),
    )?;
    let target = source.join("target");
    let output = Command::new(env!("CARGO"))
        .args(["build", "--offline", "--quiet", "--manifest-path"])
        .arg(source.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    db.create(old)?;
    db.insert(
        "items",
        &[
            Value::Int(1),
            Value::String("old"),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
            Value::Int(6),
            Value::Int(7),
        ],
    )?;
    drop(db);
    let run = |name: &str, before: bool| -> Result<()> {
        let mut cmd = Command::new(
            target
                .join("debug")
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
        );
        cmd.arg(&root);
        if before {
            cmd.arg("before");
        }
        let out = cmd.output()?;
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    };
    run("old", true)?;
    // Новый потребитель отличает откат каталога от отсутствующих полей старой строки.
    let too_new = Command::new(
        target
            .join("debug")
            .join(format!("new{}", std::env::consts::EXE_SUFFIX)),
    )
    .arg(&root)
    .arg("before")
    .output()?;
    assert!(!too_new.status.success());
    assert!(String::from_utf8_lossy(&too_new.stderr).contains("более новую VERSION"));
    let mut db = Database::open(&root)?;
    db.add_column("items", new.columns[8].clone())?;
    db.insert(
        "items",
        &[
            Value::Int(2),
            Value::String("new"),
            Value::Int(12),
            Value::Int(13),
            Value::Int(14),
            Value::Int(15),
            Value::Int(16),
            Value::Int(17),
            Value::Float(0.75),
        ],
    )?;
    drop(db);
    run("old", false)?; // Тот же исполняемый файл; после ALTER его не пересобирали.
    run("new", false)?;
    Ok(())
}
