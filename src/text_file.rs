//! UTF-8 на диске; фиксированный Layout используется только в рабочей памяти.
use crate::layout::Layout;
use crate::record::{self, Row, FLAG_DEAD, FLAG_FREE, FLAG_LIVE};
use crate::sql::{self, Token};
use crate::{Kind, Result, Schema, Value};
use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(crate) struct TextFile {
    cursor: Cursor<Vec<u8>>,
    path: PathBuf,
    schema: Schema,
    layout: Layout,
    version: u32,
    writable: bool,
    dirty: Cell<bool>,
}

impl TextFile {
    fn new(
        path: &Path,
        schema: &Schema,
        count: usize,
        version: u32,
        writable: bool,
    ) -> Result<Self> {
        let layout = Layout::from_prefix(schema, count)?;
        let mut physical = schema.clone();
        physical.columns.truncate(count);
        physical.version = 1;
        physical.columns_at_version.clear();
        // Чтение не применяет текущие MIN/MAX/NOT NULL к старым данным.
        for column in &mut physical.columns {
            column.data_type.nullable = true;
            column.data_type.kind = match column.data_type.kind {
                Kind::Int { .. } => crate::DataType::INT.kind,
                Kind::Float { .. } => crate::DataType::FLOAT.kind,
                kind => kind,
            };
        }
        physical.columns[physical.primary_key].data_type.nullable = false;
        layout.validate(&physical)?;
        Ok(Self {
            cursor: Cursor::new(Vec::new()),
            path: path.to_owned(),
            schema: physical,
            layout,
            version,
            writable,
            dirty: Cell::new(false),
        })
    }

    pub fn create(path: &Path, schema: &Schema, count: usize, version: u32) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        drop(file);
        let result = (|| {
            let file = Self::new(path, schema, count, version, true)?;
            file.dirty.set(true);
            file.sync_all()?;
            Ok(file)
        })();
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }

    pub fn open(
        path: &Path,
        schema: &Schema,
        count: usize,
        version: u32,
        writable: bool,
    ) -> Result<Self> {
        let mut file = Self::new(path, schema, count, version, writable)?;
        file.reload()?;
        Ok(file)
    }

    fn header(&self) -> String {
        format!("# MiniDB text; VERSION {}", self.version)
    }

    pub fn reload(&mut self) -> Result<()> {
        let text = fs::read_to_string(&self.path)
            .map_err(|error| format!("Не удалось прочитать UTF-8 {}: {error}. Бинарная реализация находится в ветке binary", self.path.display()))?;
        let mut lines = text.lines();
        if lines.next() != Some(self.header().as_str()) {
            return Err(format!(
                "Неверный заголовок текстового сегмента {}",
                self.path.display()
            )
            .into());
        }
        let mut bytes = Vec::new();
        for (line_index, line) in lines.enumerate() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let start = bytes.len();
            bytes.resize(start + self.layout.row_size, 0);
            let row = &mut bytes[start..];
            let result = self.parse_line(line, row);
            result
                .map_err(|error| format!("{}:{}: {error}", self.path.display(), line_index + 2))?;
        }
        self.cursor = Cursor::new(bytes);
        self.dirty.set(false);
        Ok(())
    }

    fn parse_line(&self, line: &str, row: &mut [u8]) -> Result<()> {
        let mut fields = line.split('\t');
        let flag = match fields.next() {
            Some("FREE") if fields.clone().next().is_none() => {
                row[0] = FLAG_FREE;
                return Ok(());
            }
            Some("LIVE") => FLAG_LIVE,
            Some("DEAD") => FLAG_DEAD,
            _ => return Err("Ожидалось LIVE, DEAD или FREE, затем поля через Tab".into()),
        };
        let mut tokens = Vec::new();
        for field in fields {
            let mut parsed = sql::tokenize(field)?;
            if parsed.len() != 1 {
                return Err("Каждое поле должно содержать один литерал".into());
            }
            tokens.push(parsed.remove(0));
        }
        if tokens.len() != self.schema.columns.len() {
            return Err("Неверное число полей".into());
        }
        let values = tokens
            .iter()
            .zip(&self.schema.columns)
            .map(|(token, col)| match token {
                Token::Word(word) if word.eq_ignore_ascii_case("null") => Ok(Value::Null),
                Token::Text(text) if matches!(col.data_type.kind, Kind::String { .. }) => {
                    Ok(Value::String(text))
                }
                Token::Word(word) => match col.data_type.kind {
                    Kind::Int { .. } => Ok(Value::Int(word.parse()?)),
                    Kind::Float { .. } => Ok(Value::Float(word.parse()?)),
                    _ => Err("Строка должна быть в кавычках".into()),
                },
                _ => Err("Тип поля не соответствует схеме".into()),
            })
            .collect::<Result<Vec<_>>>()?;
        record::encode_validated_layout(&self.layout, &self.schema, &values, flag, row)
    }

    pub fn len(&self) -> u64 {
        self.cursor.get_ref().len() as u64
    }

    pub fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.ensure_writable()?;
        let len = usize::try_from(len).map_err(io::Error::other)?;
        self.cursor.get_mut().resize(len, 0);
        self.dirty.set(true);
        Ok(())
    }

    fn ensure_writable(&self) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Тела старого сегмента доступны только для чтения",
            ));
        }
        Ok(())
    }

    // Старому сегменту разрешено менять только состояние строки.
    pub fn mark_dead(&mut self, offset: u64) -> io::Result<()> {
        let byte = self
            .cursor
            .get_mut()
            .get_mut(offset as usize)
            .ok_or_else(|| io::Error::other("Нет слота"))?;
        *byte = FLAG_DEAD;
        self.dirty.set(true);
        Ok(())
    }

    pub fn sync_data(&self) -> io::Result<()> {
        self.persist(true)
    }
    pub fn sync_all(&self) -> io::Result<()> {
        self.persist(true)
    }

    fn persist(&self, barrier: bool) -> io::Result<()> {
        if !self.dirty.get() {
            return Ok(());
        }
        let mut name = self
            .path
            .file_name()
            .ok_or_else(|| io::Error::other("Нет имени сегмента"))?
            .to_os_string();
        name.push(".tmp");
        let temporary = self.path.with_file_name(name);
        let mut created = false;
        let result = (|| -> Result<()> {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            let mut file = BufWriter::with_capacity(64 * 1024, file);
            created = true;
            writeln!(file, "{}", self.header())?;
            writeln!(
                file,
                "# state\t{}",
                self.schema
                    .columns
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join("\t")
            )?;
            if !self.len().is_multiple_of(self.layout.row_size as u64) {
                return Err("Неполный слот в памяти".into());
            }
            for bytes in self.cursor.get_ref().chunks_exact(self.layout.row_size) {
                let row = Row {
                    schema: &self.schema,
                    layout: &self.layout,
                    bytes,
                };
                match row.flag()? {
                    FLAG_FREE => {
                        writeln!(file, "FREE")?;
                        continue;
                    }
                    FLAG_LIVE => write!(file, "LIVE")?,
                    FLAG_DEAD => write!(file, "DEAD")?,
                    _ => unreachable!(),
                }
                for i in 0..self.layout.cols.len() {
                    write!(file, "\t")?;
                    match row.at(i)? {
                        Value::Null => write!(file, "NULL")?,
                        Value::Int(n) => write!(file, "{n}")?,
                        Value::Float(n) => write!(file, "{n:?}")?,
                        Value::String(text) => write_string(&mut file, text)?,
                    }
                }
                writeln!(file)?;
            }
            file.flush()?;
            if barrier {
                file.get_ref().sync_all()?;
            }
            drop(file);
            fs::rename(&temporary, &self.path)?;
            if barrier {
                crate::storage::sync_directory(self.path.parent().ok_or("Нет каталога сегмента")?)?;
            }
            Ok(())
        })();
        if result.is_err() && created {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| io::Error::other(error.to_string()))?;
        self.dirty.set(false);
        Ok(())
    }
}

fn write_string(file: &mut impl Write, text: &str) -> io::Result<()> {
    write!(file, "\"")?;
    for ch in text.chars() {
        match ch {
            '\n' => write!(file, "\\n")?,
            '\r' => write!(file, "\\r")?,
            '\t' => write!(file, "\\t")?,
            '\\' => write!(file, "\\\\")?,
            '"' => write!(file, "\\\"")?,
            c if c.is_control() => write!(file, "\\u{{{:x}}}", c as u32)?,
            _ => write!(file, "{ch}")?,
        }
    }
    write!(file, "\"")
}

impl Read for TextFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.cursor.read(bytes)
    }
}
impl Seek for TextFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.cursor.seek(from)
    }
}
impl Write for TextFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.ensure_writable()?;
        self.dirty.set(true);
        self.cursor.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}
