//! Data access: configuration, connection pool, migrations, accounts, rate limits.

mod accounts;
pub mod cleanup;
pub mod cli;
mod config;
mod database;
mod email_links;
mod lifecycle;
mod passwords;
mod rate_limit;

pub use accounts::{NewSession, NewUser, RegisterError, SessionUser, User};
pub use config::{ConfigError, DbConfig};
pub use database::{Database, DbError};
pub use email_links::{EmailChanged, RenameOutcome};
pub use lifecycle::{DeleteOutcome, ProvisionAction, ProvisionError, Provisioned};
pub use passwords::ResetToken;
pub use rate_limit::Hit;
pub use sqlx::types::time::OffsetDateTime;
pub use sqlx::types::uuid::Uuid;
