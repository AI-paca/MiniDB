use crate::types::identifier;
use crate::{DataType, Kind, Result};
use std::collections::{BTreeMap, HashSet};

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
    /// Число физических колонок для каждой версии. Пусто только для исходной v1.
    pub columns_at_version: BTreeMap<u32, usize>,
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
            columns_at_version: BTreeMap::new(),
            name: name.into(),
            columns,
            primary_key,
            validator: None,
        }
    }

    pub fn history(&self) -> BTreeMap<u32, usize> {
        if self.version == 1 && self.columns_at_version.is_empty() {
            BTreeMap::from([(1, self.columns.len())])
        } else {
            self.columns_at_version.clone()
        }
    }

    /// Подготовка следующей схемы без изменения файлов.
    pub fn with_added_column(&self, column: Column) -> Result<Self> {
        let mut next = self.clone();
        next.columns_at_version = self.history();
        next.version = self
            .version
            .checked_add(1)
            .ok_or("Исчерпаны версии схемы")?;
        next.columns.push(column);
        next.columns_at_version
            .insert(next.version, next.columns.len());
        check_evolution(self, &next)?;
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
        let history = self.history();
        if history.len() != self.version as usize
            || history.get(&self.version) != Some(&self.columns.len())
        {
            return Err("Неполная история COLUMNS_AT_VERSION".into());
        }
        let mut previous = 0;
        for (expected, (&version, &count)) in (1u32..).zip(&history) {
            if version != expected || count <= previous || count > self.columns.len() {
                return Err("Некорректная история COLUMNS_AT_VERSION".into());
            }
            previous = count;
        }
        let initial = history[&1];
        if self.columns[initial..]
            .iter()
            .any(|c| !c.data_type.nullable)
        {
            return Err(
                "Новая колонка должна допускать NULL: старые записи не имеют значения".into(),
            );
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
        let pk = self
            .columns
            .get(self.primary_key)
            .ok_or("Нет PRIMARY KEY")?;
        if self.primary_key >= initial
            || !matches!(pk.data_type.kind, Kind::Int { .. })
            || pk.data_type.nullable
        {
            return Err("PRIMARY KEY должен быть INT NOT NULL".into());
        }
        Ok(())
    }
}

/// Единственная совместимая эволюция — добавление nullable-полей в конец.
pub fn check_evolution(old: &Schema, new: &Schema) -> Result<()> {
    old.check()?;
    new.check()?;
    if old.name != new.name
        || old.primary_key != new.primary_key
        || old.validator != new.validator
        || old.version.checked_add(1) != Some(new.version)
        || new.columns.len() <= old.columns.len()
        || !new.columns.starts_with(&old.columns)
        || new.columns[old.columns.len()..]
            .iter()
            .any(|c| !c.data_type.nullable)
        || old
            .history()
            .iter()
            .any(|(v, n)| new.columns_at_version.get(v) != Some(n))
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
            && self.history() == other.history()
    }
}
