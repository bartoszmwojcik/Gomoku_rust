//! Outgoing mail: the sender port, its transports (file, Brevo, none) and the message templates.
//!
//! Implementations never log the address, the body or a key: failures carry a short code only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncWriteExt;

use crate::clock::Clock;
use crate::config::{EmailConfig, EmailTransport};

pub const BREVO_SEND_URL: &str = "https://api.brevo.com/v3/smtp/email";
/// Backstop for the Brevo call itself; callers pass a shorter limit.
pub const BREVO_TIMEOUT: Duration = Duration::from_secs(10);
/// A send awaited inside a user request is bounded by this.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// A message to deliver; both bodies are required so transports need no template logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// Delivery failure. The code is a short identifier (`BREVO_HTTP_429`, `BREVO_NETWORK`, `BREVO_TIMEOUT`,
/// `FILE_IO`, `UNCONFIGURED`, `TIMEOUT`) safe to log: never the key, the address or the content.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("e-mail send failed: {code}")]
pub struct EmailError {
    pub code: String,
}

impl EmailError {
    fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

/// Port for outgoing mail. A send never outlives `timeout`: it rejects with code `TIMEOUT`.
#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, message: &EmailMessage, timeout: Duration) -> Result<(), EmailError>;
}

/// Development and test transport: appends each message as one JSON line to the outbox file. The file holds full
/// messages (links with tokens), so it is git-ignored and refused in production behind https.
pub struct FileEmailSender {
    path: PathBuf,
    from: String,
    clock: Clock,
}

impl FileEmailSender {
    pub fn new(path: PathBuf, from: String, clock: Clock) -> Self {
        Self { path, from, clock }
    }

    async fn append(&self, message: &EmailMessage) -> std::io::Result<()> {
        let at = (self.clock)().format(&Rfc3339).unwrap_or_default();
        let line = json!({
            "at": at, "from": self.from, "to": message.to, "subject": message.subject,
            "text": message.text, "html": message.html,
        });
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(format!("{line}\n").as_bytes()).await?;
        // A tokio file writes on a background thread: without a flush the data can be lost when it is dropped.
        file.flush().await
    }
}

#[async_trait]
impl EmailSender for FileEmailSender {
    async fn send(&self, message: &EmailMessage, timeout: Duration) -> Result<(), EmailError> {
        match tokio::time::timeout(timeout, self.append(message)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(EmailError::new("FILE_IO")),
            Err(_) => Err(EmailError::new("TIMEOUT")),
        }
    }
}

/// Sends mail through the Brevo transactional API (HTTPS).
pub struct BrevoEmailSender {
    client: reqwest::Client,
    url: String,
    api_key: String,
    from_address: String,
    from_name: String,
}

impl BrevoEmailSender {
    pub fn new(
        url: &str,
        api_key: &str,
        from_address: &str,
        from_name: &str,
    ) -> Result<Self, reqwest::Error> {
        // A redirect would carry the api-key header to another host, so none is followed.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            url: url.to_owned(),
            api_key: api_key.to_owned(),
            from_address: from_address.to_owned(),
            from_name: from_name.to_owned(),
        })
    }

    async fn post(&self, message: &EmailMessage) -> Result<(), EmailError> {
        let request = self
            .client
            .post(&self.url)
            .header("api-key", &self.api_key)
            .header("Accept", "application/json")
            .json(&json!({
                "sender": { "email": self.from_address, "name": self.from_name },
                "to": [{ "email": message.to }],
                "subject": message.subject,
                "htmlContent": message.html,
                "textContent": message.text,
            }))
            .timeout(BREVO_TIMEOUT);
        // The body is never read: it can echo the request.
        let response = request.send().await.map_err(|error| {
            EmailError::new(if error.is_timeout() {
                "BREVO_TIMEOUT"
            } else {
                "BREVO_NETWORK"
            })
        })?;
        match response.status().as_u16() {
            201 => Ok(()),
            status => Err(EmailError::new(format!("BREVO_HTTP_{status}"))),
        }
    }
}

#[async_trait]
impl EmailSender for BrevoEmailSender {
    async fn send(&self, message: &EmailMessage, timeout: Duration) -> Result<(), EmailError> {
        tokio::time::timeout(timeout, self.post(message))
            .await
            .unwrap_or_else(|_| Err(EmailError::new("TIMEOUT")))
    }
}

/// Used when no transport is configured: every send fails with a readable code.
pub struct UnconfiguredEmailSender;

#[async_trait]
impl EmailSender for UnconfiguredEmailSender {
    async fn send(&self, _message: &EmailMessage, _timeout: Duration) -> Result<(), EmailError> {
        Err(EmailError::new("UNCONFIGURED"))
    }
}

fn is_email_address(value: &str) -> bool {
    let mut parts = value.splitn(2, '@');
    match (parts.next(), parts.next()) {
        (Some(local), Some(domain)) => {
            let plain = |s: &str| {
                !s.is_empty() && !s.contains(|c: char| c.is_whitespace() || "@<>".contains(c))
            };
            plain(local)
                && plain(domain)
                && domain.contains('.')
                && !domain.ends_with('.')
                && !domain.starts_with('.')
        }
        _ => false,
    }
}

/// Picks the transport from the validated config. An unset transport, or `brevo` without its key or a valid
/// sender address, yields the failing sender (the app keeps running; sending answers with an error) and logs a
/// warning naming only the variables, never their values.
pub fn create_email_sender(config: &EmailConfig, clock: Clock) -> Arc<dyn EmailSender> {
    match config.transport {
        Some(EmailTransport::File) => Arc::new(FileEmailSender::new(
            config.outbox_path.clone(),
            config.from_name.clone(),
            clock,
        )),
        Some(EmailTransport::Brevo) => {
            let valid_address = config
                .from_address
                .as_deref()
                .filter(|a| is_email_address(a));
            match (&config.brevo_api_key, valid_address) {
                (Some(key), Some(address)) => {
                    match BrevoEmailSender::new(
                        BREVO_SEND_URL,
                        key.expose(),
                        address,
                        &config.from_name,
                    ) {
                        Ok(sender) => {
                            if config.daily_limit.is_none() {
                                tracing::warn!(
                                    "EMAIL_TRANSPORT=brevo without EMAIL_DAILY_LIMIT: sending is not limited by the app"
                                );
                            }
                            Arc::new(sender)
                        }
                        Err(_) => Arc::new(UnconfiguredEmailSender),
                    }
                }
                _ => {
                    tracing::warn!(
                        "EMAIL_TRANSPORT=brevo needs BREVO_API_KEY and a valid EMAIL_FROM_ADDRESS; e-mail sending is disabled"
                    );
                    Arc::new(UnconfiguredEmailSender)
                }
            }
        }
        None => Arc::new(UnconfiguredEmailSender),
    }
}

/// Escapes text for an HTML body.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// How long a reset link is valid.
pub const RESET_LINK_VALID_HOURS: i64 = 1;

pub struct ResetMessageInput<'a> {
    pub to: &'a str,
    /// Player's display name; escaped in the HTML body.
    pub name: &'a str,
    /// Raw one-time token (goes only into the link, never into logs).
    pub token: &'a str,
    /// `APP_ORIGIN`, e.g. `https://gomoku.example`.
    pub origin: &'a str,
}

pub fn build_reset_link(origin: &str, token: &str) -> String {
    format!(
        "{}/reset-password?token={}",
        origin.trim_end_matches('/'),
        crate::session::encode_component(token)
    )
}

/// The password reset message. The wording never hints at whether an account exists: only the owner of a
/// confirmed address sees it. E-mails stay in Polish.
pub fn build_reset_message(input: &ResetMessageInput<'_>) -> EmailMessage {
    let link = build_reset_link(input.origin, input.token);
    let greeting = format!("Cześć {},", input.name);
    let intro = "ktoś poprosił o ustawienie nowego hasła do konta w Gomoku.";
    let cta = "Ustaw nowe hasło";
    let validity = format!("Link jest ważny {RESET_LINK_VALID_HOURS} godzinę i działa tylko raz.");
    let ignore = "Jeśli to nie Ty prosiłeś o reset, zignoruj tę wiadomość - hasło się nie zmieni.";
    let text = [
        format!("{greeting} {intro}"),
        format!("{cta}: {link}"),
        validity.clone(),
        ignore.to_owned(),
    ]
    .join("\n\n");
    let safe_link = escape_html(&link);
    let html = [
        format!("<p>{} {intro}</p>", escape_html(&greeting)),
        format!("<p><a href=\"{safe_link}\">{cta}</a></p>"),
        format!("<p>Jeśli przycisk nie działa, skopiuj adres do przeglądarki: {safe_link}</p>"),
        format!("<p>{validity}</p>"),
        format!("<p>{ignore}</p>"),
    ]
    .join("\n");
    EmailMessage {
        to: input.to.to_owned(),
        subject: "Reset hasła w Gomoku".to_owned(),
        text,
        html,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use gomoku_db::OffsetDateTime;
    use serde_json::Value;

    use super::*;
    use crate::config::Secret;

    fn message() -> EmailMessage {
        EmailMessage {
            to: "ala@example.com".into(),
            subject: "S".into(),
            text: "T".into(),
            html: "<p>H</p>".into(),
        }
    }

    fn clock() -> Clock {
        Arc::new(|| OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap())
    }

    #[tokio::test]
    async fn the_file_transport_appends_one_json_line_per_message() {
        let dir = std::env::temp_dir().join(format!(
            "gomoku-outbox-{}",
            crate::security::new_opaque_token()
        ));
        let path = dir.join("nested").join("outbox.jsonl");
        let sender = FileEmailSender::new(path.clone(), "Gomoku".into(), clock());
        sender.send(&message(), SEND_TIMEOUT).await.unwrap();
        sender.send(&message(), SEND_TIMEOUT).await.unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = content
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["to"], "ala@example.com");
        assert_eq!(lines[0]["from"], "Gomoku");
        assert_eq!(lines[0]["at"], "2027-01-15T08:00:00Z");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_file_transport_fails_with_a_code_when_the_path_is_unusable() {
        // A file where a directory is needed.
        let blocker = std::env::temp_dir().join(format!(
            "gomoku-blocker-{}",
            crate::security::new_opaque_token()
        ));
        std::fs::write(&blocker, "x").unwrap();
        let sender = FileEmailSender::new(blocker.join("outbox.jsonl"), "Gomoku".into(), clock());
        assert_eq!(
            sender.send(&message(), SEND_TIMEOUT).await,
            Err(EmailError::new("FILE_IO"))
        );
        // A device that refuses every write.
        let full = FileEmailSender::new(PathBuf::from("/dev/full"), "Gomoku".into(), clock());
        assert_eq!(
            full.send(&message(), SEND_TIMEOUT).await,
            Err(EmailError::new("FILE_IO"))
        );
        std::fs::remove_file(blocker).unwrap();
    }

    type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

    /// A local stand-in for the Brevo API answering with `status`.
    async fn mock_brevo(status: u16) -> (String, Seen) {
        let seen: Seen = Arc::default();
        async fn handler(
            State((status, seen)): State<(u16, Seen)>,
            headers: HeaderMap,
            body: String,
        ) -> StatusCode {
            seen.lock()
                .unwrap()
                .push((headers, serde_json::from_str(&body).unwrap()));
            StatusCode::from_u16(status).unwrap()
        }
        let app = Router::new()
            .route("/send", post(handler))
            .with_state((status, Arc::clone(&seen)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/send", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    #[tokio::test]
    async fn brevo_gets_the_key_the_sender_and_both_bodies() {
        let (url, seen) = mock_brevo(201).await;
        let sender =
            BrevoEmailSender::new(&url, "secret-key", "noreply@gomoku.example", "Gomoku").unwrap();
        sender.send(&message(), SEND_TIMEOUT).await.unwrap();
        let seen = seen.lock().unwrap();
        let (headers, body) = &seen[0];
        assert_eq!(headers["api-key"], "secret-key");
        assert_eq!(body["sender"]["email"], "noreply@gomoku.example");
        assert_eq!(body["to"][0]["email"], "ala@example.com");
        assert_eq!(body["htmlContent"], "<p>H</p>");
        assert_eq!(body["textContent"], "T");
    }

    #[tokio::test]
    async fn brevo_failures_are_short_codes_without_the_key_or_the_body() {
        let (url, _) = mock_brevo(429).await;
        let sender =
            BrevoEmailSender::new(&url, "secret-key", "noreply@gomoku.example", "Gomoku").unwrap();
        let error = sender.send(&message(), SEND_TIMEOUT).await.unwrap_err();
        assert_eq!(error.code, "BREVO_HTTP_429");
        assert!(!format!("{error} {error:?}").contains("secret-key"));
        // A redirect is not followed (it would carry the key elsewhere).
        let (url, _) = mock_brevo(302).await;
        let sender = BrevoEmailSender::new(&url, "k", "noreply@gomoku.example", "G").unwrap();
        assert_eq!(
            sender
                .send(&message(), SEND_TIMEOUT)
                .await
                .unwrap_err()
                .code,
            "BREVO_HTTP_302"
        );
        let down = BrevoEmailSender::new(
            "http://127.0.0.1:1/send",
            "k",
            "noreply@gomoku.example",
            "G",
        )
        .unwrap();
        assert_eq!(
            down.send(&message(), SEND_TIMEOUT).await.unwrap_err().code,
            "BREVO_NETWORK"
        );
    }

    #[tokio::test]
    async fn a_slow_provider_is_cut_off_by_the_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/send", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let sender = BrevoEmailSender::new(&url, "k", "noreply@gomoku.example", "G").unwrap();
        let error = sender
            .send(&message(), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert_eq!(error.code, "TIMEOUT");
    }

    #[tokio::test]
    async fn the_unconfigured_sender_always_fails() {
        assert_eq!(
            UnconfiguredEmailSender.send(&message(), SEND_TIMEOUT).await,
            Err(EmailError::new("UNCONFIGURED"))
        );
    }

    fn config(
        transport: Option<EmailTransport>,
        key: Option<&str>,
        address: Option<&str>,
    ) -> EmailConfig {
        EmailConfig {
            transport,
            outbox_path: std::env::temp_dir().join("gomoku-factory-outbox.jsonl"),
            from_name: "Gomoku".into(),
            from_address: address.map(str::to_owned),
            brevo_api_key: key.map(Secret::new),
            daily_limit: None,
        }
    }

    #[tokio::test]
    async fn the_factory_picks_the_transport_and_degrades_to_the_failing_sender() {
        let unconfigured = |sender: Arc<dyn EmailSender>| async move {
            sender
                .send(&message(), SEND_TIMEOUT)
                .await
                .map_err(|e| e.code)
        };
        assert_eq!(
            unconfigured(create_email_sender(&config(None, None, None), clock())).await,
            Err("UNCONFIGURED".into())
        );
        for (key, address) in [
            (None, Some("a@b.pl")),
            (Some("k"), None),
            (Some("k"), Some("not-an-address")),
        ] {
            let sender =
                create_email_sender(&config(Some(EmailTransport::Brevo), key, address), clock());
            assert_eq!(
                unconfigured(sender).await,
                Err("UNCONFIGURED".into()),
                "{key:?} {address:?}"
            );
        }
        let file = create_email_sender(&config(Some(EmailTransport::File), None, None), clock());
        assert!(file.send(&message(), SEND_TIMEOUT).await.is_ok());
        let _ = std::fs::remove_file(std::env::temp_dir().join("gomoku-factory-outbox.jsonl"));
        // A configured Brevo sender is a real one (it fails on the network here, not as unconfigured).
        let mut brevo = config(
            Some(EmailTransport::Brevo),
            Some("k"),
            Some("noreply@gomoku.example"),
        );
        brevo.daily_limit = Some(100);
        assert!(
            create_email_sender(&brevo, clock())
                .send(&message(), Duration::from_millis(1))
                .await
                .is_err()
        );
        brevo.daily_limit = None;
        let _ = create_email_sender(&brevo, clock());
    }

    #[test]
    fn address_check_accepts_plain_addresses_only() {
        for good in ["a@b.pl", "noreply@mail.gomoku.example"] {
            assert!(is_email_address(good), "{good}");
        }
        for bad in [
            "", "a", "a@", "@b.pl", "a@b", "a b@c.pl", "a@b.", "a@.pl", "a@b@c.pl", "<a>@b.pl",
        ] {
            assert!(!is_email_address(bad), "{bad}");
        }
    }

    #[test]
    fn the_reset_message_has_the_link_in_both_bodies_and_escapes_the_name() {
        let message = build_reset_message(&ResetMessageInput {
            to: "ala@example.com",
            name: "<b>Ala</b>",
            token: "a b/c",
            origin: "https://gomoku.example/",
        });
        assert_eq!(message.to, "ala@example.com");
        assert_eq!(message.subject, "Reset hasła w Gomoku");
        let link = "https://gomoku.example/reset-password?token=a%20b%2Fc";
        assert!(message.text.contains(link) && message.html.contains(link));
        assert!(message.text.contains("ważny 1 godzinę"));
        assert!(
            message.html.contains("&lt;b&gt;Ala&lt;/b&gt;") && !message.html.contains("<b>Ala</b>")
        );
        assert_eq!(
            escape_html(r#"<a href="x">&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }
}
