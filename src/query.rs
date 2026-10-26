use crate::{Result, Row, Schema, Value};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug)]
pub enum Compare {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

pub enum Expr<'a> {
    All,
    Compare(&'a str, Compare, Value<'a>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    IsNull(&'a str),
    NotNull(&'a str),
    Not(Box<Self>),
}

impl<'a> Expr<'a> {
    pub fn and(self, other: Self) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }
    pub fn or(self, other: Self) -> Self {
        Self::Or(Box::new(self), Box::new(other))
    }
    pub(crate) fn prepare(&self, schema: &Schema) -> Result<Filter<'a>> {
        match self {
            Self::All => Ok(Filter::All),
            Self::IsNull(name) => Ok(Filter::IsNull(schema.column(name)?)),
            Self::NotNull(name) => Ok(Filter::NotNull(schema.column(name)?)),
            Self::Not(inner) => Ok(Filter::Not(Box::new(inner.prepare(schema)?))),
            Self::And(a, b) => Ok(Filter::And(
                Box::new(a.prepare(schema)?),
                Box::new(b.prepare(schema)?),
            )),
            Self::Or(a, b) => Ok(Filter::Or(
                Box::new(a.prepare(schema)?),
                Box::new(b.prepare(schema)?),
            )),
            Self::Compare(name, op, value) => {
                let column = schema.column(name)?;
                // Проверяем тип значения в условии поиска
                let valid = matches!(
                    (schema.columns[column].data_type.kind, value),
                    (_, Value::Null)
                        | (crate::Kind::Int { .. }, Value::Int(_))
                        | (crate::Kind::Int { .. }, Value::Float(_))
                        | (crate::Kind::Float { .. }, Value::Float(_))
                        | (crate::Kind::Float { .. }, Value::Int(_))
                        | (crate::Kind::String { .. }, Value::String(_))
                );
                if !valid || matches!(value, Value::Float(n) if !n.is_finite()) {
                    return Err("Неверный тип значения в фильтре".into());
                }
                Ok(Filter::Compare(column, *op, *value))
            }
        }
    }
}

pub(crate) enum Filter<'a> {
    All,
    Compare(usize, Compare, Value<'a>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    IsNull(usize),
    NotNull(usize),
    Not(Box<Self>),
}

impl Filter<'_> {
    pub fn matches(&self, row: Row<'_>) -> Result<bool> {
        Ok(self.truth(row)? == Some(true))
    }

    // None — SQL UNKNOWN: NOT UNKNOWN остаётся UNKNOWN, а не превращается в true.
    fn truth(&self, row: Row<'_>) -> Result<Option<bool>> {
        match self {
            Self::All => Ok(Some(true)),
            Self::IsNull(col) => Ok(Some(row.at(*col)? == Value::Null)),
            Self::NotNull(col) => Ok(Some(row.at(*col)? != Value::Null)),
            Self::Not(inner) => Ok(inner.truth(row)?.map(|value| !value)),
            Self::And(a, b) => match a.truth(row)? {
                Some(false) => Ok(Some(false)),
                Some(true) => b.truth(row),
                None => Ok(if b.truth(row)? == Some(false) {
                    Some(false)
                } else {
                    None
                }),
            },
            Self::Or(a, b) => match a.truth(row)? {
                Some(true) => Ok(Some(true)),
                Some(false) => b.truth(row),
                None => Ok(if b.truth(row)? == Some(true) {
                    Some(true)
                } else {
                    None
                }),
            },
            Self::Compare(col, op, rhs) => {
                let Some(order) = row.at(*col)?.compare(*rhs) else {
                    return Ok(None);
                };
                Ok(Some(match op {
                    Compare::Eq => order == Ordering::Equal,
                    Compare::Ne => order != Ordering::Equal,
                    Compare::Gt => order == Ordering::Greater,
                    Compare::Lt => order == Ordering::Less,
                    Compare::Ge => order != Ordering::Less,
                    Compare::Le => order != Ordering::Greater,
                }))
            }
        }
    }
}
