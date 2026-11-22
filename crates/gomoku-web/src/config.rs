use std::net::SocketAddr;
use std::path::PathBuf;

use gomoku_core::bot::budget::{BotTimeLimits, DEFAULT_BOT_TIME_LIMITS, TimeLimit};
use gomoku_core::bot::profile::BOT_MIN_MEMORY_MB;
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
    #[error("EMAIL_TRANSPORT=file is allowed in production only with an http:// APP_ORIGIN")]
    FileTransportInProduction,
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

/// A secret that never shows up in logs: `Debug` prints a placeholder and there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTransport {
    /// Development and tests: messages are appended as JSON lines to a file.
    File,
    /// Production: the Brevo transactional API over HTTPS.
    Brevo,
}

/// Outgoing mail settings. Without a transport every send fails (accounts and games still work).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailConfig {
    pub transport: Option<EmailTransport>,
    pub outbox_path: PathBuf,
    /// Sender name shown in messages (`EMAIL_FROM_NAME`, else the older `EMAIL_FROM`).
    pub from_name: String,
    /// Verified sender address of the Brevo transport.
    pub from_address: Option<String>,
    pub brevo_api_key: Option<Secret>,
    /// Global daily send limit per pool; without it nothing is limited by the app.
    pub daily_limit: Option<u32>,
}

const CLIENT_IP_HEADERS: [&str; 3] = ["x-forwarded-for", "x-real-ip", "cf-connecting-ip"];

/// HTTP and security settings read from the environment.
/// Limits of the game chat (`CHAT_*`); the defaults are the product's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatConfig {
    pub max_length: usize,
    pub min_interval_ms: u64,
    pub max_messages_per_game: usize,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            max_length: gomoku_core::constants::CHAT_MAX_LENGTH,
            min_interval_ms: gomoku_core::constants::CHAT_MIN_INTERVAL_MS,
            max_messages_per_game: gomoku_core::constants::CHAT_MAX_MESSAGES_PER_GAME,
        }
    }
}

/// MCP agents (`MCP_*`, `CHAT_AGENT_*`): the abuse limits of the endpoint and the chat limits of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpConfig {
    /// Requests per client address per minute, whatever the method.
    pub request_limit: u32,
    /// Successful joins per address per hour.
    pub join_limit: u32,
    /// Wrong invitation codes per address (or in total, when the address is unknown) per 15 minutes.
    pub lookup_limit: u32,
    pub lookup_global_limit: u32,
    /// `get_state` and `play_move` calls per token per minute.
    pub token_limit: u32,
    /// `send_message` calls per token per minute.
    pub chat_limit: u32,
    pub chat_max_length: usize,
    pub chat_min_interval_ms: u64,
    /// Pause between the reads of a waiting `get_state` (a tuning knob; tests shorten it).
    pub poll_interval_ms: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            request_limit: 300,
            join_limit: 10,
            lookup_limit: 20,
            lookup_global_limit: 300,
            token_limit: 60,
            chat_limit: 30,
            chat_max_length: gomoku_core::constants::CHAT_AGENT_MAX_LENGTH,
            chat_min_interval_ms: gomoku_core::constants::CHAT_AGENT_MIN_INTERVAL_MS,
            poll_interval_ms: gomoku_core::agent::MCP_POLL_INTERVAL_MS,
        }
    }
}

impl McpConfig {
    fn from_lookup(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let limit = |name: &'static str, default: u32| -> Result<u32, ConfigError> {
            Ok(positive(lookup, name)?.unwrap_or(default))
        };
        Ok(Self {
            request_limit: limit("MCP_REQUEST_LIMIT", defaults.request_limit)?,
            join_limit: limit("MCP_JOIN_LIMIT", defaults.join_limit)?,
            lookup_limit: limit("MCP_LOOKUP_LIMIT", defaults.lookup_limit)?,
            lookup_global_limit: limit("MCP_LOOKUP_GLOBAL_LIMIT", defaults.lookup_global_limit)?,
            token_limit: limit("MCP_TOKEN_LIMIT", defaults.token_limit)?,
            chat_limit: limit("MCP_CHAT_LIMIT", defaults.chat_limit)?,
            chat_max_length: ranged(lookup, "CHAT_AGENT_MAX_LENGTH", 1, 1000)?
                .map_or(defaults.chat_max_length, |n| n as usize),
            chat_min_interval_ms: ranged(lookup, "CHAT_AGENT_MIN_INTERVAL_MS", 0, 3_600_000)?
                .unwrap_or(defaults.chat_min_interval_ms),
            poll_interval_ms: ranged(lookup, "MCP_POLL_INTERVAL_MS", 10, 60_000)?
                .unwrap_or(defaults.poll_interval_ms),
        })
    }
}

/// Show mode (`SHOW_*`, `BOT_SHOW_*`): the administrator's bot show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowConfig {
    pub enabled: bool,
    pub bot_vs_bot_enabled: bool,
    /// Level 2 in a bot vs bot show (a bot vs agent show needs only the bot switch).
    pub bot_level2_enabled: bool,
    pub pause_min_ms: i64,
    pub pause_max_ms: i64,
    pub bot_l2_soft_ms: i64,
}

impl Default for ShowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_vs_bot_enabled: false,
            bot_level2_enabled: false,
            pause_min_ms: 1000,
            pause_max_ms: 2000,
            bot_l2_soft_ms: DEFAULT_BOT_TIME_LIMITS.level2.soft_ms,
        }
    }
}

impl ShowConfig {
    fn from_lookup(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let pause_max_ms = ranged(lookup, "BOT_SHOW_PAUSE_MAX_MS", 0, 8000)?
            .map_or(defaults.pause_max_ms, |n| n as i64);
        let pause_min_ms = ranged(lookup, "BOT_SHOW_PAUSE_MIN_MS", 0, 8000)?
            .map_or_else(|| defaults.pause_min_ms.min(pause_max_ms), |n| n as i64);
        if pause_min_ms > pause_max_ms {
            return Err(ConfigError::Invalid("BOT_SHOW_PAUSE_MIN_MS"));
        }
        Ok(Self {
            enabled: flag(lookup, "SHOW_ENABLED")?,
            bot_vs_bot_enabled: flag(lookup, "SHOW_BOT_VS_BOT_ENABLED")?,
            bot_level2_enabled: flag(lookup, "SHOW_BOT_LEVEL2_ENABLED")?,
            pause_min_ms,
            pause_max_ms,
            bot_l2_soft_ms: ranged(lookup, "SHOW_BOT_L2_SOFT_MS", 1, 60_000)?
                .map_or(defaults.bot_l2_soft_ms, |n| n as i64),
        })
    }
}

/// The hosted bot (`BOT_*`): admission, capacity and the per-move time limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotConfig {
    pub enabled: bool,
    pub max_active_games: u32,
    pub min_available_mb: i64,
    pub fixed_memory_mb: Option<i64>,
    pub limits: BotTimeLimits,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_active_games: 3,
            min_available_mb: BOT_MIN_MEMORY_MB,
            fixed_memory_mb: None,
            limits: DEFAULT_BOT_TIME_LIMITS,
        }
    }
}

/// `true` / `false` / unset (false); anything else is an error.
fn flag(lookup: &impl Fn(&str) -> Option<String>, name: &'static str) -> Result<bool, ConfigError> {
    match lookup(name).as_deref() {
        None | Some("") | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(ConfigError::Invalid(name)),
    }
}

/// A soft and ceiling pair: each 1..60000, the ceiling not below the soft limit; the default ceiling follows the soft.
fn time_limit(
    lookup: &impl Fn(&str) -> Option<String>,
    soft_name: &'static str,
    ceiling_name: &'static str,
    default: TimeLimit,
) -> Result<TimeLimit, ConfigError> {
    let soft = ranged(lookup, soft_name, 1, 60_000)?.map_or(default.soft_ms, |n| n as i64);
    let ceiling =
        ranged(lookup, ceiling_name, 1, 60_000)?.map_or(default.ceiling_ms.max(soft), |n| n as i64);
    if ceiling < soft {
        return Err(ConfigError::Invalid(ceiling_name));
    }
    Ok(TimeLimit {
        soft_ms: soft,
        ceiling_ms: ceiling,
    })
}

impl BotConfig {
    fn from_lookup(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        Ok(Self {
            enabled: flag(lookup, "BOT_ENABLED")?,
            max_active_games: ranged(lookup, "BOT_MAX_ACTIVE_GAMES", 1, 1000)?
                .map_or(defaults.max_active_games, |n| n as u32),
            min_available_mb: ranged(
                lookup,
                "BOT_MIN_AVAILABLE_MB",
                BOT_MIN_MEMORY_MB as u64,
                32_767,
            )?
            .map_or(defaults.min_available_mb, |n| n as i64),
            fixed_memory_mb: ranged(lookup, "BOT_FIXED_MEMORY_MB", 1, 1_000_000)?.map(|n| n as i64),
            limits: BotTimeLimits {
                level1: time_limit(
                    lookup,
                    "BOT_L1_SOFT_MS",
                    "BOT_L1_CEILING_MS",
                    DEFAULT_BOT_TIME_LIMITS.level1,
                )?,
                level2: time_limit(
                    lookup,
                    "BOT_L2_SOFT_MS",
                    "BOT_L2_CEILING_MS",
                    DEFAULT_BOT_TIME_LIMITS.level2,
                )?,
            },
        })
    }
}

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
    pub email: EmailConfig,
    /// E-mail of the installation administrator (`ADMIN_EMAIL`, normalized): nobody can register on it and its
    /// address never changes through the app. Optional until the administrator panel arrives.
    pub admin_email: Option<String>,
    pub chat: ChatConfig,
    pub bot: BotConfig,
    pub mcp: McpConfig,
    pub show: ShowConfig,
}

/// An integer setting in `min..=max` written as plain digits; unset or empty means the default.
fn ranged(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, ConfigError> {
    match lookup(name).filter(|raw| !raw.is_empty()) {
        None => Ok(None),
        Some(raw) => Some(raw)
            .filter(|raw| raw.len() <= 9 && raw.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|n| (min..=max).contains(n))
            .map(Some)
            .ok_or(ConfigError::Invalid(name)),
    }
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
        let transport = match lookup("EMAIL_TRANSPORT").as_deref() {
            None | Some("") => None,
            Some("file") => Some(EmailTransport::File),
            Some("brevo") => Some(EmailTransport::Brevo),
            Some(_) => return Err(ConfigError::Invalid("EMAIL_TRANSPORT")),
        };
        // The outbox holds full messages (links with tokens), so in production it is allowed only for a plain-http
        // origin (a local stack); a real deployment must use a mail provider.
        if production && transport == Some(EmailTransport::File) && https {
            return Err(ConfigError::FileTransportInProduction);
        }
        let non_empty = |name: &str| lookup(name).filter(|raw| !raw.is_empty());
        let email = EmailConfig {
            transport,
            outbox_path: non_empty("EMAIL_OUTBOX_PATH")
                .map_or_else(|| PathBuf::from(".email-outbox.jsonl"), PathBuf::from),
            from_name: non_empty("EMAIL_FROM_NAME")
                .or_else(|| non_empty("EMAIL_FROM"))
                .unwrap_or_else(|| "Gomoku".to_owned()),
            from_address: non_empty("EMAIL_FROM_ADDRESS"),
            brevo_api_key: non_empty("BREVO_API_KEY").map(Secret::new),
            daily_limit: positive(&lookup, "EMAIL_DAILY_LIMIT")?,
        };
        let chat_defaults = ChatConfig::default();
        let chat = ChatConfig {
            max_length: ranged(&lookup, "CHAT_MAX_LENGTH", 1, 1000)?
                .map_or(chat_defaults.max_length, |n| n as usize),
            min_interval_ms: ranged(&lookup, "CHAT_MIN_INTERVAL_MS", 0, 3_600_000)?
                .unwrap_or(chat_defaults.min_interval_ms),
            max_messages_per_game: ranged(&lookup, "CHAT_MAX_MESSAGES_PER_GAME", 1, 100_000)?
                .map_or(chat_defaults.max_messages_per_game, |n| n as usize),
        };
        let admin_email = lookup("ADMIN_EMAIL")
            .map(|raw| gomoku_core::validators::normalize_email(&raw))
            .filter(|email| !email.is_empty());
        // Without the administrator the show and the panel are unreachable and nobody can reclaim them: refuse to start.
        if production && admin_email.is_none() {
            return Err(ConfigError::RequiredInProduction("ADMIN_EMAIL"));
        }
        Ok(Self {
            bind_addr,
            assets_dir,
            app_origin,
            production,
            trust_proxy_hops,
            client_ip_header,
            session_cookie_secure,
            password,
            email,
            admin_email,
            chat,
            bot: BotConfig::from_lookup(&lookup)?,
            mcp: McpConfig::from_lookup(&lookup)?,
            show: ShowConfig::from_lookup(&lookup)?,
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

    const PRODUCTION: [(&str, &str); 4] = [
        ("APP_ENV", "production"),
        ("APP_ORIGIN", "https://gomoku.example"),
        ("TRUST_PROXY_HOPS", "1"),
        ("ADMIN_EMAIL", "admin@example.com"),
    ];

    #[test]
    fn bot_defaults_and_overrides() {
        let off = AppConfig::from_lookup(env(&[])).unwrap().bot;
        assert_eq!(off, BotConfig::default());
        assert!(!off.enabled);
        assert_eq!(
            (off.limits.level1.soft_ms, off.limits.level1.ceiling_ms),
            (50, 250)
        );
        assert_eq!(
            (off.limits.level2.soft_ms, off.limits.level2.ceiling_ms),
            (1500, 3000)
        );
        let on = AppConfig::from_lookup(env(&[
            ("BOT_ENABLED", "true"),
            ("BOT_MAX_ACTIVE_GAMES", "7"),
            ("BOT_MIN_AVAILABLE_MB", "512"),
            ("BOT_FIXED_MEMORY_MB", "2048"),
            ("BOT_L1_SOFT_MS", "400"),
            ("BOT_L2_SOFT_MS", "5000"),
            ("BOT_L2_CEILING_MS", "6000"),
        ]))
        .unwrap()
        .bot;
        assert!(on.enabled);
        assert_eq!(
            (on.max_active_games, on.min_available_mb, on.fixed_memory_mb),
            (7, 512, Some(2048))
        );
        // The default ceiling follows a raised soft limit.
        assert_eq!(
            (on.limits.level1.soft_ms, on.limits.level1.ceiling_ms),
            (400, 400)
        );
        assert_eq!(
            (on.limits.level2.soft_ms, on.limits.level2.ceiling_ms),
            (5000, 6000)
        );
        assert!(
            !AppConfig::from_lookup(env(&[("BOT_ENABLED", "false")]))
                .unwrap()
                .bot
                .enabled
        );
    }

    #[test]
    fn bot_settings_are_validated() {
        for (name, value) in [
            ("BOT_ENABLED", "yes"),
            ("BOT_MAX_ACTIVE_GAMES", "0"),
            ("BOT_MIN_AVAILABLE_MB", "255"),
            ("BOT_MIN_AVAILABLE_MB", "32768"),
            ("BOT_FIXED_MEMORY_MB", "0"),
            ("BOT_L1_SOFT_MS", "60001"),
            ("BOT_L2_SOFT_MS", "abc"),
        ] {
            assert_eq!(
                AppConfig::from_lookup(env(&[(name, value)])),
                Err(ConfigError::Invalid(name)),
                "{name}={value}"
            );
        }
        assert_eq!(
            AppConfig::from_lookup(env(&[
                ("BOT_L1_SOFT_MS", "300"),
                ("BOT_L1_CEILING_MS", "200")
            ])),
            Err(ConfigError::Invalid("BOT_L1_CEILING_MS"))
        );
    }

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
    fn mcp_limits_default_and_are_validated() {
        let cfg = AppConfig::from_lookup(env(&[])).unwrap();
        assert_eq!(cfg.mcp, McpConfig::default());
        assert_eq!(
            (
                cfg.mcp.request_limit,
                cfg.mcp.join_limit,
                cfg.mcp.token_limit,
                cfg.mcp.chat_limit
            ),
            (300, 10, 60, 30)
        );
        let cfg = AppConfig::from_lookup(env(&[
            ("MCP_REQUEST_LIMIT", "5"),
            ("MCP_JOIN_LIMIT", "6"),
            ("MCP_LOOKUP_LIMIT", "7"),
            ("MCP_LOOKUP_GLOBAL_LIMIT", "8"),
            ("MCP_TOKEN_LIMIT", "9"),
            ("MCP_CHAT_LIMIT", "10"),
            ("CHAT_AGENT_MAX_LENGTH", "50"),
            ("CHAT_AGENT_MIN_INTERVAL_MS", "0"),
            ("MCP_POLL_INTERVAL_MS", "20"),
        ]))
        .unwrap();
        assert_eq!(
            cfg.mcp,
            McpConfig {
                request_limit: 5,
                join_limit: 6,
                lookup_limit: 7,
                lookup_global_limit: 8,
                token_limit: 9,
                chat_limit: 10,
                chat_max_length: 50,
                chat_min_interval_ms: 0,
                poll_interval_ms: 20,
            }
        );
        for (name, value) in [
            ("MCP_REQUEST_LIMIT", "0"),
            ("MCP_TOKEN_LIMIT", "-1"),
            ("CHAT_AGENT_MAX_LENGTH", "1001"),
            ("CHAT_AGENT_MIN_INTERVAL_MS", "3600001"),
            ("MCP_POLL_INTERVAL_MS", "9"),
            ("MCP_POLL_INTERVAL_MS", "60001"),
        ] {
            assert!(
                AppConfig::from_lookup(env(&[(name, value)])).is_err(),
                "{name}={value}"
            );
        }
    }

    #[test]
    fn chat_limits_default_and_are_validated() {
        let cfg = AppConfig::from_lookup(env(&[])).unwrap();
        assert_eq!(cfg.chat, ChatConfig::default());
        let cfg = AppConfig::from_lookup(env(&[
            ("CHAT_MAX_LENGTH", "50"),
            ("CHAT_MIN_INTERVAL_MS", "0"),
            ("CHAT_MAX_MESSAGES_PER_GAME", "10"),
        ]))
        .unwrap();
        assert_eq!(
            (
                cfg.chat.max_length,
                cfg.chat.min_interval_ms,
                cfg.chat.max_messages_per_game
            ),
            (50, 0, 10)
        );
        for (name, value) in [
            ("CHAT_MAX_LENGTH", "0"),
            ("CHAT_MAX_LENGTH", "1001"),
            ("CHAT_MIN_INTERVAL_MS", "3600001"),
            ("CHAT_MIN_INTERVAL_MS", "1e3"),
            ("CHAT_MAX_MESSAGES_PER_GAME", "100001"),
            ("CHAT_MAX_MESSAGES_PER_GAME", "-1"),
            ("CHAT_MAX_LENGTH", "1234567890"),
        ] {
            let error = AppConfig::from_lookup(env(&[(name, value)])).unwrap_err();
            assert!(
                matches!(error, ConfigError::Invalid(n) if n == name),
                "{name}={value}"
            );
        }
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
        let without_admin: Vec<_> = PRODUCTION
            .iter()
            .copied()
            .filter(|(k, _)| *k != "ADMIN_EMAIL")
            .collect();
        assert_eq!(
            AppConfig::from_lookup(env(&without_admin)),
            Err(ConfigError::RequiredInProduction("ADMIN_EMAIL"))
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
    fn reads_the_email_settings() {
        let cfg = AppConfig::from_lookup(env(&[])).unwrap();
        assert_eq!(cfg.email.transport, None);
        assert_eq!(cfg.email.from_name, "Gomoku");
        assert_eq!(cfg.email.outbox_path, PathBuf::from(".email-outbox.jsonl"));
        assert!(cfg.email.brevo_api_key.is_none() && cfg.email.daily_limit.is_none());

        let cfg = AppConfig::from_lookup(env(&[
            ("EMAIL_TRANSPORT", "brevo"),
            ("EMAIL_FROM", "Old Name"),
            ("EMAIL_FROM_ADDRESS", "noreply@gomoku.example"),
            ("BREVO_API_KEY", "xkeysib-secret"),
            ("EMAIL_DAILY_LIMIT", "176"),
            ("EMAIL_OUTBOX_PATH", "/tmp/outbox.jsonl"),
        ]))
        .unwrap();
        assert_eq!(cfg.email.transport, Some(EmailTransport::Brevo));
        assert_eq!(cfg.email.from_name, "Old Name");
        assert_eq!(
            cfg.email.from_address.as_deref(),
            Some("noreply@gomoku.example")
        );
        assert_eq!(
            cfg.email.brevo_api_key.as_ref().map(Secret::expose),
            Some("xkeysib-secret")
        );
        assert_eq!(cfg.email.daily_limit, Some(176));
        // The newer name wins over the alias, and the key never shows up in debug output.
        let named =
            AppConfig::from_lookup(env(&[("EMAIL_FROM", "Old"), ("EMAIL_FROM_NAME", "New")]))
                .unwrap();
        assert_eq!(named.email.from_name, "New");
        assert!(!format!("{cfg:?}").contains("xkeysib-secret"));
        let file = AppConfig::from_lookup(env(&[("EMAIL_TRANSPORT", "file")])).unwrap();
        assert_eq!(file.email.transport, Some(EmailTransport::File));
    }

    #[test]
    fn the_administrator_email_is_normalized_and_optional() {
        assert_eq!(AppConfig::from_lookup(env(&[])).unwrap().admin_email, None);
        assert_eq!(
            AppConfig::from_lookup(env(&[("ADMIN_EMAIL", "  ")]))
                .unwrap()
                .admin_email,
            None
        );
        let cfg = AppConfig::from_lookup(env(&[("ADMIN_EMAIL", " Admin@Example.com ")])).unwrap();
        assert_eq!(cfg.admin_email.as_deref(), Some("admin@example.com"));
    }

    #[test]
    fn rejects_bad_email_settings() {
        assert!(AppConfig::from_lookup(env(&[("EMAIL_TRANSPORT", "smtp")])).is_err());
        assert!(AppConfig::from_lookup(env(&[("EMAIL_DAILY_LIMIT", "0")])).is_err());
        // The file transport is refused in production behind https, allowed behind plain http.
        let mut pairs = PRODUCTION.to_vec();
        pairs.push(("EMAIL_TRANSPORT", "file"));
        assert_eq!(
            AppConfig::from_lookup(env(&pairs)),
            Err(ConfigError::FileTransportInProduction)
        );
        let local = AppConfig::from_lookup(env(&[
            ("APP_ENV", "production"),
            ("APP_ORIGIN", "http://localhost:3000"),
            ("TRUST_PROXY_HOPS", "1"),
            ("ADMIN_EMAIL", "admin@example.com"),
            ("EMAIL_TRANSPORT", "file"),
        ]))
        .unwrap();
        assert_eq!(local.email.transport, Some(EmailTransport::File));
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
