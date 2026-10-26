use crate::{Kind, Result, Schema};

pub(crate) const FLAG_OFFSET: usize = 0;
pub(crate) const NULL_BITMAP_OFFSET: usize = 1;

// Положение NULL-бита в формате записи.
pub(crate) fn null_bit(column: usize) -> (usize, u8) {
    (NULL_BITMAP_OFFSET + column / 8, 1 << (column % 8))
}

// Положение поля в записи
#[derive(Clone, Debug)]
pub struct ColLayout {
    pub offset: usize, // Смещение поля от начала записи
    pub width: usize,  // Ширина поля в байтах
    pub kind: Kind,    // Тип поля
}

// Расположение полей в записи фиксированной длины
#[derive(Clone, Debug)]
pub struct Layout {
    pub cols: Vec<ColLayout>, // Расположение всех полей
    pub row_size: usize,      // Размер одной записи в байтах
    pub null_bytes: usize,    // Размер карты пустых значений
    pub header: usize,        // Размер флага и карты пустых значений
}

impl Layout {
    // Расчёт размеров и смещений полей по схеме
    pub fn from_schema(schema: &Schema) -> Result<Self> {
        schema.check()?;
        Self::from_prefix(schema, schema.columns.len())
    }

    pub fn from_prefix(schema: &Schema, count: usize) -> Result<Self> {
        schema.check()?;
        if count == 0 || count > schema.columns.len() {
            return Err("Некорректное число колонок Layout".into());
        }
        let null_bytes = count.div_ceil(8);
        let header = NULL_BITMAP_OFFSET
            .checked_add(null_bytes)
            .ok_or("Переполнение заголовка Layout")?;
        let mut offset = header;
        let mut cols = Vec::with_capacity(count);
        for col in &schema.columns[..count] {
            let width = col.width()?;
            cols.push(ColLayout {
                offset,
                width,
                kind: col.data_type.kind,
            });
            offset = offset.checked_add(width).ok_or("Переполнение Layout")?;
        }
        Ok(Self {
            cols,
            row_size: offset,
            null_bytes,
            header,
        })
    }

    // Проверка физической раскладки. Числовые MIN/MAX не меняют формат поля.
    pub fn validate(&self, schema: &Schema) -> Result<()> {
        if self.row_size < self.header {
            return Err("Размер записи меньше заголовка Layout".into());
        }
        let null_bytes = self.cols.len().div_ceil(8);
        if self.cols.is_empty()
            || self.cols.len() > schema.columns.len()
            || self.null_bytes != null_bytes
            || self.header != NULL_BITMAP_OFFSET + null_bytes
        {
            return Err("Размер записи не соответствует Layout".into());
        }
        let mut offset = self.header;
        for (field, col) in self.cols.iter().zip(&schema.columns) {
            col.data_type.check()?;
            let same_type = matches!(
                (field.kind, col.data_type.kind),
                (Kind::Null, Kind::Null)
                    | (Kind::Int { .. }, Kind::Int { .. })
                    | (Kind::Float { .. }, Kind::Float { .. })
                    | (Kind::String { .. }, Kind::String { .. })
            );
            if field.offset != offset || field.width != col.width()? || !same_type {
                return Err("Layout не соответствует схеме".into());
            }
            if let Kind::String { max_bytes } = field.kind {
                if max_bytes.checked_add(2) != Some(field.width) {
                    return Err("Ширина STRING не соответствует Layout".into());
                }
            }
            offset = offset
                .checked_add(field.width)
                .ok_or("Переполнение Layout")?;
        }
        if offset != self.row_size {
            return Err("Размер записи не соответствует Layout".into());
        }
        Ok(())
    }
}
