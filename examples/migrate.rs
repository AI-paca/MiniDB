// Однократный перенос текстовой базы MiniDB в формат фиксированных записей
use minidb::sql::Literal;
use minidb::table::Table;
use minidb::{Database, Expr, Kind, Result, Schema, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Cursor, Read};
use std::path::{Path, PathBuf};

struct OldTable {
    schema: Schema,
    rows: BTreeMap<i64, Vec<Literal>>,
}
type Tables = HashMap<String, OldTable>;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("Укажите каталог: cargo run --example migrate -- data/demo")?;
    if args.next().is_some() {
        return Err("Укажите один каталог старой базы".into());
    }
    let backup = migrate(Path::new(&path))?;
    println!(
        "База перенесена: {path}\nИсходная база сохранена: {}",
        backup.display()
    );
    Ok(())
}

// Создание новой базы с сохранением всех исходных файлов
fn migrate(root: &Path) -> Result<PathBuf> {
    let original = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".lock"))?;
    original
        .try_lock()
        .map_err(|e| format!("Закройте базу перед переносом: {e}"))?;
    let temporary = sibling(root, ".migrate.tmp")?;
    let backup = sibling(root, ".legacy")?;
    if temporary.exists() || backup.exists() {
        return Err(
            "Каталог .migrate.tmp или .legacy уже существует; исходные данные не изменены".into(),
        );
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let filename = entry.file_name();
        if path.extension().is_some_and(|ext| ext == "schema")
            || filename.to_string_lossy().ends_with(".schema.sql")
        {
            return Err("В каталоге уже есть схемы нового формата".into());
        }
    }
    let mut tables = Tables::new();
    let covered = load_checkpoint(root, &mut tables)?;
    load_journal(root, covered, &mut tables)?;
    check_references(&tables)?;
    fs::create_dir(&temporary)?;
    if let Err(error) = write_database(&temporary, &tables) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    drop(original);
    if let Err(error) = install(root, &temporary, &backup) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    Ok(backup)
}

// Получение имени соседнего каталога
fn sibling(root: &Path, suffix: &str) -> Result<PathBuf> {
    let mut name = root
        .file_name()
        .ok_or("Укажите отдельный каталог базы")?
        .to_os_string();
    name.push(suffix);
    Ok(root.with_file_name(name))
}

// Проверка контрольной суммы старой записи журнала
fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

// Чтение завершённой записи с проверкой длины, CRC и COMMIT
fn frame(input: &mut Cursor<&[u8]>, words: &[&str]) -> Result<Option<String>> {
    if words.len() != 3 {
        return Err("Повреждён заголовок старого журнала".into());
    }
    let len: u32 = words[0].parse()?;
    if len == 0 || len as usize > 32 * 1024 * 1024 || words[1].parse::<u32>()? != !len {
        return Err("Повреждена длина записи старого журнала".into());
    }
    let remaining = input.get_ref().len() as u64 - input.position();
    if remaining < len as u64 + 7 {
        return Ok(None);
    }
    let mut bytes = vec![0; len as usize];
    input.read_exact(&mut bytes)?;
    let mut footer = [0; 7];
    input.read_exact(&mut footer)?;
    if &footer != b"COMMIT\n" || checksum(&bytes) != u32::from_str_radix(words[2], 16)? {
        return Err("Повреждён старый журнал: CRC или COMMIT".into());
    }
    Ok(Some(String::from_utf8(bytes)?))
}

// Чтение полного снимка старой базы
fn load_checkpoint(root: &Path, tables: &mut Tables) -> Result<u64> {
    let path = root.join(".checkpoint");
    if !path.exists() {
        return Ok(0);
    }
    let bytes = fs::read(path)?;
    let mut input = Cursor::new(bytes.as_slice());
    let mut line = String::new();
    input.read_line(&mut line)?;
    let covered: u64 = line
        .strip_prefix("MINIDB CHECKPOINT 1 ")
        .ok_or("Неизвестный снимок")?
        .trim()
        .parse()?;
    loop {
        line.clear();
        input.read_line(&mut line)?;
        if line == format!("END {covered}\n") && input.position() == bytes.len() as u64 {
            return Ok(covered);
        }
        let header = line
            .strip_prefix("STATE ")
            .ok_or("Старый снимок не завершён")?;
        let words: Vec<&str> = header.split_whitespace().collect();
        let text = frame(&mut input, &words)?.ok_or("Старый снимок оборван")?;
        apply(&text, tables)?;
    }
}

// Применение завершённых изменений после снимка
fn load_journal(root: &Path, covered: u64, tables: &mut Tables) -> Result<()> {
    let bytes = fs::read(root.join("journal.log"))?;
    let mut input = Cursor::new(bytes.as_slice());
    let mut line = String::new();
    input.read_line(&mut line)?;
    if line != "MINIDB TEXT 2\n" {
        return Err("Перенос поддерживает текстовый журнал MINIDB TEXT 2".into());
    }
    let mut last: Option<u64> = None;
    loop {
        line.clear();
        input.read_line(&mut line)?;
        if !line.ends_with('\n') {
            break;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() != 5 || words[0] != "BEGIN" {
            return Err("Повреждён BEGIN старого журнала".into());
        }
        let sequence: u64 = words[1].parse()?;
        if sequence == 0
            || last.is_some_and(|n| n.checked_add(1) != Some(sequence))
            || (last.is_none() && sequence > covered.saturating_add(1))
        {
            return Err("В старом журнале пропущены операции".into());
        }
        let Some(text) = frame(&mut input, &words[2..])? else {
            break;
        };
        if sequence > covered {
            apply(&text, tables)?;
        }
        last = Some(sequence);
    }
    if last.is_none_or(|n| n < covered) && covered != 0 {
        return Err("Старый журнал не соответствует снимку".into());
    }
    Ok(())
}

// Обработка схемы или изменений одной таблицы
fn apply(text: &str, tables: &mut Tables) -> Result<()> {
    if text.starts_with("CREATE TABLE ") {
        let schema = Schema::from_text(text)?;
        if tables.contains_key(&schema.name) {
            return Err("Повторное создание таблицы в старом журнале".into());
        }
        tables.insert(
            schema.name.clone(),
            OldTable {
                schema,
                rows: BTreeMap::new(),
            },
        );
        return Ok(());
    }
    if let Some(high) = text.strip_prefix("HIGH ") {
        let (name, id) = high.trim().split_once(' ').ok_or("Повреждён счётчик ID")?;
        id.parse::<i64>()?;
        tables.get(name).ok_or("Нет схемы для счётчика ID")?;
        return Ok(());
    }
    let mut lines = text.lines();
    let name = lines
        .next()
        .and_then(|line| line.strip_prefix("TABLE "))
        .ok_or("Нет имени таблицы")?;
    let table = tables
        .get_mut(name)
        .ok_or("В старом журнале нет схемы таблицы")?;
    for line in lines {
        if let Some(id) = line.strip_prefix("DELETE ") {
            table.rows.remove(&id.parse::<i64>()?);
        } else if let Some(fields) = line.strip_prefix("PUT ") {
            let (id, values) = fields
                .split_once('\t')
                .ok_or("Повреждён PUT старого журнала")?;
            let id = id.parse::<i64>()?;
            let values = parse_values(&table.schema, values)?;
            if values[table.schema.primary_key].value().int()? != id {
                return Err("Неверный первичный ключ в старом журнале".into());
            }
            table.rows.insert(id, values);
        } else {
            return Err("Неизвестная операция старого журнала".into());
        }
    }
    Ok(())
}

// Чтение значений старой текстовой записи по типам из схемы
fn parse_values(schema: &Schema, text: &str) -> Result<Vec<Literal>> {
    let fields: Vec<&str> = text.split('\t').collect();
    if fields.len() != schema.columns.len() {
        return Err("Число старых полей не соответствует схеме".into());
    }
    let mut values = Vec::with_capacity(fields.len());
    for (col, field) in schema.columns.iter().zip(fields) {
        let value = if field == "NULL" {
            Literal::Null
        } else {
            match col.data_type.kind {
                Kind::Null => return Err("Старый формат не содержит отложенных типов".into()),
                Kind::Int { .. } => Literal::Int(field.parse()?),
                Kind::Float { .. } => Literal::Float(field.parse()?),
                Kind::String { .. } => Literal::Text(unquote(field)?),
            }
        };
        col.data_type.validate(value.value())?;
        values.push(value);
    }
    Ok(values)
}

// Чтение старой строки с экранированием специальных символов
fn unquote(text: &str) -> Result<String> {
    let text = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or("Нет кавычек старой строки")?;
    let mut result = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        result.push(
            match chars
                .next()
                .ok_or("Оборванное экранирование старой строки")?
            {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '0' => '\0',
                '\\' => '\\',
                '"' => '"',
                _ => return Err("Неизвестное экранирование старой строки".into()),
            },
        );
    }
    Ok(result)
}

// Проверка внешних ключей восстановленных записей
fn check_references(tables: &Tables) -> Result<()> {
    if tables.is_empty() {
        return Err("В старой базе не найдены таблицы".into());
    }
    for table in tables.values() {
        for (i, col) in table.schema.columns.iter().enumerate() {
            if let Some(reference) = &col.references {
                let parent = tables
                    .get(&reference.table)
                    .ok_or("Нет родительской таблицы")?;
                if parent.schema.column(&reference.column)? != parent.schema.primary_key {
                    return Err("Старый FOREIGN KEY ссылается не на PRIMARY KEY".into());
                }
                for values in table.rows.values() {
                    let value = values[i].value();
                    if value != Value::Null && !parent.rows.contains_key(&value.int()?) {
                        return Err(format!(
                            "FOREIGN KEY: {}.{} = {value}",
                            table.schema.name, col.name
                        )
                        .into());
                    }
                }
            }
        }
    }
    Ok(())
}

// Запись и проверка таблиц нового формата
fn write_database(root: &Path, tables: &Tables) -> Result<()> {
    for old in tables.values() {
        let mut table = Table::create(root, old.schema.clone())?;
        for fields in old.rows.values() {
            let vals: Vec<_> = fields.iter().map(Literal::value).collect();
            table.insert(&vals)?;
        }
        table.sync()?;
    }
    let mut db = Database::open(root)?;
    for old in tables.values() {
        let count = db.scan(&old.schema.name, &Expr::All, |row| {
            let id = row.at(old.schema.primary_key)?.int()?;
            let expected = old
                .rows
                .get(&id)
                .ok_or("Лишний первичный ключ после переноса")?;
            for (i, field) in expected.iter().enumerate() {
                if row.at(i)? != field.value() {
                    return Err("Значение изменилось после переноса".into());
                }
            }
            Ok(())
        })?;
        if count != old.rows.len() {
            return Err("Число записей изменилось после переноса".into());
        }
    }
    db.sync()
}

// Замена каталога после успешной проверки новой базы
fn install(root: &Path, temporary: &Path, backup: &Path) -> Result<()> {
    if root.join(".history").exists() {
        fs::copy(root.join(".history"), temporary.join(".history"))?;
    }
    fs::rename(root, backup)?;
    if let Err(error) = fs::rename(temporary, root) {
        fs::rename(backup, root)?;
        return Err(error.into());
    }
    let parent = root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidb::{Column, DataType};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "minidb-migrate-{}-{}",
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

    fn packet(prefix: &str, text: &str) -> String {
        let len = text.len() as u32;
        format!(
            "{prefix} {len} {} {:08x}\n{text}COMMIT\n",
            !len,
            checksum(text.as_bytes())
        )
    }

    fn source(root: &Path) -> Result<()> {
        fs::create_dir(root)?;
        fs::write(root.join(".lock"), b"")?;
        let schema = Schema::new(
            "items",
            vec![
                Column::new("id", DataType::INT),
                Column::new(
                    "name",
                    DataType {
                        kind: Kind::String { max_bytes: 64 },
                        nullable: true,
                    },
                ),
                Column::new("score", DataType::FLOAT),
            ],
            0,
        );
        let mut snapshot = "MINIDB CHECKPOINT 1 4\n".to_owned();
        snapshot.push_str(&packet("STATE", &schema.to_text()));
        snapshot.push_str(&packet("STATE", "HIGH items 9\n"));
        snapshot.push_str(&packet(
            "STATE",
            "TABLE items\nPUT 1\t1\t\"old\"\t0.5\nPUT 2\t2\tNULL\t1\n",
        ));
        snapshot.push_str("END 4\n");
        fs::write(root.join(".checkpoint"), snapshot)?;
        let mut journal = "MINIDB TEXT 2\n".to_owned();
        journal.push_str(&packet("BEGIN 3", "HIGH items 9\n"));
        journal.push_str(&packet("BEGIN 4", "HIGH items 9\n"));
        journal.push_str(&packet("BEGIN 5", "TABLE items\nDELETE 2\n"));
        journal.push_str(&packet(
            "BEGIN 6",
            "TABLE items\nPUT 1\t1\t\"я\\t\\n\\r\\0\\\\\\\"\"\t1e-80\n",
        ));
        journal.push_str(&packet("BEGIN 7", "TABLE items\nPUT 3\t3\tNULL\t2\n"));
        journal.push_str("BEGIN 8 100 4294967195 00000000\npartial");
        fs::write(root.join("journal.log"), journal)?;
        fs::write(root.join(".history"), "select * from items;\n")?;
        Ok(())
    }

    #[test]
    fn snapshot_tail_deletes_and_strings_survive_migration() -> Result<()> {
        assert_eq!(checksum(b"123456789"), 0xcbf43926);
        let temp = Temp::new()?;
        let root = temp.0.join("db");
        source(&root)?;
        let journal = fs::read(root.join("journal.log"))?;
        let snapshot = fs::read(root.join(".checkpoint"))?;
        let backup = migrate(&root)?;
        assert_eq!(fs::read(backup.join("journal.log"))?, journal);
        assert_eq!(fs::read(backup.join(".checkpoint"))?, snapshot);
        assert_eq!(
            fs::read(root.join(".history"))?,
            fs::read(backup.join(".history"))?
        );
        assert!(!root.join("journal.log").exists());
        let mut db = Database::open(&root)?;
        assert_eq!(db.scan("items", &Expr::All, |_| Ok(()))?, 2);
        assert_eq!(
            db.get("items", 1, |row| Ok(row.get("name")?.to_string()))?,
            Some("я\t\n\r\0\\\"".into())
        );
        assert_eq!(
            db.get("items", 1, |row| Ok(
                row.get("score")? == Value::Float(1e-80)
            ))?,
            Some(true)
        );
        assert!(db.get("items", 2, |_| Ok(()))?.is_none());
        assert_eq!(
            db.insert("items", &[Value::Null, Value::Null, Value::Float(0.0)])?,
            4
        );
        Ok(())
    }

    #[test]
    fn invalid_checksum_keeps_source_unchanged() -> Result<()> {
        let temp = Temp::new()?;
        let root = temp.0.join("db");
        source(&root)?;
        let path = root.join("journal.log");
        let text = fs::read_to_string(&path)?.replace("DELETE 2", "DELETE 9");
        fs::write(&path, &text)?;
        assert!(migrate(&root).is_err());
        assert_eq!(fs::read_to_string(path)?, text);
        assert!(!root.join("items.schema.sql").exists());
        assert!(!sibling(&root, ".legacy")?.exists());
        assert!(!sibling(&root, ".migrate.tmp")?.exists());
        Ok(())
    }

    #[test]
    fn lock_and_invalid_validator_keep_source_unchanged() -> Result<()> {
        let temp = Temp::new()?;
        let root = temp.0.join("db");
        source(&root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        lock.try_lock()?;
        assert!(migrate(&root).is_err());
        drop(lock);
        let path = root.join(".checkpoint");
        let text = fs::read_to_string(&path)?;
        let mut tables = Tables::new();
        load_checkpoint(&root, &mut tables)?;
        let mut schema = tables.get("items").ok_or("Нет items")?.schema.clone();
        let old = packet("STATE", &schema.to_text());
        schema.validator = Some("missing_rule".into());
        let broken = text.replace(&old, &packet("STATE", &schema.to_text()));
        fs::write(&path, &broken)?;
        assert!(migrate(&root).is_err());
        assert_eq!(fs::read_to_string(path)?, broken);
        assert!(!sibling(&root, ".legacy")?.exists());
        Ok(())
    }
}
