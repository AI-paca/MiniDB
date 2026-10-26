use minidb::{codegen, Column, DataType, Database, Expr, Result, Schema, Value};
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
        Schema::from_text(&fs::read_to_string(root.join("notes_schema"))?)?.version,
        2
    );
    assert!(!root.join("notes_1").exists());
    assert!(root.join("notes_2").is_file());
    for path in [".notes", ".notes.v1", "notes.schema", "notes.data"] {
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
    assert_eq!(filenames, ["notes_2", "notes_schema"]);
    assert_eq!(
        fs::read(root.join("notes_2"))?,
        fs::read(backup.join("notes_2"))?
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
