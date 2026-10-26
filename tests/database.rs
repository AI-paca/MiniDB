use minidb::{Column, Compare::*, DataType, Database, Expr, Kind, Result, Schema, Value::*};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "minidb-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self(path)
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
            Column::new("text", DataType::STRING),
            Column::new("score", DataType::FLOAT),
        ],
        0,
    )
}
fn ids(db: &mut Database, filter: &Expr<'_>) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    db.select("items", &["id"], filter, |row| {
        ids.push(row[0].int()?);
        Ok(())
    })?;
    ids.sort();
    Ok(ids)
}

#[test]
fn crud_types_filters_and_restart() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    for id in 1..=3 {
        assert_eq!(
            db.insert("items", &[Null, String("имя|\n\\"), Float(id as f64)])?,
            id
        );
    }
    assert!(db
        .insert("items", &[Int(1), String("duplicate"), Float(0.0)])
        .is_err());
    assert!(db.insert("items", &[Null, Int(2), Float(0.0)]).is_err());
    assert!(db
        .insert("items", &[Null, String("bad"), Float(f64::NAN)])
        .is_err());
    for (op, expected) in [
        (Eq, vec![2]),
        (Ne, vec![1, 3]),
        (Gt, vec![3]),
        (Lt, vec![1]),
        (Ge, vec![2, 3]),
        (Le, vec![1, 2]),
    ] {
        assert_eq!(
            ids(&mut db, &Expr::Compare("score", op, Float(2.0)))?,
            expected
        );
    }
    let filter = Expr::Compare("score", Gt, Float(1.0))
        .and(Expr::Compare("score", Lt, Float(3.0)))
        .or(Expr::Compare("id", Eq, Int(1)));
    assert_eq!(ids(&mut db, &filter)?, vec![1, 2]);
    db.update(
        "items",
        &Expr::Compare("id", Eq, Int(2)),
        &[("text", String("a longer updated строка"))],
    )?;
    db.delete("items", &Expr::Compare("id", Eq, Int(3)))?;
    assert_eq!(
        db.insert("items", &[Null, String("after delete"), Float(4.0)])?,
        4
    );
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(ids(&mut db, &Expr::All)?, vec![1, 2, 4]);
    assert_eq!(
        db.get("items", 2, |r| Ok(r.get("text")?.to_string()))?,
        Some("a longer updated строка".into())
    );
    assert_eq!(
        db.get("items", 1, |r| Ok(r.get("text")?.to_string()))?,
        Some("имя|\n\\".into())
    );
    assert!(db.get("items", 3, |_| Ok(()))?.is_none());
    assert!(db
        .select("items", &["missing"], &Expr::All, |_| Ok(()))
        .is_err());
    assert!(db
        .select("items", &vec!["id"; 257], &Expr::All, |_| Ok(()))
        .is_err());
    assert!(db
        .scan("items", &Expr::Compare("id", Eq, String("1")), |_| Ok(()))
        .is_err());
    Ok(())
}

#[test]
fn fk_restrict_join_and_atomic_update() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    for id in 1..=3 {
        db.insert("items", &[Int(id), String("parent"), Float(1.0)])?;
    }
    db.create(Schema::new(
        "child",
        vec![
            Column::new("id", DataType::INT),
            Column::new("parent", DataType::INT).references("items", "id"),
        ],
        0,
    ))?;
    db.insert("child", &[Null, Int(1)])?;
    assert!(db.insert("child", &[Null, Int(999)]).is_err());
    assert!(db
        .update("child", &Expr::All, &[("parent", Int(999))])
        .is_err());
    assert!(db.delete("items", &Expr::All).is_err());
    assert!(db.update("items", &Expr::All, &[("id", Int(10))]).is_err());
    assert!(db
        .update(
            "items",
            &Expr::Compare("id", Eq, Int(1)),
            &[("id", Int(10))]
        )
        .is_err());
    assert!(db
        .update("items", &Expr::Compare("id", Eq, Int(2)), &[("id", Int(3))])
        .is_err());
    assert_eq!(ids(&mut db, &Expr::All)?, vec![1, 2, 3]);
    assert_eq!(
        db.join("child", "parent", "items", &Expr::All, |a, b| {
            assert_eq!(a.get("parent")?, b.get("id")?);
            Ok(())
        })?,
        1
    );
    assert_eq!(
        db.join_on("child", "parent", "items", "ID", &Expr::All, |a, b| {
            assert_eq!(a.get("parent")?, b.get("id")?);
            Ok(())
        })?,
        1
    );
    let mut called = false;
    let error = db
        .join_on("child", "parent", "items", "score", &Expr::All, |_, _| {
            called = true;
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("PRIMARY KEY"));
    assert!(!called);
    assert!(db
        .join_on(
            "child",
            "parent",
            "items",
            "missing",
            &Expr::All,
            |_, _| Ok(())
        )
        .is_err());
    assert!(db
        .join("child", "parent", "items", &Expr::All, |_, _| Err(
            "callback failed".into()
        ))
        .is_err());
    assert!(db.get("items", 1, |_| Ok(()))?.is_some());
    db.update(
        "items",
        &Expr::Compare("id", Eq, Int(2)),
        &[("id", Int(10))],
    )?;
    assert!(db.get("items", 2, |_| Ok(()))?.is_none());
    assert!(db.get("items", 10, |_| Ok(()))?.is_some());
    db.delete("child", &Expr::All)?;
    assert_eq!(db.delete("items", &Expr::All)?, 3);
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(ids(&mut db, &Expr::All)?, vec![]);
    assert_eq!(db.insert("items", &[Null, String("new"), Float(1.0)])?, 11);
    Ok(())
}

#[test]
fn null_limits_and_native_validator_survive_restart() -> Result<()> {
    fn rule(row: minidb::Row<'_>) -> Result<()> {
        if row.get("text")? == String("forbidden") {
            return Err("forbidden".into());
        }
        Ok(())
    }
    let rules: &[(&str, minidb::database::Validator)] = &[("rule_v1", rule)];
    let temp = Temp::new();
    let mut db = Database::open_with(&temp.0, rules)?;
    let base = DataType {
        kind: Kind::Int { min: -10, max: 10 },
        nullable: false,
    };
    let wider = DataType {
        kind: Kind::Int { min: -20, max: 5 },
        ..base
    };
    assert!(wider.validate(Int(-15)).is_ok());
    assert!(base.validate(Int(-15)).is_err());
    let mut schema = schema();
    schema.columns[1].data_type.nullable = true;
    schema.columns[2].data_type.kind = Kind::Float { min: 0.0, max: 1.0 };
    schema.validator = Some("rule_v1".into());
    db.create(schema)?;
    db.insert("items", &[Null, Null, Float(0.5)])?;
    assert!(db
        .insert("items", &[Null, String("forbidden"), Float(0.5)])
        .is_err());
    assert!(db.insert("items", &[Null, Null, Float(2.0)]).is_err());
    assert_eq!(ids(&mut db, &Expr::IsNull("text"))?, vec![1]);
    assert!(ids(&mut db, &Expr::Compare("text", Ne, String("anything")))?.is_empty());
    drop(db);
    assert!(Database::open(&temp.0).is_err());
    let mut db = Database::open_with(&temp.0, rules)?;
    assert!(db
        .update("items", &Expr::All, &[("text", String("forbidden"))])
        .is_err());
    assert_eq!(ids(&mut db, &Expr::IsNull("text"))?, vec![1]);
    Ok(())
}

#[test]
fn fixed_width_strings_and_deleted_rows() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    let mut wide = schema();
    wide.columns[1].data_type.kind = Kind::String { max_bytes: 65_535 };
    db.create(wide)?;
    // Проверяем одинаковый размер записей с разной длиной текста
    for id in 1..=150 {
        let text = "я".repeat(if id == 130 { 30_000 } else { 300 + id as usize });
        db.insert("items", &[Int(id), String(&text), Float(0.0)])?;
    }
    db.update(
        "items",
        &Expr::Compare("id", Eq, Int(1)),
        &[("text", String("changed"))],
    )?;
    db.delete("items", &Expr::Compare("id", Eq, Int(2)))?;
    let mut count = 0;
    db.scan("items", &Expr::All, |row| {
        let id = row.get("id")?.int()?;
        let expected = if id == 1 {
            "changed".into()
        } else {
            "я".repeat(if id == 130 { 30_000 } else { 300 + id as usize })
        };
        assert_eq!(row.get("text")?, String(&expected));
        count += 1;
        Ok(())
    })?;
    assert_eq!(count, 149);
    Ok(())
}

#[test]
fn backup_restore_and_exclusive_lock() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(temp.0.join("source"))?;
    assert!(Database::open(temp.0.join("source")).is_err());
    db.create(schema())?;
    db.insert("items", &[Null, String("snapshot"), Float(1.0)])?;
    db.backup(temp.0.join("backup"))?;
    assert!(db.backup(temp.0.join("backup")).is_err());
    db.delete("items", &Expr::All)?;
    let mut restored = Database::restore(temp.0.join("backup"), temp.0.join("restored"), &[])?;
    assert_eq!(ids(&mut restored, &Expr::All)?, vec![1]);
    assert!(Database::restore(temp.0.join("backup"), temp.0.join("restored"), &[]).is_err());
    Ok(())
}

#[test]
fn text_roundtrips_controls_empty_strings_float_and_case() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    let mut schema = schema();
    schema.name = "MixedCase".into();
    schema.columns[1].name = "Message".into();
    db.create(schema)?;
    for text in ["", "NULL", "\t\n\r\0\\\"' я", "spaces  ", "COMMIT\nBEGIN"] {
        let id = db.insert("MixedCase", &[Null, String(text), Float(1e-80)])?;
        assert_eq!(
            db.get("MixedCase", id, |row| Ok(row.get("Message")?.to_string()))?,
            Some(text.into())
        );
    }
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(
        db.scan("MixedCase", &Expr::All, |row| {
            assert_eq!(row.get("score")?, Float(1e-80));
            Ok(())
        })?,
        5
    );
    let text = fs::read_to_string(temp.0.join("MixedCase"))?;
    assert!(!text.contains('\0'));
    assert!(text.contains("\\u{0}"));
    Ok(())
}

#[test]
fn update_null_pk_preserves_parent_and_children() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    db.insert("items", &[Int(3), String("parent"), Float(0.0)])?;
    db.insert("items", &[Int(100), String("other"), Float(0.0)])?;
    db.create(Schema::new(
        "child",
        vec![
            Column::new("id", DataType::INT),
            Column::new("parent", DataType::INT).references("items", "id"),
        ],
        0,
    ))?;
    db.insert("child", &[Int(1), Int(3)])?;
    assert_eq!(
        db.update(
            "items",
            &Expr::Compare("id", Eq, Int(3)),
            &[("id", Null), ("text", String("done"))]
        )?,
        1
    );
    assert_eq!(ids(&mut db, &Expr::All)?, [3, 100]);
    assert_eq!(
        db.get("items", 3, |r| Ok(r.get("text")?.to_string()))?,
        Some("done".into())
    );
    assert_eq!(db.update("items", &Expr::All, &[("id", Null)])?, 0);
    assert_eq!(db.delete("items", &Expr::Compare("id", Eq, Int(999)))?, 0);
    assert_eq!(
        db.join("child", "parent", "items", &Expr::All, |_, row| {
            assert_eq!(row.get("id")?, Int(3));
            Ok(())
        })?,
        1
    );
    Ok(())
}

#[test]
fn numeric_filters_compare_int_and_float_in_both_directions() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(schema())?;
    for id in 1..=3 {
        db.insert(
            "items",
            &[Int(id), String("numeric"), Float(id as f64 + 0.25)],
        )?;
    }
    assert_eq!(ids(&mut db, &Expr::Compare("id", Gt, Float(1.5)))?, [2, 3]);
    assert_eq!(ids(&mut db, &Expr::Compare("score", Gt, Int(2)))?, [2, 3]);
    assert!(ids(&mut db, &Expr::Compare("id", Eq, Float(1.5)))?.is_empty());
    assert!(ids(&mut db, &Expr::Compare("id", Eq, Float(f64::NAN))).is_err());
    Ok(())
}

#[test]
fn self_foreign_keys_restrict_external_rows_but_not_deleted_rows() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(Schema::new(
        "nodes",
        vec![
            Column::new("id", DataType::INT),
            Column::new(
                "parent",
                DataType {
                    nullable: true,
                    ..DataType::INT
                },
            )
            .references("nodes", "id"),
        ],
        0,
    ))?;
    db.insert("nodes", &[Int(1), Int(1)])?;
    db.insert("nodes", &[Int(2), Int(1)])?;
    assert!(db
        .delete("nodes", &Expr::Compare("id", Eq, Int(1)))
        .is_err());
    assert_eq!(db.scan("nodes", &Expr::All, |_| Ok(()))?, 2);
    assert_eq!(db.delete("nodes", &Expr::All)?, 2);
    assert_eq!(db.scan("nodes", &Expr::All, |_| Ok(()))?, 0);
    drop(db);
    let mut db = Database::open(&temp.0)?;
    assert_eq!(db.insert("nodes", &[Null, Null])?, 3);
    Ok(())
}

#[test]
fn projection_matches_requested_size_for_small_and_wide_tables() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    let mut columns = vec![Column::new("id", DataType::INT)];
    for i in 1..40 {
        columns.push(Column::new(&format!("c{i}"), DataType::INT));
    }
    db.create(Schema::new("wide", columns, 0))?;
    let values: Vec<_> = (1..=40).map(Int).collect();
    db.insert("wide", &values)?;
    db.select("wide", &["c39", "id"], &Expr::All, |values| {
        assert_eq!(values, [Int(40), Int(1)]);
        Ok(())
    })?;
    db.select("wide", &[], &Expr::All, |row| {
        assert_eq!(row, values);
        Ok(())
    })?;
    Ok(())
}

#[test]
fn self_foreign_key_update_checks_replacement_values_and_remaining_referrers() -> Result<()> {
    let temp = Temp::new();
    let mut db = Database::open(&temp.0)?;
    db.create(Schema::new(
        "nodes",
        vec![
            Column::new("id", DataType::INT),
            Column::new(
                "parent",
                DataType {
                    nullable: true,
                    ..DataType::INT
                },
            )
            .references("nodes", "id"),
        ],
        0,
    ))?;
    db.insert("nodes", &[Int(1), Int(1)])?;
    // Старое самоссылочное значение исчезает вместе со старой версией строки.
    assert_eq!(
        db.update("nodes", &Expr::All, &[("id", Int(3)), ("parent", Int(3))])?,
        1
    );
    db.insert("nodes", &[Int(2), Int(3)])?;
    assert!(db
        .update(
            "nodes",
            &Expr::Compare("id", Eq, Int(3)),
            &[("id", Int(4)), ("parent", Int(4))]
        )
        .is_err());
    db.delete("nodes", &Expr::Compare("id", Eq, Int(2)))?;
    // Нельзя оставить ссылку на PK, исчезающий из конечного индекса.
    assert!(db.update("nodes", &Expr::All, &[("id", Int(4))]).is_err());
    assert_eq!(db.get("nodes", 3, |row| row.get("parent")?.int())?, Some(3));
    Ok(())
}
