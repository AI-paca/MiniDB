use minidb::layout::Layout;
use minidb::record;
use minidb::table::Table;
use minidb::{codegen, Column, Compare, DataType, Database, Expr, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "minidb-names-{}-{}",
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
        "notes",
        vec![
            Column::new("id", DataType::INT),
            Column::new("name", DataType::STRING),
        ],
        0,
    )
}
fn score() -> Column {
    Column::new(
        "score",
        DataType {
            nullable: true,
            ..DataType::FLOAT
        },
    )
}

#[test]
fn sql_schema_discovery_codegen_backup_and_restore_use_exact_suffixes() -> Result<()> {
    let temp = Temp::new()?;
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    db.create(schema())?;
    db.insert("notes", &[Value::Int(1), Value::String("saved")])?;
    db.add_column("notes", score())?;
    db.insert(
        "notes",
        &[Value::Int(2), Value::String("new"), Value::Float(0.5)],
    )?;
    db.sync()?;
    assert_eq!(
        Schema::from_text(&fs::read_to_string(root.join("notes.schema.sql"))?)?.version,
        2
    );
    assert!(root.join("notes.v1.data").is_file());
    assert!(root.join("notes.v2.data").is_file());
    for path in ["notes", ".notes", ".notes.v1", "notes.schema", "notes.data"] {
        assert!(!root.join(path).exists(), "{path}");
    }
    // Произвольные SQL, временные файлы, README и dot-файлы не объявляют таблицы.
    for (filename, text) in [
        ("README", "table notes"),
        ("Cargo.toml", "[package]"),
        ("migration.sql", "DROP TABLE notes;"),
        ("notes.version_backup.data", "not a segment"),
        ("notes.v.data", "not a version"),
        ("notes.schema.tmp", "incomplete"),
        ("notes.schema.sql.tmp", "incomplete"),
        (".editorconfig", "root = true"),
    ] {
        fs::write(root.join(filename), text)?;
    }
    let generated = codegen::generate_dir(&root)?;
    assert_eq!(generated.matches("pub struct Note").count(), 1);
    db.backup("snapshot")?;
    let backup = temp.0.join("db_backups/snapshot");
    let mut filenames = fs::read_dir(&backup)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    filenames.sort();
    assert_eq!(
        filenames,
        ["notes.schema.sql", "notes.v1.data", "notes.v2.data"]
    );
    assert_eq!(
        fs::read(root.join("notes.v1.data"))?,
        fs::read(backup.join("notes.v1.data"))?
    );
    drop(db);
    let mut reopened = Database::open(&root)?;
    assert_eq!(reopened.table_names()?, ["notes"]);
    assert_eq!(reopened.scan("notes", &Expr::All, |r| r.validate())?, 2);
    drop(reopened);
    let mut restored = Database::restore(&backup, temp.0.join("restored"), &[])?;
    assert_eq!(
        restored.get("notes", 1, |r| Ok(r.get("score")? == Value::Null))?,
        Some(true)
    );
    assert_eq!(
        restored.get("notes", 2, |r| Ok(r.get("score")? == Value::Float(0.5)))?,
        Some(true)
    );
    assert_eq!(codegen::generate_dir(backup)?, generated);
    Ok(())
}

#[test]
fn legacy_schema_and_segment_names_remain_readable_and_restorable() -> Result<()> {
    for filename in ["notes.data", "notes.v1.data"] {
        let temp = Temp::new()?;
        let root = temp.0.join("db");
        fs::create_dir(&root)?;
        let schema = schema();
        let layout = Layout::from_schema(&schema)?;
        let mut bytes = vec![0; layout.row_size];
        record::encode(
            &layout,
            &schema,
            &[Value::Int(1), Value::String("legacy")],
            &mut bytes,
        )?;
        fs::write(root.join("notes.schema"), schema.to_text())?;
        fs::write(root.join(filename), &bytes)?;
        let generated = codegen::generate_dir(&root)?;
        let mut db = Database::open(&root)?;
        assert_eq!(
            db.get(
                "notes",
                1,
                |r| Ok(r.get("name")? == Value::String("legacy"))
            )?,
            Some(true)
        );
        db.add_column("notes", score())?;
        assert_eq!(fs::read(root.join(filename))?, bytes);
        db.update(
            "notes",
            &Expr::Compare("id", Compare::Eq, Value::Int(1)),
            &[("score", Value::Float(0.8))],
        )?;
        assert!(root.join("notes.v2.data").is_file());
        db.backup("snapshot")?;
        let backup = temp.0.join("db_backups/snapshot");
        assert!(backup.join(filename).exists());
        assert!(backup.join("notes.v2.data").exists());
        assert!(backup.join("notes.schema").exists());
        assert_ne!(codegen::generate_dir(&root)?, generated);
        drop(db);
        let mut restored = Database::restore(&backup, temp.0.join("restored"), &[])?;
        assert_eq!(
            restored.get("notes", 1, |r| Ok(r.get("score")? == Value::Float(0.8)))?,
            Some(true)
        );
    }
    Ok(())
}

#[test]
fn duplicate_schemas_and_segments_are_rejected_without_overwriting_data() -> Result<()> {
    let temp = Temp::new()?;
    let mut table = Table::create(&temp.0, schema())?;
    table.insert(&[Value::Int(1), Value::String("saved")])?;
    drop(table);
    let schema_bytes = fs::read(temp.0.join("notes.schema.sql"))?;
    let data = fs::read(temp.0.join("notes.v1.data"))?;
    fs::write(temp.0.join("notes.schema"), &schema_bytes)?;
    assert!(Database::open(&temp.0).is_err());
    assert!(codegen::generate_dir(&temp.0).is_err());
    fs::remove_file(temp.0.join("notes.schema"))?;
    fs::write(temp.0.join("notes.data"), &data)?;
    assert!(Database::open(&temp.0).is_err());
    fs::remove_file(temp.0.join("notes.data"))?;
    fs::write(temp.0.join("notes.v99.data"), [])?;
    assert!(Database::open(&temp.0).is_err());
    assert_eq!(fs::read(temp.0.join("notes.schema.sql"))?, schema_bytes);
    assert_eq!(fs::read(temp.0.join("notes.v1.data"))?, data);
    let orphan = Temp::new()?;
    fs::write(orphan.0.join("notes.v1.data"), &data)?;
    assert!(Table::create(&orphan.0, schema()).is_err());
    assert!(!orphan.0.join("notes.schema.sql").exists());
    assert_eq!(fs::read(orphan.0.join("notes.v1.data"))?, data);
    Ok(())
}
