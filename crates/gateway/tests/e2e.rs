//! 端到端集成测试：多协议入口 → mock 多协议上游。
//!
//! 覆盖 M6 全矩阵链路：起本地 mock 上游（Anthropic / OpenAI Chat / Gemini），经真实
//! `dispatch::handle_request` 走完整生命周期（路由解析 → Core IR 转换 → 上游调用
//! → 回程转换 → usage 落库），断言客户端最终收到的报文。入口协议覆盖
//! Responses / Anthropic / Chat，上游覆盖 Anthropic / Chat / Gemini。

use std::sync::Arc;

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moonbridge_core::Protocol;
use moonbridge_gateway::{bootstrap, dispatch, AppState, GatewayConfig};
use moonbridge_store::{Database, Endpoint, Provider, Route};
use serde_json::{json, Value};

/// 起一个 mock Anthropic 上游（`POST /v1/messages`），按请求 `stream` 返回 JSON 或 SSE。
/// 返回其 base_url。
async fn spawn_mock_anthropic() -> String {
    async fn messages(Json(body): Json<Value>) -> Response {
        if body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false) {
            // 标准 Anthropic Messages SSE 事件序列
            let sse = concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
                "event: content_block_stop\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
                "event: message_stop\n",
                "data: {\"type\":\"message_stop\"}\n\n",
            );
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(sse))
                .unwrap()
        } else {
            Json(json!({
                "id": "msg_1",
                "model": "claude-x",
                "role": "assistant",
                "content": [{ "type": "text", "text": "Hello from mock" }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 10, "output_tokens": 5 }
            }))
            .into_response()
        }
    }

    let app = Router::new().route("/v1/messages", post(messages));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 构造指向 mock 上游的网关状态（provider `mock` + route `test-model`）。
async fn setup_state() -> (Arc<AppState>, Arc<Database>) {
    let base_url = spawn_mock_anthropic().await;
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&Provider {
        key: "mock".into(),
        endpoints: vec![Endpoint {
            protocol: "anthropic".into(),
            base_url,
            api_key: "sk-test".into(),
        }],
        version: Some("2023-06-01".into()),
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    db.upsert_route(&Route {
        alias: "test-model".into(),
        model_slug: "claude-x".into(),
        provider_key: "mock".into(),
        extra: Value::Null,
    })
    .unwrap();
    let state = bootstrap(GatewayConfig::default(), db.clone()).unwrap();
    (state, db)
}

/// 非流式：Responses 入口 → Anthropic 上游 → Responses 输出。
#[tokio::test]
async fn e2e_non_stream_responses_to_anthropic() {
    let (state, db) = setup_state().await;

    let body = json!({
        "model": "test-model",
        "instructions": "You are helpful",
        "input": "Hi",
        "stream": false
    });
    let resp = dispatch::handle_request(state, Protocol::OpenAiResponse, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    assert_eq!(resp.status(), 200);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out: Value = serde_json::from_slice(&bytes).unwrap();

    // 客户端收到的是 OpenAI Responses 对象
    assert_eq!(out["object"], "response");
    assert_eq!(out["status"], "completed");
    assert!(out["output"].is_array(), "应含 output 数组: {out}");
    assert!(
        out.to_string().contains("Hello from mock"),
        "上游文本应流转到 Responses 输出: {out}"
    );
    assert_eq!(out["usage"]["input_tokens"], 10);

    // usage 已落库
    let sum = db.usage_summary().unwrap();
    assert_eq!(sum.requests, 1);
    assert_eq!(sum.input_tokens, 10);
    assert_eq!(sum.output_tokens, 5);
}

/// 流式：Responses 入口（stream=true）→ Anthropic SSE → Responses SSE。
#[tokio::test]
async fn e2e_stream_responses_to_anthropic() {
    let (state, db) = setup_state().await;

    let body = json!({
        "model": "test-model",
        "input": "Hi",
        "stream": true
    });
    let resp = dispatch::handle_request(state, Protocol::OpenAiResponse, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    assert_eq!(resp.status(), 200);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);

    // 客户端收到 SSE，且上游文本增量完整流转
    assert!(text.contains("data:"), "应为 SSE 格式: {text}");
    assert!(text.contains("Hello"), "SSE 应含首段增量: {text}");
    assert!(text.contains("world"), "SSE 应含次段增量: {text}");

    // 流式请求同样落库一条 usage
    let sum = db.usage_summary().unwrap();
    assert_eq!(sum.requests, 1);
}

// ============================================================================
// M6 全矩阵：更多 mock 上游 + 通用装配 + 跨协议链路
// ============================================================================

/// 起一个 mock OpenAI Chat 上游（`POST /v1/chat/completions`）。
async fn spawn_mock_openai_chat() -> String {
    async fn chat(Json(body): Json<Value>) -> Response {
        if body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false) {
            let sse = concat!(
                "data: {\"id\":\"c1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"model\":\"gpt-4o\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":4,\"total_tokens\":12}}\n\n",
                "data: [DONE]\n\n",
            );
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(sse))
                .unwrap()
        } else {
            Json(json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "model": "gpt-4o",
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": "Hello from chat" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12 }
            }))
            .into_response()
        }
    }
    let app = Router::new().route("/v1/chat/completions", post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 起一个 mock Gemini 上游。Gemini 路径含模型名与 `:generateContent` / `:streamGenerateContent`
/// 后缀，用 fallback 捕获全部路径，按路径是否含 `streamGenerateContent` 决定流式与否。
async fn spawn_mock_gemini() -> String {
    async fn handler(uri: axum::http::Uri, Json(_body): Json<Value>) -> Response {
        if uri.path().contains("streamGenerateContent") {
            let sse = concat!(
                "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hello\"}]},\"index\":0}],\"modelVersion\":\"gemini-2.0-flash\",\"responseId\":\"r1\"}\n\n",
                "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\" world\"}]},\"index\":0}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"!\"}]},\"finishReason\":\"STOP\",\"index\":0}],\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":4,\"totalTokenCount\":12}}\n\n",
            );
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from(sse))
                .unwrap()
        } else {
            Json(json!({
                "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Hello from gemini" }] }, "finishReason": "STOP", "index": 0 }],
                "usageMetadata": { "promptTokenCount": 8, "candidatesTokenCount": 4, "totalTokenCount": 12 },
                "modelVersion": "gemini-2.0-flash",
                "responseId": "resp-1"
            }))
            .into_response()
        }
    }
    let app = Router::new().fallback(handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 通用装配：以指定上游协议 + base_url 构造 provider `mock` 与 route `alias → model_slug`。
async fn setup_with(
    protocol: &str,
    base_url: String,
    upstream_model: &str,
    alias: &str,
) -> (Arc<AppState>, Arc<Database>) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&Provider {
        key: "mock".into(),
        endpoints: vec![Endpoint {
            protocol: protocol.into(),
            base_url,
            api_key: "sk-test".into(),
        }],
        version: None,
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    db.upsert_route(&Route {
        alias: alias.into(),
        model_slug: upstream_model.into(),
        provider_key: "mock".into(),
        extra: Value::Null,
    })
    .unwrap();
    let state = bootstrap(GatewayConfig::default(), db.clone()).unwrap();
    (state, db)
}

/// Chat 入口 → Chat 上游（非流式）。
#[tokio::test]
async fn e2e_non_stream_chat_to_chat() {
    let base_url = spawn_mock_openai_chat().await;
    let (state, db) = setup_with("openai-chat", base_url, "gpt-4o", "test-model").await;

    let body = json!({
        "model": "test-model",
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": false
    });
    let resp = dispatch::handle_request(state, Protocol::OpenAiChat, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    assert_eq!(resp.status(), 200);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(out["object"], "chat.completion");
    assert_eq!(out["choices"][0]["message"]["content"], "Hello from chat");
    assert_eq!(out["usage"]["prompt_tokens"], 8);

    let sum = db.usage_summary().unwrap();
    assert_eq!(sum.requests, 1);
    assert_eq!(sum.input_tokens, 8);
}

/// Chat 入口 → Chat 上游（流式 SSE）。
#[tokio::test]
async fn e2e_stream_chat_to_chat() {
    let base_url = spawn_mock_openai_chat().await;
    let (state, db) = setup_with("openai-chat", base_url, "gpt-4o", "test-model").await;

    let body = json!({
        "model": "test-model",
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": true
    });
    let resp = dispatch::handle_request(state, Protocol::OpenAiChat, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Hello"), "SSE 应含首段增量: {text}");
    assert!(text.contains("world"), "SSE 应含次段增量: {text}");
    assert!(text.contains("[DONE]"), "Chat SSE 应以 [DONE] 结束: {text}");
    assert_eq!(db.usage_summary().unwrap().requests, 1);
}

/// 起一个对任意路径/方法都返回指定状态码的 mock 上游（用于故障转移测试）。
async fn spawn_mock_status(status: axum::http::StatusCode) -> String {
    async fn fail(status: axum::http::StatusCode) -> Response {
        (status, "boom").into_response()
    }
    // fallback：不限路径，保证跨协议 Adapter 的任意请求路径都命中失败响应
    let app = Router::new().fallback(move || fail(status));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Anthropic 入口 → Anthropic 上游（非流式，同协议直通）。
#[tokio::test]
async fn e2e_non_stream_anthropic_entry() {
    let (state, _db) = setup_with(
        "anthropic",
        spawn_mock_anthropic().await,
        "claude-x",
        "test-model",
    )
    .await;

    let body = json!({
        "model": "test-model",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": false
    });
    let resp = dispatch::handle_request(state, Protocol::Anthropic, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(out["type"], "message");
    assert_eq!(out["role"], "assistant");
    assert_eq!(out["content"][0]["text"], "Hello from mock");
    assert_eq!(out["usage"]["input_tokens"], 10);
}

/// Chat 入口 → Gemini 上游（非流式，跨协议）。
#[tokio::test]
async fn e2e_non_stream_chat_to_gemini() {
    let base_url = spawn_mock_gemini().await;
    let (state, db) = setup_with("google-genai", base_url, "gemini-2.0-flash", "test-model").await;

    let body = json!({
        "model": "test-model",
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": false
    });
    let resp = dispatch::handle_request(state, Protocol::OpenAiChat, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(out["object"], "chat.completion");
    assert_eq!(out["choices"][0]["message"]["content"], "Hello from gemini");
    assert_eq!(db.usage_summary().unwrap().input_tokens, 8);
}

/// Anthropic 入口 → Gemini 上游（流式，跨协议 SSE 转换）。
#[tokio::test]
async fn e2e_stream_anthropic_entry_to_gemini() {
    let base_url = spawn_mock_gemini().await;
    let (state, db) = setup_with("google-genai", base_url, "gemini-2.0-flash", "test-model").await;

    let body = json!({
        "model": "test-model",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": true
    });
    let resp = dispatch::handle_request(state, Protocol::Anthropic, body, vec![], None)
        .await
        .expect("dispatch 应成功");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    // Anthropic 入口 SSE：message_start → content_block_delta(text) → message_stop
    assert!(text.contains("message_start"), "应含 Anthropic 起始事件: {text}");
    assert!(text.contains("Hello"), "应含首段增量: {text}");
    assert!(text.contains("world"), "应含次段增量: {text}");
    assert!(text.contains("message_stop"), "应含 Anthropic 结束事件: {text}");
    assert_eq!(db.usage_summary().unwrap().requests, 1);
}

/// 多端点故障转移：连接拒绝 + 500 + 429 的端点依次跳过，末位健康端点成功。
#[tokio::test]
async fn e2e_failover_to_healthy_endpoint() {
    let good = spawn_mock_anthropic().await;
    let server_500 = spawn_mock_status(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
    let server_429 = spawn_mock_status(axum::http::StatusCode::TOO_MANY_REQUESTS).await;

    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&Provider {
        key: "mock".into(),
        endpoints: vec![
            // 连接拒绝（未监听端口）
            Endpoint { protocol: "anthropic".into(), base_url: "http://127.0.0.1:9".into(), api_key: "sk-first".into() },
            Endpoint { protocol: "anthropic".into(), base_url: server_500, api_key: "sk-second".into() },
            Endpoint { protocol: "anthropic".into(), base_url: server_429, api_key: "".into() }, // 空 Key 沿用上一非空
            Endpoint { protocol: "anthropic".into(), base_url: good, api_key: "sk-good".into() },
        ],
        version: Some("2023-06-01".into()),
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    db.upsert_route(&Route {
        alias: "test-model".into(),
        model_slug: "claude-x".into(),
        provider_key: "mock".into(),
        extra: Value::Null,
    })
    .unwrap();
    let state = bootstrap(GatewayConfig::default(), db.clone()).unwrap();

    let body = json!({
        "model": "test-model",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": false
    });
    let resp = dispatch::handle_request(state, Protocol::Anthropic, body, vec![], None)
        .await
        .expect("故障转移后应成功");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(out["content"][0]["text"], "Hello from mock");
    assert_eq!(db.usage_summary().unwrap().requests, 1);
}

/// 非 4xx/5xx 可重试类错误：4xx 快速失败，不转移。
#[tokio::test]
async fn e2e_no_failover_on_client_error() {
    let good = spawn_mock_anthropic().await;
    let bad = spawn_mock_status(axum::http::StatusCode::UNAUTHORIZED).await;

    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&Provider {
        key: "mock".into(),
        endpoints: vec![
            Endpoint { protocol: "anthropic".into(), base_url: bad, api_key: "sk-test".into() },
            Endpoint { protocol: "anthropic".into(), base_url: good, api_key: "sk-test".into() },
        ],
        version: Some("2023-06-01".into()),
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    db.upsert_route(&Route {
        alias: "test-model".into(),
        model_slug: "claude-x".into(),
        provider_key: "mock".into(),
        extra: Value::Null,
    })
    .unwrap();
    let state = bootstrap(GatewayConfig::default(), db.clone()).unwrap();

    let body = json!({
        "model": "test-model",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": false
    });
    let err = dispatch::handle_request(state, Protocol::Anthropic, body, vec![], None)
        .await
        .expect_err("401 应直接失败");
    assert!(matches!(err, moonbridge_gateway::GatewayError::Upstream { status: 401, .. }));
    // 仅首端点被尝试（错误请求也落一条 usage，但未发生转移）
    assert_eq!(db.usage_summary().unwrap().requests, 1);
}

/// 跨协议故障转移：首个端点（openai-chat）500 后切换到 Anthropic 端点成功，
/// 验证协议绑定在端点上时 Adapter 逐端点选取。
#[tokio::test]
async fn e2e_failover_across_protocols() {
    let anthropic = spawn_mock_anthropic().await;
    let chat_500 = spawn_mock_status(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;

    let db = Arc::new(Database::open_in_memory().unwrap());
    db.upsert_provider(&Provider {
        key: "mock".into(),
        endpoints: vec![
            Endpoint { protocol: "openai-chat".into(), base_url: chat_500, api_key: "sk-a".into() },
            Endpoint { protocol: "anthropic".into(), base_url: anthropic, api_key: "sk-b".into() },
        ],
        version: None,
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        created_at: 0,
        updated_at: 0,
    })
    .unwrap();
    db.upsert_route(&Route {
        alias: "test-model".into(),
        model_slug: "claude-x".into(),
        provider_key: "mock".into(),
        extra: Value::Null,
    })
    .unwrap();
    let state = bootstrap(GatewayConfig::default(), db.clone()).unwrap();

    // Anthropic 入口；首个端点按 openai-chat 构造请求失败后转移到 Anthropic 端点
    let body = json!({
        "model": "test-model",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "Hi" }],
        "stream": false
    });
    let resp = dispatch::handle_request(state, Protocol::Anthropic, body, vec![], None)
        .await
        .expect("跨协议转移后应成功");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let out: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(out["content"][0]["text"], "Hello from mock");
    assert_eq!(db.usage_summary().unwrap().requests, 1);
}
