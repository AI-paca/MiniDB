// Копирование, проверка и установка резервной копии.
use super::{lock, Database, Validator};
use crate::Result;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};

pub(super) fn create(db: &mut Database, name: &Path) -> Result<()> {
    ensure_tables(&db.path)?;
    db.sync()?;
    let destination = backup_path(&db.path, name)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(&destination)?;
    if let Err(error) = copy_tables(&db.path, &destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    Ok(())
}

pub(super) fn restore(
    backup: &Path,
    destination: &Path,
    validators: &[(&str, Validator)],
) -> Result<Database> {
    let backup = backup_path(destination, backup)?;
    let temporary = sibling(destination, ".restore.tmp")?;
    let previous = sibling(destination, ".restore.old")?;
    if destination == temporary
        || destination == previous
        || temporary.exists()
        || previous.exists()
    {
        return Err("Временный каталог восстановления уже существует".into());
    }
    let previous_lock = if destination.exists() {
        Some(lock(destination)?)
    } else {
        None
    };
    if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    checked_copy(&backup, &temporary, validators)?;
    drop(previous_lock);
    let opened = install_restore(&temporary, destination, &previous, validators)?;
    if previous.exists() {
        fs::remove_dir_all(previous)?;
    }
    Ok(opened)
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or("Нет имени каталога базы")?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}

fn empty_error(source: &Path) -> Box<dyn std::error::Error + Send + Sync> {
    if source.join("journal.log").exists() || source.join("journal.wal").exists() {
        "Копия в старом формате: нужны *.schema.sql (или *.schema) и бинарные сегменты".into()
    } else {
        "В базе нет ни одной таблицы: копировать нечего".into()
    }
}

fn ensure_tables(root: &Path) -> Result<()> {
    if crate::storage::schema_files(root)?.is_empty() {
        return Err(empty_error(root));
    }
    Ok(())
}

// Получение пути к резервной копии по имени или явному пути
pub(super) fn backup_path(root: &Path, name: &Path) -> Result<PathBuf> {
    let mut parts = name.components();
    if let (Some(Component::Normal(_)), None) = (parts.next(), parts.next()) {
        let root = fs::canonicalize(root).or_else(|_| -> std::io::Result<PathBuf> {
            if root.is_absolute() {
                Ok(root.to_owned())
            } else {
                Ok(std::env::current_dir()?.join(root))
            }
        })?;
        let mut backups = root.into_os_string();
        backups.push("_backups");
        Ok(PathBuf::from(backups).join(name))
    } else {
        Ok(name.to_owned())
    }
}

// Копирование файлов схем и данных
fn copy_tables(source: &Path, destination: &Path) -> Result<()> {
    let schemas = crate::storage::schema_files(source)?;
    if schemas.is_empty() {
        return Err(empty_error(source));
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        if entry.file_type()?.is_file()
            && schemas.iter().any(|(table, schema)| {
                path == *schema || crate::storage::is_segment(&filename, table)
            })
        {
            let target = destination.join(entry.file_name());
            fs::copy(path, &target)?;
            OpenOptions::new().write(true).open(target)?.sync_all()?;
        }
    }
    crate::storage::sync_directory(destination)?;
    Ok(())
}

// Создание и проверка временной копии базы
fn checked_copy(source: &Path, temporary: &Path, validators: &[(&str, Validator)]) -> Result<()> {
    fs::create_dir(temporary)?;
    let copied = (|| -> Result<()> {
        copy_tables(source, temporary)?;
        let mut check = Database::open_with(temporary, validators)?;
        for table in check.tables.values_mut() {
            table.check()?;
        }
        check.sync()
    })();
    if copied.is_err() {
        let _ = fs::remove_dir_all(temporary);
    }
    copied
}

// Замена каталога базы и возврат старого каталога при ошибке открытия
fn install_restore(
    temporary: &Path,
    destination: &Path,
    previous: &Path,
    validators: &[(&str, Validator)],
) -> Result<Database> {
    if destination.exists() {
        if let Err(error) = fs::rename(destination, previous) {
            let _ = fs::remove_dir_all(temporary);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::rename(temporary, destination) {
        if previous.exists() {
            fs::rename(previous, destination)?;
        }
        let _ = fs::remove_dir_all(temporary);
        return Err(error.into());
    }
    let opened = Database::open_with(destination, validators);
    if opened.is_err() {
        fs::rename(destination, temporary)?;
        if previous.exists() {
            fs::rename(previous, destination)?;
        }
        let _ = fs::remove_dir_all(temporary);
    }
    opened
}
