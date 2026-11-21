//! The JSON API envelope, errors, the CSRF check and the request body reader.

use std::collections::BTreeMap;

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use gomoku_core::auth::FieldIssue;
use serde::Serialize;
use serde_json::{Value, json};

use crate::app::AppState;

/// Header every state-changing request must carry (set by the form script); a second CSRF layer.
pub const CSRF_HEADER: &str = "x-gomoku-request";
/// Largest accepted request body of the JSON API; the biggest legitimate one is a chat message.
pub const API_BODY_MAX_BYTES: usize = 64 * 1024;
/// Never from a cache: the answers are about one session or one game.
const NO_STORE: &str = "private, no-store";

/// An error answered as `{ ok: false, error: { code, message, fieldErrors?, fieldIssues?, data? } }`.
#[derive(Debug, Clone)]
pub struct ApiError(Box<Inner>);

#[derive(Debug, Clone)]
struct Inner {
    status: StatusCode,
    code: String,
    message: String,
    field_errors: BTreeMap<String, Vec<String>>,
    field_issues: BTreeMap<String, Vec<Value>>,
    data: Option<Value>,
    retry_after_seconds: Option<u64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self(Box::new(Inner {
            status,
            code: code.to_owned(),
            message: message.to_owned(),
            field_errors: BTreeMap::new(),
            field_issues: BTreeMap::new(),
            data: None,
            retry_after_seconds: None,
        }))
    }

    /// Replaces the machine-readable code of the error.
    pub fn with_code(mut self, code: &str) -> Self {
        code.clone_into(&mut self.0.code);
        self
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.0.data = Some(data);
        self
    }

    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.0.retry_after_seconds = Some(seconds);
        self
    }

    /// Adds an error for one field: a message plus its machine-readable code.
    pub fn with_field(mut self, field: &str, issue_code: &str, message: &str) -> Self {
        self.0
            .field_errors
            .entry(field.to_owned())
            .or_default()
            .push(message.to_owned());
        self.0
            .field_issues
            .entry(field.to_owned())
            .or_default()
            .push(json!({ "code": issue_code }));
        self
    }

    pub fn validation(message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "VALIDATION", message)
    }

    /// A validation error carrying the issues found per field.
    pub fn invalid_fields(fields: Vec<(&str, Vec<FieldIssue>)>) -> Self {
        let mut error = Self::validation("Nieprawidłowe dane.");
        for (field, issues) in fields {
            for issue in issues {
                error = error.with_field(field, issue.code(), issue_message(issue));
            }
        }
        error
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "Zaloguj się, aby kontynuować.",
        )
        .with_data(json!({ "reason": "session_required" }))
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "FORBIDDEN", "Żądanie odrzucone.")
            .with_data(json!({ "reason": "request_rejected" }))
    }

    pub fn conflict(code: &str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn too_many_requests(retry_after_seconds: u64) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
            "Zbyt wiele prób. Spróbuj ponownie później.",
        )
        .with_data(json!({ "reason": "attempts" }))
        .with_retry_after(retry_after_seconds)
    }

    pub fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "PAYLOAD_TOO_LARGE",
            "Żądanie jest zbyt duże.",
        )
    }

    /// A generic `500`; the cause is logged by the caller and never leaked.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "Wystąpił nieoczekiwany błąd.",
        )
    }

    /// Logs the cause (name only: the message of a database error may contain the query with personal data)
    /// and answers a generic `500`.
    pub fn internal_logged(context: &str, cause: &dyn std::fmt::Debug) -> Self {
        tracing::error!(context, cause = ?cause, "unhandled API error");
        Self::internal()
    }

    fn body(&self) -> Value {
        let inner = &self.0;
        let mut error = json!({ "code": inner.code, "message": inner.message });
        if !inner.field_errors.is_empty() {
            error["fieldErrors"] = json!(inner.field_errors);
            error["fieldIssues"] = json!(inner.field_issues);
        }
        if let Some(data) = &inner.data {
            error["data"] = data.clone();
        }
        json!({ "ok": false, "error": error })
    }
}

/// Polish text of a field issue (the stable fallback; the client translates by code).
pub fn issue_message(issue: FieldIssue) -> &'static str {
    match issue {
        FieldIssue::EmailRequired => "Podaj adres e-mail.",
        FieldIssue::EmailTooLong => "Adres e-mail może mieć najwyżej 254 znaków.",
        FieldIssue::EmailInvalid => "Podaj poprawny adres e-mail.",
        FieldIssue::DisplayNameRequired => "Podaj nazwę gracza.",
        FieldIssue::DisplayNameTooShort => "Nazwa gracza musi mieć co najmniej 3 znaki.",
        FieldIssue::DisplayNameTooLong => "Nazwa gracza może mieć najwyżej 24 znaków.",
        FieldIssue::DisplayNameInvalid => {
            "Nazwa gracza może zawierać tylko litery, cyfry oraz znaki _ i - (bez spacji)."
        }
        FieldIssue::DisplayNameReserved => "Ta nazwa jest zastrzeżona.",
        FieldIssue::PasswordRequired => "Podaj hasło.",
        FieldIssue::PasswordTooShort => "Hasło musi mieć co najmniej 8 znaków.",
        FieldIssue::PasswordTooLong => "Hasło może mieć najwyżej 72 bajtów.",
        FieldIssue::LoginPasswordTooLong => "Hasło może mieć najwyżej 1024 znaków.",
        FieldIssue::CurrentPasswordRequired => "Podaj obecne hasło.",
        FieldIssue::PasswordUnchanged => "Nowe hasło musi się różnić od obecnego.",
        FieldIssue::TokenRequired => "Podaj token z linku.",
        FieldIssue::TokenTooLong => "Token jest za długi.",
    }
}

fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry_after = self.0.retry_after_seconds;
        let mut response = (self.0.status, axum::Json(self.body())).into_response();
        if let Some(value) =
            retry_after.and_then(|seconds| HeaderValue::from_str(&seconds.to_string()).ok())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        with_no_store(response)
    }
}

/// A successful answer: `{ ok: true, data }` with the given status.
pub fn ok<T: Serialize>(status: StatusCode, data: T) -> Response {
    with_no_store((status, axum::Json(json!({ "ok": true, "data": data }))).into_response())
}

fn is_state_changing(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// CSRF protection: the `Origin` header is mandatory and must equal `APP_ORIGIN` exactly (without `APP_ORIGIN`,
/// outside production: `http://localhost:<port>`), and the custom `X-Gomoku-Request` header must be present.
/// `Host` is deliberately not used: a proxy may rewrite it.
pub fn is_same_origin(request_headers: &axum::http::HeaderMap, app_origin: Option<&str>) -> bool {
    let origin = request_headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let allowed = match (app_origin, origin) {
        (Some(expected), Some(actual)) => expected == actual,
        (None, Some(actual)) => actual
            .strip_prefix("http://localhost:")
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())),
        (_, None) => false,
    };
    allowed && request_headers.contains_key(CSRF_HEADER)
}

/// Axum middleware for `/api`: rejects state-changing requests that fail [`is_same_origin`].
pub async fn csrf(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if is_state_changing(request.method())
        && !is_same_origin(request.headers(), state.config.app_origin.as_deref())
    {
        tracing::warn!(
            event = "csrf_rejected",
            "request rejected by the CSRF check"
        );
        return ApiError::forbidden().into_response();
    }
    next.run(request).await
}

/// Reads and parses a JSON request body: a body over the limit is a `413` (by the declared length and by
/// counting the bytes), anything unparsable a `400`, never a `500`.
pub async fn read_json(body: Body, headers: &axum::http::HeaderMap) -> Result<Value, ApiError> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|length| length > API_BODY_MAX_BYTES) {
        return Err(ApiError::payload_too_large());
    }
    let bytes = to_bytes(body, API_BODY_MAX_BYTES)
        .await
        .map_err(|_| ApiError::payload_too_large())?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::validation("Nieprawidłowe dane żądania."))
}

/// A string field of a JSON object body; `None` when absent or not a string.
pub fn string_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key)?.as_str()
}
