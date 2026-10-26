// Сравнение скорости поиска полным перебором и через индекс
use minidb::{Column, Compare, DataType, Database, Expr, Kind, Result, Schema, Value};
use std::hint::black_box;
use std::time::Instant;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).unwrap_or("scratch/bench".into());
    let rows: i64 = std::env::args().nth(2).unwrap_or("5000".into()).parse()?;
    if !(1000..=1_000_000).contains(&rows) {
        return Err("Число записей: 1000..1000000".into());
    }
    let mut db = Database::open(&path)?;
    if db.schema("items").is_err() {
        db.create(Schema::new(
            "items",
            vec![
                Column::new("id", DataType::INT),
                Column::new(
                    "payload",
                    DataType {
                        kind: Kind::String { max_bytes: 64 },
                        ..DataType::STRING
                    },
                ),
            ],
            0,
        ))?;
        for id in 1..=rows {
            db.insert(
                "items",
                &[Value::Int(id), Value::String("benchmark record")],
            )?;
        }
    }
    if db.scan("items", &Expr::All, |_| Ok(()))? != rows as usize {
        return Err("В каталоге другой набор данных; используйте новый путь".into());
    }
    let queries = 1000;
    // Читаем данные перед измерением, чтобы заполнить файловый кэш
    db.scan("items", &Expr::All, |row| {
        black_box(row.get("id")?);
        Ok(())
    })?;
    db.get("items", 1, |row| {
        black_box(row.get("id")?);
        Ok(())
    })?;
    let start = Instant::now();
    let mut scan_sum = 0;
    for i in 0..queries {
        let key = (i * 7919) % rows + 1;
        db.scan(
            "items",
            &Expr::Compare("id", Compare::Eq, Value::Int(key)),
            |row| {
                scan_sum += black_box(row.get("id")?.int()?);
                Ok(())
            },
        )?;
    }
    let scan = start.elapsed();
    let start = Instant::now();
    let mut index_sum = 0;
    for i in 0..queries {
        let key = (i * 7919) % rows + 1;
        index_sum += db
            .get("items", key, |row| Ok(black_box(row.get("id")?.int()?)))?
            .ok_or("Потеряна запись")?;
    }
    let indexed = start.elapsed();
    assert_eq!(scan_sum, index_sum);
    let csv = format!("rows,queries,scan_ms,index_ms,speedup,checksum\n{rows},{queries},{:.3},{:.3},{:.2},{scan_sum}\n",
        scan.as_secs_f64() * 1000.0, indexed.as_secs_f64() * 1000.0, scan.as_secs_f64() / indexed.as_secs_f64());
    print!("{csv}");
    std::fs::write(std::path::Path::new(&path).join("benchmark.csv"), csv)?;
    Ok(())
}
