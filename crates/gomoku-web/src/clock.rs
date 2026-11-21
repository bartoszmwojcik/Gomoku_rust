use std::sync::Arc;

use gomoku_db::OffsetDateTime;

/// Source of the current time; injected so tests control it.
pub type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// The real clock.
pub fn system_clock() -> Clock {
    Arc::new(OffsetDateTime::now_utc)
}
