use minidb::table::Table;
use minidb::{Column, DataType, Database, Expr, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "minidb-text-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
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

#[test]
fn utf8_files_are_the_storage_and_ide_edits_are_read_after_reopen() -> Result<()> {
    let temp = Temp::new()?;
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    let message = "Русский\t\n\r\0\\\"' \u{1b}\u{85}";
    db.insert("items", &[Value::Int(1), Value::String(message)])?;
    db.insert("items", &[Value::Int(2), Value::String("edit me")])?;
    let path = temp.0.join("items");
    let text = fs::read_to_string(&path)?;
    assert!(text.contains("# state\tid\tname\nLIVE\t1\t\"Русский"));
    assert!(text.contains("\\u{0}"));
    assert!(text.contains("\\u{1b}"));
    assert!(!text.contains('\0'));
    assert!(!temp.0.join("items.v1.data").exists());
    drop(db);
    fs::write(&path, text.replace("\"edit me\"", "\"edited in IDE\""))?;
    let mut db = Database::open(&temp.0)?;
    assert_eq!(
        db.get("items", 1, |r| Ok(r.get("name")? == Value::String(message)))?,
        Some(true)
    );
    assert_eq!(
        db.get("items", 2, |r| Ok(
            r.get("name")? == Value::String("edited in IDE")
        ))?,
        Some(true)
    );
    assert_eq!(
        db.insert("items", &[Value::Null, Value::String("next")])?,
        3
    );
    assert!(fs::read_to_string(path)?.contains("LIVE\t3\t\"next\""));
    Ok(())
}

#[test]
fn seed_evolution_and_migration_use_readable_files_in_version_directories() -> Result<()> {
    let temp = Temp::new()?;
    let root = temp.0.join("db");
    let mut db = Database::open(&root)?;
    db.set_write_barriers(false)?;
    db.create(schema())?;
    db.insert("items", &[Value::Int(1), Value::String("old")])?;
    db.add_column(
        "items",
        Column::new(
            "score",
            DataType {
                nullable: true,
                ..DataType::FLOAT
            },
        ),
    )?;
    db.insert(
        "items",
        &[Value::Int(2), Value::String("new"), Value::Float(0.5)],
    )?;
    assert_eq!(
        db.get("items", 1, |r| Ok(r.get("score")? == Value::Null))?,
        Some(true)
    );
    db.update("items", &Expr::All, &[("score", Value::Float(1.0))])?;
    db.set_write_barriers(true)?;
    assert!(fs::read_to_string(root.join("items"))?.contains("DEAD\t1\t\"old\""));
    assert!(fs::read_to_string(root.join("v2/items"))?.contains("LIVE\t1\t\"old\"\t1.0"));
    db.backup("snapshot")?;
    let mut restored =
        Database::restore(temp.0.join("db_backups/snapshot"), temp.0.join("copy"), &[])?;
    assert_eq!(
        restored.scan("items", &Expr::All, |r| {
            r.validate()?;
            assert_eq!(r.get("score")?, Value::Float(1.0));
            Ok(())
        })?,
        2
    );
    assert_eq!(
        fs::read_to_string(root.join("v2/items"))?,
        fs::read_to_string(temp.0.join("copy/v2/items"))?
    );
    Ok(())
}

#[test]
fn malformed_text_is_rejected_with_a_path_and_never_rewritten() -> Result<()> {
    let temp = Temp::new()?;
    drop(Table::create(&temp.0, schema())?);
    let path = temp.0.join("items");
    for text in [
        "broken".to_owned(),
        "# MiniDB text; VERSION 2\n".into(),
        "# MiniDB text; VERSION 1\nMISSING\t1\t\"name\"\n".into(),
        "# MiniDB text; VERSION 1\nLIVE\t1\n".into(),
        "# MiniDB text; VERSION 1\nLIVE\twrong\t\"name\"\n".into(),
        "# MiniDB text; VERSION 1\nLIVE\t1\t\"\\u{zz}\"\n".into(),
        format!(
            "# MiniDB text; VERSION 1\nLIVE\t1\t\"{}\"\n",
            "x".repeat(65)
        ),
    ] {
        fs::write(&path, &text)?;
        let error = match Table::open(&temp.0, "items") {
            Ok(_) => panic!("accepted {text:?}"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(&path.display().to_string()));
        assert_eq!(fs::read_to_string(&path)?, text);
    }
    fs::write(&path, [0xff])?;
    assert!(Table::open(&temp.0, "items").is_err());
    assert_eq!(fs::read(path)?, [0xff]);
    Ok(())
}

#[test]
fn temporary_file_collision_preserves_both_data_and_the_existing_temporary_file() -> Result<()> {
    let temp = Temp::new()?;
    let mut table = Table::create(&temp.0, schema())?;
    table.insert(&[Value::Int(7), Value::String("saved")])?;
    let before = fs::read(temp.0.join("items"))?;
    fs::write(temp.0.join("items.tmp"), "editor work")?;
    assert!(table
        .insert(&[Value::Int(8), Value::String("new")])
        .is_err());
    assert_eq!(fs::read(temp.0.join("items"))?, before);
    assert_eq!(fs::read_to_string(temp.0.join("items.tmp"))?, "editor work");
    assert!(table.get(7).is_err());
    fs::remove_file(temp.0.join("items.tmp"))?;
    table.rebuild_index()?;
    assert!(table.get(7)?.is_some());
    assert!(table.get(8)?.is_none());
    Ok(())
}

#[test]
fn unknown_versions_and_duplicate_schemas_are_not_silently_ignored() -> Result<()> {
    let temp = Temp::new()?;
    drop(Table::create(&temp.0, schema())?);
    fs::copy(temp.0.join("items.schema.sql"), temp.0.join("items.schema"))?;
    assert!(Database::open(&temp.0).is_err());
    fs::remove_file(temp.0.join("items.schema"))?;
    fs::create_dir(temp.0.join("v9"))?;
    fs::write(temp.0.join("v9/items"), "# MiniDB text; VERSION 9\n")?;
    assert!(Table::open(&temp.0, "items")
        .err()
        .unwrap()
        .to_string()
        .contains("истории схемы"));
    assert!(temp.0.join("v9/items").exists());
    Ok(())
}

#[test]
fn binary_files_are_refused_without_creating_a_text_shadow() -> Result<()> {
    let temp = Temp::new()?;
    fs::write(temp.0.join("items.schema.sql"), schema().to_text())?;
    let binary = [1, 0, 1, 0, 0, 0];
    fs::write(temp.0.join("items.v1.data"), binary)?;
    assert!(Database::open(&temp.0)
        .err()
        .unwrap()
        .to_string()
        .contains("ветке binary"));
    assert_eq!(fs::read(temp.0.join("items.v1.data"))?, binary);
    assert!(!temp.0.join("items").exists());
    Ok(())
}

#[test]
fn reading_does_not_apply_narrowed_constraints_but_check_does() -> Result<()> {
    let temp = Temp::new()?;
    let mut table = Table::create(&temp.0, schema())?;
    table.insert(&[Value::Int(1), Value::String("saved")])?;
    drop(table);
    let mut narrowed = schema();
    narrowed.columns[0].data_type.kind = minidb::Kind::Int { min: 2, max: 10 };
    fs::write(temp.0.join("items.schema.sql"), narrowed.to_text())?;
    let mut table = Table::open(&temp.0, "items")?;
    assert_eq!(table.get(1)?.unwrap().get("name")?, Value::String("saved"));
    assert!(table.check().is_err());
    Ok(())
}
