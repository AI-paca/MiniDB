use crate::tables::experiments::Experiment;

// Этот файл принадлежит потребителю; генератор его не меняет.
impl Experiment {
    pub fn validate(&self) -> minidb::Result<()> {
        if self.name.trim().is_empty() {
            return Err("Имя эксперимента не должно быть пустым".into());
        }
        Ok(())
    }

    pub fn score(&self) -> f64 {
        self.name.len() as f64
    }
}
