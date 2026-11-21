use thiserror::Error;

const DEFAULT_POOL_MAX: u32 = 10;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value of {0}: expected a positive integer")]
    InvalidNumber(&'static str),
}

/// Database settings read from the environment (`DATABASE_URL`, `DATABASE_POOL_MAX`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbConfig {
    pub url: String,
    pub pool_max: u32,
}

impl DbConfig {
    /// Reads settings through `lookup`, so tests do not touch the process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let url = lookup("DATABASE_URL")
            .filter(|v| !v.is_empty())
            .ok_or(ConfigError::Missing("DATABASE_URL"))?;
        let pool_max = match lookup("DATABASE_POOL_MAX") {
            None => DEFAULT_POOL_MAX,
            Some(raw) => raw
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(ConfigError::InvalidNumber("DATABASE_POOL_MAX"))?,
        };
        Ok(Self { url, pool_max })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn reads_url_and_defaults_pool_size() {
        let cfg = DbConfig::from_lookup(env(&[("DATABASE_URL", "postgres://x")])).unwrap();
        assert_eq!(
            cfg,
            DbConfig {
                url: "postgres://x".into(),
                pool_max: 10
            }
        );
    }

    #[test]
    fn reads_custom_pool_size() {
        let cfg = DbConfig::from_lookup(env(&[("DATABASE_URL", "u"), ("DATABASE_POOL_MAX", "3")]))
            .unwrap();
        assert_eq!(cfg.pool_max, 3);
    }

    #[test]
    fn rejects_missing_or_empty_url() {
        assert_eq!(
            DbConfig::from_lookup(env(&[])),
            Err(ConfigError::Missing("DATABASE_URL"))
        );
        assert_eq!(
            DbConfig::from_lookup(env(&[("DATABASE_URL", "")])),
            Err(ConfigError::Missing("DATABASE_URL"))
        );
    }

    #[test]
    fn rejects_bad_pool_size() {
        for bad in ["0", "-1", "abc"] {
            let result =
                DbConfig::from_lookup(env(&[("DATABASE_URL", "u"), ("DATABASE_POOL_MAX", bad)]));
            assert_eq!(result, Err(ConfigError::InvalidNumber("DATABASE_POOL_MAX")));
        }
    }
}
