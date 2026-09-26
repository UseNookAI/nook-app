//! The local OpenAI-compatible gateway on 127.0.0.1. Ports `ai.nook.agent.gateway`
//! (GatewayController, GatewayAuthFilter, GatewayInfo, EnvelopeStrip, VideoController) and the
//! gateway half of the original's Spring web setup (`application.yml`: loopback only, the port
//! from `GatewayPort.choose()`).
//!
//! # Starting and stopping it
//!
//! The original was Spring's embedded web server, started with the app and stopped with it. Here
//! the app does it itself:
//!
//! 1. In `Nook::start`, after `runtime.start().await`:
//!    `let gateway = Gateway::start(runtime.clone(), studio.clone(), &home).await?;` binds
//!    127.0.0.1 on [`gateway_port::choose`](crate::gateway_port::choose) (41434 when free, else
//!    any free port, `NOOK_RS_GATEWAY_PORT` pins one), generates the per-start token and writes
//!    `gateway.json` ([`GatewayInfo`]). A bind failure is an error the app logs; Nook works
//!    without its gateway (the UI calls the services directly).
//! 2. Keep the [`GatewayHandle`]: `port()`, `token()`, `base_url()`.
//! 3. In `Nook::shutdown`, before the studio and the runtime shut down:
//!    `gateway.stop().await` stops accepting, ends the `/runtime/events` streams, waits a few
//!    seconds for requests in flight and removes `gateway.json` (when it still holds this start's
//!    token). Dropping the handle does the same without waiting.
//!
//! # The surface
//!
//! Every route but `/health` needs `Authorization: Bearer <token>` (or `X-Api-Key: <token>`).
//!
//! | Route | What |
//! |---|---|
//! | `GET /health` | `{"status":"ok","readiness":...}`, open |
//! | `GET /v1/models` | installed models, OpenAI's list shape plus task, size, residency |
//! | `POST /v1/chat/completions` | forwarded to the model's engine (loaded when needed), SSE passed through; the chat thinking policy applied and the empty `<think></think>` envelope stripped |
//! | `POST /v1/completions`, `POST /v1/embeddings` | forwarded as they are |
//! | `POST /v1/audio/transcriptions` | multipart `file` (+ `model`) → `{"text"}` |
//! | `POST /v1/images/generations` | `{"prompt","model","size","steps","negative_prompt"}` → `b64_json` |
//! | `POST /v1/videos`, `GET /v1/videos[/{id}[/content]]`, `POST /v1/videos/{id}/cancel`, `DELETE /v1/videos/{id}` | OpenAI's Videos API shape over [`VideoStudio`] |
//! | `GET /runtime/status`, `/runtime/catalog`, `/runtime/downloads`, `/runtime/events/recent` | the runtime's state |
//! | `POST /runtime/models/{id}/load`, `unload`, `download`, `pin?pinned=`; `DELETE /runtime/models/{id}` | runtime control |
//! | `GET /runtime/events` | server-sent runtime events (`event:<kind>`, `data:<json>`) |
//!
//! `X-Nook-Priority: background` queues a request behind interactive ones on the same engine.
//! Errors are OpenAI's shape, `{"error":{"message","type"}}`, with `type` `server_error` for 5xx
//! and `invalid_request_error` otherwise.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runtime::RuntimeManager;
use crate::video::VideoStudio;
use crate::Home;

pub mod auth;
pub mod controller;
pub mod envelope_strip;
pub mod info;
mod multipart;
pub mod video_controller;

pub use info::GatewayInfo;

/// The largest request body read. Spring read a JSON body whatever its size; this cap only keeps
/// a runaway client from filling memory (a chat with images, or an hour of 16 kHz audio, is far
/// below it).
pub const BODY_LIMIT: usize = 256 << 20;
/// How long stopping waits for requests in flight.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Starts the gateway; see the module docs.
pub struct Gateway;

impl Gateway {
    /// Starts the gateway on [`gateway_port::choose`](crate::gateway_port::choose)'s port.
    pub async fn start(
        runtime: Arc<RuntimeManager>,
        studio: Arc<VideoStudio>,
        home: &Home,
    ) -> Result<GatewayHandle> {
        Gateway::start_on(crate::gateway_port::choose(), runtime, studio, home).await
    }

    /// Starts the gateway on 127.0.0.1:`port` (0 for any free port, as the tests do) and writes
    /// `gateway.json` under the home.
    pub async fn start_on(
        port: u16,
        runtime: Arc<RuntimeManager>,
        studio: Arc<VideoStudio>,
        home: &Home,
    ) -> Result<GatewayHandle> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| format!("Could not start the local gateway on 127.0.0.1:{port}"))?;
        let port = listener.local_addr()?.port();
        let info = Arc::new(GatewayInfo::new(home.gateway_file(), port));
        let shutdown = CancellationToken::new();
        let app = router(runtime, studio, info.clone(), shutdown.clone());
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
            {
                tracing::warn!("The local gateway stopped: {e}");
            }
        });
        info.write_file();
        Ok(GatewayHandle {
            info,
            shutdown,
            task: Mutex::new(Some(task)),
        })
    }
}

/// The running gateway. [`GatewayHandle::stop`] it before exit.
pub struct GatewayHandle {
    info: Arc<GatewayInfo>,
    shutdown: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl GatewayHandle {
    pub fn port(&self) -> u16 {
        self.info.port()
    }
    pub fn token(&self) -> &str {
        self.info.token()
    }
    /// `http://127.0.0.1:<port>`; OpenAI clients take `<base_url>/v1`.
    pub fn base_url(&self) -> String {
        self.info.base_url()
    }
    pub fn info(&self) -> &Arc<GatewayInfo> {
        &self.info
    }

    /// Stops accepting requests, ends the event streams, waits up to five seconds for requests in
    /// flight, and removes `gateway.json`.
    pub async fn stop(&self) {
        self.shutdown.cancel();
        let task = self.task.lock().take();
        if let Some(mut task) = task {
            if tokio::time::timeout(STOP_WAIT, &mut task).await.is_err() {
                task.abort();
            }
        }
        self.info.remove_file();
    }
}

impl Drop for GatewayHandle {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.info.remove_file();
    }
}

/// The whole surface behind the token check: the runtime's routes, the video routes, and
/// Spring's answers for a route or method that does not exist.
pub fn router(
    runtime: Arc<RuntimeManager>,
    studio: Arc<VideoStudio>,
    info: Arc<GatewayInfo>,
    shutdown: CancellationToken,
) -> Router {
    controller::router(runtime, shutdown)
        .merge(video_controller::router(studio))
        .fallback(|uri: Uri| async move { spring_error(StatusCode::NOT_FOUND, uri.path()) })
        .method_not_allowed_fallback(|uri: Uri| async move {
            spring_error(StatusCode::METHOD_NOT_ALLOWED, uri.path())
        })
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(axum::middleware::from_fn_with_state(
            info,
            auth::require_token,
        ))
}

// ------------------------------------------------------------------ shared by the controllers

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// The gateway's error answer, OpenAI's shape: `{"error":{"message","type"}}`.
pub(crate) fn error(status: StatusCode, message: impl Into<Option<String>>) -> Response {
    let message = message
        .into()
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("").to_string());
    let kind = if status.as_u16() >= 500 {
        "server_error"
    } else {
        "invalid_request_error"
    };
    json_response(
        status,
        &ErrorBody {
            error: ErrorDetail { message, kind },
        },
    )
}

/// The catch-all for a failure no route expected (`@ExceptionHandler(Exception.class)`): logged,
/// then a 500.
pub(crate) fn server_error(e: &anyhow::Error) -> Response {
    tracing::warn!("Gateway request failed: {e:#}");
    error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

/// A JSON body with a status.
pub(crate) fn json_response(status: StatusCode, body: &impl Serialize) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// What Spring Boot answered for a request no handler took (no such route, method or media
/// type): `{"timestamp","status","error","path"}`.
pub(crate) fn spring_error(status: StatusCode, path: &str) -> Response {
    #[derive(Serialize)]
    struct SpringError<'a> {
        timestamp: String,
        status: u16,
        error: &'a str,
        path: &'a str,
    }
    json_response(
        status,
        &SpringError {
            timestamp: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3f+00:00")
                .to_string(),
            status: status.as_u16(),
            error: status.canonical_reason().unwrap_or(""),
            path,
        },
    )
}

/// Whether a request's body is JSON (`consumes = APPLICATION_JSON_VALUE`): `application/json`
/// with any parameters. Spring answered 415 otherwise, a missing Content-Type included.
pub(crate) fn is_json(headers: &HeaderMap) -> bool {
    media_type(headers).is_some_and(|t| t.eq_ignore_ascii_case("application/json"))
}

/// The request's media type without its parameters.
pub(crate) fn media_type(headers: &HeaderMap) -> Option<String> {
    let ct = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    Some(ct.split(';').next().unwrap_or("").trim().to_string())
}

/// Reads a raw JSON body as Jackson's `readTree` did. A body that is not JSON, or no body at
/// all, is the caller's mistake: 400, not the 500 the catch-all gave it (QA 2026-09-23, QA-06).
/// Only whitespace reads as nothing (Jackson's missing node), so the route's own check answers.
// The error is the answer itself, returned at once: not worth a box.
#[allow(clippy::result_large_err)]
pub(crate) fn read_json(body: &[u8]) -> std::result::Result<Value, Response> {
    if body.is_empty() {
        return Err(unreadable("the request has no readable body".into()));
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|e| unreadable(e.to_string()))
}

fn unreadable(why: String) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        format!("The request body is not valid JSON: {why}"),
    )
}

/// Jackson's `path(key).asText(null)`.
pub(crate) fn text_of(json: &Value, key: &str) -> Option<String> {
    crate::video::studio::text(json.get(key))
}

/// Jackson's `path(key).asBoolean(false)`: true, a non-zero number, or the text "true".
pub(crate) fn bool_of(json: &Value, key: &str) -> bool {
    match json.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => s.trim() == "true",
        _ => false,
    }
}

/// Runs work on its own task, so a client that goes away does not cut a model load or a render
/// off halfway (the servlet thread finished its work whether anyone was still listening).
#[allow(clippy::result_large_err)]
pub(crate) async fn detached<T: Send + 'static>(
    work: impl std::future::Future<Output = T> + Send + 'static,
) -> std::result::Result<T, Response> {
    tokio::spawn(work).await.map_err(|e| {
        server_error(&anyhow::anyhow!(
            "The request's work stopped unexpectedly: {e}"
        ))
    })
}

#[cfg(test)]
mod tests;
