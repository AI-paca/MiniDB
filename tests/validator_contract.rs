use minidb::{Column, DataType, Database, Expr, Kind, Result, Row, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "minidb-validator-contract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn live_validator(row: Row<'_>) -> Result<()> {
    assert!(row.live(), "validator must see the proposed LIVE record");
    assert_eq!(row.flag()?, minidb::record::FLAG_LIVE);
    row.validate()?;
    assert!(row.get("id")?.int()? > 0, "auto-ID precedes validation");
    if row.get("name")? == Value::String("reject") {
        return Err("rejected by validator".into());
    }
    Ok(())
}

fn schema() -> Schema {
    let mut schema = Schema::new(
        "items",
        vec![
            Column::new("id", DataType::INT),
            Column::new("name", DataType::STRING),
        ],
        0,
    );
    schema.validator = Some("live".into());
    schema
}

#[test]
fn insert_validator_sees_live_record_and_rejection_writes_nothing() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open_with(&temp.0, &[("live", live_validator)])?;
    db.create(schema())?;
    let data_path = db.data_file("items")?.to_owned();
    let before = fs::read(&data_path)?;
    let error = db
        .insert("items", &[Value::Null, Value::String("reject")])
        .unwrap_err();
    assert!(error.to_string().contains("rejected by validator"));
    assert_eq!(fs::read(&data_path)?, before);
    assert_eq!(db.scan("items", &Expr::All, |_| Ok(()))?, 0);
    assert_eq!(
        db.insert("items", &[Value::Null, Value::String("accepted")])?,
        1,
        "rejection must not consume the auto-ID"
    );
    drop(db);
    let mut reopened = Database::open_with(&temp.0, &[("live", live_validator)])?;
    assert_eq!(reopened.scan("items", &Expr::All, |_| Ok(()))?, 1);
    assert_eq!(reopened.get("items", 1, |row| Ok(row.live()))?, Some(true));
    Ok(())
}

#[test]
fn inferred_insert_validator_sees_live_before_type_and_data_are_published() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open_with(&temp.0, &[("live", live_validator)])?;
    db.create(schema())?;
    db.add_column("items", Column::new("extra", DataType::NULL))?;
    let before = db.schema("items")?.to_text();
    let data_path = db.data_file("items")?.to_owned();
    let bytes = fs::read(&data_path)?;
    let error = db
        .insert(
            "items",
            &[Value::Null, Value::String("reject"), Value::Float(0.75)],
        )
        .unwrap_err();
    assert!(error.to_string().contains("rejected by validator"));
    assert_eq!(db.schema("items")?.to_text(), before);
    assert_eq!(db.data_file("items")?, data_path);
    assert_eq!(fs::read(&data_path)?, bytes);
    assert!(matches!(
        db.schema("items")?.columns[2].data_type.kind,
        Kind::Null
    ));
    assert_eq!(db.scan("items", &Expr::All, |_| Ok(()))?, 0);
    assert_eq!(
        db.insert(
            "items",
            &[Value::Null, Value::String("accepted"), Value::Float(0.75)],
        )?,
        1
    );
    assert!(matches!(
        db.schema("items")?.columns[2].data_type.kind,
        Kind::Float { .. }
    ));
    drop(db);
    let mut reopened = Database::open_with(&temp.0, &[("live", live_validator)])?;
    assert_eq!(reopened.scan("items", &Expr::All, |_| Ok(()))?, 1);
    assert_eq!(
        reopened.get("items", 1, |row| {
            assert!(row.live());
            Ok(row.get("extra")? == Value::Float(0.75))
        })?,
        Some(true)
    );
    Ok(())
}
