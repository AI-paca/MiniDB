use minidb::layout::Layout;
use minidb::record::{self, Row};
use minidb::table::Table;
use minidb::{Column, Compare, DataType, Database, Expr, Kind, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "minidb-architecture-{}-{}",
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
    let name = DataType {
        kind: Kind::String { max_bytes: 16 },
        nullable: true,
    };
    Schema::new(
        "items",
        vec![
            Column::new("id", DataType::INT),
            Column::new("name", name),
            Column::new("acc", DataType::FLOAT),
        ],
        0,
    )
}
fn values(id: i64) -> [Value<'static>; 3] {
    [Value::Int(id), Value::String("имя"), Value::Float(0.5)]
}

#[test]
fn a_schema_text_file_roundtrip() -> Result<()> {
    let temp = Temp::new()?;
    let mut original = schema();
    original.name = "MixedCase".into();
    original.columns[2].references = Some(minidb::Reference {
        table: "Parent".into(),
        column: "pk".into(),
    });
    original.validator = Some("rule_v1".into());
    let table = Table::create(&temp.0, original.clone())?;
    let text = fs::read_to_string(temp.0.join("MixedCase_schema"))?;
    let decoded = Schema::from_text(&text)?;
    assert_eq!(decoded, original);
    assert_eq!(decoded.to_text(), original.to_text());
    assert_eq!(decoded.name, original.name);
    assert_eq!(decoded.primary_key, original.primary_key);
    assert_eq!(decoded.validator, original.validator);
    for (a, b) in decoded.columns.iter().zip(&original.columns) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.data_type, b.data_type);
        assert_eq!(
            a.references.as_ref().map(|r| (&r.table, &r.column)),
            b.references.as_ref().map(|r| (&r.table, &r.column))
        );
    }
    assert!(temp.0.join("MixedCase_schema").exists());
    assert!(Table::create(&temp.0, original).is_err());
    assert_eq!(fs::read_to_string(temp.0.join("MixedCase_schema"))?, text);
    drop(table);
    assert_eq!(Table::open(&temp.0, "MixedCase")?.schema().to_text(), text);
    assert!(Schema::from_text("broken schema").is_err());
    Ok(())
}

#[test]
fn b_layout_offsets_and_bitmap() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    assert_eq!(layout.row_size, 36);
    assert_eq!(layout.null_bytes, 1);
    assert_eq!(layout.header, 2);
    let offsets: Vec<usize> = layout.cols.iter().map(|c| c.offset).collect();
    assert_eq!(offsets, [2, 10, 28]);
    assert_eq!(schema.columns[1].width()?, 18);
    let mut many = Schema::new("many", vec![Column::new("id", DataType::INT)], 0);
    for i in 1..9 {
        many.columns.push(Column::new(
            &format!("c{i}"),
            DataType {
                nullable: true,
                ..DataType::INT
            },
        ));
    }
    let layout = Layout::from_schema(&many)?;
    assert_eq!(layout.null_bytes, 2);
    assert_eq!(layout.cols[8].offset, 67);
    let mut vals = [Value::Null; 9];
    vals[0] = Value::Int(1);
    let mut bytes = vec![0; layout.row_size];
    record::encode(&layout, &many, &vals, &mut bytes)?;
    assert_eq!(bytes[2] & 1, 1);
    assert_eq!(
        Row {
            bytes: &bytes,
            schema: &many,
            layout: &layout
        }
        .at(8)?,
        Value::Null
    );
    Ok(())
}

#[test]
fn c_record_roundtrip_validation_and_direct_access() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let mut out = vec![0xaa; layout.row_size];
    for vals in [values(7), [Value::Int(8), Value::Null, Value::Float(-1.25)]] {
        record::encode(&layout, &schema, &vals, &mut out)?;
        let row = Row {
            bytes: &out,
            layout: &layout,
            schema: &schema,
        };
        assert!(row.live());
        for (i, value) in vals.iter().enumerate() {
            assert_eq!(row.at(i)?, *value);
        }
        assert_eq!(row.get("name")?, vals[1]);
    }
    let before = out.clone();
    for vals in [
        [Value::Null, Value::Null, Value::Float(0.0)],
        [
            Value::Int(1),
            Value::String("12345678901234567"),
            Value::Float(0.0),
        ],
        [Value::Int(1), Value::Null, Value::Float(f64::NAN)],
    ] {
        assert!(record::encode(&layout, &schema, &vals, &mut out).is_err());
        assert_eq!(out, before);
    }
    assert!(record::encode(&layout, &schema, &[], &mut out).is_err());
    assert_eq!(out, before);
    assert!(record::encode(&layout, &schema, &values(1), &mut out[..10]).is_err());
    assert_eq!(out, before);
    let wide = Schema::new(
        "wide",
        vec![
            Column::new("id", DataType::INT),
            Column::new("a", DataType::STRING),
            Column::new("b", DataType::STRING),
            Column::new("c", DataType::STRING),
            Column::new("last", DataType::INT),
        ],
        0,
    );
    let layout = Layout::from_schema(&wide)?;
    let mut bytes = vec![0; layout.row_size];
    record::encode(
        &layout,
        &wide,
        &[
            Value::Int(1),
            Value::String("a"),
            Value::String("b"),
            Value::String("c"),
            Value::Int(42),
        ],
        &mut bytes,
    )?;
    // Проверяем чтение пятого поля при повреждённом предыдущем поле
    bytes[layout.cols[1].offset + 2] = 0xff;
    let row = Row {
        bytes: &bytes,
        schema: &wide,
        layout: &layout,
    };
    assert!(row.at(1).is_err());
    assert_eq!(row.at(4)?, Value::Int(42));
    assert!(row.at(5).is_err());
    let before = bytes.clone();
    let long = "x".repeat(65_536);
    assert!(record::encode(
        &layout,
        &wide,
        &[
            Value::Int(1),
            Value::String(&long),
            Value::String(""),
            Value::String(""),
            Value::Int(42)
        ],
        &mut bytes
    )
    .is_err());
    assert_eq!(bytes, before);
    Ok(())
}

#[test]
fn e_foreign_keys_create_insert_update_and_restrict() -> Result<()> {
    let temp = Temp::new()?;
    let root = temp.0.join("db");
    let output = std::process::Command::new("sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/run.sh"))
        .arg(&root)
        .env("MINIDB_BIN", env!("CARGO_BIN_EXE_minidb"))
        .stdin(fs::File::open(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/sample"
        ))?)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut db = Database::open(&root)?;
    let bad = Schema::new(
        "bad",
        vec![
            Column::new("id", DataType::INT),
            Column::new("dataset_id", DataType::INT).references("datasets", "missing"),
        ],
        0,
    );
    assert!(db.create_table(bad).is_err());
    let bad = Schema::new(
        "bad",
        vec![
            Column::new("id", DataType::INT),
            Column::new("dataset_id", DataType::INT).references("datasets", "size"),
        ],
        0,
    );
    assert!(db.create_table(bad).is_err());
    let bad = Schema::new(
        "bad",
        vec![Column::new("id", DataType::INT).references("absent", "id")],
        0,
    );
    assert!(db.create_table(bad).is_err());
    assert!(!root.join("bad_schema").exists());
    let error = db
        .insert(
            "experiments",
            &[
                Value::Null,
                Value::Int(99),
                Value::Int(1),
                Value::Float(0.9),
                Value::String("running"),
            ],
        )
        .err()
        .ok_or("Ожидалась ошибка FK")?;
    assert_eq!(
        error.to_string(),
        "FOREIGN KEY: experiments.dataset_id = 99, в datasets нет id = 99"
    );
    assert!(db
        .update("experiments", &Expr::All, &[("dataset_id", Value::Int(99))])
        .is_err());
    let dataset = Expr::Compare("id", Compare::Eq, Value::Int(1));
    assert!(db.delete("datasets", &dataset).is_err());
    assert!(db
        .update("datasets", &dataset, &[("id", Value::Int(99))])
        .is_err());
    assert_eq!(
        db.delete(
            "experiments",
            &Expr::Compare("dataset_id", Compare::Eq, Value::Int(1))
        )?,
        5
    );
    assert_eq!(db.delete("datasets", &dataset)?, 1);
    let child = Schema::new(
        "nullable_child",
        vec![
            Column::new("id", DataType::INT),
            Column::new(
                "parent",
                DataType {
                    nullable: true,
                    ..DataType::INT
                },
            )
            .references("datasets", "id"),
        ],
        0,
    );
    db.create_table(child)?;
    db.insert("nullable_child", &[Value::Null, Value::Null])?;
    let shared = Schema::new(
        "shared_pk",
        vec![Column::new("id", DataType::INT).references("datasets", "id")],
        0,
    );
    db.create_table(shared)?;
    // Проверяем внешний ключ после подстановки авто-ID: dataset 1 уже удалён
    assert!(db.insert("shared_pk", &[Value::Null]).is_err());
    drop(db);
    let mut db = Database::open(&root)?;
    assert_eq!(db.scan("datasets", &Expr::All, |_| Ok(()))?, 4);
    assert_eq!(db.scan("experiments", &Expr::All, |_| Ok(()))?, 15);
    Ok(())
}

#[test]
fn e_backup_restore_and_failed_copy_preserve_root() -> Result<()> {
    let temp = Temp::new()?;
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    db.create_table(schema())?;
    db.insert("items", &values(1))?;
    db.insert("items", &values(2))?;
    db.delete("items", &Expr::Compare("id", Compare::Eq, Value::Int(2)))?;
    db.backup("snapshot")?;
    let backup = temp.0.join("db_backups/snapshot");
    let saved = fs::read(root.join("items_1"))?;
    assert_eq!(fs::read(backup.join("items_1"))?, saved);
    assert_eq!(
        fs::read(backup.join("items_schema"))?,
        fs::read(root.join("items_schema"))?
    );
    assert!(db.backup("snapshot").is_err());
    db.insert("items", &values(3))?;
    assert!(Database::restore("snapshot", &root, &[]).is_err());
    drop(db);
    // Проверяем сохранность базы при ошибке восстановления
    let current = fs::read(root.join("items_1"))?;
    assert!(Database::restore(temp.0.join("missing"), &root, &[]).is_err());
    assert_eq!(fs::read(root.join("items_1"))?, current);
    let broken = temp.0.join("broken");
    fs::create_dir(&broken)?;
    fs::copy(backup.join("items_schema"), broken.join("items_schema"))?;
    fs::write(broken.join("items_1"), b"partial")?;
    assert!(Database::restore(&broken, &root, &[]).is_err());
    assert_eq!(fs::read(root.join("items_1"))?, current);
    let legacy = temp.0.join("legacy");
    fs::create_dir(&legacy)?;
    fs::write(legacy.join("journal.log"), b"old format")?;
    assert!(Database::restore(&legacy, &root, &[]).is_err());
    assert_eq!(fs::read(root.join("items_1"))?, current);
    let mut restored = Database::restore("snapshot", &root, &[])?;
    assert_eq!(fs::read(root.join("items_1"))?, saved);
    assert_eq!(restored.scan("items", &Expr::All, |_| Ok(()))?, 1);
    assert!(restored.get("items", 1, |_| Ok(()))?.is_some());
    assert!(restored.get("items", 2, |_| Ok(()))?.is_none());
    let new_root = temp.0.join("new");
    let mut copy = Database::restore(&backup, &new_root, &[])?;
    assert_eq!(copy.scan("items", &Expr::All, |_| Ok(()))?, 1);
    Ok(())
}

#[test]
fn update_null_pk_preserves_id_and_remove_reports_presence() -> Result<()> {
    let temp = Temp::new()?;
    let mut table = Table::create(&temp.0, schema())?;
    table.insert(&values(3))?;
    table.insert(&values(100))?;
    table.update(3, &[Value::Null, Value::String("done"), Value::Float(0.8)])?;
    let row = table.get(3)?.ok_or("UPDATE потерял PK")?;
    assert_eq!(row.get("name")?, Value::String("done"));
    assert!(table.get(101)?.is_none());
    assert!(table.update(3, &[]).is_err());
    assert!(!table.remove(999)?);
    assert!(table.remove(3)?);
    assert!(!table.remove(3)?);
    assert_eq!(table.insert(&values(4))?, 0);
    assert_eq!(table.insert(&values(5))?, 72);
    table.insert(&values(i64::MAX))?;
    table.update(4, &[Value::Null, Value::Null, Value::Float(1.0)])?;
    assert!(table.get(4)?.is_some());
    let before = fs::read(temp.0.join("items_1"))?;
    assert!(table
        .insert(&[Value::Null, Value::Null, Value::Float(0.0)])
        .is_err());
    assert_eq!(fs::read(temp.0.join("items_1"))?, before);
    Ok(())
}

#[test]
fn high_water_survives_slot_reuse_lower_pk_and_backup() -> Result<()> {
    let temp = Temp::new()?;
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    db.create(schema())?;
    db.insert("items", &values(100))?;
    assert_eq!(db.delete("items", &Expr::All)?, 1);
    db.insert("items", &values(1))?;
    assert_eq!(
        fs::read_to_string(root.join("items_1"))?
            .lines()
            .filter(|l| l.starts_with("LIVE\t"))
            .count(),
        1
    );
    db.backup("snapshot")?;
    drop(db);
    let mut db = Database::open(&root)?;
    assert_eq!(
        db.insert("items", &[Value::Null, Value::Null, Value::Float(0.0)])?,
        101
    );
    db.update(
        "items",
        &Expr::Compare("id", Compare::Eq, Value::Int(101)),
        &[("id", Value::Int(2))],
    )?;
    drop(db);
    let mut db = Database::open(&root)?;
    assert_eq!(
        db.insert("items", &[Value::Null, Value::Null, Value::Float(0.0)])?,
        102
    );
    let mut restored = Database::restore(
        temp.0.join("db_backups/snapshot"),
        temp.0.join("restored"),
        &[],
    )?;
    assert_eq!(
        restored.insert("items", &[Value::Null, Value::Null, Value::Float(0.0)])?,
        101
    );
    Ok(())
}
