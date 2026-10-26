use minidb::layout::Layout;
use minidb::types::{DEFAULT_STRING_BYTES, MAX_STRING_BYTES};
use minidb::{Column, DataType, Kind, Result, Schema, Value};
use std::cmp::Ordering::{Equal, Greater, Less};

#[test]
fn string_default_and_schema_capacity_match_the_u16_format() -> Result<()> {
    assert_eq!(DEFAULT_STRING_BYTES, 64);
    assert_eq!(MAX_STRING_BYTES, 65_535);
    let schema = Schema::from_text("CREATE TABLE labels (id INT PRIMARY KEY, name STRING);")?;
    assert_eq!(
        schema.columns[1].data_type,
        DataType {
            nullable: true,
            ..DataType::STRING
        }
    );
    assert_eq!(Layout::from_schema(&schema)?.row_size, 76);
    assert!(DataType::STRING
        .validate(Value::String(&"я".repeat(32)))
        .is_ok());
    assert!(DataType::STRING
        .validate(Value::String(&"я".repeat(33)))
        .is_err());
    for capacity in [0, 64, 65_535] {
        Schema::from_text(&format!(
            "CREATE TABLE valid (id INT PRIMARY KEY, text STRING({capacity}));"
        ))?;
    }
    for capacity in [65_536, 100_000, 1_048_576, usize::MAX] {
        let error = Schema::from_text(&format!(
            "CREATE TABLE invalid (id INT PRIMARY KEY, text STRING({capacity}));"
        ))
        .err()
        .ok_or("Ожидалась ошибка ёмкости STRING")?;
        assert!(error.to_string().contains("65535"));
    }
    let largest = DataType {
        kind: Kind::String {
            max_bytes: MAX_STRING_BYTES,
        },
        nullable: false,
    };
    let mut text = "я".repeat(32_767);
    text.push('x');
    largest.validate(Value::String(&text))?;
    text.push('x');
    assert!(largest.validate(Value::String(&text)).is_err());
    Ok(())
}

#[test]
fn mixed_numeric_comparison_preserves_integer_precision() {
    for (integer, float, expected) in [
        (1, 1.0, Equal),
        (1, 1.5, Less),
        (2, 1.5, Greater),
        (-1, -1.5, Greater),
        (-2, -1.5, Less),
        (0, -0.0, Equal),
        (0, f64::MIN_POSITIVE, Less),
        (0, -f64::MIN_POSITIVE, Greater),
        (9_007_199_254_740_993, 9_007_199_254_740_992.0, Greater),
        (i64::MIN, i64::MIN as f64, Equal),
        (i64::MIN, i64::MIN as f64 - 2048.0, Greater),
        (i64::MAX, i64::MAX as f64, Less),
        (i64::MAX, i64::MAX as f64 - 1024.0, Greater),
        (i64::MAX, f64::INFINITY, Less),
        (i64::MIN, f64::NEG_INFINITY, Greater),
    ] {
        assert_eq!(
            Value::Int(integer).compare(Value::Float(float)),
            Some(expected)
        );
        assert_eq!(
            Value::Float(float).compare(Value::Int(integer)),
            Some(expected.reverse())
        );
    }
    assert_eq!(Value::Int(0).compare(Value::Float(f64::NAN)), None);
    assert_eq!(Value::Float(f64::NAN).compare(Value::Int(0)), None);
    assert_eq!(Value::Null.compare(Value::Int(0)), None);
    assert_eq!(Value::Int(1).compare(Value::String("1")), None);
}

#[test]
fn stored_column_spelling_is_preserved_but_lookup_ignores_case() -> Result<()> {
    let schema = Schema::from_text("CREATE TABLE MixedCase (id INT PRIMARY KEY, Name STRING);")?;
    assert_eq!(schema.name, "MixedCase");
    assert_eq!(schema.columns[1].name, "Name");
    assert_eq!(schema.column("name")?, 1);
    assert_eq!(schema.column("NAME")?, 1);
    assert!(
        Schema::from_text("CREATE TABLE dup (id INT PRIMARY KEY, Name STRING, name STRING);")
            .is_err()
    );
    let original = Schema::new(
        "kept",
        vec![
            Column::new("id", DataType::INT),
            Column::new("Mixed", DataType::STRING),
        ],
        0,
    );
    assert_eq!(Schema::from_text(&original.to_text())?, original);
    Ok(())
}

#[test]
fn layout_construction_and_validation_report_overflow_and_short_header() -> Result<()> {
    let overflowing = Column::new(
        "text",
        DataType {
            kind: Kind::String {
                max_bytes: usize::MAX,
            },
            nullable: false,
        },
    );
    assert!(overflowing
        .width()
        .unwrap_err()
        .to_string()
        .contains("Переполнение"));
    let invalid = Schema::new(
        "bad",
        vec![Column::new("id", DataType::INT), overflowing],
        0,
    );
    assert!(Layout::from_schema(&invalid).is_err());
    let schema = Schema::new("valid", vec![Column::new("id", DataType::INT)], 0);
    let mut layout = Layout::from_schema(&schema)?;
    layout.row_size = layout.header - 1;
    assert!(layout
        .validate(&schema)
        .unwrap_err()
        .to_string()
        .contains("заголовка"));
    Ok(())
}
