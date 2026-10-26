mod backup;
mod update;

use crate::record;
use crate::table::Table;
use crate::{Column, Expr, Result, Row, Schema, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_DATABASE_PATH: &str = "sample";

pub type Validator = fn(Row<'_>) -> Result<()>;
const MAX_CHANGE: usize = 16 * 1024 * 1024;

// Владеем Vec и сужаем время жизни только на одну строку. IntoIter::map
// сбрасывает все ссылки в Null и возвращает тот же буфер с 'static-значениями.
// Это безопасный in-place collect (без transmute и без копирования строк).
fn project_row<'row>(
    row: Row<'row>,
    indices: &[usize],
    mut values: Vec<Value<'row>>,
    visit: &mut impl FnMut(&[Value<'_>]) -> Result<()>,
) -> Result<Vec<Value<'static>>> {
    for (value, &index) in values.iter_mut().zip(indices) {
        *value = row.at(index)?;
    }
    let result = visit(&values);
    let cleared = values.into_iter().map(|_| Value::Null).collect();
    result?;
    Ok(cleared)
}

// Блокировка базы данных на время работы
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

// Получение блокировки каталога базы
fn lock(root: &Path) -> Result<Lock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".lock"))?;
    file.try_lock()
        .map_err(|e| format!("База уже открыта другим процессом: {e}"))?;
    Ok(Lock(file))
}

// База данных и открытые таблицы
pub struct Database {
    path: PathBuf,                          // Каталог базы
    _lock: Lock,                            // Блокировка от повторного открытия
    tables: HashMap<String, Table>,         // Таблицы по именам
    validators: HashMap<String, Validator>, // Функции проверки записей по именам
}

impl Database {
    // Открытие базы данных без дополнительных проверок записей
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, &[])
    }

    // Открытие базы и регистрация функций проверки записей
    pub fn open_with(path: impl AsRef<Path>, validators: &[(&str, Validator)]) -> Result<Self> {
        fs::create_dir_all(path.as_ref())?;
        let path = fs::canonicalize(path.as_ref())?;
        let lock = lock(&path)?;
        let mut tables = HashMap::new();
        for (name, _) in crate::storage::schema_files(&path)? {
            tables.insert(name.clone(), Table::open(&path, &name)?);
        }
        // Проверяем, не осталась ли база в старом формате
        if tables.is_empty()
            && (path.join("journal.log").exists() || path.join("journal.wal").exists())
        {
            return Err(format!(
                "Старый формат базы. Перенос: cargo run --locked --offline --example migrate -- {}",
                path.display()
            )
            .into());
        }
        let mut rules = HashMap::new();
        for &(name, rule) in validators {
            if rules.insert(name.to_owned(), rule).is_some() {
                return Err("Повторное имя валидатора".into());
            }
        }
        let db = Self {
            path,
            _lock: lock,
            tables,
            validators: rules,
        };
        for table in db.tables.values() {
            db.check_schema(&table.schema)?;
        }
        Ok(db)
    }

    /// Абсолютный путь единственной открытой базы, а не каталога запуска.
    pub fn directory(&self) -> &Path {
        &self.path
    }

    // Получение таблицы по имени
    fn table(&self, name: &str) -> Result<&Table> {
        self.tables
            .get(name)
            .ok_or_else(|| format!("Нет таблицы {name}").into())
    }

    // Получение таблицы для чтения или изменения данных
    fn table_mut(&mut self, name: &str) -> Result<&mut Table> {
        self.tables
            .get_mut(name)
            .ok_or_else(|| format!("Нет таблицы {name}").into())
    }

    // Получение схемы таблицы по имени
    pub fn schema(&self, table: &str) -> Result<&Schema> {
        Ok(&self.table(table)?.schema)
    }

    /// Общий каталог хранения таблицы. Конкретные пути: schema_file/segment_files.
    pub fn table_directory(&self, name: &str) -> Result<&Path> {
        self.table(name)?;
        Ok(&self.path)
    }

    pub fn schema_file(&self, name: &str) -> Result<&Path> {
        Ok(self.table(name)?.schema_path())
    }

    pub fn segment_files<'a>(&'a self, name: &str) -> Result<impl Iterator<Item = &'a Path>> {
        Ok(self
            .table(name)?
            .segments()
            .iter()
            .map(|segment| segment.path()))
    }

    // Получение отсортированного списка имён таблиц
    pub fn table_names(&self) -> Result<Vec<&str>> {
        let mut names = Vec::with_capacity(self.tables.len());
        for name in self.tables.keys() {
            names.push(name.as_str());
        }
        names.sort_unstable();
        Ok(names)
    }

    // Проверка схемы, внешних ключей и имени функции проверки
    fn check_schema(&self, schema: &Schema) -> Result<()> {
        schema.check()?;
        if let Some(name) = &schema.validator {
            if !self.validators.contains_key(name) {
                return Err(format!("Передайте Rust-валидатор {name} в Database::open_with; SQL-консоль не исполняет Rust-валидаторы").into());
            }
        }
        for col in &schema.columns {
            if let Some(target) = &col.references {
                let parent = if target.table == schema.name {
                    schema
                } else {
                    self.schema(&target.table)?
                };
                if parent.column(&target.column)? != parent.primary_key
                    || !matches!(col.data_type.kind, crate::Kind::Int { .. })
                {
                    return Err("FOREIGN KEY должен быть INT и ссылаться на PRIMARY KEY существующей таблицы".into());
                }
            }
        }
        Ok(())
    }

    // Создание таблицы через основной метод create_table
    pub fn create(&mut self, schema: Schema) -> Result<()> {
        self.create_table(schema)
    }

    // Проверка схемы и создание новой таблицы
    pub fn create_table(&mut self, schema: Schema) -> Result<()> {
        if self.tables.contains_key(&schema.name) {
            return Err("Таблица уже существует".into());
        }
        self.check_schema(&schema)?;
        let name = schema.name.clone();
        let table = Table::create(&self.path, schema)?;
        self.tables.insert(name, table);
        Ok(())
    }

    // Добавление nullable-столбца с новой версией сегмента.
    pub fn add_column(&mut self, table: &str, column: Column) -> Result<()> {
        let schema = self.schema(table)?.with_added_column(column.clone())?;
        self.check_schema(&schema)?;
        self.table_mut(table)?.add_column(column)
    }

    // Проверка наличия родительских записей для внешних ключей
    fn check_values(
        &self,
        schema: &Schema,
        values: &[Value<'_>],
        final_keys: Option<(&HashSet<i64>, &HashSet<i64>)>,
    ) -> Result<()> {
        if values.len() != schema.columns.len() {
            return Err("Неверное число полей".into());
        }
        self.check_foreign_keys(schema, |i| Ok(values[i]), final_keys)
    }

    fn check_foreign_keys<'a>(
        &self,
        schema: &Schema,
        mut at: impl FnMut(usize) -> Result<Value<'a>>,
        final_keys: Option<(&HashSet<i64>, &HashSet<i64>)>,
    ) -> Result<()> {
        for (i, col) in schema.columns.iter().enumerate() {
            if let Some(target) = &col.references {
                let value = at(i)?;
                if value == Value::Null {
                    continue;
                }
                let id = value.int()?;
                let current = self.table(&target.table)?.index.contains_key(&id);
                let present = if target.table == schema.name {
                    match final_keys {
                        Some((removed, new_ids)) => {
                            new_ids.contains(&id) || (current && !removed.contains(&id))
                        }
                        None => current || value == at(schema.primary_key)?,
                    }
                } else {
                    current
                };
                if !present {
                    return Err(format!(
                        "FOREIGN KEY: {}.{} = {value}, в {} нет {} = {value}",
                        schema.name, col.name, target.table, target.column
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    // Поиск функции проверки записи по имени из схемы
    fn validator(&self, schema: &Schema) -> Result<Option<Validator>> {
        match &schema.validator {
            Some(name) => Ok(Some(
                *self
                    .validators
                    .get(name)
                    .ok_or("Не зарегистрирован валидатор")?,
            )),
            None => Ok(None),
        }
    }

    // Добавление записи с проверкой внешних ключей и авто-ID
    pub fn insert(&mut self, table: &str, values: &[Value<'_>]) -> Result<i64> {
        let stored = self.table(table)?;
        let schema = &stored.schema;
        let normalized = stored.normalize_insert(values)?;
        let vals = normalized.as_slice();
        let pk = schema.primary_key;
        self.check_values(schema, vals, None)?;
        let validator = self.validator(schema)?;
        let table = self.table_mut(table)?;
        let row = table.prepare(vals, record::FLAG_FREE)?;
        let id = row.at(pk)?.int()?;
        if let Some(rule) = validator {
            rule(row)?;
        }
        table.insert_prepared()?;
        Ok(id)
    }

    /// Проверка версии перед любыми изменениями типизированного потребителя.
    pub fn check_version(&self, table: &str, version: u32) -> Result<()> {
        self.table(table)?.check_version(version)
    }

    pub fn insert_versioned(
        &mut self,
        table: &str,
        version: u32,
        values: &[Value<'_>],
    ) -> Result<i64> {
        self.check_version(table, version)?;
        self.insert(table, values)
    }

    // Получение записи по первичному ключу и передача её в функцию чтения
    pub fn get<T>(
        &mut self,
        table: &str,
        id: i64,
        read: impl FnOnce(Row<'_>) -> Result<T>,
    ) -> Result<Option<T>> {
        match self.table_mut(table)?.get(id)? {
            Some(row) => Ok(Some(read(row)?)),
            None => Ok(None),
        }
    }

    // Перебор записей таблицы по условию
    pub fn scan(
        &mut self,
        table: &str,
        filter: &Expr<'_>,
        mut visit: impl FnMut(Row<'_>) -> Result<()>,
    ) -> Result<usize> {
        let filter = filter.prepare(self.schema(table)?)?;
        let mut count = 0;
        self.table_mut(table)?.scan(|row| {
            if filter.matches(row)? {
                visit(row)?;
                count += 1;
            }
            Ok(())
        })?;
        Ok(count)
    }
    // Получение выбранных столбцов по условию
    pub fn select(
        &mut self,
        table: &str,
        columns: &[&str],
        filter: &Expr<'_>,
        mut visit: impl FnMut(&[Value<'_>]) -> Result<()>,
    ) -> Result<usize> {
        let schema = self.schema(table)?;
        if columns.len() > 256 {
            return Err("Проекция превышает 256 полей".into());
        }
        let indices: Vec<usize> = if columns.is_empty() {
            (0..schema.columns.len()).collect()
        } else {
            let mut indices = Vec::with_capacity(columns.len());
            for name in columns {
                indices.push(schema.column(name)?);
            }
            indices
        };
        let mut buffer = vec![Value::Null; indices.len()];
        self.scan(table, filter, |row| {
            buffer = project_row(row, &indices, std::mem::take(&mut buffer), &mut visit)?;
            Ok(())
        })
    }

    /// Проверяет пакет перед записью; возвращает число фактически изменённых строк.
    /// Строки, совпавшие с фильтром, но сохранившие все значения, не учитываются.
    pub fn update(
        &mut self,
        table: &str,
        filter: &Expr<'_>,
        values: &[(&str, Value<'_>)],
    ) -> Result<usize> {
        update::execute(self, table, filter, values)
    }

    // Проверка внешних ключей и вызов функции проверки записи
    fn validate_row(
        &self,
        row: Row<'_>,
        removed: &HashSet<i64>,
        new_ids: &HashSet<i64>,
    ) -> Result<()> {
        // apply проверяет общий Layout один раз перед этим циклом.
        row.validate_fields()?;
        self.check_foreign_keys(row.schema, |i| row.at(i), Some((removed, new_ids)))?;
        if let Some(rule) = self.validator(row.schema)? {
            rule(row)?;
        }
        Ok(())
    }

    // Удаление записей по условию с проверкой внешних ссылок
    pub fn delete(&mut self, table: &str, filter: &Expr<'_>) -> Result<usize> {
        let pk = self.schema(table)?.primary_key;
        let mut ids = HashSet::new();
        self.scan(table, filter, |row| {
            ids.insert(row.at(pk)?.int()?);
            if ids.len() > MAX_CHANGE / 8 {
                return Err("DELETE слишком велик; разбейте операцию".into());
            }
            Ok(())
        })?;
        self.check_references(table, &ids, &ids)?;
        let mut count = 0;
        for id in ids {
            count += usize::from(self.table_mut(table)?.remove(id)?);
        }
        Ok(count)
    }

    // Поиск действующих записей, ссылающихся на удаляемые ключи
    fn check_references(
        &mut self,
        parent: &str,
        ids: &HashSet<i64>,
        replaced_self_rows: &HashSet<i64>,
    ) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for table in self.tables.values_mut() {
            let mut columns = Vec::new();
            for (i, col) in table.schema.columns.iter().enumerate() {
                if col.references.as_ref().is_some_and(|r| r.table == parent) {
                    columns.push(i);
                }
            }
            if !columns.is_empty() {
                let same_table = table.schema.name == parent;
                table.scan(|row| {
                    if same_table
                        && replaced_self_rows.contains(&row.at(row.schema.primary_key)?.int()?)
                    {
                        return Ok(());
                    }
                    for &column in &columns {
                        if let Value::Int(id) = row.at(column)? {
                            if ids.contains(&id) {
                                return Err("RESTRICT: на запись ссылается FOREIGN KEY".into());
                            }
                        }
                    }
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
    /// Соединяет INT-поле `left.column` исключительно с PRIMARY KEY таблицы `right`.
    /// Для явно заданной правой колонки используйте `join_on`.
    pub fn join(
        &mut self,
        left: &str,
        column: &str,
        right: &str,
        filter: &Expr<'_>,
        visit: impl FnMut(Row<'_>, Row<'_>) -> Result<()>,
    ) -> Result<usize> {
        let right_pk = self.schema(right)?.columns[self.schema(right)?.primary_key]
            .name
            .clone();
        self.join_on(left, column, right, &right_pk, filter, visit)
    }

    /// Явный ON: правая колонка проверяется и должна быть PRIMARY KEY.
    pub fn join_on(
        &mut self,
        left: &str,
        column: &str,
        right: &str,
        right_column: &str,
        filter: &Expr<'_>,
        mut visit: impl FnMut(Row<'_>, Row<'_>) -> Result<()>,
    ) -> Result<usize> {
        let right_schema = self.schema(right)?;
        if right_schema.column(right_column)? != right_schema.primary_key {
            return Err("Справа в JOIN должен быть PRIMARY KEY".into());
        }
        let index = self.schema(left)?.column(column)?;
        if !matches!(
            self.schema(left)?.columns[index].data_type.kind,
            crate::Kind::Int { .. }
        ) {
            return Err("JOIN ожидает INT-поле".into());
        }
        if left == right {
            return Err("Self JOIN не поддерживается".into());
        }
        let filter = filter.prepare(self.schema(left)?)?;
        let [Some(left_table), Some(right_table)] = self.tables.get_disjoint_mut([left, right])
        else {
            return Err("Нет таблицы для JOIN".into());
        };
        let mut count = 0;
        left_table.scan(|row| {
            if filter.matches(row)? {
                if let Value::Int(id) = row.at(index)? {
                    if let Some(other) = right_table.get(id)? {
                        visit(row, other)?;
                        count += 1;
                    }
                }
            }
            Ok(())
        })?;
        Ok(count)
    }

    // Сохранение всех таблиц на диск
    pub fn sync(&mut self) -> Result<()> {
        for table in self.tables.values_mut() {
            table.sync()?;
        }
        crate::storage::sync_directory(&self.path)?;
        Ok(())
    }

    /// Разрешает короткое имя копии относительно этой базы, включая RESTORE в CLI.
    pub fn backup_location(&self, name: impl AsRef<Path>) -> Result<PathBuf> {
        backup::backup_path(&self.path, name.as_ref())
    }

    // Создание резервной копии файлов схем и данных.
    pub fn backup(&mut self, name: impl AsRef<Path>) -> Result<()> {
        backup::create(self, name.as_ref())
    }

    // Восстановление из проверенной временной копии.
    pub fn restore(
        backup: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        validators: &[(&str, Validator)],
    ) -> Result<Self> {
        backup::restore(backup.as_ref(), destination.as_ref(), validators)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if let Err(error) = self.sync() {
            let _ = writeln!(
                std::io::stderr(),
                "Ошибка сохранения базы {}: {error}",
                self.path.display()
            );
        }
    }
}
