// Текстовый формат схемы таблицы
use crate::{Kind, Schema};
use std::fmt::Write;

// Получение текстового описания схемы таблицы
pub fn schema_text(schema: &Schema) -> String {
    let mut text = format!("CREATE TABLE {} (\n", schema.name);
    for (i, col) in schema.columns.iter().enumerate() {
        let kind = match col.data_type.kind {
            Kind::Int { min, max } => format!("INT MIN {min} MAX {max}"),
            Kind::Float { min, max } => format!("FLOAT MIN {min:e} MAX {max:e}"),
            Kind::String { max_bytes } => format!("STRING({max_bytes})"),
        };
        let _ = write!(
            text,
            "  {} {} {}",
            col.name,
            kind,
            if col.data_type.nullable {
                "NULL"
            } else {
                "NOT NULL"
            }
        );
        if i == schema.primary_key {
            text.push_str(" PRIMARY KEY");
        }
        if let Some(target) = &col.references {
            let _ = write!(text, " REFERENCES {}({})", target.table, target.column);
        }
        text.push_str(if i + 1 == schema.columns.len() {
            "\n"
        } else {
            ",\n"
        });
    }
    text.push(')');
    if let Some(name) = &schema.validator {
        let _ = write!(text, " VALIDATOR {name}");
    }
    text.push_str(";\n");
    let _ = writeln!(text, "VERSION {}", schema.version);
    for (version, count) in schema.history() {
        let _ = writeln!(text, "COLUMNS_AT_VERSION {version} = {count}");
    }
    text
}

// Каталог синхронизируется только там, где std::fs::File поддерживает этот путь.
pub(crate) fn sync_directory(path: &std::path::Path) -> crate::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Новая схема — name.schema.sql; прежняя name.schema остаётся читаемой.
pub(crate) fn schema_path(root: &std::path::Path, name: &str) -> crate::Result<std::path::PathBuf> {
    let canonical = root.join(format!("{name}.schema.sql"));
    let previous = root.join(format!("{name}.schema"));
    if canonical.exists() && previous.exists() {
        return Err(format!("Две схемы таблицы {name}: {name}.schema.sql и {name}.schema").into());
    }
    Ok(if previous.exists() {
        previous
    } else {
        canonical
    })
}

pub(crate) fn segment_path(root: &std::path::Path, name: &str, version: u32) -> std::path::PathBuf {
    root.join(format!("{name}.v{version}.data"))
}

pub(crate) fn existing_segment(
    root: &std::path::Path,
    name: &str,
    version: u32,
) -> crate::Result<std::path::PathBuf> {
    let canonical = segment_path(root, name, version);
    let previous = root.join(format!("{name}.data"));
    if version == 1 && previous.exists() {
        if canonical.exists() {
            return Err(format!("Несколько файлов сегмента {name} VERSION {version}").into());
        }
        return Ok(previous);
    }
    Ok(canonical)
}

pub(crate) fn is_segment(filename: &str, table: &str) -> bool {
    filename == format!("{table}.data")
        || filename
            .strip_prefix(&format!("{table}.v"))
            .and_then(|rest| rest.strip_suffix(".data"))
            .is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
            })
}

/// Точный фильтр *.schema.sql / *.schema, без эвристик по содержимому или dot-файлам.
pub(crate) fn schema_files(
    root: &std::path::Path,
) -> crate::Result<Vec<(String, std::path::PathBuf)>> {
    let mut files = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let filename = entry.file_name();
        let Some(filename) = filename.to_str() else {
            continue;
        };
        let Some(name) = filename
            .strip_suffix(".schema.sql")
            .or_else(|| filename.strip_suffix(".schema"))
        else {
            continue;
        };
        crate::types::identifier(name)?;
        if files.insert(name.to_owned(), entry.path()).is_some() {
            return Err(format!("Две схемы таблицы {name}").into());
        }
    }
    Ok(files.into_iter().collect())
}
