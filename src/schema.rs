use crate::types::identifier;
use crate::{DataType, Kind, Result};
use std::collections::HashSet;

// Ссылка на столбец другой таблицы
#[derive(Clone, Debug)]
pub struct Reference {
    pub table: String,
    pub column: String,
}

// Столбец таблицы
#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,                  // Имя столбца
    pub data_type: DataType,           // Тип и ограничения
    pub references: Option<Reference>, // Внешний ключ
}

impl Column {
    // Создание столбца с заданным именем и типом
    pub fn new(name: &str, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            references: None,
        }
    }
    // Получение ширины поля в байтах
    pub fn width(&self) -> Result<usize> {
        match self.data_type.kind {
            Kind::Null => Ok(0),
            Kind::Int { .. } | Kind::Float { .. } => Ok(8),
            Kind::String { max_bytes } => max_bytes
                .checked_add(2)
                .ok_or_else(|| "Переполнение ширины STRING".into()),
        }
    }
    // Задание внешнего ключа для столбца
    pub fn references(mut self, table: &str, column: &str) -> Self {
        self.references = Some(Reference {
            table: table.into(),
            column: column.into(),
        });
        self
    }
}

impl PartialEq for Column {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.data_type == other.data_type
            && self.references.as_ref().map(|r| (&r.table, &r.column))
                == other.references.as_ref().map(|r| (&r.table, &r.column))
    }
}

// Схема таблицы
#[derive(Clone, Debug)]
pub struct Schema {
    pub version: u32,
    /// Поколение единственного файла данных; версия схемы от него независима.
    pub generation: u64,
    pub name: String,         // Имя таблицы
    pub columns: Vec<Column>, // Столбцы таблицы
    pub primary_key: usize,   // Номер столбца первичного ключа
    // Имя функции проверки записи, переданной в open_with
    pub validator: Option<String>,
}

impl Schema {
    // Создание схемы таблицы
    pub fn new(name: &str, columns: Vec<Column>, primary_key: usize) -> Self {
        Self {
            version: 1,
            generation: 1,
            name: name.into(),
            columns,
            primary_key,
            validator: None,
        }
    }

    /// Подготовка следующей схемы без изменения файлов.
    pub fn with_added_column(&self, column: Column) -> Result<Self> {
        let mut next = self.clone();
        next.version = self
            .version
            .checked_add(1)
            .ok_or("Исчерпаны версии схемы")?;
        next.columns.push(column);
        next.generation = self
            .generation
            .checked_add(1)
            .ok_or("Исчерпаны поколения")?;
        check_evolution(self, &next)?;
        Ok(next)
    }

    /// Только поля, добавленные после v1, могут ожидать первого ненулевого значения.
    pub(crate) fn infer_types(&self, values: &[(usize, crate::Value<'_>)]) -> Result<Option<Self>> {
        let mut next = None;
        for &(index, value) in values {
            if !matches!(self.columns[index].data_type.kind, Kind::Null)
                || value == crate::Value::Null
            {
                continue;
            }
            let kind = match value {
                crate::Value::Null => continue,
                crate::Value::Int(_) => DataType::INT.kind,
                crate::Value::Float(_) => DataType::FLOAT.kind,
                crate::Value::String(text) => Kind::String {
                    max_bytes: text.len().max(crate::types::DEFAULT_STRING_BYTES),
                },
            };
            let next = next.get_or_insert_with(|| self.clone());
            next.columns[index].data_type.kind = kind;
        }
        if let Some(next) = &next {
            check_resolution(self, next)?;
        }
        Ok(next)
    }

    // Преобразование схемы в текст
    pub fn to_text(&self) -> String {
        crate::storage::schema_text(self)
    }

    // Чтение схемы из текста
    pub fn from_text(text: &str) -> Result<Self> {
        let schema = crate::sql::stored_schema(text)?;
        schema.check()?;
        Ok(schema)
    }

    // Поиск номера столбца по имени
    pub fn column(&self, name: &str) -> Result<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("Нет столбца {}.{name}", self.name).into())
    }
    // Проверка имён столбцов, типов и первичного ключа
    pub fn check(&self) -> Result<()> {
        if self.version == 0 {
            return Err("VERSION должен быть >= 1".into());
        }
        if self.generation == 0 {
            return Err("GENERATION должен быть >= 1".into());
        }
        identifier(&self.name)?;
        if let Some(name) = &self.validator {
            identifier(name)?;
        }
        if self.columns.is_empty() || self.columns.len() > 256 {
            return Err("Таблица должна иметь 1..256 полей".into());
        }
        let mut names = HashSet::new();
        for col in &self.columns {
            identifier(&col.name)?;
            col.data_type.check()?;
            if !names.insert(col.name.to_ascii_lowercase()) {
                return Err("Повторное имя столбца".into());
            }
        }
        for col in &self.columns {
            if matches!(col.data_type.kind, Kind::Null)
                && (self.version == 1 || !col.data_type.nullable || col.references.is_some())
            {
                return Err("Отложенный тип NULL разрешён только nullable-столбцам, добавленным после v1, без REFERENCES".into());
            }
        }
        let pk = self
            .columns
            .get(self.primary_key)
            .ok_or("Нет PRIMARY KEY")?;
        if !matches!(pk.data_type.kind, Kind::Int { .. }) || pk.data_type.nullable {
            return Err("PRIMARY KEY должен быть INT NOT NULL".into());
        }
        Ok(())
    }
}

/// Разрешение отложенного типа не добавляет полей и не меняет VERSION.
pub(crate) fn check_resolution(old: &Schema, new: &Schema) -> Result<()> {
    old.check()?;
    new.check()?;
    if old.version != new.version
        || old.generation != new.generation
        || old.name != new.name
        || old.primary_key != new.primary_key
        || old.validator != new.validator
        || old.columns.len() != new.columns.len()
        || old.columns.iter().zip(&new.columns).any(|(a, b)| {
            a != b
                && !(matches!(a.data_type.kind, Kind::Null)
                    && !matches!(b.data_type.kind, Kind::Null)
                    && a.name == b.name
                    && a.data_type.nullable
                    && b.data_type.nullable
                    && a.references.is_none()
                    && b.references.is_none())
        })
    {
        return Err("Можно определить только ещё не заданный тип нового столбца NULL".into());
    }
    Ok(())
}

/// Единственная совместимая эволюция — добавление nullable-полей в конец.
pub fn check_evolution(old: &Schema, new: &Schema) -> Result<()> {
    old.check()?;
    new.check()?;
    if old.name != new.name
        || old.primary_key != new.primary_key
        || old.validator != new.validator
        || old.version.checked_add(1) != Some(new.version)
        || old.generation.checked_add(1) != Some(new.generation)
        || new.columns.len() <= old.columns.len()
        || !new.columns.starts_with(&old.columns)
        || new.columns[old.columns.len()..]
            .iter()
            .any(|c| !c.data_type.nullable)
    {
        return Err(
            "Эволюция схемы допускает только добавление nullable-колонок в конец с VERSION + 1"
                .into(),
        );
    }
    Ok(())
}

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.version == other.version
            && self.columns == other.columns
            && self.primary_key == other.primary_key
            && self.validator == other.validator
            && self.generation == other.generation
    }
}
