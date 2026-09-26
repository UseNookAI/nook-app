//! Ports `runtime/InferenceClient.java`: the HTTP client for one engine process. Speaks the
//! OpenAI-compatible surface of llama-server.

use std::future::Future;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const PROPS_TIMEOUT: Duration = Duration::from_secs(5);
const CHAT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const STREAM_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const EMBEDDINGS_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Talks to one llama-server. Cheap to clone (the connection pool is shared).
///
/// Timeouts are the original's and, as with Java's `HttpClient`, bound the wait for the answer to
/// start (the response headers), not the time spent reading a long body or stream.
#[derive(Clone, Debug)]
pub struct InferenceClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl InferenceClient {
    /// A client for `base_url` (`http://127.0.0.1:<port>`), sending `api_key` as a bearer token.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> InferenceClient {
        // The engine is on this machine: no proxy, and no content encoding, so the gateway can
        // pass the engine's bytes and headers through as they are.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("engine client could not be configured ({e}); using the defaults");
                reqwest::Client::new()
            });
        InferenceClient {
            base_url: base_url.into(),
            api_key,
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut b = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .header(CONTENT_TYPE, "application/json");
        if let Some(key) = &self.api_key {
            b = b.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        b
    }

    /// True when the engine answers `/health` with 200 within three seconds.
    pub async fn health(&self) -> bool {
        match headers_within(
            HEALTH_TIMEOUT,
            self.request(reqwest::Method::GET, "/health").send(),
        )
        .await
        {
            Ok(r) => r.status().as_u16() == 200,
            Err(_) => false,
        }
    }

    /// The engine's `/props` (context per slot, slot count, chat template...).
    pub async fn props(&self) -> Result<Value> {
        let r = headers_within(
            PROPS_TIMEOUT,
            self.request(reqwest::Method::GET, "/props").send(),
        )
        .await?;
        let body = r.text().await?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Full JSON of a non-streaming chat completion.
    pub async fn chat(&self, mut body: Value) -> Result<Value> {
        set_stream(&mut body, false);
        self.post_json("/v1/chat/completions", &body, CHAT_TIMEOUT)
            .await
    }

    /// llama-server's own `/completion`: raw text in, text out, with the engine's timings.
    pub async fn completion(&self, mut body: Value) -> Result<Value> {
        set_stream(&mut body, false);
        self.post_json("/completion", &body, COMPLETION_TIMEOUT)
            .await
    }

    /// Convenience: the assistant text of a non-streaming completion.
    pub async fn chat_text(&self, body: Value) -> Result<String> {
        let n = self.chat(body).await?;
        Ok(n["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    async fn post_json(&self, path: &str, body: &Value, timeout: Duration) -> Result<Value> {
        let r = headers_within(
            timeout,
            self.request(reqwest::Method::POST, path)
                .body(body.to_string())
                .send(),
        )
        .await?;
        let status = r.status().as_u16();
        let text = r.text().await?;
        if status / 100 != 2 {
            bail!("Engine returned HTTP {status}: {text}");
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Streams a chat completion. `on_delta` receives content fragments as they arrive. Returns
    /// the final usage node (None when the engine sent none) once the stream ends. Cancelling
    /// closes the stream, which aborts generation on the server, and returns what was seen.
    pub async fn chat_stream(
        &self,
        mut body: Value,
        mut on_delta: impl FnMut(&str) + Send,
        cancel: &CancellationToken,
    ) -> Result<Option<Value>> {
        set_stream(&mut body, true);
        let send = self
            .request(reqwest::Method::POST, "/v1/chat/completions")
            .body(body.to_string())
            .send();
        let r = tokio::select! {
            r = headers_within(STREAM_TIMEOUT, send) => r?,
            _ = cancel.cancelled() => return Ok(None),
        };
        let status = r.status().as_u16();
        if status / 100 != 2 {
            let err = r.text().await.unwrap_or_default();
            bail!("Engine returned HTTP {status}: {err}");
        }
        let mut usage = None;
        let mut stream = r.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut ended = false;
        'read: while !ended {
            let chunk = tokio::select! {
                biased;
                // closing the stream aborts generation on the server
                _ = cancel.cancelled() => break 'read,
                c = stream.next() => c,
            };
            match chunk {
                Some(c) => buf.extend_from_slice(
                    &c.map_err(|e| anyhow!("The engine's stream broke off: {e}"))?,
                ),
                None => {
                    ended = true;
                    if buf.last() != Some(&b'\n') {
                        buf.push(b'\n');
                    }
                }
            }
            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=nl).collect();
                if cancel.is_cancelled() {
                    break 'read;
                }
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_end_matches(['\n', '\r']);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    break 'read;
                }
                if data.is_empty() {
                    continue;
                }
                let node: Value =
                    serde_json::from_str(data).context("The engine sent a malformed event")?;
                if let Some(error) = node.get("error") {
                    let message = match error.get("message") {
                        Some(Value::String(s)) => s.clone(),
                        Some(m) if !m.is_null() => m.to_string(),
                        _ => error.to_string(),
                    };
                    bail!("Engine error: {message}");
                }
                if let Some(content) = node["choices"][0]["delta"]["content"].as_str() {
                    if !content.is_empty() {
                        on_delta(content);
                    }
                }
                if let Some(u) = node.get("usage").filter(|u| !u.is_null()) {
                    usage = Some(u.clone());
                }
            }
        }
        Ok(usage)
    }

    /// One embedding per input.
    pub async fn embeddings(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = json!({ "input": inputs });
        let n = self
            .post_json("/v1/embeddings", &body, EMBEDDINGS_TIMEOUT)
            .await?;
        Ok(n["data"]
            .as_array()
            .map(|data| {
                data.iter()
                    .map(|d| {
                        d["embedding"]
                            .as_array()
                            .map(|e| e.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Raw pass-through for the gateway: forwards a request body to a path and returns the
    /// response as it comes (status, headers, a body to stream on).
    pub async fn forward(
        &self,
        path: &str,
        body: String,
        stream: bool,
    ) -> Result<reqwest::Response> {
        let timeout = if stream { STREAM_TIMEOUT } else { CHAT_TIMEOUT };
        headers_within(
            timeout,
            self.request(reqwest::Method::POST, path).body(body).send(),
        )
        .await
    }
}

fn set_stream(body: &mut Value, stream: bool) {
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), Value::Bool(stream));
    }
}

/// Waits for the response headers at most `timeout` (Java's request timeout).
async fn headers_within(
    timeout: Duration,
    send: impl Future<Output = reqwest::Result<reqwest::Response>>,
) -> Result<reqwest::Response> {
    match tokio::time::timeout(timeout, send).await {
        Ok(r) => r.map_err(|e| anyhow!("Could not reach the engine: {e}")),
        Err(_) => Err(anyhow!("request timed out")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::downloader::tests::serve;
    use axum::body::{Body, Bytes};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn sse(events: &[&str]) -> Response {
        let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn health_props_chat_and_errors() {
        let seen = Arc::new(Mutex::new(Vec::<(Option<String>, Value)>::new()));
        let s = seen.clone();
        let router = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/props", get(|| async { Json(json!({"total_slots": 2})) }))
            .route(
                "/v1/chat/completions",
                post(move |headers: HeaderMap, body: Bytes| {
                    let s = s.clone();
                    async move {
                        let v: Value = serde_json::from_slice(&body).unwrap();
                        let auth = headers.get("authorization").map(|h| h.to_str().unwrap().to_string());
                        s.lock().push((auth, v.clone()));
                        if v["model"] == "broken" {
                            return (StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string()).into_response();
                        }
                        (StatusCode::OK, json!({"choices":[{"message":{"content":"hello"}}]}).to_string()).into_response()
                    }
                }),
            )
            .route("/completion", post(|| async { Json(json!({"content": "raw", "timings": {"predicted_per_second": 30.0}})) }))
            .route(
                "/v1/embeddings",
                post(|Json(v): Json<Value>| async move {
                    let n = v["input"].as_array().unwrap().len();
                    Json(json!({"data": (0..n).map(|i| json!({"embedding": [i as f64, 0.5]})).collect::<Vec<_>>()}))
                }),
            );
        let base = serve(router).await;
        let client = InferenceClient::new(&base, Some("secret".into()));
        assert!(client.health().await);
        assert_eq!(client.props().await.unwrap()["total_slots"], 2);
        assert_eq!(
            client
                .chat_text(json!({"model": "m", "stream": true}))
                .await
                .unwrap(),
            "hello"
        );
        {
            let seen = seen.lock();
            assert_eq!(seen[0].0.as_deref(), Some("Bearer secret"));
            assert_eq!(seen[0].1["stream"], false, "a non-streaming call says so");
        }
        let err = client
            .chat(json!({"model": "broken"}))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Engine returned HTTP 500: boom");
        assert_eq!(
            client.completion(json!({"prompt": "x"})).await.unwrap()["content"],
            "raw"
        );
        let e = client.embeddings(&["a".into(), "b".into()]).await.unwrap();
        assert_eq!(e, vec![vec![0.0, 0.5], vec![1.0, 0.5]]);

        let forwarded = client
            .forward("/completion", "{}".into(), false)
            .await
            .unwrap();
        assert_eq!(forwarded.status().as_u16(), 200);

        // Nothing listening: not healthy, no panic.
        let dead = InferenceClient::new("http://127.0.0.1:9", None);
        assert!(!dead.health().await);
    }

    #[tokio::test]
    async fn streams_deltas_and_returns_the_usage() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|Json(v): Json<Value>| async move {
                assert_eq!(v["stream"], true);
                sse(&[
                    r#"{"choices":[{"delta":{"role":"assistant"}}]}"#,
                    r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
                    r#"{"choices":[{"delta":{"content":""}}]}"#,
                    r#"{"choices":[{"delta":{"content":"lo"}}],"usage":null}"#,
                    r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
                    "[DONE]",
                    r#"{"choices":[{"delta":{"content":"after done"}}]}"#,
                ])
            }),
        );
        let base = serve(router).await;
        let client = InferenceClient::new(&base, None);
        let mut text = String::new();
        let usage = client
            .chat_stream(
                json!({"messages": []}),
                |d| text.push_str(d),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(text, "Hello");
        assert_eq!(usage.unwrap()["completion_tokens"], 2);
    }

    #[tokio::test]
    async fn an_error_event_fails_the_stream() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                sse(&[
                    r#"{"choices":[{"delta":{"content":"x"}}]}"#,
                    r#"{"error":{"code":500,"message":"the context is full"}}"#,
                ])
            }),
        );
        let base = serve(router).await;
        let client = InferenceClient::new(&base, None);
        let err = client
            .chat_stream(json!({}), |_| {}, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Engine error: the context is full");

        let failing = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Response::builder()
                    .status(503)
                    .body(Body::from("loading model"))
                    .unwrap()
            }),
        );
        let base = serve(failing).await;
        let err = InferenceClient::new(&base, None)
            .chat_stream(json!({}), |_| {}, &CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Engine returned HTTP 503: loading model");
    }

    #[tokio::test]
    async fn cancelling_stops_reading_the_stream() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let stream = futures::stream::unfold(0u32, |n| async move {
                    if n > 0 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    let event = format!(
                        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"t{n} \"}}}}]}}\n\n"
                    );
                    Some((Ok::<_, std::io::Error>(Bytes::from(event)), n + 1))
                });
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let base = serve(router).await;
        let client = InferenceClient::new(&base, None);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let mut count = 0;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.chat_stream(
                json!({}),
                |_| {
                    count += 1;
                    if count == 3 {
                        c.cancel();
                    }
                },
                &cancel,
            ),
        )
        .await
        .expect("the endless stream ends when cancelled")
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(count, 3);
    }
}
