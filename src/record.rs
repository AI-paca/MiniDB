use crate::layout::{null_bit, Layout, FLAG_OFFSET};
use crate::{Kind, Result, Schema, Value};

pub const FLAG_FREE: u8 = 0; // Свободное место
pub const FLAG_LIVE: u8 = 1; // Действующая запись
pub const FLAG_DEAD: u8 = 2; // Удалённая запись

// Проверка состояния слота; неизвестный флаг — ошибка формата, а не DEAD.
pub fn flag(bytes: &[u8]) -> Result<u8> {
    let flag = *bytes.get(FLAG_OFFSET).ok_or("Отсутствует flag записи")?;
    check_flag(flag)?;
    Ok(flag)
}

fn check_flag(flag: u8) -> Result<()> {
    match flag {
        FLAG_FREE | FLAG_LIVE | FLAG_DEAD => Ok(()),
        _ => Err("Файл данных повреждён: неизвестный flag".into()),
    }
}

// Совместимый API: кодирование действующей записи.
pub fn encode(layout: &Layout, schema: &Schema, vals: &[Value<'_>], out: &mut [u8]) -> Result<()> {
    encode_with_flag(layout, schema, vals, FLAG_LIVE, out)
}

// Выбор флага явный: INSERT готовит FREE, UPDATE — LIVE.
// Любая ошибка входных данных обнаруживается до изменения out.
pub fn encode_with_flag(
    layout: &Layout,
    schema: &Schema,
    vals: &[Value<'_>],
    flag: u8,
    out: &mut [u8],
) -> Result<()> {
    // Layout публичен и изменяем: публичный кодировщик не может доверять ему.
    layout.validate(schema)?;
    encode_validated_layout(layout, schema, vals, flag, out)
}

// Внутренний путь Table: физический Layout уже проверен в create/open,
// и после ADD COLUMN проверяется заново. Значения и размер буфера проверяем всегда.
pub(crate) fn encode_validated_layout(
    layout: &Layout,
    schema: &Schema,
    vals: &[Value<'_>],
    flag: u8,
    out: &mut [u8],
) -> Result<()> {
    check_flag(flag)?;
    if out.len() != layout.row_size {
        return Err("Размер записи не соответствует Layout".into());
    }
    if vals.len() != schema.columns.len() || layout.cols.len() != schema.columns.len() {
        return Err("Неверное число полей".into());
    }
    // Проверки самого формата не зависят от DataType::validate.
    for ((col, schema_col), &value) in layout.cols.iter().zip(&schema.columns).zip(vals) {
        match (col.kind, value) {
            (_, Value::Null)
            | (Kind::Int { .. }, Value::Int(_))
            | (Kind::Float { .. }, Value::Float(_)) => (),
            (Kind::String { max_bytes }, Value::String(text)) => {
                if text.len() > max_bytes {
                    return Err("STRING превышает размер поля".into());
                }
                u16::try_from(text.len()).map_err(|_| "STRING длиннее 65535 байт")?;
            }
            _ => return Err("Тип значения не совпадает с колонкой".into()),
        }
        if value == Value::Null && !schema_col.data_type.nullable {
            return Err(format!(
                "Поле {}.{} обязательно (NOT NULL): укажите значение",
                schema.name, schema_col.name
            )
            .into());
        }
        schema_col
            .data_type
            .validate(value)
            .map_err(|error| format!("Поле {}.{}: {error}", schema.name, schema_col.name))?;
    }
    out.fill(0);
    out[FLAG_OFFSET] = flag;
    for (i, &value) in vals.iter().enumerate() {
        let col = &layout.cols[i];
        match (col.kind, value) {
            (_, Value::Null) => {
                let (byte, mask) = null_bit(i);
                out[byte] |= mask;
            }
            (Kind::Int { .. }, Value::Int(n)) => {
                let field = &mut out[col.offset..col.offset + col.width];
                field.copy_from_slice(&n.to_le_bytes());
            }
            (Kind::Float { .. }, Value::Float(n)) => {
                let field = &mut out[col.offset..col.offset + col.width];
                field.copy_from_slice(&n.to_le_bytes());
            }
            (Kind::String { .. }, Value::String(text)) => {
                let field = &mut out[col.offset..col.offset + col.width];
                field[..2].copy_from_slice(&(text.len() as u16).to_le_bytes());
                field[2..2 + text.len()].copy_from_slice(text.as_bytes());
            }
            _ => return Err("Тип значения не совпадает с колонкой".into()),
        }
    }
    Ok(())
}

// Запись в рабочем буфере таблицы
#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub bytes: &'a [u8],    // Байты записи
    pub layout: &'a Layout, // Смещения и размеры полей
    pub schema: &'a Schema, // Схема таблицы
}

impl<'a> Row<'a> {
    // Декодирование по физическому Layout: bounds, длина STRING и UTF-8.
    // NOT NULL и ограничения значений проверяются отдельно в validate().
    pub fn at(self, i: usize) -> Result<Value<'a>> {
        if i >= self.schema.columns.len() {
            return Err("Нет столбца с таким номером".into());
        }
        if i >= self.layout.cols.len() {
            return Ok(Value::Null);
        }
        let col = self
            .layout
            .cols
            .get(i)
            .ok_or("Нет столбца с таким номером")?;
        let (byte, mask) = null_bit(i);
        let null = *self.bytes.get(byte).ok_or("Повреждён null-bitmap")?;
        if null & mask != 0 {
            return Ok(Value::Null);
        }
        let end = col
            .offset
            .checked_add(col.width)
            .ok_or("Повреждён offset")?;
        let field = self.bytes.get(col.offset..end).ok_or("Повреждена запись")?;
        let value = match col.kind {
            Kind::Null => {
                return Err("Поле отложенного типа NULL содержит ненулевое значение".into())
            }
            Kind::Int { .. } => Value::Int(i64::from_le_bytes(field.try_into()?)),
            Kind::Float { .. } => Value::Float(f64::from_le_bytes(field.try_into()?)),
            Kind::String { max_bytes } => {
                let prefix = field.get(..2).ok_or("Повреждена длина STRING")?;
                let len = u16::from_le_bytes(prefix.try_into()?) as usize;
                if len > max_bytes {
                    return Err("Повреждена длина STRING".into());
                }
                let text = field.get(2..2 + len).ok_or("Повреждён STRING")?;
                Value::String(std::str::from_utf8(text)?)
            }
        };
        Ok(value)
    }

    // Чтение поля по имени
    pub fn get(self, name: &str) -> Result<Value<'a>> {
        self.at(self.schema.column(name)?)
    }

    // Проверенное состояние слота. Неполный/неизвестный флаг даёт Err.
    pub fn flag(self) -> Result<u8> {
        flag(self.bytes)
    }

    // Только предикат LIVE, не проверка корректности флага. Для неё есть flag().
    pub fn live(self) -> bool {
        self.bytes.get(FLAG_OFFSET) == Some(&FLAG_LIVE)
    }

    // Явная проверка формата и ограничений текущей схемы (запись/CHECK).
    pub fn validate(self) -> Result<()> {
        self.layout.validate(self.schema)?;
        self.validate_fields()
    }

    // Внутренний цикл: Layout проверен один раз перед обходом записей.
    pub(crate) fn validate_fields(self) -> Result<()> {
        if self.bytes.len() != self.layout.row_size {
            return Err("Размер записи не соответствует Layout".into());
        }
        self.flag()?;
        for (i, col) in self
            .schema
            .columns
            .iter()
            .take(self.layout.cols.len())
            .enumerate()
        {
            col.data_type.validate(self.at(i)?)?;
        }
        Ok(())
    }
}
