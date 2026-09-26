//! Ports `gateway/GatewayAuthFilter.java`.
//!
//! Every gateway request must carry the per-start bearer token from gateway.json. The health
//! endpoint is open so a caller can tell "gateway up, wrong token" from "gateway down".

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::info::GatewayInfo;

/// The 401 body, as the original wrote it.
pub const UNAUTHORIZED_BODY: &str = "{\"error\":{\"message\":\"Missing or invalid gateway token. Read it from gateway.json in the Nook home directory.\",\"type\":\"unauthorized\"}}";

/// The token check in front of every route (a Spring `OncePerRequestFilter`). The token comes
/// from `Authorization` (`Bearer <token>` or the token alone) or, when that header is absent,
/// from `X-Api-Key`.
pub async fn require_token(
    State(info): State<Arc<GatewayInfo>>,
    request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/health" {
        return next.run(request).await;
    }
    let headers = request.headers();
    let presented = match headers.get(header::AUTHORIZATION) {
        Some(v) => v.to_str().ok(),
        None => headers.get("x-api-key").and_then(|v| v.to_str().ok()),
    };
    if !info.matches(presented) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(UNAUTHORIZED_BODY),
        )
            .into_response();
    }
    next.run(request).await
}
