// Текстовый формат схемы таблицы
use crate::{Kind, Schema};
use std::fmt::Write;

// Получение текстового описания схемы таблицы
pub fn schema_text(schema: &Schema) -> String {
    let mut text = format!("CREATE TABLE {} (\n", schema.name);
    for (i, col) in schema.columns.iter().enumerate() {
        let kind = match col.data_type.kind {
            Kind::Null => "NULL".into(),
            Kind::Int { min, max } => format!("INT MIN {min} MAX {max}"),
            Kind::Float { min, max } => format!("FLOAT MIN {min:e} MAX {max:e}"),
            Kind::String { max_bytes } => format!("STRING({max_bytes})"),
        };
        let _ = write!(
            text,
            "  {} {}{}",
            col.name,
            kind,
            if matches!(col.data_type.kind, Kind::Null) {
                ""
            } else if col.data_type.nullable {
                " NULL"
            } else {
                " NOT NULL"
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

/// v1 рядом со схемой; последующие версии — в каталогах vN, без расширений.
pub(crate) fn segment_path(root: &std::path::Path, name: &str, version: u32) -> std::path::PathBuf {
    if version == 1 {
        root.join(name)
    } else {
        root.join(format!("v{version}")).join(name)
    }
}

pub(crate) fn existing_segment(
    root: &std::path::Path,
    name: &str,
    version: u32,
) -> crate::Result<std::path::PathBuf> {
    let binary = root.join(format!("{name}.v{version}.data"));
    if binary.exists() || root.join(format!("{name}.data")).exists() {
        return Err("Каталог содержит бинарные данные: откройте его в ветке binary; текстовая ветка хранит UTF-8 без расширений".into());
    }
    Ok(segment_path(root, name, version))
}

pub(crate) fn segment_files(
    root: &std::path::Path,
    name: &str,
) -> crate::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    if root.join(format!("{name}.data")).exists() {
        return Err("Бинарная база: используйте ветку binary".into());
    }
    if root.join(name).exists() {
        files.push(root.join(name));
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let filename = entry.file_name();
        if filename
            .to_str()
            .and_then(|s| s.strip_prefix(&format!("{name}.v")))
            .and_then(|s| s.strip_suffix(".data"))
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("Бинарная база: используйте ветку binary".into());
        }
        let Some(version) = filename.to_str().and_then(|s| s.strip_prefix('v')) else {
            continue;
        };
        if !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_digit())
            && entry.file_type()?.is_dir()
        {
            let path = entry.path().join(name);
            if path.exists() {
                files.push(path);
            }
        }
    }
    Ok(files)
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
