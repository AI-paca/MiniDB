use minidb::layout::Layout;
use minidb::record::{self, Row, FLAG_DEAD, FLAG_FREE, FLAG_LIVE};
use minidb::{Column, DataType, Kind, Result, Schema, Value};

fn schema() -> Schema {
    Schema::new(
        "records",
        vec![
            Column::new("id", DataType::INT),
            Column::new("score", DataType::FLOAT),
            Column::new(
                "text",
                DataType {
                    kind: Kind::String { max_bytes: 4 },
                    nullable: true,
                },
            ),
        ],
        0,
    )
}

#[test]
fn numeric_storage_is_strict_and_uses_the_column_representation() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let mut out = vec![0; layout.row_size];
    record::encode(
        &layout,
        &schema,
        &[Value::Int(1), Value::Float(1.0), Value::Null],
        &mut out,
    )?;
    let int = &layout.cols[0];
    let float = &layout.cols[1];
    assert_eq!(
        &out[int.offset..int.offset + int.width],
        &1i64.to_le_bytes()
    );
    assert_eq!(
        &out[float.offset..float.offset + float.width],
        &1f64.to_le_bytes()
    );
    let before = out.clone();
    assert!(DataType::FLOAT.validate(Value::Int(1)).is_err());
    assert!(DataType::INT.validate(Value::Float(1.0)).is_err());
    for vals in [
        [Value::Int(1), Value::Int(1), Value::Null],
        [Value::Float(1.0), Value::Float(1.0), Value::Null],
        [Value::Int(1), Value::String("1"), Value::Null],
        [Value::Int(1), Value::Float(1.0), Value::Int(1)],
    ] {
        assert!(record::encode(&layout, &schema, &vals, &mut out).is_err());
        assert_eq!(out, before);
    }
    Ok(())
}

#[test]
fn string_capacity_is_checked_in_bytes_before_mutating_output() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let mut out = vec![0; layout.row_size];
    record::encode(
        &layout,
        &schema,
        &[Value::Int(1), Value::Float(0.0), Value::String("яя")],
        &mut out,
    )?;
    assert_eq!(
        Row {
            bytes: &out,
            layout: &layout,
            schema: &schema
        }
        .at(2)?,
        Value::String("яя")
    );
    let before = out.clone();
    for text in ["12345", "яяя"] {
        assert!(record::encode(
            &layout,
            &schema,
            &[Value::Int(1), Value::Float(0.0), Value::String(text)],
            &mut out
        )
        .is_err());
        assert_eq!(out, before);
    }
    // Максимальная ёмкость поля точно совпадает с пределом двухбайтной длины.
    let mut wide = schema.clone();
    wide.columns[2].data_type.kind = Kind::String { max_bytes: 65_535 };
    let layout = Layout::from_schema(&wide)?;
    let text = "x".repeat(65_535);
    let mut out = vec![0; layout.row_size];
    record::encode(
        &layout,
        &wide,
        &[Value::Int(1), Value::Float(0.0), Value::String(&text)],
        &mut out,
    )?;
    let before = out.clone();
    let too_long = format!("{text}x");
    assert!(record::encode(
        &layout,
        &wide,
        &[Value::Int(1), Value::Float(0.0), Value::String(&too_long)],
        &mut out
    )
    .is_err());
    assert_eq!(out, before);
    Ok(())
}

#[test]
fn reading_ignores_current_constraints_and_validation_is_explicit() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let vals = [Value::Int(3), Value::Float(0.99), Value::Null];
    let mut out = vec![0; layout.row_size];
    record::encode(&layout, &schema, &vals, &mut out)?;
    for column in [0, 1, 2] {
        let mut narrowed = schema.clone();
        match column {
            0 => narrowed.columns[0].data_type.kind = Kind::Int { min: 0, max: 2 },
            1 => {
                narrowed.columns[1].data_type.kind = Kind::Float {
                    min: 0.0,
                    max: 0.95,
                }
            }
            _ => narrowed.columns[2].data_type.nullable = false,
        }
        let row = Row {
            bytes: &out,
            layout: &layout,
            schema: &narrowed,
        };
        for (i, &value) in vals.iter().enumerate() {
            assert_eq!(row.at(i)?, value);
        }
        assert!(row.validate().is_err());
    }
    // Ограничения в старом Layout не мешают явной проверке новой схемы,
    // если формат тот же и сохранённые значения удовлетворяют новой схеме.
    let mut narrowed = schema.clone();
    narrowed.columns[1].data_type.kind = Kind::Float { min: 0.0, max: 1.0 };
    Row {
        bytes: &out,
        layout: &layout,
        schema: &narrowed,
    }
    .validate()?;
    let col = &layout.cols[1];
    out[col.offset..col.offset + col.width].copy_from_slice(&f64::NAN.to_le_bytes());
    let row = Row {
        bytes: &out,
        layout: &layout,
        schema: &schema,
    };
    assert!(matches!(row.at(1)?, Value::Float(n) if n.is_nan()));
    assert!(row.validate().is_err());
    Ok(())
}

#[test]
fn raw_reading_still_rejects_structural_damage_without_touching_other_fields() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let mut out = vec![0; layout.row_size];
    record::encode(
        &layout,
        &schema,
        &[Value::Int(7), Value::Float(0.0), Value::String("x")],
        &mut out,
    )?;
    let original = out.clone();
    let text = &layout.cols[2];
    out[text.offset + 2] = 255;
    let row = Row {
        bytes: &out,
        layout: &layout,
        schema: &schema,
    };
    assert!(row.at(2).is_err());
    assert_eq!(row.at(0)?, Value::Int(7));
    out.copy_from_slice(&original);
    out[text.offset..text.offset + 2].copy_from_slice(&5u16.to_le_bytes());
    let row = Row {
        bytes: &out,
        layout: &layout,
        schema: &schema,
    };
    assert!(row.at(2).is_err());
    for bytes in [&original[..1], &original[..original.len() - 1]] {
        assert!(Row {
            bytes,
            layout: &layout,
            schema: &schema
        }
        .at(2)
        .is_err());
    }
    let mut broken = layout.clone();
    broken.cols[0].offset = usize::MAX;
    assert!(Row {
        bytes: &original,
        layout: &broken,
        schema: &schema
    }
    .at(0)
    .is_err());
    assert!(Row {
        bytes: &original,
        layout: &layout,
        schema: &schema
    }
    .at(usize::MAX)
    .is_err());
    Ok(())
}

#[test]
fn flags_are_explicit_and_invalid_flags_leave_output_unchanged() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let vals = [Value::Int(1), Value::Float(0.5), Value::Null];
    let mut out = vec![0; layout.row_size];
    for flag in [FLAG_FREE, FLAG_DEAD, FLAG_LIVE] {
        record::encode_with_flag(&layout, &schema, &vals, flag, &mut out)?;
        let row = Row {
            bytes: &out,
            layout: &layout,
            schema: &schema,
        };
        assert_eq!(row.flag()?, flag);
        assert_eq!(row.live(), flag == FLAG_LIVE);
        assert_eq!(row.at(0)?, Value::Int(1));
        row.validate()?;
    }
    let before = out.clone();
    assert!(record::encode_with_flag(&layout, &schema, &vals, 3, &mut out).is_err());
    assert_eq!(out, before);
    out[0] = 3;
    let row = Row {
        bytes: &out,
        layout: &layout,
        schema: &schema,
    };
    assert!(!row.live()); // Предикат LIVE не заменяет проверку корректности flag().
    assert!(row.flag().is_err());
    assert!(row.validate().is_err());
    assert!(record::flag(&[]).is_err());
    Ok(())
}

#[test]
fn public_encoder_rejects_mutated_layouts_before_changing_output() -> Result<()> {
    let schema = schema();
    let layout = Layout::from_schema(&schema)?;
    let vals = [Value::Int(1), Value::Float(0.5), Value::Null];
    for variant in 0..8 {
        let mut broken = layout.clone();
        match variant {
            0 => broken.cols[0].width = 7,
            1 => broken.cols[0].offset = usize::MAX,
            2 => broken.row_size = 0,
            3 => broken.header = 0,
            4 => broken.null_bytes = 0,
            5 => broken.cols[0].kind = Kind::String { max_bytes: 6 },
            6 => broken.cols.clear(),
            _ => broken.cols[2].kind = Kind::String { max_bytes: 100 },
        }
        let mut out = vec![0xaa; layout.row_size];
        let before = out.clone();
        assert!(record::encode(&broken, &schema, &vals, &mut out).is_err());
        assert_eq!(out, before);
    }
    Ok(())
}
