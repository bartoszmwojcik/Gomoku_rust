//! The transport: the guard layer and the `rmcp` handler that carries the tools.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorData, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;

use super::tools::AgentTools;
use crate::app::AppState;
use crate::client_address::client_key;
use gomoku_core::agent::{MCP_BODY_MAX_BYTES, MCP_SERVER_NAME, MCP_SERVER_VERSION};

const TOOLS_JSON: &str = include_str!("tools.json");

/// The four tools exactly as published (descriptions and schemas are part of the contract).
pub fn tool_list() -> Vec<Tool> {
    serde_json::from_str(TOOLS_JSON).unwrap_or_default()
}

#[derive(Clone)]
struct GomokuServer {
    tools: AgentTools,
    listing: Arc<Vec<Tool>>,
}

impl ServerHandler for GomokuServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(MCP_SERVER_NAME, MCP_SERVER_VERSION))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.listing.as_ref().clone(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let headers = context
            .extensions
            .get::<axum::http::request::Parts>()
            .map(|parts| parts.headers.clone())
            .unwrap_or_default();
        let args = request.arguments.unwrap_or_default();
        let Some(outcome) = self
            .tools
            .call(&request.name, &args, &headers, &context.ct)
            .await
        else {
            return Err(ErrorData::invalid_params(
                format!("Tool {} not found", request.name),
                None,
            ));
        };
        let result = match outcome {
            Ok(data) => CallToolResult::structured(data),
            Err(failure) => CallToolResult::structured_error(failure.error_json()),
        };
        Ok(CallToolResponse::Complete(result))
    }
}

/// A refusal before the protocol layer: JSON-RPC shaped, `id: null`, `data.code` is the machine reason.
fn refusal(status: StatusCode, message: &str, code: &str) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": -32000, "message": message, "data": { "code": code } },
    });
    (status, axum::Json(body)).into_response()
}

/// `http://localhost:<port>`, the only origin accepted when `APP_ORIGIN` is not configured (development).
fn is_local_origin(origin: &str) -> bool {
    origin
        .strip_prefix("http://localhost:")
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

fn origin_allowed(origin: Option<&str>, app_origin: Option<&str>) -> bool {
    match (origin, app_origin) {
        (None, _) => true,
        (Some(actual), Some(expected)) => actual == expected,
        (Some(actual), None) => is_local_origin(actual),
    }
}

/// The checks of the endpoint, in order: method, origin, the per-address request limit.
async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        let mut response = refusal(
            StatusCode::METHOD_NOT_ALLOWED,
            "Metoda niedozwolona. Użyj POST.",
            "METHOD_NOT_ALLOWED",
        );
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or("\u{0}"));
    if !origin_allowed(origin, state.config.app_origin.as_deref()) {
        return refusal(StatusCode::FORBIDDEN, "Żądanie odrzucone.", "FORBIDDEN");
    }
    let Ok(key) = client_key(request.headers(), "mcp", &state.config) else {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "Nie można ustalić adresu klienta.",
            "ADDRESS_UNAVAILABLE",
        );
    };
    match state.db.rate_limit_hit(&key, 60_000).await {
        Ok(hit) if i64::from(hit.count) > i64::from(state.config.mcp.request_limit) => {
            let mut response = refusal(
                StatusCode::TOO_MANY_REQUESTS,
                "Zbyt wiele żądań. Spróbuj ponownie później.",
                "RATE_LIMITED",
            );
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(u64::try_from(hit.retry_after_seconds).unwrap_or(1)),
            );
            return response;
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(event = "mcp_internal", context = "request limit", cause = ?error, "mcp guard failed");
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Wystąpił nieoczekiwany błąd.",
                "INTERNAL",
            );
        }
    }
    next.run(request).await
}

/// The router of `/api/mcp` (mounted outside `/api`: no cookies, no CSRF header).
pub fn mcp_router(state: &AppState) -> Router<AppState> {
    let server = GomokuServer {
        tools: AgentTools::new(state.clone()),
        listing: Arc::new(tool_list()),
    };
    // Stateless: no session id is issued or read; answers are JSON. The host check is meaningless behind a proxy (the
    // guard validates `Origin` itself).
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(MCP_BODY_MAX_BYTES)
        .disable_allowed_hosts();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service("/api/mcp", service)
        .route_layer(from_fn_with_state(state.clone(), guard))
}
