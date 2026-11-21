use std::net::SocketAddr;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid BIND_ADDR: {0}")]
    BindAddr(String),
}

/// HTTP settings read from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub assets_dir: PathBuf,
}

impl AppConfig {
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let raw = lookup("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:3000".to_string());
        let bind_addr = raw.parse().map_err(|_| ConfigError::BindAddr(raw))?;
        let assets_dir =
            lookup("ASSETS_DIR").map_or_else(|| PathBuf::from("assets"), PathBuf::from);
        Ok(Self {
            bind_addr,
            assets_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_defaults() {
        let cfg = AppConfig::from_lookup(|_| None).unwrap();
        assert_eq!(cfg.bind_addr, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(cfg.assets_dir, PathBuf::from("assets"));
    }

    #[test]
    fn reads_overrides() {
        let cfg = AppConfig::from_lookup(|k| match k {
            "BIND_ADDR" => Some("127.0.0.1:8080".into()),
            "ASSETS_DIR" => Some("/srv/assets".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.bind_addr.port(), 8080);
        assert_eq!(cfg.assets_dir, PathBuf::from("/srv/assets"));
    }

    #[test]
    fn rejects_bad_bind_addr() {
        let err = AppConfig::from_lookup(|_| Some("nope".into())).unwrap_err();
        assert_eq!(err, ConfigError::BindAddr("nope".into()));
    }
}
