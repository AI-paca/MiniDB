use minidb::table::Table;
use minidb::{Column, Compare, DataType, Database, Expr, Result, Schema, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "minidb-generations-{}-{}",
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
fn vacuum_compacts_slots_keeps_schema_version_and_preserves_the_deleted_maximum() -> Result<()> {
    let temp = Temp::new()?;
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    for id in [1, 2, 100] {
        db.insert("items", &[Value::Int(id), Value::String("body")])?;
    }
    db.delete("items", &Expr::Compare("id", Compare::Eq, Value::Int(100)))?;
    let before = fs::metadata(db.data_file("items")?)?.len();
    assert_eq!(db.vacuum("items")?, 1);
    assert_eq!(
        (db.schema("items")?.version, db.schema("items")?.generation),
        (1, 2)
    );
    assert!(!temp.0.join("items_1").exists());
    assert!(fs::metadata(db.data_file("items")?)?.len() < before);
    assert_eq!(db.vacuum("items")?, 0);
    assert_eq!(db.schema("items")?.generation, 2);
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(
        db.insert("items", &[Value::Null, Value::String("next")])?,
        101
    );
    let mut ids = Vec::new();
    assert_eq!(
        db.select(
            "items",
            &["id"],
            &Expr::Compare("id", Compare::Eq, Value::Int(101)),
            |values| {
                ids.push(values[0].int()?);
                Ok(())
            }
        )?,
        1
    );
    assert_eq!(ids, [101]);
    Ok(())
}

#[test]
fn opening_recovers_both_sides_of_the_schema_switch_and_cleans_only_engine_temporaries(
) -> Result<()> {
    for published in [false, true] {
        let temp = Temp::new()?;
        let mut old = Table::create(&temp.0, schema())?;
        old.insert(&[Value::Int(7), Value::String("old")])?;
        old.sync()?;
        let old_text = fs::read_to_string(temp.0.join("items_1"))?;
        drop(old);
        let next_text = old_text
            .replace("GENERATION 1", "GENERATION 2")
            .replace("\"old\"", "\"new\"");
        fs::write(temp.0.join("items_2"), &next_text)?;
        let mut next = schema();
        next.generation = 2;
        let staged = next.to_text() + "-- minidb-highest-id: 7\n";
        if published {
            fs::write(temp.0.join("items_schema"), &staged)?;
        }
        fs::write(temp.0.join("items_schema.tmp.999.0"), staged)?;
        fs::write(temp.0.join("items_2.tmp.999.1"), "unfinished")?;
        fs::write(temp.0.join("items_2.tmp"), "editor")?;
        let mut table = Table::open(&temp.0, "items")?;
        assert_eq!(table.schema().generation, if published { 2 } else { 1 });
        assert_eq!(
            table.get(7)?.unwrap().get("name")?,
            Value::String(if published { "new" } else { "old" })
        );
        assert!(!temp
            .0
            .join(if published { "items_1" } else { "items_2" })
            .exists());
        assert!(!temp.0.join("items_schema.tmp.999.0").exists());
        assert!(!temp.0.join("items_2.tmp.999.1").exists());
        assert_eq!(fs::read_to_string(temp.0.join("items_2.tmp"))?, "editor");
        table.insert(&[Value::Null, Value::String("after recovery")])?;
        table.sync()?;
        assert!(table.get(8)?.is_some());
    }
    Ok(())
}

#[test]
fn missing_current_data_is_an_error_and_does_not_delete_the_remaining_generation() -> Result<()> {
    let temp = Temp::new()?;
    drop(Table::create(&temp.0, schema())?);
    fs::rename(temp.0.join("items_1"), temp.0.join("items_2"))?;
    assert!(Table::open(&temp.0, "items").is_err());
    assert!(temp.0.join("items_2").exists());
    assert!(!temp.0.join("items_1").exists());
    Ok(())
}

#[test]
fn a_failed_rewrite_preserves_the_published_file_schema_and_existing_target() -> Result<()> {
    let temp = Temp::new()?;
    let mut table = Table::create(&temp.0, schema())?;
    table.insert(&[Value::Int(1), Value::String("saved")])?;
    table.sync()?;
    let schema_before = fs::read(temp.0.join("items_schema"))?;
    let data_before = fs::read(temp.0.join("items_1"))?;
    fs::write(temp.0.join("items_2"), "do not overwrite")?;
    let column = Column::new(
        "extra",
        DataType {
            nullable: true,
            ..DataType::INT
        },
    );
    assert!(table.add_column(column.clone()).is_err());
    assert_eq!(fs::read(temp.0.join("items_schema"))?, schema_before);
    assert_eq!(fs::read(temp.0.join("items_1"))?, data_before);
    assert_eq!(
        fs::read_to_string(temp.0.join("items_2"))?,
        "do not overwrite"
    );
    assert!(table.get(1)?.is_some());
    fs::remove_file(temp.0.join("items_2"))?;
    // Failure during data encoding happens before publication too.
    let mut narrowed = table.schema().clone();
    narrowed.columns[0].data_type.kind = minidb::Kind::Int { min: 2, max: 10 };
    drop(table);
    fs::write(temp.0.join("items_schema"), narrowed.to_text())?;
    let mut table = Table::open(&temp.0, "items")?;
    assert!(table.add_column(column).is_err());
    assert!(!temp.0.join("items_2").exists());
    assert_eq!(table.schema().generation, 1);
    assert_eq!(fs::read(temp.0.join("items_1"))?, data_before);
    Ok(())
}

#[test]
fn select_by_primary_key_checks_the_remaining_condition_and_or_keeps_both_keys() -> Result<()> {
    let temp = Temp::new()?;
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    for id in 1..=4 {
        db.insert("items", &[Value::Int(id), Value::String("name")])?;
    }
    let eq = |id| Expr::Compare("id", Compare::Eq, Value::Int(id));
    for (filter, expected) in [
        (eq(3), vec![3]),
        (eq(99), vec![]),
        (eq(2).and(eq(3)), vec![]),
        (eq(2).or(eq(3)), vec![2, 3]),
        (eq(3).or(eq(3)), vec![3]),
    ] {
        let mut ids = Vec::new();
        let count = db.select("items", &["id"], &filter, |values| {
            ids.push(values[0].int()?);
            Ok(())
        })?;
        ids.sort_unstable();
        assert_eq!(count, expected.len());
        assert_eq!(ids, expected);
    }
    Ok(())
}
