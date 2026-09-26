//! Ports `gateway/GatewayController.java`.
//!
//! The local gateway: an OpenAI-compatible surface plus runtime control, bound to loopback and
//! protected by the per-start token. Inference requests are forwarded to the engine that serves
//! the requested model, loading it first when needed.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    bool_of, detached, envelope_strip, error, is_json, json_response, multipart, read_json,
    server_error, spring_error, text_of,
};
use crate::events::{self, topic};
use crate::runtime::api::Priority;
use crate::runtime::engine_process::State as EngineState;
use crate::runtime::{thinking, RuntimeManager};

/// The runtime behind the routes, and the gateway's stop signal for the event streams.
#[derive(Clone)]
pub(crate) struct Api {
    runtime: Arc<RuntimeManager>,
    shutdown: CancellationToken,
}

/// The OpenAI-compatible and runtime routes.
pub fn router(runtime: Arc<RuntimeManager>, shutdown: CancellationToken) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/images/generations", post(image_generations))
        .route("/runtime/status", get(status))
        .route("/runtime/catalog", get(catalog))
        .route("/runtime/models/{id}/load", post(load))
        .route("/runtime/models/{id}/download", post(download))
        .route("/runtime/downloads", get(downloads))
        .route("/runtime/models/{id}", delete(delete_model))
        .route("/runtime/models/{id}/unload", post(unload))
        .route("/runtime/models/{id}/pin", post(pin))
        .route("/runtime/events/recent", get(recent_events))
        .route("/runtime/events", get(runtime_events))
        .with_state(Api { runtime, shutdown })
}

// ------------------------------------------------------------------ open

async fn health(State(api): State<Api>) -> Response {
    Json(serde_json::json!({"status": "ok", "readiness": api.runtime.readiness()})).into_response()
}

// ------------------------------------------------------------------ OpenAI-compatible

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: &'static str,
    owned_by: &'static str,
    task: String,
    display_name: String,
    bytes: u64,
    resident: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpu_layers: Option<i32>,
}

async fn models(State(api): State<Api>) -> Response {
    let data = api
        .runtime
        .registry()
        .list()
        .into_iter()
        .map(|m| {
            let engine = api.runtime.engine(&m.id);
            ModelEntry {
                resident: engine
                    .as_ref()
                    .is_some_and(|e| e.state() == EngineState::Ready),
                gpu_layers: engine.map(|e| e.plan().gpu_layers),
                id: m.id,
                object: "model",
                owned_by: "nook",
                task: m.task,
                display_name: m.display_name,
                bytes: m.bytes,
            }
        })
        .collect();
    json_response(
        StatusCode::OK,
        &ModelList {
            object: "list",
            data,
        },
    )
}

async fn chat_completions(
    State(api): State<Api>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_json(&headers) {
        return spring_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, uri.path());
    }
    let mut json = match read_json(&body) {
        Ok(json) => json,
        Err(bad) => return bad,
    };
    let Some(model) = text_of(&json, "model").filter(|m| !m.trim().is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "The request must name a model.".to_string(),
        );
    };
    let stream = bool_of(&json, "stream");
    let mut body = String::from_utf8_lossy(&body).into_owned();
    // A client that set chat_template_kwargs or reasoning_effort decided for itself; the rest get
    // the app's chat policy (Settings › General, workers.json "thinking"), off by default. Either
    // way the empty envelope a thinking model's template leaves is kept out of the reply.
    let switchable = json.is_object() && thinking::switchable(&model);
    if switchable {
        let think = thinking::chat_thinks(&api.runtime.registry().worker_preferences());
        if thinking::apply(&mut json, &model, think) {
            body = json.to_string();
        }
    }
    forward(
        &api,
        "/v1/chat/completions",
        model,
        body,
        stream,
        priority_of(&headers),
        switchable,
    )
    .await
}

/// `X-Nook-Priority: background` queues behind interactive requests; anything else is
/// interactive.
fn priority_of(headers: &HeaderMap) -> Priority {
    let background = headers
        .get("x-nook-priority")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.trim().eq_ignore_ascii_case("background"));
    if background {
        Priority::Background
    } else {
        Priority::Interactive
    }
}

async fn completions(
    State(api): State<Api>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    plain_forward(api, uri, headers, body, "/v1/completions", true).await
}

async fn embeddings(State(api): State<Api>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    plain_forward(api, uri, headers, body, "/v1/embeddings", false).await
}

/// Completions and embeddings: the body goes to the engine as it came.
async fn plain_forward(
    api: Api,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    path: &'static str,
    may_stream: bool,
) -> Response {
    if !is_json(&headers) {
        return spring_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, uri.path());
    }
    let json = match read_json(&body) {
        Ok(json) => json,
        Err(bad) => return bad,
    };
    let Some(model) = text_of(&json, "model") else {
        return error(
            StatusCode::BAD_REQUEST,
            "The request must name a model.".to_string(),
        );
    };
    let stream = may_stream && bool_of(&json, "stream");
    let body = String::from_utf8_lossy(&body).into_owned();
    forward(
        &api,
        path,
        model,
        body,
        stream,
        priority_of(&headers),
        false,
    )
    .await
}

/// Takes a lease on the model's engine (loading it when needed), forwards the body, and streams
/// the engine's answer back with its status and content type. The lease is held until the answer
/// has been passed on, or the client goes away. `strip_envelope` passes a chat reply through
/// [`envelope_strip`], whole or event by event.
async fn forward(
    api: &Api,
    path: &'static str,
    model: String,
    body: String,
    stream: bool,
    priority: Priority,
    strip_envelope: bool,
) -> Response {
    let runtime = api.runtime.clone();
    let lease = match detached(async move { runtime.acquire(&model, priority).await }).await {
        Ok(Ok(lease)) => lease,
        Ok(Err(e)) => return error(StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")),
        Err(failed) => return failed,
    };
    let upstream = match lease.client().forward(path, body, stream).await {
        Ok(r) => r,
        Err(e) => {
            drop(lease);
            return error(
                StatusCode::BAD_GATEWAY,
                format!("Engine request failed: {e:#}"),
            );
        }
    };
    let status = upstream.status().as_u16();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));
    let strip = strip_envelope && status / 100 == 2;
    let body = if strip && stream {
        let events = envelope_strip::stream(upstream.bytes_stream());
        Body::from_stream(events.map(move |chunk| {
            let _held = &lease;
            chunk
        }))
    } else if strip {
        Body::from_stream(futures::stream::once(async move {
            let _held = lease;
            let whole = upstream.bytes().await?;
            Ok::<_, reqwest::Error>(Bytes::from(envelope_strip::whole(&whole).into_owned()))
        }))
    } else {
        Body::from_stream(upstream.bytes_stream().map(move |chunk| {
            let _held = &lease;
            chunk
        }))
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|e| server_error(&anyhow::anyhow!(e)))
}

// ------------------------------------------------------------------ speech and images

async fn transcriptions(
    State(api): State<Api>,
    uri: Uri,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let boundary = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .and_then(multipart::boundary);
    let Some(boundary) = boundary else {
        return spring_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, uri.path());
    };
    let parts = multipart::parse(&body, &boundary).unwrap_or_default();
    let Some(file) = parts.iter().find(|p| p.name == "file") else {
        return error(
            StatusCode::BAD_REQUEST,
            "Required part 'file' is not present.".to_string(),
        );
    };
    // A request parameter: the query string first, then the form's field.
    let model = query.get("model").cloned().or_else(|| {
        parts
            .iter()
            .find(|p| p.name == "model" && p.filename.is_none())
            .map(multipart::Part::text)
    });
    let audio = file.data.clone();
    let runtime = api.runtime.clone();
    let result = detached(async move {
        let dir = runtime.config().home.temp_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join(format!("gw-{}.wav", uuid::Uuid::new_v4().simple()));
        tokio::fs::write(&tmp, &audio).await?;
        let text = runtime.transcribe(&tmp, model.as_deref()).await;
        let _ = tokio::fs::remove_file(&tmp).await;
        text
    })
    .await;
    match result {
        Ok(Ok(text)) => Json(serde_json::json!({ "text": text })).into_response(),
        Ok(Err(e)) => error(StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")),
        Err(failed) => failed,
    }
}

#[derive(Serialize)]
struct ImageList {
    created: i64,
    data: Vec<ImageItem>,
}

#[derive(Serialize)]
struct ImageItem {
    b64_json: String,
    file: String,
    seed: u64,
    elapsed_ms: u64,
}

async fn image_generations(
    State(api): State<Api>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !is_json(&headers) {
        return spring_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, uri.path());
    }
    let json = match read_json(&body) {
        Ok(json) => json,
        Err(bad) => return bad,
    };
    let Some(prompt) = text_of(&json, "prompt").filter(|p| !p.trim().is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "The request must include a prompt.".to_string(),
        );
    };
    let (width, height) = text_of(&json, "size")
        .and_then(|s| parse_size(&s))
        .map_or((None, None), |(w, h)| (Some(w), Some(h)));
    let steps = json
        .get("steps")
        .map(int_of)
        .and_then(|s| u32::try_from(s).ok());
    let model = text_of(&json, "model");
    let negative = text_of(&json, "negative_prompt");
    let runtime = api.runtime.clone();
    let result = detached(async move {
        let result = runtime
            .generate_image(
                model.as_deref(),
                &prompt,
                negative.as_deref(),
                width,
                height,
                steps,
            )
            .await?;
        let png = tokio::fs::read(&result.file).await?;
        anyhow::Ok((result, png))
    })
    .await;
    match result {
        Ok(Ok((result, png))) => json_response(
            StatusCode::OK,
            &ImageList {
                created: chrono::Utc::now().timestamp(),
                data: vec![ImageItem {
                    b64_json: base64::engine::general_purpose::STANDARD.encode(png),
                    file: result.file.display().to_string(),
                    seed: result.seed,
                    elapsed_ms: result.elapsed_ms,
                }],
            },
        ),
        Ok(Err(e)) => error(StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")),
        Err(failed) => failed,
    }
}

/// `"<w>x<h>"` in digits, as the original's `\d+x\d+`.
fn parse_size(size: &str) -> Option<(u32, u32)> {
    let (w, h) = size.split_once('x')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(w) || !digits(h) {
        return None;
    }
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Jackson's `asInt()`: a number, or text that reads as one; 0 otherwise.
fn int_of(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        Value::Bool(true) => 1,
        _ => 0,
    }
}

// ------------------------------------------------------------------ runtime control

async fn status(State(api): State<Api>) -> Response {
    json_response(StatusCode::OK, &api.runtime.status().await)
}

async fn catalog(State(api): State<Api>) -> Response {
    json_response(StatusCode::OK, &api.runtime.catalog().all())
}

async fn load(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let runtime = api.runtime.clone();
    let model = id.clone();
    match detached(async move { runtime.load(&model).await.map(|_| ()) }).await {
        Ok(Ok(())) => Json(serde_json::json!({ "loaded": id })).into_response(),
        Ok(Err(e)) => error(StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")),
        Err(failed) => failed,
    }
}

#[derive(Serialize)]
struct DownloadAnswer<'a> {
    model: &'a str,
    state: &'static str,
}

async fn download(State(api): State<Api>, Path(id): Path<String>) -> Response {
    if api.runtime.registry().is_installed(&id) {
        return json_response(
            StatusCode::OK,
            &DownloadAnswer {
                model: &id,
                state: "installed",
            },
        );
    }
    if api.runtime.catalog().find(&id).is_none() {
        return error(StatusCode::NOT_FOUND, format!("Unknown catalog model {id}"));
    }
    match api.runtime.download_async(&id) {
        Ok(started) => json_response(
            StatusCode::ACCEPTED,
            &DownloadAnswer {
                model: &id,
                state: if started {
                    "started"
                } else {
                    "already_downloading"
                },
            },
        ),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")),
    }
}

async fn downloads(State(api): State<Api>) -> Response {
    json_response(StatusCode::OK, &api.runtime.downloads())
}

async fn delete_model(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let runtime = api.runtime.clone();
    let model = id.clone();
    let deleted = detached(async move {
        runtime.unload(&model).await;
        runtime.registry().delete(&model)
    })
    .await;
    match deleted {
        Ok(Ok(deleted)) => {
            Json(serde_json::json!({ "model": id, "deleted": deleted })).into_response()
        }
        Ok(Err(e)) => server_error(&e),
        Err(failed) => failed,
    }
}

async fn unload(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let runtime = api.runtime.clone();
    let model = id.clone();
    match detached(async move { runtime.unload(&model).await }).await {
        Ok(()) => Json(serde_json::json!({ "unloaded": id })).into_response(),
        Err(failed) => failed,
    }
}

async fn pin(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    let pinned = match query
        .get("pinned")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        None => true,
        Some(v) => match spring_boolean(v) {
            Some(b) => b,
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid value for 'pinned': {v}"),
                )
            }
        },
    };
    api.runtime.pin(&id, pinned);
    Json(serde_json::json!({ "model": id, "pinned": pinned })).into_response()
}

/// Spring's text-to-boolean conversion.
fn spring_boolean(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

async fn recent_events(State(api): State<Api>) -> Response {
    json_response(StatusCode::OK, &api.runtime.recent_events())
}

/// Runtime events as they happen, as server-sent events named by their kind with the event as
/// JSON (`event:model_loaded` / `data:{...}`, Spring's `SseEmitter` framing). The stream stays
/// open until the client leaves or the gateway stops.
async fn runtime_events(State(api): State<Api>) -> Response {
    let events = futures::stream::unfold(events::subscribe(), |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(e) if e.topic == topic::RUNTIME => {
                    let kind = e.payload["kind"].as_str().unwrap_or("runtime").to_string();
                    let frame = format!("event:{kind}\ndata:{}\n\n", e.payload);
                    return Some((Ok::<_, std::convert::Infallible>(Bytes::from(frame)), rx));
                }
                Ok(_) => continue,
                // A slow client misses what it could not keep up with, as a failed send dropped
                // an event before.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .take_until(api.shutdown.clone().cancelled_owned());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(events))
        .unwrap_or_else(|e| server_error(&anyhow::anyhow!(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_priorities_and_switches_read_as_before() {
        assert_eq!(parse_size("512x768"), Some((512, 768)));
        assert_eq!(parse_size("512x"), None);
        assert_eq!(parse_size("x512"), None);
        assert_eq!(parse_size("512 x 768"), None);
        assert_eq!(parse_size("-5x10"), None);
        let mut h = HeaderMap::new();
        assert_eq!(priority_of(&h), Priority::Interactive);
        h.insert("x-nook-priority", HeaderValue::from_static(" Background "));
        assert_eq!(priority_of(&h), Priority::Background);
        h.insert("x-nook-priority", HeaderValue::from_static("batch"));
        assert_eq!(priority_of(&h), Priority::Interactive);
        assert_eq!(spring_boolean("OFF"), Some(false));
        assert_eq!(spring_boolean("yes"), Some(true));
        assert_eq!(spring_boolean("maybe"), None);
        assert_eq!(int_of(&serde_json::json!("20")), 20);
        assert_eq!(int_of(&serde_json::json!(4.7)), 4);
        assert_eq!(int_of(&serde_json::json!("many")), 0);
    }
}
