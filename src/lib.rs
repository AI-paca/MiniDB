// Файловая база данных MiniDB и SQL-консоль
pub mod codegen;
pub mod database;
pub mod layout;
pub mod query;
pub mod record;
pub mod schema;
pub mod sql;
mod storage;
pub mod table;
mod text_file;
pub mod types;

pub use database::Database;
pub use query::{Compare, Expr};
pub use table::{Column, Reference, Row, Schema};
pub use types::{DataType, Kind, Value};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
