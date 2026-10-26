use minidb::{
    codegen, sql, Column, Compare, DataType, Database, Expr, Kind, Result, Schema, Value,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "minidb-infer-{}-{}",
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
fn pending(db: &Database, field: &str) -> bool {
    let schema = db.schema("items").unwrap();
    matches!(
        schema.columns[schema.column(field).unwrap()].data_type.kind,
        Kind::Null
    )
}

#[test]
fn uppercase_grammar_preserves_literal_case_paths_and_original_tokens() -> Result<()> {
    let commands = sql::parse("aLtEr tAbLe datasets aDd cOlUmN comment sTrInG(128) nUlL; InSeRt InTo datasets (name) VaLuEs ('MiXeD ALTER Null'); oPeN 'Scratch/CasePath';")?;
    assert!(
        matches!(&commands[0], sql::Command::AddColumn { column, .. } if column.name == "comment" && column.data_type.kind == Kind::String {max_bytes:128})
    );
    assert!(
        matches!(&commands[1], sql::Command::Insert { values, .. } if values[0].value() == Value::String("MiXeD ALTER Null"))
    );
    assert!(matches!(&commands[2], sql::Command::Open(path) if path == "Scratch/CasePath"));
    assert_eq!(
        sql::tokenize("aLtEr 'MiXeD'")?,
        [
            sql::Token::Word("aLtEr".into()),
            sql::Token::Text("MiXeD".into())
        ]
    );
    for alias in ["help", "HELP", "-H", ".HeLp", "--HELP"] {
        assert!(matches!(
            sql::parse(alias)?.as_slice(),
            [sql::Command::Help]
        ));
    }
    for statement in [
        "ALTER TABLE items ADD note",
        "alter table items add column note NULL",
        "AlTeR TABLE items ADD COLUMN note INT",
    ] {
        assert!(matches!(
            sql::parse(statement)?.as_slice(),
            [sql::Command::AddColumn { .. }]
        ));
    }
    Ok(())
}

#[test]
fn untyped_columns_stay_null_across_inserts_empty_updates_and_restart() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    db.insert("items", &[Value::Int(1), Value::String("before")])?;
    db.add_column("items", Column::new("note", DataType::NULL))?;
    db.insert(
        "items",
        &[Value::Int(2), Value::String("after"), Value::Null],
    )?;
    assert_eq!(
        db.update(
            "items",
            &Expr::Compare("id", Compare::Eq, Value::Int(999)),
            &[("note", Value::Int(7))]
        )?,
        0
    );
    assert!(pending(&db, "note"));
    assert!(codegen::generate(db.schema("items")?)
        .unwrap_err()
        .to_string()
        .contains("ещё NULL"));
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert!(pending(&db, "note"));
    assert_eq!(
        db.scan("items", &Expr::IsNull("note"), |r| r.validate())?,
        2
    );
    Ok(())
}

#[test]
fn first_update_sets_the_type_once_without_changing_schema_version() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    for id in 1..=2 {
        db.insert("items", &[Value::Int(id), Value::String("old")])?;
    }
    db.add_column("items", Column::new("count", DataType::NULL))?;
    let generation = db.schema("items")?.generation;
    assert_eq!(
        db.update(
            "items",
            &Expr::Compare("id", Compare::Eq, Value::Int(1)),
            &[("count", Value::Int(42))]
        )?,
        1
    );
    assert_eq!(db.schema("items")?.version, 2);
    assert_eq!(db.schema("items")?.generation, generation + 1);
    assert!(matches!(
        db.schema("items")?.columns[2].data_type.kind,
        Kind::Int { .. }
    ));
    assert_eq!(
        db.get("items", 1, |r| Ok(r.get("count")? == Value::Int(42)))?,
        Some(true)
    );
    assert_eq!(
        db.get("items", 2, |r| Ok(r.get("count")? == Value::Null))?,
        Some(true)
    );
    let catalog = fs::read(temp.0.join("items_schema"))?;
    assert!(db
        .update("items", &Expr::All, &[("count", Value::String("wrong"))])
        .is_err());
    assert_eq!(fs::read(temp.0.join("items_schema"))?, catalog);
    db.update("items", &Expr::All, &[("count", Value::Null)])?;
    assert!(!pending(&db, "count"));
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(db.scan("items", &Expr::IsNull("count"), |_| Ok(()))?, 2);
    assert!(codegen::generate(db.schema("items")?)?.contains("Option<i64>"));
    Ok(())
}

#[test]
fn insert_infers_multiple_types_and_rebuilds_layouts_without_losing_other_values() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.set_write_barriers(false)?;
    db.create(schema())?;
    db.insert("items", &[Value::Int(1), Value::String("old")])?;
    db.add_column("items", Column::new("note", DataType::NULL))?;
    db.add_column("items", Column::new("score", DataType::NULL))?;
    let message = "MiXeD Русский\t\n".repeat(8);
    db.insert(
        "items",
        &[
            Value::Int(2),
            Value::String("new"),
            Value::String(&message),
            Value::Float(0.25),
        ],
    )?;
    assert_eq!(
        db.get("items", 1, |r| Ok(r.get("note")? == Value::Null))?,
        Some(true)
    );
    assert_eq!(
        db.get("items", 2, |r| Ok(r.get("note")?
            == Value::String(&message)
            && r.get("score")? == Value::Float(0.25)))?,
        Some(true)
    );
    db.add_column("items", Column::new("count", DataType::NULL))?;
    assert_eq!(
        db.update("items", &Expr::All, &[("count", Value::Int(9))])?,
        2
    );
    db.set_write_barriers(true)?;
    db.backup(temp.0.join("snapshot"))?;
    drop(db);
    let mut restored = Database::restore(temp.0.join("snapshot"), temp.0.join("restored"), &[])?;
    assert_eq!(
        restored.scan("items", &Expr::All, |r| {
            r.validate()?;
            assert_eq!(r.get("count")?, Value::Int(9));
            Ok(())
        })?,
        2
    );
    assert_eq!(
        restored.get("items", 2, |r| Ok(r.get("note")?
            == Value::String(&message)
            && r.get("score")? == Value::Float(0.25)))?,
        Some(true)
    );
    Ok(())
}

#[test]
fn invalid_writes_do_not_pin_a_type_or_consume_an_auto_id() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    db.insert("items", &[Value::Int(1), Value::String("old")])?;
    db.add_column("items", Column::new("extra", DataType::NULL))?;
    let before = fs::read(temp.0.join("items_schema"))?;
    for values in [
        vec![Value::Int(1), Value::String("duplicate"), Value::Int(7)],
        vec![Value::Null, Value::Int(9), Value::Int(7)],
        vec![Value::Null, Value::Null, Value::Int(7)],
        vec![Value::Null, Value::String("nan"), Value::Float(f64::NAN)],
    ] {
        assert!(db.insert("items", &values).is_err());
        assert!(pending(&db, "extra"));
        assert_eq!(fs::read(temp.0.join("items_schema"))?, before);
    }
    assert!(db
        .update(
            "items",
            &Expr::All,
            &[("name", Value::Int(7)), ("extra", Value::Float(0.5))]
        )
        .is_err());
    assert!(pending(&db, "extra"));
    assert_eq!(
        db.insert(
            "items",
            &[Value::Null, Value::String("valid"), Value::Int(7)]
        )?,
        2
    );
    Ok(())
}

#[test]
fn validator_failure_does_not_resolve_a_pending_type() -> Result<()> {
    fn rule(row: minidb::Row<'_>) -> Result<()> {
        if row.get("name")? == Value::String("reject") {
            return Err("rejected".into());
        }
        Ok(())
    }
    let temp = Temp::new();
    let mut db = Database::open_with(&temp.0, &[("rule", rule)])?;
    let mut schema = schema();
    schema.validator = Some("rule".into());
    db.create(schema)?;
    db.insert("items", &[Value::Int(1), Value::String("old")])?;
    db.add_column("items", Column::new("extra", DataType::NULL))?;
    assert!(db
        .insert(
            "items",
            &[Value::Null, Value::String("reject"), Value::Int(7)]
        )
        .is_err());
    assert!(pending(&db, "extra"));
    assert!(db
        .update(
            "items",
            &Expr::All,
            &[
                ("name", Value::String("reject")),
                ("extra", Value::Float(0.5))
            ]
        )
        .is_err());
    assert!(pending(&db, "extra"));
    Ok(())
}

#[test]
fn only_added_untyped_columns_infer_and_the_table_api_also_supports_them() -> Result<()> {
    let temp = Temp::new();
    let bad = Schema::new(
        "bad",
        vec![
            Column::new("id", DataType::INT),
            Column::new("value", DataType::NULL),
        ],
        0,
    );
    assert!(bad.check().is_err());
    let mut table = minidb::table::Table::create(&temp.0, schema())?;
    table.insert(&[Value::Int(1), Value::String("old")])?;
    table.add_column(Column::new("extra", DataType::NULL))?;
    table.update(1, &[Value::Null, Value::String("old"), Value::Float(0.5)])?;
    assert_eq!(table.get(1)?.unwrap().get("extra")?, Value::Float(0.5));
    assert!(table
        .update(1, &[Value::Int(1), Value::Int(8), Value::Float(0.5)])
        .is_err());
    assert!(table
        .update(1, &[Value::Int(1), Value::String("old"), Value::Int(8)])
        .is_err());
    Ok(())
}

#[test]
fn resolving_a_zero_width_column_preserves_later_fields_and_old_bitmap_layouts() -> Result<()> {
    let temp = Temp::new();
    let mut columns = vec![Column::new("id", DataType::INT)];
    for index in 1..8 {
        columns.push(Column::new(&format!("n{index}"), DataType::INT));
    }
    let mut db = Database::open(&temp.0)?;
    db.create(Schema::new("wide", columns, 0))?;
    let mut values: Vec<_> = (1..=8).map(Value::Int).collect();
    db.insert("wide", &values)?;
    db.add_column("wide", Column::new("pending", DataType::NULL))?;
    db.add_column(
        "wide",
        Column::new(
            "tail",
            DataType {
                nullable: true,
                ..DataType::STRING
            },
        ),
    )?;
    values[0] = Value::Int(2);
    values.extend([Value::Null, Value::String("tail after pending")]);
    db.insert("wide", &values)?;
    db.update(
        "wide",
        &Expr::Compare("id", Compare::Eq, Value::Int(2)),
        &[("pending", Value::Int(99))],
    )?;
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(
        db.get("wide", 2, |r| Ok(r.get("tail")?
            == Value::String("tail after pending")
            && r.get("pending")? == Value::Int(99)
            && r.get("n7")? == Value::Int(8)))?,
        Some(true)
    );
    assert_eq!(
        db.get("wide", 1, |r| Ok(r.get("tail")? == Value::Null
            && r.get("pending")? == Value::Null
            && r.get("n7")? == Value::Int(8)))?,
        Some(true)
    );
    Ok(())
}

#[test]
fn all_null_original_and_explicitly_typed_columns_never_change_type() -> Result<()> {
    let temp = Temp::new();
    let mut initial = schema();
    initial.columns[1].data_type.nullable = true;
    let mut db = Database::open(&temp.0)?;
    db.create(initial)?;
    db.insert("items", &[Value::Int(1), Value::Null])?;
    db.add_column(
        "items",
        Column::new(
            "explicit",
            DataType {
                nullable: true,
                ..DataType::FLOAT
            },
        ),
    )?;
    assert!(db
        .update("items", &Expr::All, &[("name", Value::Int(7))])
        .is_err());
    assert!(db
        .update("items", &Expr::All, &[("explicit", Value::String("wrong"))])
        .is_err());
    assert!(matches!(
        db.schema("items")?.columns[1].data_type.kind,
        Kind::String { .. }
    ));
    assert!(matches!(
        db.schema("items")?.columns[2].data_type.kind,
        Kind::Float { .. }
    ));
    Ok(())
}
