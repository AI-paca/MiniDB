use crate::layout::Layout;
pub use crate::record::Row;
use crate::record::{self, FLAG_DEAD, FLAG_FREE, FLAG_LIVE};
pub use crate::schema::{Column, Reference, Schema};
use crate::{Result, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const READ_BUFFER_SIZE: usize = 64 * 1024;
const HIGHEST_ID_PREFIX: &str = "-- minidb-highest-id: ";

// Ошибка внутреннего инварианта, отличимая от ошибок формата данных.
#[derive(Debug)]
pub struct IndexMismatch;

impl std::fmt::Display for IndexMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Индекс рассинхронизирован")
    }
}

impl std::error::Error for IndexMismatch {}

/// Физический файл одной версии; старые тела записей не переписываются.
pub struct Segment {
    pub version: u32,
    pub layout: Layout,
    file: File,
    path: PathBuf,
    slot_count: u64,
    free: Vec<u64>,
}

impl Segment {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn slot_count(&self) -> u64 {
        self.slot_count
    }
    pub fn free_slot_count(&self) -> usize {
        self.free.len()
    }
}

pub struct Table {
    pub(crate) schema: Schema,
    pub(crate) layout: Layout, // Текущий формат для подготовки INSERT/UPDATE.
    segments: Vec<Segment>,
    pub(crate) index: HashMap<i64, (u32, u64)>, // Версия сегмента, byte offset.
    pub(crate) highest_id: i64,
    saved_highest_id: i64,
    schema_path: PathBuf,
    write_failed: bool,
    buf: Vec<u8>,
    scratch: Vec<u8>, // Переиспользуемый буфер для прежнего DEAD-слота.
}

impl Table {
    // Получение схемы таблицы
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    // Получение размеров и смещений полей
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn schema_path(&self) -> &Path {
        &self.schema_path
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    // Создание новой таблицы использует только версионные имена файлов.
    pub fn create(root: &Path, schema: Schema) -> Result<Self> {
        schema.check()?;
        let layout = Layout::from_schema(&schema)?;
        fs::create_dir_all(root)?;
        let schema_path = root.join(format!("{}.schema.sql", schema.name));
        if root.join(format!("{}.schema", schema.name)).exists()
            || root.join(format!("{}.data", schema.name)).exists()
        {
            return Err("Файл данных таблицы уже существует".into());
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if crate::storage::is_segment(&entry.file_name().to_string_lossy(), &schema.name) {
                return Err("Файл сегмента таблицы уже существует".into());
            }
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&schema_path)?;
        let mut segments = Vec::new();
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            for (version, count) in schema.history() {
                let path = crate::storage::segment_path(root, &schema.name, version);
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                created.push(path.clone());
                file.sync_all()?;
                let file = if version == schema.version {
                    file
                } else {
                    File::open(&path)?
                };
                segments.push(Segment {
                    version,
                    layout: Layout::from_prefix(&schema, count)?,
                    file,
                    path,
                    slot_count: 0,
                    free: Vec::new(),
                });
            }
            output.write_all(schema.to_text().as_bytes())?;
            output.sync_all()?;
            crate::storage::sync_directory(root)?;
            Ok(())
        })();
        drop(output);
        if let Err(error) = result {
            drop(segments);
            for path in created {
                let _ = fs::remove_file(path);
            }
            let _ = fs::remove_file(schema_path);
            return Err(error);
        }
        Ok(Self {
            buf: vec![0; layout.row_size],
            scratch: Vec::new(),
            segments,
            schema,
            layout,
            index: HashMap::new(),
            highest_id: 0,
            saved_highest_id: 0,
            schema_path,
            write_failed: false,
        })
    }

    pub fn open(root: &Path, name: &str) -> Result<Self> {
        crate::types::identifier(name)?;
        let schema_path = crate::storage::schema_path(root, name)?;
        let text = fs::read_to_string(&schema_path)?;
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
        let schema = Schema::from_text(&text)?;
        if schema.name != name {
            return Err("Имя файла схемы не совпадает с именем таблицы".into());
        }
        let layout = Layout::from_schema(&schema)?;
        let mut segments = Vec::new();
        for (version, count) in schema.history() {
            let path = crate::storage::existing_segment(root, name, version)?;
            let physical = Layout::from_prefix(&schema, count)?;
            physical.validate(&schema)?;
            let file = OpenOptions::new()
                .read(true)
                .write(version == schema.version)
                .open(&path)?;
            segments.push(Segment {
                version,
                layout: physical,
                file,
                path,
                slot_count: 0,
                free: Vec::new(),
            });
        }
        // Неизвестные сегменты нельзя молча пропускать: это потеря видимых данных.
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let filename = entry.file_name();
            let filename = filename.to_string_lossy();
            if crate::storage::is_segment(&filename, name)
                && !segments.iter().any(|s| s.path == entry.path())
            {
                return Err(format!("Сегмент {filename} отсутствует в истории схемы").into());
            }
        }
        let mut table = Self {
            buf: vec![0; layout.row_size],
            scratch: Vec::new(),
            schema,
            layout,
            segments,
            index: HashMap::new(),
            highest_id,
            saved_highest_id: highest_id,
            schema_path,
            write_failed: false,
        };
        table.rebuild_index()?;
        Ok(table)
    }

    pub fn rebuild_index(&mut self) -> Result<()> {
        self.write_failed = true;
        self.index.clear();
        for segment in &mut self.segments {
            let size = segment.layout.row_size as u64;
            let len = segment.file.metadata()?.len();
            let mut slots = len / size;
            if len % size != 0 {
                segment.file.seek(SeekFrom::Start(slots * size))?;
                let mut flag = [0];
                segment.file.read_exact(&mut flag)?;
                if flag[0] != FLAG_FREE || segment.version != self.schema.version {
                    return Err("Файл данных повреждён: неполный слот".into());
                }
                slots += 1;
                segment.file.set_len(slots * size)?;
            }
            segment.slot_count = slots;
            segment.free.clear();
            segment.file.seek(SeekFrom::Start(0))?;
            let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, &mut segment.file);
            let mut bytes = vec![0; segment.layout.row_size];
            for slot in 0..slots {
                reader.read_exact(&mut bytes)?;
                match record::flag(&bytes)? {
                    FLAG_LIVE | FLAG_DEAD => {
                        let row = Row {
                            bytes: &bytes,
                            schema: &self.schema,
                            layout: &segment.layout,
                        };
                        let id = row.at(self.schema.primary_key)?.int()?;
                        self.highest_id = self.highest_id.max(id);
                        if row.live() {
                            if self
                                .index
                                .insert(id, (segment.version, slot * size))
                                .is_some()
                            {
                                return Err("Файл данных повреждён: PRIMARY KEY дублируется".into());
                            }
                        } else {
                            segment.free.push(slot);
                        }
                    }
                    FLAG_FREE => segment.free.push(slot),
                    _ => unreachable!(),
                }
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

    // Атомарная замена текстовой схемы вместе со счётчиком ID.
    fn save_schema(&self, schema: &Schema, highest: i64) -> Result<()> {
        let temporary = self.schema_path.with_extension("tmp");
        let result = (|| -> Result<()> {
            let mut output = File::create(&temporary)?;
            writeln!(output, "{}{HIGHEST_ID_PREFIX}{highest}", schema.to_text())?;
            output.sync_all()?;
            drop(output);
            fs::rename(&temporary, &self.schema_path)?;
            crate::storage::sync_directory(self.schema_path.parent().ok_or("Нет каталога схемы")?)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(temporary);
            return Err(error);
        }
        Ok(())
    }

    // Append и DEAD сохраняют PK в сегментах. Checkpoint нужен перед его утратой
    // (переиспользование DEAD / смена LIVE PK) и при явном sync.
    fn checkpoint_highest_id(&mut self) -> Result<()> {
        if self.highest_id > self.saved_highest_id {
            self.save_schema(&self.schema, self.highest_id)?;
            self.saved_highest_id = self.highest_id;
        }
        Ok(())
    }

    fn preserve_dead_slot(&mut self) -> Result<()> {
        let segment = self.segments.last_mut().ok_or("Нет сегментов")?;
        let Some(&slot) = segment.free.last() else {
            return Ok(());
        };
        segment
            .file
            .seek(SeekFrom::Start(slot * segment.layout.row_size as u64))?;
        self.scratch.resize(segment.layout.row_size, 0);
        segment.file.read_exact(&mut self.scratch)?;
        let old_id = match record::flag(&self.scratch)? {
            FLAG_DEAD => Row {
                bytes: &self.scratch,
                schema: &self.schema,
                layout: &segment.layout,
            }
            .at(self.schema.primary_key)?
            .int()?,
            FLAG_FREE => return Ok(()), // Тело незавершённой записи не интерпретируем.
            _ => return Err(IndexMismatch.into()),
        };
        if old_id > self.saved_highest_id {
            self.checkpoint_highest_id()?;
        }
        Ok(())
    }

    pub fn add_column(&mut self, column: Column) -> Result<()> {
        self.check_ready()?;
        let schema = self.schema.with_added_column(column)?;
        let layout = Layout::from_schema(&schema)?;
        let root = self.schema_path.parent().ok_or("Нет каталога схемы")?;
        let path = crate::storage::segment_path(root, &schema.name, schema.version);
        // Закрываем пишущий дескриптор прежнего сегмента после сохранения схемы.
        let old_read = File::open(&self.segments.last().ok_or("Нет сегментов")?.path)?;
        self.sync()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        if let Err(error) = file.sync_all() {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error.into());
        }
        if let Err(error) = self.save_schema(&schema, self.highest_id) {
            // rename мог уже состояться: в этом случае сегмент нужен при повторном open.
            if fs::read_to_string(&self.schema_path)
                .ok()
                .and_then(|t| Schema::from_text(&t).ok())
                .is_some_and(|s| s.version == schema.version)
            {
                self.write_failed = true;
            } else {
                drop(file);
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        self.segments.last_mut().ok_or("Нет сегментов")?.file = old_read;
        self.segments.push(Segment {
            version: schema.version,
            layout: layout.clone(),
            file,
            path,
            slot_count: 0,
            free: Vec::new(),
        });
        self.saved_highest_id = self.highest_id;
        self.schema = schema;
        self.buf.resize(layout.row_size, 0);
        self.layout = layout;
        Ok(())
    }

    pub fn check_version(&self, version: u32) -> Result<()> {
        if version != self.schema.version {
            return Err(format!(
                "VERSION {version} устарела: текущая VERSION {}; пересоберите потребителя",
                self.schema.version
            )
            .into());
        }
        Ok(())
    }

    pub fn insert_versioned(&mut self, version: u32, vals: &[Value<'_>]) -> Result<u64> {
        self.check_version(version)?;
        self.insert(vals)
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
        self.prepare(&values, FLAG_FREE)?;
        self.insert_prepared()
    }

    // Коммит тела через FREE -> LIVE только в текущем сегменте.
    fn append_current(&mut self) -> Result<(u32, u64)> {
        let id = self.row().at(self.schema.primary_key)?.int()?;
        self.preserve_dead_slot()?;
        let segment = self.segments.last_mut().ok_or("Нет сегментов")?;
        let slot = match segment.free.last() {
            Some(&slot) => slot,
            None => {
                let slot = segment.slot_count;
                segment
                    .file
                    .set_len((slot + 1) * segment.layout.row_size as u64)?;
                segment.slot_count += 1;
                segment.free.push(slot);
                slot
            }
        };
        let offset = slot * segment.layout.row_size as u64;
        segment.file.seek(SeekFrom::Start(offset))?;
        segment.file.write_all(&[FLAG_FREE])?;
        segment.file.sync_data()?;
        self.buf[0] = FLAG_FREE;
        segment.file.seek(SeekFrom::Start(offset))?;
        segment.file.write_all(&self.buf)?;
        segment.file.sync_data()?;
        segment.file.seek(SeekFrom::Start(offset))?;
        segment.file.write_all(&[FLAG_LIVE])?;
        segment.file.sync_data()?;
        self.buf[0] = FLAG_LIVE;
        segment.free.pop();
        self.highest_id = self.highest_id.max(id);
        Ok((segment.version, offset))
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
        Ok(location.1)
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
        let location = self.replace_at(location, pk)?;
        self.index.remove(&pk);
        self.index.insert(id, location);
        self.write_failed = false;
        Ok(())
    }

    // Из старого сегмента строка переносится; из текущего — обновляется на месте.
    fn replace_at(&mut self, location: (u32, u64), old_id: i64) -> Result<(u32, u64)> {
        if location.0 == self.schema.version {
            let id = self.row().at(self.schema.primary_key)?.int()?;
            if old_id != id && old_id > self.saved_highest_id {
                self.checkpoint_highest_id()?;
            }
            let segment = self.segments.last_mut().ok_or("Нет сегментов")?;
            segment.file.seek(SeekFrom::Start(location.1))?;
            segment.file.write_all(&self.buf)?;
            segment.file.flush()?;
            self.highest_id = self.highest_id.max(id);
            Ok(location)
        } else {
            let new_location = self.append_current()?;
            self.mark_dead(location)?;
            Ok(new_location)
        }
    }

    // Для старого файла отдельно открывается только запись флага; тела неизменны.
    fn mark_dead(&mut self, (version, offset): (u32, u64)) -> Result<()> {
        let segment = self
            .segments
            .iter_mut()
            .find(|s| s.version == version)
            .ok_or("Нет сегмента")?;
        if version == self.schema.version {
            segment.file.seek(SeekFrom::Start(offset))?;
            segment.file.write_all(&[FLAG_DEAD])?;
            segment.file.sync_data()?;
        } else {
            let mut flags = OpenOptions::new().write(true).open(&segment.path)?;
            flags.seek(SeekFrom::Start(offset))?;
            flags.write_all(&[FLAG_DEAD])?;
            flags.sync_data()?;
        }
        let slot = offset / segment.layout.row_size as u64;
        if !segment.free.contains(&slot) {
            segment.free.push(slot);
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
        for ((&location, &(old_id, _)), row_bytes) in
            offsets.iter().zip(ids).zip(bytes.chunks_exact(size))
        {
            self.buf.copy_from_slice(row_bytes);
            locations.push(self.replace_at(location, old_id)?);
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
        let Some(&(version, offset)) = self.index.get(&pk) else {
            return Ok(None);
        };
        let segment = self
            .segments
            .iter_mut()
            .find(|s| s.version == version)
            .ok_or("Нет сегмента")?;
        segment.file.seek(SeekFrom::Start(offset))?;
        let bytes = &mut self.buf[..segment.layout.row_size];
        segment.file.read_exact(bytes)?;
        let row = Row {
            bytes,
            schema: &self.schema,
            layout: &segment.layout,
        };
        if row.flag()? != FLAG_LIVE || row.at(self.schema.primary_key)?.int()? != pk {
            return Err(IndexMismatch.into());
        }
        Ok(Some(row))
    }

    pub fn scan(&mut self, mut visit: impl FnMut(Row<'_>) -> Result<()>) -> Result<()> {
        self.check_ready()?;
        for segment in &mut self.segments {
            segment.file.seek(SeekFrom::Start(0))?;
            let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, &mut segment.file);
            let bytes = &mut self.buf[..segment.layout.row_size];
            for _ in 0..segment.slot_count {
                reader.read_exact(bytes)?;
                match record::flag(bytes)? {
                    FLAG_LIVE => visit(Row {
                        bytes,
                        schema: &self.schema,
                        layout: &segment.layout,
                    })?,
                    FLAG_DEAD | FLAG_FREE => (),
                    _ => unreachable!(),
                }
            }
        }
        Ok(())
    }

    // Явная проверка значений живых записей по текущей схеме.
    pub fn check(&mut self) -> Result<usize> {
        self.schema.check()?;
        for segment in &self.segments {
            segment.layout.validate(&self.schema)?;
        }
        let mut count = 0;
        self.scan(|row| {
            row.validate_fields()?;
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    // Сохранение данных таблицы на диск.
    pub fn sync(&mut self) -> Result<()> {
        // Старые дескрипторы только для чтения; их DEAD-флаги уже синхронизированы.
        if let Some(segment) = self.segments.last_mut() {
            segment.file.sync_all()?;
        }
        self.checkpoint_highest_id()?;
        Ok(())
    }
}

impl Drop for Table {
    // Сохранение данных при закрытии таблицы
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
            let before = fs::read(root.join("items.v1.data"))?;
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
            assert_eq!(fs::read(root.join("items.v1.data"))?, before);
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
            let path = root.join("items.v1.data");
            table.segments[0].file = File::open(&path)?;
            assert!(table.remove(7).is_err());
            assert!(table.get(7).is_err());
            assert!(table.insert(&[Value::Int(8)]).is_err());
            table.segments[0].file = OpenOptions::new().read(true).write(true).open(&path)?;
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
    fn batch_pk_swap_across_segments_uses_original_offsets_and_rejects_outside_collisions(
    ) -> Result<()> {
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
            let before = fs::read(root.join("items.v1.data"))?;
            assert!(table.update_batch(&[(1, 2)], &bytes[..size]).is_err());
            assert_eq!(fs::read(root.join("items.v1.data"))?, before);
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
            let initial = fs::read(root.join("items.schema.sql"))?;
            let size = table.layout.row_size;
            let mut bytes = vec![0; size * 2];
            for (new, out) in [1, 2].into_iter().zip(bytes.chunks_exact_mut(size)) {
                record::encode(&table.layout, &table.schema, &[Value::Int(new)], out)?;
            }
            table.update_batch(&[(100, 1), (200, 2)], &bytes)?;
            assert_ne!(fs::read(root.join("items.schema.sql"))?, initial);
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
