// Представление данных в SQL-консоли. Файлы хранилища здесь не изменяются.
use minidb::{Database, Result, Row, Schema, Value};

pub(crate) fn header(headers: &[String]) {
    for h in headers {
        print!("| {:width$} ", h, width = h.chars().count().max(12));
    }
    println!("|");
    for h in headers {
        print!("+-{}-", "-".repeat(h.chars().count().max(12)));
    }
    println!("+");
}
fn cell(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn print_cells(headers: &[String], cells: impl Iterator<Item = String>) {
    for (h, value) in headers.iter().zip(cells) {
        let width = h.chars().count().max(12);
        print!("| {:width$} ", cell(&value, width));
    }
    println!("|");
}

pub(crate) fn print_values(headers: &[String], values: &[Value<'_>]) {
    print_cells(
        headers,
        values.iter().map(|value| match value {
            Value::Float(n) if n.abs() >= 0.000001 && n.abs() < 1e12 => format!("{n:.6}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned(),
            Value::String(s) => format!("{s:?}"),
            _ => value.to_string(),
        }),
    );
}

// Возвращаем тот же буфер без ссылок на строки, которые будут прочитаны заново.
pub(crate) fn print_join<'row>(
    headers: &[String],
    fields: &[(bool, usize)],
    left: Row<'row>,
    right: Row<'row>,
    mut values: Vec<Value<'row>>,
) -> Result<Vec<Value<'static>>> {
    for (value, &(side, index)) in values.iter_mut().zip(fields) {
        *value = if side {
            right.at(index)?
        } else {
            left.at(index)?
        };
    }
    print_values(headers, &values);
    Ok(values.into_iter().map(|_| Value::Null).collect())
}

pub(crate) fn describe(schema: &Schema) {
    println!(
        "Таблица {}, версия {}, поколение {}",
        schema.name, schema.version, schema.generation
    );
    let headers: Vec<String> = ["Поле", "Тип", "NULL", "Ограничения (PK/FK, MIN/MAX)"]
        .map(str::to_owned)
        .into();
    header(&headers);
    for (i, col) in schema.columns.iter().enumerate() {
        let (kind, bounds) = match col.data_type.kind {
            minidb::Kind::Null => ("NULL".into(), "Тип по первому значению".into()),
            minidb::Kind::Int { min, max } => (
                "INT".into(),
                if min != i64::MIN || max != i64::MAX {
                    format!("MIN {min} MAX {max}")
                } else {
                    String::new()
                },
            ),
            minidb::Kind::Float { min, max } => (
                "FLOAT".into(),
                if min != -f64::MAX || max != f64::MAX {
                    format!("MIN {min} MAX {max}")
                } else {
                    String::new()
                },
            ),
            minidb::Kind::String { max_bytes } => (format!("STRING({max_bytes})"), String::new()),
        };
        let mut rules = Vec::new();
        if i == schema.primary_key {
            rules.push("PRIMARY KEY".into());
        }
        if let Some(reference) = &col.references {
            rules.push(format!(
                "REFERENCES {}({})",
                reference.table, reference.column
            ));
        }
        if !bounds.is_empty() {
            rules.push(bounds);
        }
        let cells = [
            col.name.clone(),
            kind,
            if col.data_type.nullable {
                "да".into()
            } else {
                "нет".into()
            },
            rules.join(", "),
        ];
        print_cells(&headers, cells.into_iter());
    }
    if schema
        .columns
        .iter()
        .any(|c| matches!(c.data_type.kind, minidb::Kind::Null))
    {
        println!(
            "Тип NULL: первый INSERT/UPDATE с ненулевым значением определит тип нового столбца."
        );
    }
}

/// Показывает точный источник данных, чтобы не путать активную базу с архивами.
pub(crate) fn database_path(db: &Database) {
    println!("Активная база: {}", db.directory().display());
}
