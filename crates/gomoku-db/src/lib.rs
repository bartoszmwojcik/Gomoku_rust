//! Data access: configuration, connection pool, migrations.

mod config;
mod database;

pub use config::{ConfigError, DbConfig};
pub use database::{Database, DbError};
