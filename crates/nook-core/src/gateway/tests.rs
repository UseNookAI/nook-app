//! The gateway over HTTP on 127.0.0.1: the token, the routes' answers to requests they cannot
//! read (GatewayControllerTest), the runtime routes, chats forwarded to a fake llama-server with
//! the envelope stripped, speech and images through the fake engines, and the video routes
//! (VideoControllerTest).

#[cfg(windows)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
#[cfg(windows)]
use futures::StreamExt;
use serde_json::{json, Value};

use super::*;
#[cfg(windows)]
use crate::runtime::engine_component::EngineComponent;
use crate::runtime::manager::testing::*;
use crate::video::studio::testing::{wait_until, FakeRuntime, Mode};
use crate::video::{Status, VideoStudio};

struct Gw {
    rig: Rig,
    handle: GatewayHandle,
    http: reqwest::Client,
}

impl Gw {
    async fn start(rig: Rig) -> Gw {
        let studio = VideoStudio::for_runtime(rig.manager.clone());
        let handle = Gateway::start_on(0, rig.manager.clone(), studio, &rig.home)
            .await
            .unwrap();
        Gw {
            rig,
            handle,
            http: client(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.handle.base_url())
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .get(self.url(path))
            .bearer_auth(self.handle.token())
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http
            .post(self.url(path))
            .bearer_auth(self.handle.token())
    }

    fn post_json(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        self.post(path)
            .header("content-type", "application/json")
            .body(body.to_string())
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn json_of(r: reqwest::Response) -> (u16, Value) {
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    let json = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, json)
}

#[tokio::test]
async fn the_token_guards_everything_but_health_and_gateway_json_says_where() {
    let gw = Gw::start(rig(RigSpec::default())).await;
    let raw = client();

    let file: Value =
        serde_json::from_str(&std::fs::read_to_string(gw.rig.home.gateway_file()).unwrap())
            .unwrap();
    assert_eq!(file["port"], gw.handle.port());
    assert_eq!(file["token"], gw.handle.token());
    assert_eq!(file["baseUrl"], gw.handle.base_url());
    assert_eq!(
        file["openaiBaseUrl"],
        format!("{}/v1", gw.handle.base_url())
    );

    let (status, health) = json_of(raw.get(gw.url("/health")).send().await.unwrap()).await;
    assert_eq!(status, 200, "open without a token");
    assert_eq!(
        health,
        json!({"status": "ok", "readiness": "ENGINE_MISSING"})
    );

    let refused = raw.get(gw.url("/v1/models")).send().await.unwrap();
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        refused.headers()["content-type"].to_str().unwrap(),
        "application/json"
    );
    assert_eq!(refused.text().await.unwrap(), auth::UNAUTHORIZED_BODY);
    let wrong = raw
        .get(gw.url("/v1/models"))
        .bearer_auth("0".repeat(64))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let unknown = raw.get(gw.url("/no/such/route")).send().await.unwrap();
    assert_eq!(
        unknown.status(),
        StatusCode::UNAUTHORIZED,
        "the filter comes first"
    );

    for request in [
        gw.get("/v1/models"),
        raw.get(gw.url("/v1/models"))
            .header("X-Api-Key", gw.handle.token()),
        raw.get(gw.url("/v1/models"))
            .header("Authorization", gw.handle.token()),
    ] {
        let (status, models) = json_of(request.send().await.unwrap()).await;
        assert_eq!(status, 200);
        assert_eq!(models, json!({"object": "list", "data": []}));
    }

    let (status, missing) = json_of(gw.get("/no/such/route").send().await.unwrap()).await;
    assert_eq!(status, 404);
    assert_eq!(missing["status"], 404);
    assert_eq!(missing["error"], "Not Found");
    assert_eq!(missing["path"], "/no/such/route");
    let (status, _) = json_of(gw.get("/v1/chat/completions").send().await.unwrap()).await;
    assert_eq!(status, 405);

    let port = gw.handle.port();
    gw.handle.stop().await;
    assert!(
        !gw.rig.home.gateway_file().exists(),
        "gateway.json goes with the gateway"
    );
    assert!(
        raw.get(format!("http://127.0.0.1:{port}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_err(),
        "nothing answers on the port"
    );
}

/// What the gateway answers a request it cannot read: the caller's mistake, a 400, never a 500.
#[tokio::test]
async fn a_request_it_cannot_read_is_a_bad_request() {
    let gw = Gw::start(rig(RigSpec::default())).await;
    for route in [
        "/v1/chat/completions",
        "/v1/completions",
        "/v1/embeddings",
        "/v1/images/generations",
        "/v1/videos",
    ] {
        let r = gw
            .post(route)
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap();
        let (status, body) = json_of(r).await;
        assert_eq!(status, 400, "{route}: {body}");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("The request body is not valid JSON: "),
            "{body}"
        );
    }
    let (status, body) = json_of(
        gw.post("/v1/chat/completions")
            .header("content-type", "application/json")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400, "no body");
    assert_eq!(
        body["error"]["message"],
        "The request body is not valid JSON: the request has no readable body"
    );

    let (status, body) = json_of(
        gw.post_json("/v1/chat/completions", &json!({"model": " "}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["message"], "The request must name a model.");
    let (status, body) = json_of(
        gw.post_json("/v1/embeddings", &json!({"input": "x"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["message"], "The request must name a model.");
    let (status, body) = json_of(
        gw.post_json("/v1/images/generations", &json!({"prompt": ""}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        body["error"]["message"],
        "The request must include a prompt."
    );

    let (status, body) = json_of(
        gw.post("/v1/chat/completions")
            .header("content-type", "text/plain")
            .body("{}")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 415, "consumes JSON only");
    assert_eq!(body["path"], "/v1/chat/completions");
    let (status, _) = json_of(
        gw.post("/v1/audio/transcriptions")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 415, "consumes a form only");
    let (status, body) = json_of(
        gw.post("/v1/audio/transcriptions")
            .header("content-type", "multipart/form-data; boundary=b")
            .body("--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nw\r\n--b--\r\n")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        body["error"]["message"],
        "Required part 'file' is not present."
    );
    gw.handle.stop().await;
}

#[tokio::test]
async fn the_runtime_routes_report_and_control_the_runtime() {
    let gw = Gw::start(rig(RigSpec::default())).await;
    sidecar_model(
        &gw.rig,
        "whisper",
        "ggml-small.bin",
        "whisper-small",
        "speech",
    );

    let (status, s) = json_of(gw.get("/runtime/status").send().await.unwrap()).await;
    assert_eq!(status, 200);
    assert_eq!(s["readiness"], "ENGINE_MISSING");
    assert_eq!(s["installedModels"], json!(["whisper-small"]));
    let (_, catalog) = json_of(gw.get("/runtime/catalog").send().await.unwrap()).await;
    assert!(!catalog.as_array().unwrap().is_empty());
    assert!(catalog[0].get("displayName").is_some());
    let (_, downloads) = json_of(gw.get("/runtime/downloads").send().await.unwrap()).await;
    assert_eq!(downloads, json!({}));

    let (status, d) = json_of(
        gw.post("/runtime/models/whisper-small/download")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(d, json!({"model": "whisper-small", "state": "installed"}));
    let (status, d) = json_of(
        gw.post("/runtime/models/nope/download")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(d["error"]["message"], "Unknown catalog model nope");

    let (status, p) = json_of(
        gw.post("/runtime/models/whisper-small/pin")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(p, json!({"model": "whisper-small", "pinned": true}));
    assert!(gw.rig.manager.is_pinned("whisper-small"));
    let (_, p) = json_of(
        gw.post("/runtime/models/whisper-small/pin?pinned=false")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(p["pinned"], false);
    assert!(!gw.rig.manager.is_pinned("whisper-small"));
    let (status, _) = json_of(
        gw.post("/runtime/models/whisper-small/pin?pinned=maybe")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 400);

    let (status, u) = json_of(gw.post("/runtime/models/nope/unload").send().await.unwrap()).await;
    assert_eq!(status, 200);
    assert_eq!(u, json!({"unloaded": "nope"}));
    let (status, l) = json_of(gw.post("/runtime/models/nope/load").send().await.unwrap()).await;
    assert_eq!(status, 503);
    assert_eq!(l["error"]["type"], "server_error");
    assert_eq!(
        l["error"]["message"],
        "The Nook runtime is not installed yet."
    );
    let (status, c) = json_of(
        gw.post_json(
            "/v1/chat/completions",
            &json!({"model": "nope", "messages": []}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(
        c["error"]["message"],
        "The Nook runtime is not installed yet."
    );

    let (status, del) = json_of(
        gw.http
            .delete(gw.url("/runtime/models/whisper-small"))
            .bearer_auth(gw.handle.token())
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(del, json!({"model": "whisper-small", "deleted": true}));
    gw.rig.manager.registry().invalidate();
    assert!(!gw.rig.manager.registry().is_installed("whisper-small"));

    gw.rig
        .manager
        .notify("probe_measured", Some("m"), Some("from a test".into()));
    let (_, recent) = json_of(gw.get("/runtime/events/recent").send().await.unwrap()).await;
    assert_eq!(recent[0]["kind"], "probe_measured");
    assert_eq!(recent[0]["modelId"], "m");
    gw.handle.stop().await;
    gw.rig.manager.shutdown().await;
}

/// A model the engine cannot load says why on the chat route and the load route alike, and a
/// file that is no language model is listed for what it is (2026-09-26).
#[cfg(windows)]
#[tokio::test]
async fn a_model_that_cannot_run_says_why_on_every_route() {
    use crate::runtime::engine_process::tests::{fake_engine_saying, UNKNOWN_ARCHITECTURE};
    use crate::runtime::gguf_metadata::testing::{write_gguf_named, Kv};
    let gw = Gw::start(rig(RigSpec {
        free_mb: Some(8000),
        ..RigSpec::default()
    }))
    .await;
    install(&gw.rig, EngineComponent::Llama);
    fake_engine_saying(&gw.rig.bin, "llama-server.cmd", UNKNOWN_ARCHITECTURE, 1);
    let id = text_model(&gw.rig, "DS4-Flash-Q2_K.gguf", 100, vec![]);
    let why = "DS4-Flash-Q2_K.gguf can't run in Nook: the engine (llama.cpp b10752) doesn't know its model architecture 'deepseek4-vision'.";
    let (status, c) = json_of(
        gw.post_json(
            "/v1/chat/completions",
            &json!({"model": id, "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(c["error"]["message"], why);
    let (status, l) = json_of(
        gw.post(&format!("/runtime/models/{id}/load"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(l["error"]["message"], why);

    let hub = gw.rig.home.models_dir().join("hub").join("a-b");
    let file = write_gguf_named(
        &hub,
        "Vision-Encoder.gguf",
        &[("general.architecture", Kv::Str("clip"))],
        0,
    );
    std::fs::write(
        crate::runtime::model_registry::sidecar_of(&file),
        r#"{"id":"encoder","displayName":"Vision Encoder","task":"chat","source":"huggingface"}"#,
    )
    .unwrap();
    gw.rig.manager.registry().invalidate();
    let (_, models) = json_of(gw.get("/v1/models").send().await.unwrap()).await;
    let encoder = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "encoder")
        .cloned()
        .unwrap();
    assert_eq!(encoder["task"], "unsupported");
    let (status, e) = json_of(
        gw.post("/runtime/models/encoder/load")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(
        e["error"]["message"],
        "Vision Encoder can't run in Nook: its architecture 'clip' is a vision encoder or projector, not a language model."
    );
    gw.handle.stop().await;
    gw.rig.manager.shutdown().await;
}

// ------------------------------------------------------------------ with fake engines

/// A fake llama-server's chat side: a Qwen3-style reply with the empty envelope, whole or as
/// events (a little apart, as tokens come), echoing the template switch it was sent; completions
/// that stream a `<think>` the gateway must leave alone; embeddings; and a refusal on
/// `"force_status"`.
#[cfg(windows)]
fn chat_router(key: String) -> axum::Router {
    use axum::body::Body;
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;

    fn authorized(headers: &HeaderMap, key: &str) -> bool {
        headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| h == format!("Bearer {key}"))
    }
    fn sse(events: Vec<String>) -> Response {
        let stream = futures::stream::iter(events).then(|e| async move {
            tokio::time::sleep(Duration::from_millis(15)).await;
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(e))
        });
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }
    let chat_key = key.clone();
    let completion_key = key.clone();
    axum::Router::new()
        .route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, body: String| {
                let ok = authorized(&headers, &chat_key);
                async move {
                    if !ok {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    let req: Value = serde_json::from_str(&body).unwrap_or_default();
                    if let Some(code) = req.get("force_status").and_then(Value::as_u64) {
                        return Response::builder()
                            .status(code as u16)
                            .header("content-type", "application/json")
                            .body(Body::from(
                                r#"{"error":{"message":"<think></think> engine says no"}}"#,
                            ))
                            .unwrap();
                    }
                    let kwargs = req.get("chat_template_kwargs").cloned().unwrap_or(Value::Null);
                    if req["stream"] == true {
                        let delta = |content: &str| {
                            format!(
                                "data: {}\n\n",
                                json!({"choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]})
                            )
                        };
                        return sse(vec![
                            format!(
                                "data: {}\n\n",
                                json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}],"kwargs":kwargs})
                            ),
                            delta("<think>"),
                            delta("\n\n"),
                            delta("</think>"),
                            delta("\n\nNOOK"),
                            delta("_QA_OK"),
                            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"completion_tokens\":6}}\n\n".into(),
                            "data: [DONE]\n\n".into(),
                        ]);
                    }
                    axum::Json(json!({
                        "choices":[{"index":0,"message":{"role":"assistant","content":"<think>\n\n</think>\n\nNOOK_QA_OK"},"finish_reason":"stop"}],
                        "usage":{"completion_tokens":6},
                        "kwargs": kwargs,
                    }))
                    .into_response()
                }
            }),
        )
        .route(
            "/v1/completions",
            post(move |headers: HeaderMap, body: String| {
                let ok = authorized(&headers, &completion_key);
                async move {
                    if !ok {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    let req: Value = serde_json::from_str(&body).unwrap_or_default();
                    if req["stream"] == true {
                        return sse(vec![
                            "data: {\"choices\":[{\"text\":\"<think>\",\"index\":0}]}\n\n".into(),
                            "data: {\"choices\":[{\"text\":\"</think>hi\",\"index\":0}]}\n\n".into(),
                            "data: [DONE]\n\n".into(),
                        ]);
                    }
                    axum::Json(json!({"choices":[{"text":"<think></think>hi","index":0}]}))
                        .into_response()
                }
            }),
        )
        .route(
            "/v1/embeddings",
            post(move |headers: HeaderMap| {
                let ok = authorized(&headers, &key);
                async move {
                    if !ok {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    axum::Json(json!({"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.5,0.25]}]}))
                        .into_response()
                }
            }),
        )
}

/// Serves every fake engine the rig starts, as the manager's `serve_fakes` does, with the chat
/// side above on each llama-server.
#[cfg(windows)]
fn serve_engines(bin: PathBuf) -> tokio::task::JoinHandle<()> {
    use crate::runtime::engine_process::tests::fake_llama_router;
    use crate::runtime::whisper_process::tests::fake_whisper_server;
    tokio::spawn(async move {
        let mut served = 0;
        loop {
            if let Ok(text) = std::fs::read_to_string(bin.join("args.txt")) {
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                for line in lines.iter().skip(served) {
                    let args: Vec<&str> = line.split_whitespace().collect();
                    let after = |flag: &str| {
                        args.iter()
                            .position(|a| *a == flag)
                            .and_then(|i| args.get(i + 1))
                            .map(|s| s.to_string())
                    };
                    let Some(port) = after("--port").and_then(|p| p.parse::<u16>().ok()) else {
                        continue;
                    };
                    match after("--api-key") {
                        Some(key) => {
                            let total: i32 = after("--ctx-size")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            let slots: i32 = after("--parallel")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1);
                            let router =
                                fake_llama_router(Some(key.clone()), total / slots.max(1), slots)
                                    .merge(chat_router(key));
                            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                                .await
                                .unwrap();
                            tokio::spawn(async move {
                                let _ = axum::serve(listener, router).await;
                            });
                        }
                        None => {
                            fake_whisper_server(port, Arc::new(parking_lot::Mutex::new(Vec::new())))
                                .await
                        }
                    }
                }
                served = lines.len();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

/// Reads a streamed answer to its end, chunk by chunk.
#[cfg(windows)]
async fn chunks(r: reqwest::Response) -> Vec<String> {
    let mut out = Vec::new();
    let mut body = r.bytes_stream();
    while let Some(chunk) = body.next().await {
        out.push(String::from_utf8(chunk.unwrap().to_vec()).unwrap());
    }
    out
}

#[cfg(windows)]
#[tokio::test]
async fn chats_go_to_the_models_engine_without_the_empty_envelope() {
    let gw = Gw::start(rig(RigSpec {
        free_mb: Some(8000),
        ..RigSpec::default()
    }))
    .await;
    let m = gw.rig.manager.clone();
    install(&gw.rig, EngineComponent::Llama);
    let id = text_model(&gw.rig, "Qwen3-Tiny-Q4.gguf", 100, vec![]);
    assert!(crate::runtime::thinking::switchable(&id), "{id}");
    let engines = serve_engines(gw.rig.bin.clone());

    // The event stream, open before anything loads.
    let events = gw.get("/runtime/events").send().await.unwrap();
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    let mut events = events.bytes_stream();

    // A whole reply: loaded on demand, the thinking switch off by default, no envelope.
    let r = gw
        .post_json(
            "/v1/chat/completions",
            &json!({"model": id, "messages": [{"role": "user", "content": "Reply NOOK_QA_OK"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["cache-control"], "no-cache");
    let (status, reply) = json_of(r).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["choices"][0]["message"]["content"], "NOOK_QA_OK");
    assert_eq!(
        reply["usage"]["completion_tokens"], 6,
        "everything else stays"
    );
    assert_eq!(reply["kwargs"], json!({"enable_thinking": false}));
    assert!(m.engine(&id).is_some());

    // A client that decided for itself keeps its switch.
    let (_, own) = json_of(
        gw.post_json(
            "/v1/chat/completions",
            &json!({"model": id, "messages": [], "chat_template_kwargs": {"enable_thinking": true}}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(own["kwargs"], json!({"enable_thinking": true}));

    // Streamed: event by event, the envelope held back, the rest as it came.
    let r = gw
        .post_json(
            "/v1/chat/completions",
            &json!({"model": id, "messages": [], "stream": true}),
        )
        .header("X-Nook-Priority", "background")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let parts = chunks(r).await;
    let got = parts.concat();
    assert!(parts.len() > 1, "passed on as it came: {parts:?}");
    assert!(
        !got.contains("<think>") && !got.contains("</think>"),
        "{got}"
    );
    assert!(got.contains(r#""content":"NOOK""#), "{got}");
    assert!(got.contains(r#""content":"_QA_OK""#), "{got}");
    assert!(got.contains(r#""enable_thinking":false"#), "{got}");
    assert!(got.trim_end().ends_with("data: [DONE]"), "{got}");

    // Completions and embeddings pass as they are.
    let r = gw
        .post_json(
            "/v1/completions",
            &json!({"model": id, "prompt": "x", "stream": true}),
        )
        .send()
        .await
        .unwrap();
    let got = chunks(r).await.concat();
    assert!(
        got.contains("<think>") && got.contains("</think>hi"),
        "{got}"
    );
    let (_, whole) = json_of(
        gw.post_json("/v1/completions", &json!({"model": id, "prompt": "x"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(whole["choices"][0]["text"], "<think></think>hi");
    let (status, e) = json_of(
        gw.post_json(
            "/v1/embeddings",
            &json!({"model": id, "input": "x", "stream": true}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(e["data"][0]["embedding"], json!([0.5, 0.25]));

    // The engine's refusal comes back as the engine said it, untouched.
    let r = gw
        .post_json(
            "/v1/chat/completions",
            &json!({"model": id, "messages": [], "force_status": 400}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        r.text().await.unwrap(),
        r#"{"error":{"message":"<think></think> engine says no"}}"#
    );

    // The model list shows it resident; every lease was let go.
    let (_, models) = json_of(gw.get("/v1/models").send().await.unwrap()).await;
    let entry = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(entry["object"], "model");
    assert_eq!(entry["owned_by"], "nook");
    assert_eq!(entry["task"], "chat");
    assert_eq!(entry["resident"], true);
    assert_eq!(entry["gpu_layers"], 999);
    wait_until("the leases to be let go", || {
        m.engine(&id).unwrap().in_flight() == 0
    })
    .await;
    let (_, status) = json_of(gw.get("/runtime/status").send().await.unwrap()).await;
    assert_eq!(status["engines"][0]["active"], 0);

    // Unloading through the gateway is an event on the stream.
    let (_, u) = json_of(
        gw.post(&format!("/runtime/models/{id}/unload"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(u["unloaded"], json!(id));
    // Other tests' runtimes share the event bus: look for this model's events.
    let ours = |seen: &str, kind: &str| {
        seen.split("\n\n").any(|frame| {
            let Some((name, data)) = frame.split_once('\n') else {
                return false;
            };
            let data: Value =
                serde_json::from_str(data.trim_start_matches("data:")).unwrap_or_default();
            name == format!("event:{kind}") && data["kind"] == kind && data["modelId"] == json!(id)
        })
    };
    let mut seen = String::new();
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = events.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if ours(&seen, "model_evicted") {
                return true;
            }
        }
        false
    })
    .await;
    assert!(matches!(found, Ok(true)), "{seen}");
    assert!(ours(&seen, "model_loaded"), "{seen}");

    // Loading through the gateway, then stopping it: the event stream ends.
    let (status, l) = json_of(
        gw.post(&format!("/runtime/models/{id}/load"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{l}");
    assert_eq!(l, json!({"loaded": id}));
    gw.handle.stop().await;
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while events.next().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "the event stream closes with the gateway");
    m.shutdown().await;
    engines.abort();
}

#[cfg(windows)]
#[tokio::test]
async fn speech_and_images_go_through_their_engines() {
    use base64::Engine as _;
    let gw = Gw::start(rig(RigSpec {
        free_mb: Some(8000),
        ..RigSpec::default()
    }))
    .await;
    install(&gw.rig, EngineComponent::Whisper);
    install(&gw.rig, EngineComponent::Sd);
    sidecar_model(
        &gw.rig,
        "whisper",
        "ggml-small.bin",
        "whisper-small",
        "speech",
    );
    sidecar_model(&gw.rig, "sd", "sd_turbo.safetensors", "sd-turbo", "image");
    let engines = serve_engines(gw.rig.bin.clone());

    let form = "--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n--XyZ\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-small\r\n--XyZ--\r\n";
    let (status, t) = json_of(
        gw.post("/v1/audio/transcriptions")
            .header("content-type", "multipart/form-data; boundary=XyZ")
            .body(form)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t, json!({"text": "hello there"}));
    let leftovers = std::fs::read_dir(gw.rig.home.temp_dir())
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("gw-"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "the upload's temporary file is gone");

    let (status, img) = json_of(
        gw.post_json(
            "/v1/images/generations",
            &json!({"prompt": "a fox", "size": "256x256", "steps": "2"}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{img}");
    let item = &img["data"][0];
    let file = PathBuf::from(item["file"].as_str().unwrap());
    assert!(file.starts_with(gw.rig.home.images_dir()));
    let png = base64::engine::general_purpose::STANDARD
        .decode(item["b64_json"].as_str().unwrap())
        .unwrap();
    assert_eq!(png, std::fs::read(&file).unwrap());
    assert!(item["seed"].is_u64() && item["elapsed_ms"].is_u64());
    assert!(img["created"].as_i64().unwrap() > 1_700_000_000);
    assert_eq!(
        event(&gw.rig.manager, "image_rendering", Some("sd-turbo"))
            .unwrap()
            .detail
            .as_deref(),
        Some("256x256 steps=2")
    );
    gw.handle.stop().await;
    gw.rig.manager.shutdown().await;
    engines.abort();
}

// ------------------------------------------------------------------ the video routes

/// The video routes alone on a loopback port (the original's standalone MockMvc), on a studio
/// with a fake runtime.
async fn videos(
    mode: Mode,
) -> (
    tempfile::TempDir,
    Arc<FakeRuntime>,
    Arc<VideoStudio>,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = FakeRuntime::new(mode);
    let studio = VideoStudio::new(runtime.clone(), dir.path().join("videos"));
    let base =
        crate::runtime::downloader::tests::serve(video_controller::router(studio.clone())).await;
    (dir, runtime, studio, base)
}

#[tokio::test]
async fn create_queues_a_clip() {
    let (_dir, _runtime, _studio, base) = videos(Mode::WaitsForRelease).await;
    let (status, v) = json_of(
        client()
            .post(format!("{base}/v1/videos"))
            .header("content-type", "application/json")
            .body(r#"{"prompt":"a fox"}"#)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert!(v["id"].as_str().unwrap().starts_with("vid_"));
    assert_eq!(v["object"], "video");
    assert_eq!(v["status"], "queued");
    assert_eq!(v["progress"], 0);
    assert_eq!(v["prompt"], "a fox");
    assert!(v["model"].is_null());
    assert!(v.get("stage").is_none() && v.get("size").is_none());
}

#[tokio::test]
async fn the_callers_mistakes_are_400s_and_a_missing_model_is_503() {
    let (_dir, runtime, _studio, base) = videos(Mode::Renders).await;
    let post = |body: &'static str| {
        client()
            .post(format!("{base}/v1/videos"))
            .header("content-type", "application/json")
            .body(body)
            .send()
    };
    let (status, e) = json_of(post("{").await.unwrap()).await;
    assert_eq!(status, 400);
    assert_eq!(e["error"]["type"], "invalid_request_error");
    let (status, _) = json_of(post(r#"{"prompt":" "}"#).await.unwrap()).await;
    assert_eq!(status, 400);
    *runtime.problem.lock() = Some("No video model is downloaded yet.".into());
    let (status, e) = json_of(post(r#"{"prompt":"a fox"}"#).await.unwrap()).await;
    assert_eq!(status, 503);
    assert_eq!(e["error"]["message"], "No video model is downloaded yet.");
    assert_eq!(e["error"]["type"], "server_error");
}

#[tokio::test]
async fn a_running_clip_reports_its_stage_and_unfinished_clips_say_so() {
    let (_dir, _runtime, studio, base) = videos(Mode::SamplesUntilStopped).await;
    let http = client();
    let clip = studio.submit("a fox", None).unwrap();
    let id = clip.id.clone();
    wait_until("the clip to sample", || {
        studio.clip(&id).unwrap().stage == Some(crate::runtime::VideoStage::Sampling)
    })
    .await;
    let (status, v) = json_of(
        http.get(format!("{base}/v1/videos/{id}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(v["status"], "in_progress");
    assert_eq!(v["stage"], "sampling");
    assert_eq!(v["stage_done"], 5);
    assert_eq!(v["stage_total"], 20);
    assert_eq!(v["progress"], 25, "0.09 + 0.65 x 5/20");

    let (status, e) = json_of(
        http.get(format!("{base}/v1/videos/{id}/content"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(
        e["error"]["message"],
        format!("Video {id} is in_progress, not completed.")
    );
    let (status, e) = json_of(
        http.delete(format!("{base}/v1/videos/{id}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(
        e["error"]["message"],
        format!("Video {id} is still in_progress; cancel it first.")
    );

    let (status, _) = json_of(
        http.post(format!("{base}/v1/videos/{id}/cancel"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    wait_until("the clip to stop", || {
        studio.clip(&id).unwrap().status == Status::Cancelled
    })
    .await;
    let (_, v) = json_of(
        http.get(format!("{base}/v1/videos/{id}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["status"], "cancelled");

    for (method, path) in [
        (reqwest::Method::GET, "/v1/videos/nope"),
        (reqwest::Method::DELETE, "/v1/videos/nope"),
        (reqwest::Method::GET, "/v1/videos/nope/content"),
        (reqwest::Method::POST, "/v1/videos/nope/cancel"),
    ] {
        let (status, e) = json_of(
            http.request(method, format!("{base}{path}"))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(e["error"]["message"], "No video nope.");
    }
}

#[tokio::test]
async fn a_finished_clip_downloads() {
    let (_dir, _runtime, studio, base) = videos(Mode::Renders).await;
    let http = client();
    let clip = studio.submit("a fox", None).unwrap();
    let id = clip.id.clone();
    wait_until("the clip to finish", || {
        studio.clip(&id).unwrap().status == Status::Done
    })
    .await;
    let (status, list) = json_of(http.get(format!("{base}/v1/videos")).send().await.unwrap()).await;
    assert_eq!(status, 200);
    assert_eq!(list["object"], "list");
    let v = &list["data"][0];
    assert_eq!(v["status"], "completed");
    assert_eq!(v["progress"], 100);
    assert_eq!(v["size"], "832x480");
    assert_eq!(v["seconds"], "2.1");
    assert_eq!(v["frames"], 33);
    assert_eq!(v["fps"], 16);
    assert_eq!(v["seed"], 7);
    assert_eq!(v["elapsed_ms"], 90_000);
    assert!(v["file"].as_str().unwrap().ends_with(&format!("{id}.avi")));
    assert!(v["created_at"].as_i64().unwrap() > 1_700_000_000);

    let r = http
        .get(format!("{base}/v1/videos/{id}/content"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "video/x-msvideo");
    assert_eq!(
        r.headers()["content-disposition"],
        format!("attachment; filename=\"{id}.avi\"").as_str()
    );
    assert_eq!(r.headers()["accept-ranges"], "bytes");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &[1, 2, 3]);
    let part = http
        .get(format!("{base}/v1/videos/{id}/content"))
        .header("Range", "bytes=1-")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.headers()["content-range"], "bytes 1-2/3");
    assert_eq!(part.bytes().await.unwrap().as_ref(), &[2, 3]);
    let outside = http
        .get(format!("{base}/v1/videos/{id}/content"))
        .header("Range", "bytes=9-")
        .send()
        .await
        .unwrap();
    assert_eq!(outside.status(), StatusCode::RANGE_NOT_SATISFIABLE);

    let (status, d) = json_of(
        http.delete(format!("{base}/v1/videos/{id}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        d,
        json!({"id": id, "object": "video.deleted", "deleted": true})
    );
    assert!(studio.clip(&id).is_none());
}

/// The video routes are behind the same token on the whole gateway.
#[tokio::test]
async fn the_video_routes_are_part_of_the_gateway() {
    let gw = Gw::start(rig(RigSpec::default())).await;
    let (status, _) = json_of(client().get(gw.url("/v1/videos")).send().await.unwrap()).await;
    assert_eq!(status, 401);
    let (status, list) = json_of(gw.get("/v1/videos").send().await.unwrap()).await;
    assert_eq!(status, 200);
    assert_eq!(list, json!({"object": "list", "data": []}));
    let (status, e) = json_of(
        gw.post_json("/v1/videos", &json!({"prompt": "a fox"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(e["error"]["message"], "No video model is downloaded yet.");
    gw.handle.stop().await;
}
