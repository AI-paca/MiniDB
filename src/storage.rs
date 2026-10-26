// Текстовый формат схемы таблицы
use crate::{Kind, Schema};
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

// Уникальное служебное имя не блокирует запись после обрыва предыдущего процесса
// и не перезаписывает пользовательские файлы с суффиксом .tmp.
pub(crate) fn temporary_file(
    path: &std::path::Path,
) -> crate::Result<(std::path::PathBuf, std::fs::File)> {
    loop {
        let mut name = path.file_name().ok_or("Нет имени файла")?.to_os_string();
        name.push(format!(
            ".tmp.{}.{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = path.with_file_name(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

pub(crate) fn is_temporary(filename: &str, table: &str) -> bool {
    let Some((base, suffix)) = filename.rsplit_once(".tmp.") else {
        return false;
    };
    let mut parts = suffix.split('.');
    if !matches!((parts.next(), parts.next(), parts.next()), (Some(pid), Some(serial), None)
        if !pid.is_empty() && !serial.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) && serial.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    base == format!("{table}_schema")
        || base
            .strip_prefix(&format!("{table}_"))
            .is_some_and(|generation| {
                !generation.is_empty() && generation.bytes().all(|b| b.is_ascii_digit())
            })
}

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
    let _ = writeln!(text, "GENERATION {}", schema.generation);
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

/// Единственное каноническое имя схемы; каталог таблиц выводится из *_schema.
pub(crate) fn schema_path(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    root.join(format!("{name}_schema"))
}

pub(crate) fn data_path(root: &std::path::Path, name: &str, generation: u64) -> std::path::PathBuf {
    root.join(format!("{name}_{generation}"))
}

/// Только десятичные суффиксы: schema/tmp/backup и каталоги не являются данными.
pub(crate) fn data_files(
    root: &std::path::Path,
    name: &str,
) -> crate::Result<Vec<(u64, std::path::PathBuf)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let filename = entry.file_name();
        let Some(suffix) = filename
            .to_str()
            .and_then(|s| s.strip_prefix(&format!("{name}_")))
        else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(generation) = suffix.parse::<u64>() {
            files.push((generation, entry.path()));
        }
    }
    files.sort_by_key(|(generation, _)| *generation);
    Ok(files)
}

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
        if filename.ends_with(".schema.sql") || filename.ends_with(".schema") {
            return Err("Старый формат схемы/сегментов не поддерживается; экспортируйте данные прежней программой; новую базу можно наполнить SQL-командами из файла sample".into());
        }
        let Some(name) = filename.strip_suffix("_schema") else {
            continue;
        };
        crate::types::identifier(name)?;
        if files.insert(name.to_owned(), entry.path()).is_some() {
            return Err(format!("Две схемы таблицы {name}").into());
        }
    }
    Ok(files.into_iter().collect())
}
