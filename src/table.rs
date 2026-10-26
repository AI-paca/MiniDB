use crate::layout::Layout;
pub use crate::record::Row;
use crate::record::{self, FLAG_DEAD, FLAG_FREE, FLAG_LIVE};
pub use crate::schema::{Column, Reference, Schema};
use crate::text_file::TextFile;
use crate::{Result, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const READ_BUFFER_SIZE: usize = 64 * 1024;
const HIGHEST_ID_PREFIX: &str = "-- minidb-highest-id: ";

#[derive(Debug)]
pub struct IndexMismatch;
impl std::fmt::Display for IndexMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Индекс рассинхронизирован")
    }
}
impl std::error::Error for IndexMismatch {}

/// Один файл данных. Смещения индекса относятся к фиксированным слотам в RAM.
pub struct Table {
    pub(crate) schema: Schema,
    pub(crate) layout: Layout,
    file: TextFile,
    path: PathBuf,
    pub(crate) index: HashMap<i64, u64>,
    pub(crate) highest_id: i64,
    saved_highest_id: i64,
    schema_path: PathBuf,
    write_failed: bool,
    slot_count: u64,
    free: Vec<u64>,
    buf: Vec<u8>,
    scratch: Vec<u8>,
    pub(crate) write_barriers: bool,
}

impl Table {
    pub fn schema(&self) -> &Schema {
        &self.schema
    }
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn schema_path(&self) -> &Path {
        &self.schema_path
    }
    pub fn data_file(&self) -> &Path {
        &self.path
    }
    pub fn slot_count(&self) -> u64 {
        self.slot_count
    }
    pub fn free_slot_count(&self) -> usize {
        self.free.len()
    }

    pub fn create(root: &Path, schema: Schema) -> Result<Self> {
        schema.check()?;
        let layout = Layout::from_schema(&schema)?;
        fs::create_dir_all(root)?;
        let schema_path = crate::storage::schema_path(root, &schema.name);
        if schema_path.exists() || !crate::storage::data_files(root, &schema.name)?.is_empty() {
            return Err("Файлы таблицы уже существуют".into());
        }
        let path = crate::storage::data_path(root, &schema.name, schema.generation);
        let file = TextFile::create(&path, &schema)?;
        let table = Self {
            buf: vec![0; layout.row_size],
            scratch: Vec::new(),
            schema,
            layout,
            file,
            path,
            index: HashMap::new(),
            highest_id: 0,
            saved_highest_id: 0,
            schema_path,
            write_failed: false,
            slot_count: 0,
            free: Vec::new(),
            write_barriers: true,
        };
        if let Err(error) = table.save_schema(&table.schema, 0) {
            // После rename схема уже могла стать видимой: её данные удалять нельзя.
            if !table.schema_path.exists() {
                let _ = fs::remove_file(&table.path);
            }
            return Err(error);
        }
        Ok(table)
    }

    pub fn open(root: &Path, name: &str) -> Result<Self> {
        crate::types::identifier(name)?;
        let schema_path = crate::storage::schema_path(root, name);
        let text = fs::read_to_string(&schema_path)?;
        let schema = Schema::from_text(&text)?;
        if schema.name != name {
            return Err("Имя файла схемы не совпадает с именем таблицы".into());
        }
        let layout = Layout::from_schema(&schema)?;
        let path = crate::storage::data_path(root, name, schema.generation);
        // Не превращаем потерю текущего файла в пустую таблицу и не удаляем
        // оставшиеся поколения, пока не убедились, что выбранный файл читается.
        let file = TextFile::open(&path, &schema)?;
        let mut highest_id = 0;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix(HIGHEST_ID_PREFIX) {
                let id: i64 = value.parse().map_err(|_| "Повреждён счётчик auto-ID")?;
                if id < 0 {
                    return Err("Повреждён счётчик auto-ID".into());
                }
                highest_id = highest_id.max(id);
            }
        }
        let mut table = Self {
            buf: vec![0; layout.row_size],
            scratch: Vec::new(),
            schema,
            layout,
            file,
            path,
            index: HashMap::new(),
            highest_id,
            saved_highest_id: highest_id,
            schema_path,
            write_failed: false,
            slot_count: 0,
            free: Vec::new(),
            write_barriers: true,
        };
        table.rebuild_loaded_index()?;
        let mut removed = false;
        for (_, candidate) in crate::storage::data_files(root, name)? {
            if candidate != table.path {
                fs::remove_file(candidate)?;
                removed = true;
            }
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|filename| crate::storage::is_temporary(filename, name))
            {
                fs::remove_file(entry.path())?;
                removed = true;
            }
        }
        if removed {
            crate::storage::sync_directory(root)?;
        }
        Ok(table)
    }

    pub(crate) fn resolve_types(&mut self, schema: &Schema) -> Result<()> {
        if self.schema == *schema {
            return Ok(());
        }
        self.check_ready()?;
        crate::schema::check_resolution(&self.schema, schema)?;
        let mut next = schema.clone();
        next.generation = next
            .generation
            .checked_add(1)
            .ok_or("Исчерпаны поколения")?;
        self.rewrite(next)
    }

    fn infer_for_values(
        &mut self,
        values: &[Value<'_>],
        flag: u8,
        old_id: Option<i64>,
    ) -> Result<()> {
        let entries: Vec<_> = values.iter().copied().enumerate().collect();
        let Some(schema) = self.schema.infer_types(&entries)? else {
            return Ok(());
        };
        let layout = Layout::from_schema(&schema)?;
        let mut bytes = vec![0; layout.row_size];
        record::encode_validated_layout(&layout, &schema, values, flag, &mut bytes)?;
        let id = values[schema.primary_key].int()?;
        if self.index.contains_key(&id) && old_id != Some(id) {
            return Err("PRIMARY KEY дублируется".into());
        }
        self.resolve_types(&schema)
    }

    pub fn rebuild_index(&mut self) -> Result<()> {
        self.write_failed = true;
        let published = Schema::from_text(&fs::read_to_string(&self.schema_path)?)?;
        if published != self.schema {
            return Err(
                "Схема на диске изменилась: повторно откройте базу перед продолжением".into(),
            );
        }
        self.file.reload()?;
        self.rebuild_loaded_index()
    }

    fn rebuild_loaded_index(&mut self) -> Result<()> {
        self.write_failed = true;
        self.index.clear();
        self.free.clear();
        let size = self.layout.row_size as u64;
        let len = self.file.len();
        if !len.is_multiple_of(size) {
            return Err("Неполный слот в памяти".into());
        }
        self.slot_count = len / size;
        self.file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, &mut self.file);
        for slot in 0..self.slot_count {
            reader.read_exact(&mut self.buf)?;
            match record::flag(&self.buf)? {
                FLAG_LIVE | FLAG_DEAD => {
                    let row = Row {
                        bytes: &self.buf,
                        schema: &self.schema,
                        layout: &self.layout,
                    };
                    let id = row.at(self.schema.primary_key)?.int()?;
                    self.highest_id = self.highest_id.max(id);
                    if row.live() {
                        if self.index.insert(id, slot * size).is_some() {
                            return Err("Файл данных повреждён: PRIMARY KEY дублируется".into());
                        }
                    } else {
                        self.free.push(slot);
                    }
                }
                FLAG_FREE => self.free.push(slot),
                _ => unreachable!(),
            }
        }
        self.write_failed = false;
        Ok(())
    }

    fn check_ready(&self) -> Result<()> {
        if self.write_failed {
            return Err(
                "После ошибки записи требуется rebuild_index или повторное открытие".into(),
            );
        }
        Ok(())
    }

    // Точка переключения: данные уже синхронизированы, затем схема и каталог.
    fn save_schema(&self, schema: &Schema, highest: i64) -> Result<()> {
        let (temporary, mut output) = crate::storage::temporary_file(&self.schema_path)?;
        let result = (|| -> Result<()> {
            output.write_all(schema.to_text().as_bytes())?;
            if highest > 0 {
                writeln!(output, "{HIGHEST_ID_PREFIX}{highest}")?;
            }
            output.sync_all()?;
            drop(output);
            fs::rename(&temporary, &self.schema_path)?;
            crate::storage::sync_directory(self.schema_path.parent().ok_or("Нет каталога схемы")?)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn checkpoint_highest_id(&mut self) -> Result<()> {
        if self.highest_id > self.saved_highest_id {
            self.save_schema(&self.schema, self.highest_id)?;
            self.saved_highest_id = self.highest_id;
        }
        Ok(())
    }

    fn preserve_dead_slot(&mut self) -> Result<()> {
        let Some(&slot) = self.free.last() else {
            return Ok(());
        };
        self.file
            .seek(SeekFrom::Start(slot * self.layout.row_size as u64))?;
        self.scratch.resize(self.layout.row_size, 0);
        self.file.read_exact(&mut self.scratch)?;
        let old_id = match record::flag(&self.scratch)? {
            FLAG_DEAD => Row {
                bytes: &self.scratch,
                schema: &self.schema,
                layout: &self.layout,
            }
            .at(self.schema.primary_key)?
            .int()?,
            FLAG_FREE => return Ok(()),
            _ => return Err(IndexMismatch.into()),
        };
        if old_id > self.saved_highest_id {
            self.checkpoint_highest_id()?;
        }
        Ok(())
    }

    pub fn add_column(&mut self, column: Column) -> Result<()> {
        self.check_ready()?;
        self.rewrite(self.schema.with_added_column(column)?)
    }

    pub fn vacuum(&mut self) -> Result<u64> {
        self.check_ready()?;
        let dead = self.slot_count - self.index.len() as u64;
        if dead == 0 {
            return Ok(0);
        }
        let mut schema = self.schema.clone();
        schema.generation = schema
            .generation
            .checked_add(1)
            .ok_or("Исчерпаны поколения")?;
        self.rewrite(schema)?;
        Ok(dead)
    }

    fn encode_rewritten<'row>(
        row: Row<'row>,
        layout: &Layout,
        schema: &Schema,
        mut values: Vec<Value<'row>>,
        out: &mut [u8],
    ) -> Result<Vec<Value<'static>>> {
        for (i, value) in values.iter_mut().enumerate() {
            *value = if i < row.schema.columns.len() {
                row.at(i)?
            } else {
                Value::Null
            };
        }
        record::encode_validated_layout(layout, schema, &values, FLAG_LIVE, out)?;
        Ok(values.into_iter().map(|_| Value::Null).collect())
    }

    // ALTER, разрешение типа и VACUUM собирают все LIVE-строки в новом поколении.
    fn rewrite(&mut self, schema: Schema) -> Result<()> {
        self.check_ready()?;
        let layout = Layout::from_schema(&schema)?;
        layout.validate(&schema)?;
        if self.schema.generation.checked_add(1) != Some(schema.generation) {
            return Err("Перезапись требует GENERATION + 1".into());
        }
        let root = self
            .schema_path
            .parent()
            .ok_or("Нет каталога схемы")?
            .to_owned();
        let path = crate::storage::data_path(&root, &schema.name, schema.generation);
        // create_new не затирает чужой файл при коллизии.
        let mut target = TextFile::create(&path, &schema)?;
        let mut out = vec![0; layout.row_size];
        let new_columns = schema.columns.len();
        let mut written = 0;
        let result = (|| -> Result<()> {
            self.file.seek(SeekFrom::Start(0))?;
            let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, &mut self.file);
            let mut values = vec![Value::Null; new_columns];
            for _ in 0..self.slot_count {
                reader.read_exact(&mut self.buf)?;
                if record::flag(&self.buf)? != FLAG_LIVE {
                    continue;
                }
                let row = Row {
                    bytes: &self.buf,
                    schema: &self.schema,
                    layout: &self.layout,
                };
                values = Self::encode_rewritten(
                    row,
                    &layout,
                    &schema,
                    std::mem::take(&mut values),
                    &mut out,
                )?;
                target.write_all(&out)?;
                written += 1;
            }
            target.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        if let Err(error) = self.save_schema(&schema, self.highest_id) {
            let committed = fs::read_to_string(&self.schema_path)
                .ok()
                .and_then(|text| Schema::from_text(&text).ok())
                .is_some_and(|current| current.generation == schema.generation);
            if committed {
                self.write_failed = true;
            } else {
                let _ = fs::remove_file(&path);
            }
            return Err(error);
        }
        let previous = std::mem::replace(&mut self.path, path);
        self.file = target;
        self.schema = schema;
        self.buf.resize(layout.row_size, 0);
        self.layout = layout;
        self.slot_count = written;
        self.saved_highest_id = self.highest_id;
        self.rebuild_loaded_index()?;
        // Неудачная уборка не отменяет уже опубликованное поколение; open повторит её.
        if fs::remove_file(previous).is_ok() {
            crate::storage::sync_directory(&root)?;
        }
        Ok(())
    }
    // Получение записи из рабочего буфера
    fn row(&self) -> Row<'_> {
        Row {
            schema: &self.schema,
            layout: &self.layout,
            bytes: &self.buf[..self.layout.row_size],
        }
    }

    // Подготовка записи без генерации ID; вызывающий код задаёт семантику NULL.
    pub(crate) fn prepare(&mut self, vals: &[Value<'_>], flag: u8) -> Result<Row<'_>> {
        self.check_ready()?;
        record::encode_validated_layout(&self.layout, &self.schema, vals, flag, &mut self.buf)?;
        Ok(self.row())
    }

    // Единственное место подстановки auto-ID для обоих уровней API.
    pub(crate) fn normalize_insert<'a>(&self, vals: &[Value<'a>]) -> Result<Vec<Value<'a>>> {
        self.check_ready()?;
        if vals.len() != self.schema.columns.len() {
            return Err("Неверное число полей".into());
        }
        let mut values = vals.to_vec();
        let pk = &mut values[self.schema.primary_key];
        if *pk == Value::Null {
            *pk = Value::Int(self.highest_id.checked_add(1).ok_or("Исчерпаны auto-ID")?);
        }
        Ok(values)
    }

    // Добавление записи с возвратом её смещения.
    pub fn insert(&mut self, vals: &[Value<'_>]) -> Result<u64> {
        let values = self.normalize_insert(vals)?;
        if self
            .schema
            .columns
            .iter()
            .any(|c| matches!(c.data_type.kind, crate::Kind::Null))
        {
            self.infer_for_values(&values, FLAG_FREE, None)?;
        }
        self.prepare(&values, FLAG_FREE)?;
        self.insert_prepared()
    }

    fn append_current(&mut self) -> Result<u64> {
        let id = self.row().at(self.schema.primary_key)?.int()?;
        self.preserve_dead_slot()?;
        let slot = if let Some(&slot) = self.free.last() {
            slot
        } else {
            let slot = self.slot_count;
            self.file
                .set_len((slot + 1) * self.layout.row_size as u64)?;
            self.slot_count += 1;
            self.free.push(slot);
            slot
        };
        let offset = slot * self.layout.row_size as u64;
        self.buf[0] = FLAG_LIVE;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&self.buf)?;
        if self.write_barriers {
            self.file.sync_data()?;
        }
        self.free.pop();
        self.highest_id = self.highest_id.max(id);
        Ok(offset)
    }
    pub(crate) fn insert_prepared(&mut self) -> Result<u64> {
        self.check_ready()?;
        if self.row().flag()? != FLAG_FREE {
            return Err("INSERT ожидает подготовленную FREE-запись".into());
        }
        let id = self.row().at(self.schema.primary_key)?.int()?;
        if self.index.contains_key(&id) {
            return Err("PRIMARY KEY дублируется".into());
        }
        self.write_failed = true;
        let location = self.append_current()?;
        self.index.insert(id, location);
        self.write_failed = false;
        Ok(location)
    }

    // Обновление записи по первичному ключу
    pub fn update(&mut self, pk: i64, vals: &[Value<'_>]) -> Result<()> {
        self.check_ready()?;
        if !self.index.contains_key(&pk) {
            return Err("Запись не существует".into());
        }
        if vals.len() != self.schema.columns.len() {
            return Err("Неверное число полей".into());
        }
        let column = self.schema.primary_key;
        if self
            .schema
            .columns
            .iter()
            .any(|c| matches!(c.data_type.kind, crate::Kind::Null))
        {
            let mut values = vals.to_vec();
            if values[column] == Value::Null {
                values[column] = Value::Int(pk);
            }
            self.infer_for_values(&values, FLAG_LIVE, Some(pk))?;
        }
        let id = if vals.get(column) == Some(&Value::Null) {
            let mut values = vals.to_vec();
            values[column] = Value::Int(pk);
            self.prepare(&values, FLAG_LIVE)?.at(column)?.int()?
        } else {
            self.prepare(vals, FLAG_LIVE)?.at(column)?.int()?
        };
        self.update_prepared(pk, id)
    }

    fn update_prepared(&mut self, pk: i64, id: i64) -> Result<()> {
        self.check_ready()?;
        let location = *self.index.get(&pk).ok_or("Запись не существует")?;
        if pk != id && self.index.contains_key(&id) {
            return Err("PRIMARY KEY дублируется".into());
        }
        self.write_failed = true;
        self.write_at(location, pk)?;
        self.index.remove(&pk);
        self.index.insert(id, location);
        self.write_failed = false;
        Ok(())
    }

    fn write_at(&mut self, offset: u64, old_id: i64) -> Result<()> {
        let id = self.row().at(self.schema.primary_key)?.int()?;
        if old_id != id && old_id > self.saved_highest_id {
            self.checkpoint_highest_id()?;
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&self.buf)?;
        if self.write_barriers {
            self.file.sync_data()?;
        }
        self.highest_id = self.highest_id.max(id);
        Ok(())
    }

    fn mark_dead(&mut self, offset: u64) -> Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&[FLAG_DEAD])?;
        if self.write_barriers {
            self.file.sync_data()?;
        }
        let slot = offset / self.layout.row_size as u64;
        if !self.free.contains(&slot) {
            self.free.push(slot);
        }
        Ok(())
    }
    // Обновление записи из проверенного набора байтов
    pub(crate) fn update_bytes(&mut self, pk: i64, bytes: &[u8]) -> Result<()> {
        self.check_ready()?;
        if bytes.len() != self.layout.row_size {
            return Err("Неверный размер записи".into());
        }
        let row = Row {
            bytes,
            schema: &self.schema,
            layout: &self.layout,
        };
        row.validate()?;
        if row.flag()? != FLAG_LIVE {
            return Err("Ожидалась действующая запись".into());
        }
        let id = row.at(self.schema.primary_key)?.int()?;
        self.buf[..self.layout.row_size].copy_from_slice(bytes);
        self.update_prepared(pk, id)
    }

    // Пакет проверяется по конечному набору PK: освобождаемые ключи можно занять.
    // Все смещения берутся из старого индекса; перестановка PK не затирает их.
    // Ошибка I/O может оставить часть записей изменённой — это не транзакция.
    pub(crate) fn update_batch(&mut self, ids: &[(i64, i64)], bytes: &[u8]) -> Result<()> {
        self.check_ready()?;
        self.layout.validate(&self.schema)?;
        let size = self.layout.row_size;
        let expected = ids.len().checked_mul(size).ok_or("UPDATE слишком велик")?;
        if bytes.len() != expected {
            return Err("Неверный размер пакета UPDATE".into());
        }
        if ids.is_empty() {
            return Ok(());
        }
        if let &[(old_id, id)] = ids {
            let row = Row {
                bytes,
                schema: &self.schema,
                layout: &self.layout,
            };
            if row.at(self.schema.primary_key)?.int()? != id {
                return Err("Неверная запись в пакете UPDATE".into());
            }
            return self.update_bytes(old_id, bytes);
        }
        let mut old_ids = HashSet::with_capacity(ids.len());
        let mut new_ids = HashSet::with_capacity(ids.len());
        let mut offsets = Vec::with_capacity(ids.len());
        for (&(old_id, id), row_bytes) in ids.iter().zip(bytes.chunks_exact(size)) {
            let offset = *self.index.get(&old_id).ok_or("Запись не существует")?;
            if !old_ids.insert(old_id) || !new_ids.insert(id) {
                return Err("PRIMARY KEY дублируется в пакете".into());
            }
            let row = Row {
                bytes: row_bytes,
                schema: &self.schema,
                layout: &self.layout,
            };
            row.validate_fields()?;
            if row.flag()? != FLAG_LIVE || row.at(self.schema.primary_key)?.int()? != id {
                return Err("Неверная запись в пакете UPDATE".into());
            }
            offsets.push(offset);
        }
        if new_ids
            .iter()
            .any(|id| self.index.contains_key(id) && !old_ids.contains(id))
        {
            return Err("PRIMARY KEY дублируется".into());
        }
        self.write_failed = true;
        let mut locations = Vec::with_capacity(offsets.len());
        let barriers = self.write_barriers;
        self.write_barriers = false;
        let result = (|| -> Result<()> {
            for ((&location, &(old_id, _)), row_bytes) in
                offsets.iter().zip(ids).zip(bytes.chunks_exact(size))
            {
                self.buf.copy_from_slice(row_bytes);
                self.write_at(location, old_id)?;
                locations.push(location);
            }
            Ok(())
        })();
        self.write_barriers = barriers;
        result?;
        if barriers {
            self.file.sync_data()?;
        }
        for &(old_id, _) in ids {
            self.index.remove(&old_id);
        }
        for (&(_, id), &offset) in ids.iter().zip(&locations) {
            self.index.insert(id, offset);
        }
        self.write_failed = false;
        Ok(())
    }

    pub fn remove(&mut self, pk: i64) -> Result<bool> {
        self.check_ready()?;
        let Some(&location) = self.index.get(&pk) else {
            return Ok(false);
        };
        self.write_failed = true;
        self.mark_dead(location)?;
        self.index.remove(&pk);
        self.write_failed = false;
        Ok(true)
    }

    pub fn get(&mut self, pk: i64) -> Result<Option<Row<'_>>> {
        self.check_ready()?;
        let Some(&offset) = self.index.get(&pk) else {
            return Ok(None);
        };
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut self.buf)?;
        let row = self.row();
        if row.flag()? != FLAG_LIVE || row.at(self.schema.primary_key)?.int()? != pk {
            return Err(IndexMismatch.into());
        }
        Ok(Some(row))
    }

    pub fn scan(&mut self, mut visit: impl FnMut(Row<'_>) -> Result<()>) -> Result<()> {
        self.check_ready()?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, &mut self.file);
        for _ in 0..self.slot_count {
            reader.read_exact(&mut self.buf)?;
            match record::flag(&self.buf)? {
                FLAG_LIVE => visit(Row {
                    bytes: &self.buf,
                    schema: &self.schema,
                    layout: &self.layout,
                })?,
                FLAG_DEAD | FLAG_FREE => (),
                _ => unreachable!(),
            }
        }
        Ok(())
    }

    pub fn check(&mut self) -> Result<usize> {
        self.schema.check()?;
        self.layout.validate(&self.schema)?;
        let mut count = 0;
        self.scan(|row| {
            row.validate_fields()?;
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    pub fn sync(&mut self) -> Result<()> {
        self.check_ready()?;
        self.file.sync_all()?;
        self.checkpoint_highest_id()
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        if let Err(error) = self.sync() {
            let _ = writeln!(
                std::io::stderr(),
                "Ошибка сохранения таблицы {}: {error}",
                self.schema.name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataType;

    #[test]
    fn update_bytes_rejects_bad_lengths_and_flags_without_changing_data() -> Result<()> {
        let root = std::env::temp_dir().join(format!("minidb-update-bytes-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let result = (|| -> Result<()> {
            let schema = Schema::new("items", vec![Column::new("id", DataType::INT)], 0);
            let mut table = Table::create(&root, schema)?;
            table.insert(&[Value::Int(7)])?;
            let original = table.get(7)?.ok_or("Нет записи")?.bytes.to_vec();
            let before = fs::read(root.join("items_1"))?;
            for len in [0, 1, original.len() - 1, original.len() + 1] {
                let mut bytes = original.clone();
                bytes.resize(len, 0);
                assert!(table.update_bytes(7, &bytes).is_err());
            }
            for flag in [FLAG_FREE, FLAG_DEAD, 255] {
                let mut bytes = original.clone();
                bytes[0] = flag;
                assert!(table.update_bytes(7, &bytes).is_err());
            }
            assert_eq!(fs::read(root.join("items_1"))?, before);
            assert_eq!(table.get(7)?.ok_or("Нет записи")?.bytes, original);
            table.update_bytes(7, &original)?;
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn failed_write_requires_rebuild_before_reuse() -> Result<()> {
        let root = std::env::temp_dir().join(format!("minidb-write-error-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let result = (|| -> Result<()> {
            let schema = Schema::new("items", vec![Column::new("id", DataType::INT)], 0);
            let mut table = Table::create(&root, schema)?;
            table.insert(&[Value::Int(7)])?;
            let path = root.join("items_1");
            fs::rename(&path, path.with_file_name("saved"))?;
            fs::create_dir(&path)?;
            assert!(table.remove(7).is_err());
            assert!(table.get(7).is_err());
            assert!(table.insert(&[Value::Int(8)]).is_err());
            fs::remove_dir(&path)?;
            fs::rename(path.with_file_name("saved"), &path)?;
            table.rebuild_index()?;
            assert!(table.get(7)?.is_some());
            assert!(table.remove(7)?);
            assert!(!table.remove(7)?);
            table.insert(&[Value::Null])?;
            assert!(table.get(8)?.is_some());
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn batch_pk_swap_in_one_file_uses_original_offsets_and_rejects_outside_collisions() -> Result<()>
    {
        let root = std::env::temp_dir().join(format!("minidb-pk-swap-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        let result = (|| -> Result<()> {
            let schema = Schema::new(
                "items",
                vec![
                    Column::new("id", DataType::INT),
                    Column::new("name", DataType::STRING),
                ],
                0,
            );
            let mut table = Table::create(&root, schema)?;
            table.insert(&[Value::Int(1), Value::String("first")])?;
            table.add_column(Column::new(
                "extra",
                DataType {
                    nullable: true,
                    ..DataType::INT
                },
            ))?;
            table.insert(&[Value::Int(2), Value::String("second"), Value::Null])?;
            let size = table.layout.row_size;
            let mut bytes = vec![0; size * 2];
            record::encode(
                &table.layout,
                &table.schema,
                &[Value::Int(2), Value::String("first"), Value::Null],
                &mut bytes[..size],
            )?;
            record::encode(
                &table.layout,
                &table.schema,
                &[Value::Int(1), Value::String("second"), Value::Null],
                &mut bytes[size..],
            )?;
            let before = fs::read(table.data_file())?;
            assert!(table.update_batch(&[(1, 2)], &bytes[..size]).is_err());
            assert_eq!(fs::read(table.data_file())?, before);
            table.update_batch(&[(1, 2), (2, 1)], &bytes)?;
            assert_eq!(
                table.get(1)?.ok_or("Нет PK 1")?.get("name")?,
                Value::String("second")
            );
            assert_eq!(
                table.get(2)?.ok_or("Нет PK 2")?.get("name")?,
                Value::String("first")
            );
            drop(table);
            let mut table = Table::open(&root, "items")?;
            assert_eq!(
                table.get(1)?.ok_or("Нет PK 1")?.get("name")?,
                Value::String("second")
            );
            assert_eq!(
                table.get(2)?.ok_or("Нет PK 2")?.get("name")?,
                Value::String("first")
            );
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
    #[test]
    fn batch_lowering_pk_checkpoints_before_overwriting_live_maximum() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("minidb-batch-highest-{}", std::process::id()));
        fs::create_dir(&root)?;
        let result = (|| -> Result<()> {
            let mut table = Table::create(
                &root,
                Schema::new("items", vec![Column::new("id", DataType::INT)], 0),
            )?;
            table.insert(&[Value::Int(100)])?;
            table.insert(&[Value::Int(200)])?;
            let initial = fs::read(root.join("items_schema"))?;
            let size = table.layout.row_size;
            let mut bytes = vec![0; size * 2];
            for (new, out) in [1, 2].into_iter().zip(bytes.chunks_exact_mut(size)) {
                record::encode(&table.layout, &table.schema, &[Value::Int(new)], out)?;
            }
            table.update_batch(&[(100, 1), (200, 2)], &bytes)?;
            assert_ne!(fs::read(root.join("items_schema"))?, initial);
            assert_eq!(table.saved_highest_id, 200);
            assert_eq!(table.highest_id, 200);
            // rebuild использует уже перезаписанные слоты; максимум есть только в checkpoint.
            let reopened = Table::open(&root, "items")?;
            assert_eq!(reopened.highest_id, 200);
            Ok(())
        })();
        fs::remove_dir_all(root)?;
        result
    }
}
