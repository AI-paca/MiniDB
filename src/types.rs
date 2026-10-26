use crate::Result;
use std::cmp::Ordering;
use std::fmt;

pub const DEFAULT_STRING_BYTES: usize = 64;
pub const MAX_STRING_BYTES: usize = u16::MAX as usize;

// Значение поля записи. Текст берётся из буфера без копирования
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Int(i64),
    Float(f64),
    String(&'a str),
}

impl Value<'_> {
    // Числа сравниваются между собой, строки — только со строками.
    pub fn compare(self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Some(a.cmp(&b)),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(&b),
            (Self::Int(a), Self::Float(b)) => compare_int_float(a, b),
            (Self::Float(a), Self::Int(b)) => compare_int_float(b, a).map(Ordering::reverse),
            (Self::String(a), Self::String(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    // Получение целого числа из значения
    pub fn int(self) -> Result<i64> {
        match self {
            Self::Int(value) => Ok(value),
            _ => Err("Ожидался INT".into()),
        }
    }
}

// Не переводим i64 в f64: после 2^53 это может сделать разные числа равными.
fn compare_int_float(integer: i64, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    // i64::MAX округляется в f64 до 2^63, которое уже вне диапазона i64.
    if float >= -(i64::MIN as f64) {
        return Some(Ordering::Less);
    }
    if float < i64::MIN as f64 {
        return Some(Ordering::Greater);
    }
    match integer.cmp(&(float as i64)) {
        Ordering::Equal => 0.0f64.partial_cmp(&float.fract()),
        order => Some(order),
    }
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(out, "NULL"),
            Self::Int(value) => write!(out, "{value}"),
            Self::Float(value) => write!(out, "{value}"),
            Self::String(value) => write!(out, "{value}"),
        }
    }
}

// Виды полей и ограничения их значений
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Null, // Тип ещё не выбран: допустимы только NULL, первая запись задаст тип.
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
    String { max_bytes: usize },
}

// Тип поля и ограничения его значений
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataType {
    pub kind: Kind,     // Тип и допустимые границы
    pub nullable: bool, // Разрешено ли значение NULL
}

impl DataType {
    pub const NULL: Self = Self {
        kind: Kind::Null,
        nullable: true,
    };
    pub const INT: Self = Self {
        kind: Kind::Int {
            min: i64::MIN,
            max: i64::MAX,
        },
        nullable: false,
    };
    pub const FLOAT: Self = Self {
        kind: Kind::Float {
            min: -f64::MAX,
            max: f64::MAX,
        },
        nullable: false,
    };
    pub const STRING: Self = Self {
        kind: Kind::String {
            max_bytes: DEFAULT_STRING_BYTES,
        },
        nullable: false,
    };

    // Проверка значения по типу и ограничениям поля
    pub fn validate(self, value: Value<'_>) -> Result<()> {
        let valid = match (self.kind, value) {
            (_, Value::Null) => self.nullable,
            (Kind::Int { min, max }, Value::Int(n)) => n >= min && n <= max,
            (Kind::Float { min, max }, Value::Float(n)) => n.is_finite() && n >= min && n <= max,
            (Kind::String { max_bytes }, Value::String(s)) => {
                max_bytes <= MAX_STRING_BYTES && s.len() <= max_bytes
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(format!("Значение {value:?} нарушает тип {self:?}").into())
        }
    }

    // Проверка допустимых границ типа
    pub(crate) fn check(self) -> Result<()> {
        let valid = match self.kind {
            Kind::Null => self.nullable,
            Kind::Int { min, max } => min <= max,
            Kind::Float { min, max } => min.is_finite() && max.is_finite() && min <= max,
            Kind::String { max_bytes } => {
                if max_bytes > MAX_STRING_BYTES {
                    return Err(format!(
                        "Длина STRING должна быть в диапазоне 0..={MAX_STRING_BYTES} байт"
                    )
                    .into());
                }
                true
            }
        };
        if valid {
            Ok(())
        } else {
            Err("Некорректные границы типа".into())
        }
    }
}

// Проверка имени таблицы, столбца или функции
pub(crate) fn identifier(name: &str) -> Result<()> {
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.len() > 64
    {
        return Err(format!("Недопустимое имя: {name}").into());
    }
    Ok(())
}
