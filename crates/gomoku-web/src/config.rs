use std::net::SocketAddr;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid BIND_ADDR: {0}")]
    BindAddr(String),
    #[error("invalid value of {0}")]
    Invalid(&'static str),
    #[error("{0} is required in production")]
    RequiredInProduction(&'static str),
    #[error("SESSION_COOKIE_SECURE=false is not allowed with an https APP_ORIGIN")]
    InsecureCookieWithHttps,
}

/// argon2id parameters; the defaults follow the OWASP recommendation (19 MiB, 2 iterations, 1 lane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for PasswordParams {
    fn default() -> Self {
        Self {
            m_cost_kib: 19_456,
            t_cost: 2,
            p_cost: 1,
        }
    }
}

const CLIENT_IP_HEADERS: [&str; 3] = ["x-forwarded-for", "x-real-ip", "cf-connecting-ip"];

/// HTTP and security settings read from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub assets_dir: PathBuf,
    /// Exact origin of the app (`https://host`); the CSRF check compares `Origin` with it.
    pub app_origin: Option<String>,
    pub production: bool,
    /// Number of our own proxies in front of the app; needed to read the client address.
    pub trust_proxy_hops: Option<usize>,
    pub client_ip_header: String,
    pub session_cookie_secure: bool,
    pub password: PasswordParams,
}

fn positive(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
) -> Result<Option<u32>, ConfigError> {
    match lookup(name).filter(|raw| !raw.is_empty()) {
        None => Ok(None),
        Some(raw) => raw
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or(ConfigError::Invalid(name)),
    }
}

impl AppConfig {
    /// Reads settings through `lookup`, so tests do not touch the process environment. In production
    /// (`APP_ENV=production`) `APP_ORIGIN` and `TRUST_PROXY_HOPS` are required: starting without them would
    /// break the CSRF check and every rate limit, so the process refuses to start.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let raw = lookup("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:3000".to_string());
        let bind_addr = raw.parse().map_err(|_| ConfigError::BindAddr(raw))?;
        let assets_dir =
            lookup("ASSETS_DIR").map_or_else(|| PathBuf::from("assets"), PathBuf::from);
        let production = lookup("APP_ENV").as_deref() == Some("production");

        let app_origin = lookup("APP_ORIGIN").filter(|raw| !raw.is_empty());
        if let Some(origin) = &app_origin {
            let valid = (origin.starts_with("http://") || origin.starts_with("https://"))
                && !origin.ends_with('/')
                && origin.matches('/').count() == 2;
            if !valid {
                return Err(ConfigError::Invalid("APP_ORIGIN"));
            }
        }
        let trust_proxy_hops = positive(&lookup, "TRUST_PROXY_HOPS")?.map(|n| n as usize);
        if production && app_origin.is_none() {
            return Err(ConfigError::RequiredInProduction("APP_ORIGIN"));
        }
        if production && trust_proxy_hops.is_none() {
            return Err(ConfigError::RequiredInProduction("TRUST_PROXY_HOPS"));
        }

        let client_ip_header = lookup("CLIENT_IP_HEADER")
            .filter(|raw| !raw.is_empty())
            .map_or_else(
                || "x-forwarded-for".to_string(),
                |raw| raw.to_ascii_lowercase(),
            );
        if !CLIENT_IP_HEADERS.contains(&client_ip_header.as_str()) {
            return Err(ConfigError::Invalid("CLIENT_IP_HEADER"));
        }

        let secure_override = match lookup("SESSION_COOKIE_SECURE").as_deref() {
            None | Some("") => None,
            Some("true") => Some(true),
            Some("false") => Some(false),
            Some(_) => return Err(ConfigError::Invalid("SESSION_COOKIE_SECURE")),
        };
        let https = app_origin
            .as_deref()
            .is_some_and(|origin| origin.starts_with("https://"));
        let session_cookie_secure = secure_override.unwrap_or(production);
        if !session_cookie_secure && https {
            return Err(ConfigError::InsecureCookieWithHttps);
        }

        let defaults = PasswordParams::default();
        let password = PasswordParams {
            m_cost_kib: positive(&lookup, "PASSWORD_HASH_M_KIB")?.unwrap_or(defaults.m_cost_kib),
            t_cost: positive(&lookup, "PASSWORD_HASH_T_COST")?.unwrap_or(defaults.t_cost),
            p_cost: defaults.p_cost,
        };
        Ok(Self {
            bind_addr,
            assets_dir,
            app_origin,
            production,
            trust_proxy_hops,
            client_ip_header,
            session_cookie_secure,
            password,
        })
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
                .map(|(_, v)| (*v).to_string())
        }
    }

    const PRODUCTION: [(&str, &str); 3] = [
        ("APP_ENV", "production"),
        ("APP_ORIGIN", "https://gomoku.example"),
        ("TRUST_PROXY_HOPS", "1"),
    ];

    #[test]
    fn uses_development_defaults() {
        let cfg = AppConfig::from_lookup(env(&[])).unwrap();
        assert_eq!(cfg.bind_addr, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(cfg.assets_dir, PathBuf::from("assets"));
        assert_eq!(cfg.app_origin, None);
        assert!(!cfg.production && !cfg.session_cookie_secure);
        assert_eq!(cfg.client_ip_header, "x-forwarded-for");
        assert_eq!(cfg.password, PasswordParams::default());
    }

    #[test]
    fn reads_overrides() {
        let cfg = AppConfig::from_lookup(env(&[
            ("BIND_ADDR", "127.0.0.1:8080"),
            ("ASSETS_DIR", "/srv/assets"),
            ("CLIENT_IP_HEADER", "CF-Connecting-IP"),
            ("PASSWORD_HASH_M_KIB", "8"),
            ("PASSWORD_HASH_T_COST", "1"),
        ]))
        .unwrap();
        assert_eq!(cfg.bind_addr.port(), 8080);
        assert_eq!(cfg.assets_dir, PathBuf::from("/srv/assets"));
        assert_eq!(cfg.client_ip_header, "cf-connecting-ip");
        assert_eq!((cfg.password.m_cost_kib, cfg.password.t_cost), (8, 1));
    }

    #[test]
    fn production_needs_origin_and_proxy_hops_and_is_secure() {
        let cfg = AppConfig::from_lookup(env(&PRODUCTION)).unwrap();
        assert!(cfg.production && cfg.session_cookie_secure);
        assert_eq!(cfg.app_origin.as_deref(), Some("https://gomoku.example"));
        assert_eq!(cfg.trust_proxy_hops, Some(1));
        assert_eq!(
            AppConfig::from_lookup(env(&[("APP_ENV", "production"), ("TRUST_PROXY_HOPS", "1")])),
            Err(ConfigError::RequiredInProduction("APP_ORIGIN"))
        );
        assert_eq!(
            AppConfig::from_lookup(env(&[
                ("APP_ENV", "production"),
                ("APP_ORIGIN", "https://x.pl")
            ])),
            Err(ConfigError::RequiredInProduction("TRUST_PROXY_HOPS"))
        );
    }

    #[test]
    fn rejects_bad_values() {
        for (name, value) in [
            ("BIND_ADDR", "nope"),
            ("APP_ORIGIN", "gomoku.example"),
            ("APP_ORIGIN", "https://x.pl/"),
            ("APP_ORIGIN", "https://x.pl/path"),
            ("TRUST_PROXY_HOPS", "0"),
            ("TRUST_PROXY_HOPS", "abc"),
            ("CLIENT_IP_HEADER", "x-evil"),
            ("SESSION_COOKIE_SECURE", "yes"),
            ("PASSWORD_HASH_M_KIB", "-1"),
        ] {
            assert!(
                AppConfig::from_lookup(env(&[(name, value)])).is_err(),
                "{name}={value}"
            );
        }
        assert_eq!(
            AppConfig::from_lookup(env(&[("BIND_ADDR", "nope")])),
            Err(ConfigError::BindAddr("nope".into()))
        );
    }

    #[test]
    fn an_insecure_cookie_is_refused_with_an_https_origin() {
        let mut pairs = PRODUCTION.to_vec();
        pairs.push(("SESSION_COOKIE_SECURE", "false"));
        assert_eq!(
            AppConfig::from_lookup(env(&pairs)),
            Err(ConfigError::InsecureCookieWithHttps)
        );
        let cfg = AppConfig::from_lookup(env(&[
            ("APP_ORIGIN", "http://localhost:3000"),
            ("SESSION_COOKIE_SECURE", "false"),
        ]))
        .unwrap();
        assert!(!cfg.session_cookie_secure);
        let forced = AppConfig::from_lookup(env(&[("SESSION_COOKIE_SECURE", "true")])).unwrap();
        assert!(forced.session_cookie_secure);
    }
}
