pub mod tables {
    include!(concat!(env!("OUT_DIR"), "/tables.rs"));
}
mod experiments_rules;

use minidb::{Database, Result, Schema};
use tables::experiments::Experiment;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).ok_or("Укажите каталог demo-базы")?;
    let mut db = Database::open_with(path, &[("experiment_rules", Experiment::validator_adapter)])?;
    if db.table_names()?.is_empty() {
        db.create(Schema::from_text(tables::experiments::SCHEMA)?)?;
    }
    let experiment = Experiment {
        id: 1,
        name: "simulation".into(),
        ..Default::default()
    };
    experiment.insert(&mut db)?;
    let saved = db
        .get("experiments", 1, Experiment::from_row)?
        .ok_or("Нет записи")?;
    println!("{}: score={}", saved.name, saved.score());
    Ok(())
}
