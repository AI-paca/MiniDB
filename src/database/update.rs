// Подготовка и проверка полного пакета UPDATE перед записью.
use super::{Database, MAX_CHANGE};
use crate::record;
use crate::table::Table;
use crate::{Expr, Result, Row, Schema, Value};
use std::collections::{HashMap, HashSet};

pub(super) fn execute(
    db: &mut Database,
    table: &str,
    filter: &Expr<'_>,
    values: &[(&str, Value<'_>)],
) -> Result<usize> {
    let original = db.schema(table)?;
    let inferred = if original
        .columns
        .iter()
        .any(|c| matches!(c.data_type.kind, crate::Kind::Null))
    {
        let fields = values
            .iter()
            .map(|(name, value)| Ok((original.column(name)?, *value)))
            .collect::<Result<Vec<_>>>()?;
        original.infer_types(&fields)?
    } else {
        None
    };
    let layout = if let Some(schema) = &inferred {
        db.check_schema(schema)?;
        crate::layout::Layout::from_schema(schema)?
    } else {
        db.table(table)?.layout.clone()
    };
    let schema = inferred.unwrap_or_else(|| original.clone());
    let assignments = update_assignments(&schema, values)?;
    let plan = stage_updates(db.table_mut(table)?, filter, &assignments, &schema, &layout)?;
    apply(db, table, &schema, plan)
}

fn apply(db: &mut Database, table: &str, schema: &Schema, plan: UpdatePlan) -> Result<usize> {
    if plan.ids.is_empty() {
        return Ok(0);
    }
    let layout = crate::layout::Layout::from_schema(schema)?;
    layout.validate(schema)?;
    for ((old_id, id), bytes) in plan
        .ids
        .iter()
        .zip(plan.bytes.chunks_exact(layout.row_size))
    {
        if old_id != id && db.table(table)?.index.contains_key(id) && !plan.removed.contains(id) {
            return Err("PRIMARY KEY дублируется".into());
        }
        db.validate_row(
            Row {
                schema,
                layout: &layout,
                bytes,
            },
            &plan.removed,
            &plan.new_ids,
        )?;
    }
    let replaced: HashSet<_> = plan.ids.iter().map(|&(id, _)| id).collect();
    db.check_references(table, &plan.removed, &replaced)?;
    let table = db.table_mut(table)?;
    table.resolve_types(schema)?;
    let count = plan.ids.len();
    table.update_batch(&plan.ids, &plan.bytes)?;
    Ok(count)
}

// Новые записи для одного обновления
struct UpdatePlan {
    ids: Vec<(i64, i64)>,
    bytes: Vec<u8>,
    removed: HashSet<i64>,
    new_ids: HashSet<i64>,
}

impl UpdatePlan {
    // Добавление изменённой записи в общий пакет обновления
    fn push(
        &mut self,
        row: Row<'_>,
        layout: &crate::layout::Layout,
        vals: &[Value<'_>],
    ) -> Result<()> {
        let old_id = row.at(row.schema.primary_key)?.int()?;
        let id = vals[row.schema.primary_key].int()?;
        // Сравнение значений исключает перезапись строки без изменений.
        if vals
            .iter()
            .enumerate()
            .all(|(i, &value)| row.at(i).is_ok_and(|old| old == value))
        {
            for (col, &value) in row.schema.columns.iter().zip(vals) {
                col.data_type.validate(value)?;
            }
            return Ok(());
        }
        let start = self.bytes.len();
        let end = start
            .checked_add(layout.row_size)
            .ok_or("UPDATE слишком велик")?;
        if end > MAX_CHANGE {
            return Err("UPDATE превышает 16 МиБ; разбейте операцию".into());
        }
        self.bytes.resize(end, 0);
        record::encode_validated_layout(
            layout,
            row.schema,
            vals,
            record::FLAG_LIVE,
            &mut self.bytes[start..end],
        )?;
        if !self.new_ids.insert(id) {
            return Err("PRIMARY KEY дублируется".into());
        }
        if old_id != id {
            self.removed.insert(old_id);
        }
        self.ids.push((old_id, id));
        Ok(())
    }
}

// Проверка имён и значений изменяемых столбцов
fn update_assignments<'a>(
    schema: &Schema,
    values: &[(&str, Value<'a>)],
) -> Result<HashMap<usize, Value<'a>>> {
    let mut assignments = HashMap::new();
    for &(name, value) in values {
        let column = schema.column(name)?;
        if assignments.insert(column, value).is_some() {
            return Err("Поле присвоено дважды".into());
        }
        if column != schema.primary_key || value != Value::Null {
            schema.columns[column].data_type.validate(value)?;
        }
    }
    Ok(assignments)
}

// Буфер передаётся по владению: заимствования Row живут только до конца stage_row.
fn stage_row<'row>(
    plan: &mut UpdatePlan,
    row: Row<'row>,
    layout: &crate::layout::Layout,
    assignments: &HashMap<usize, Value<'row>>,
    mut vals: Vec<Value<'row>>,
) -> Result<Vec<Value<'static>>> {
    for (i, slot) in vals.iter_mut().enumerate() {
        *slot = match assignments.get(&i) {
            Some(&value) => value,
            None => row.at(i)?,
        };
    }
    if vals[row.schema.primary_key] == Value::Null {
        vals[row.schema.primary_key] = row.at(row.schema.primary_key)?;
    }
    plan.push(row, layout, &vals)?;
    Ok(vals.into_iter().map(|_| Value::Null).collect())
}

// Подготовка всех изменяемых записей перед сохранением
fn stage_updates(
    table: &mut Table,
    filter: &Expr<'_>,
    assignments: &HashMap<usize, Value<'_>>,
    target: &Schema,
    layout: &crate::layout::Layout,
) -> Result<UpdatePlan> {
    let filter = filter.prepare(&table.schema)?;
    let mut plan = UpdatePlan {
        ids: Vec::new(),
        bytes: Vec::new(),
        removed: HashSet::new(),
        new_ids: HashSet::new(),
    };
    let mut buffer = vec![Value::Null; table.schema.columns.len()];
    table.scan(|row| {
        if filter.matches(row)? {
            buffer = stage_row(
                &mut plan,
                Row {
                    schema: target,
                    ..row
                },
                layout,
                assignments,
                std::mem::take(&mut buffer),
            )?;
        }
        Ok(())
    })?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Column, DataType};

    #[test]
    fn pk_swap_is_validated_against_the_final_index() -> Result<()> {
        let root = std::env::temp_dir().join(format!("minidb-plan-swap-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let result = (|| -> Result<()> {
            let mut db = Database::open(&root)?;
            let schema = Schema::new(
                "items",
                vec![
                    Column::new("id", DataType::INT),
                    Column::new("name", DataType::STRING),
                ],
                0,
            );
            db.create(schema.clone())?;
            db.insert("items", &[Value::Int(1), Value::String("first")])?;
            db.insert("items", &[Value::Int(2), Value::String("second")])?;
            let mut plan = UpdatePlan {
                ids: Vec::new(),
                bytes: Vec::new(),
                removed: HashSet::new(),
                new_ids: HashSet::new(),
            };
            for (old, new, name) in [(1, 2, "first"), (2, 1, "second")] {
                let layout = db.table("items")?.layout.clone();
                let row = db.table_mut("items")?.get(old)?.ok_or("Нет строки")?;
                plan.push(row, &layout, &[Value::Int(new), Value::String(name)])?;
            }
            assert_eq!(apply(&mut db, "items", &schema, plan)?, 2);
            assert_eq!(
                db.get(
                    "items",
                    1,
                    |r| Ok(r.get("name")? == Value::String("second"))
                )?,
                Some(true)
            );
            assert_eq!(
                db.get("items", 2, |r| Ok(r.get("name")? == Value::String("first")))?,
                Some(true)
            );
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }
}
