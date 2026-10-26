use minidb::{Column, DataType, Database, Expr, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "minidb-backup-check-{}-{}",
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
    Schema::new("items", vec![Column::new("id", DataType::INT)], 0)
}

#[test]
fn backup_and_restore_reject_sources_without_tables_but_allow_empty_tables() -> Result<()> {
    let temp = Temp::new()?;
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    let backup = temp.0.join("empty-backup");
    assert!(db
        .backup(&backup)
        .unwrap_err()
        .to_string()
        .contains("копировать нечего"));
    assert!(!backup.exists());
    db.create(schema())?;
    db.backup(&backup)?; // Таблица без строк — корректный бэкап, в нём есть схема.
    let copy = Database::restore(&backup, temp.0.join("copy.db"), &[])?;
    assert_eq!(copy.table_names()?, ["items"]);
    drop(copy);
    db.insert("items", &[Value::Int(1)])?;
    drop(db);
    let before = fs::read(root.join("items.v1.data"))?;
    let empty = temp.0.join("empty");
    fs::create_dir(&empty)?;
    fs::write(empty.join("README"), "not a table")?;
    assert!(Database::restore(&empty, &root, &[])
        .err()
        .unwrap()
        .to_string()
        .contains("копировать нечего"));
    assert_eq!(fs::read(root.join("items.v1.data"))?, before);
    assert!(!temp.0.join("db.restore.tmp").exists());
    assert!(!temp.0.join("db.restore.old").exists());
    Ok(())
}

#[test]
fn restore_appends_suffixes_to_the_full_directory_name() -> Result<()> {
    let temp = Temp::new()?;
    let mut source = Database::open(temp.0.join("source"))?;
    source.create(schema())?;
    source.insert("items", &[Value::Int(7)])?;
    let backup = temp.0.join("backup");
    source.backup(&backup)?;
    // Старый with_extension конфликтовал бы с этим каталогом для обоих назначений.
    fs::create_dir(temp.0.join("my.restore.tmp"))?;
    fs::write(temp.0.join("my.restore.tmp/sentinel"), "keep")?;
    for name in ["my.db", "my.data"] {
        let mut restored = Database::restore(&backup, temp.0.join(name), &[])?;
        assert_eq!(restored.scan("items", &Expr::All, |_| Ok(()))?, 1);
        assert!(restored.get("items", 7, |_| Ok(()))?.is_some());
        assert!(!temp.0.join(format!("{name}.restore.tmp")).exists());
        assert!(!temp.0.join(format!("{name}.restore.old")).exists());
    }
    assert_eq!(
        fs::read_to_string(temp.0.join("my.restore.tmp/sentinel"))?,
        "keep"
    );
    fs::create_dir(temp.0.join("my.db.restore.tmp"))?;
    assert!(Database::restore(&backup, temp.0.join("my.db"), &[]).is_err());
    assert!(temp.0.join("my.db.restore.tmp").exists());
    Ok(())
}
