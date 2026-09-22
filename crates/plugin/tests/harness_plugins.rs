//! plugins/harness/ 预置会话剥离插件的集成测试。
//!
//! 每个 agent 一个插件文件，直接 include_str! 跑真实脚本：
//! 入站 raw 钩子把客户端自带的会话标识转为 `msg.session_id`，并剥除原头。

use std::sync::Arc;

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse, Protocol};
use moonbridge_plugin::{HostBridge, HttpRequest, HttpResponse, LuaRuntime, SessionStore};
use moonbridge_protocol::{RawBody, RawMessage, RawStage, ReqCtx};

struct DummyBridge;

#[async_trait]
impl HostBridge for DummyBridge {
    async fn http_request(&self, _req: HttpRequest) -> Result<HttpResponse, String> {
        Err("test bridge: http_request 未实现".into())
    }
    async fn provider_invoke(
        &self,
        _provider: &str,
        _model: &str,
        _req: CoreRequest,
    ) -> Result<CoreResponse, String> {
        Err("test bridge: provider_invoke 未实现".into())
    }
}

fn runtime(name: &str, script: &str) -> LuaRuntime {
    LuaRuntime::new(
        name,
        script,
        &serde_json::json!({}),
        Arc::new(DummyBridge),
        SessionStore::new(),
    )
    .unwrap()
}

fn inbound(headers: Vec<(&str, &str)>, body: RawBody) -> RawMessage {
    RawMessage {
        stage: RawStage::ClientRequest,
        protocol: Protocol::Anthropic,
        provider: None,
        method: Some("POST".into()),
        url: Some("http://localhost/v1/messages".into()),
        status: None,
        headers: headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body,
        session_id: None,
    }
}

fn ctx() -> ReqCtx {
    ReqCtx::new("req-1", Protocol::Anthropic)
}

/// 每个 harness 插件的清单都须可求值、声明 raw_request 能力。
#[test]
fn harness_plugins_have_valid_manifests() {
    for (name, script) in [
        (
            "opencode",
            include_str!("../../../plugins/harness/opencode.lua"),
        ),
        (
            "claude-code",
            include_str!("../../../plugins/harness/claude-code.lua"),
        ),
        ("codex", include_str!("../../../plugins/harness/codex.lua")),
        (
            "grok-build",
            include_str!("../../../plugins/harness/grok-build.lua"),
        ),
        (
            "kimi-code",
            include_str!("../../../plugins/harness/kimi-code.lua"),
        ),
        ("dsh", include_str!("../../../plugins/harness/dsh.lua")),
    ] {
        let manifest = LuaRuntime::manifest_of_script(name, script)
            .unwrap_or_else(|e| panic!("{name} manifest 求值失败: {e}"));
        assert!(
            manifest.capabilities.iter().any(|c| c == "raw_request"),
            "{name} 应声明 raw_request"
        );
    }
}

#[tokio::test]
async fn opencode_plugin_bridges_session_header_and_strips_it() {
    let rt = runtime(
        "opencode",
        include_str!("../../../plugins/harness/opencode.lua"),
    );
    let mut msg = inbound(
        vec![
            ("x-opencode-session", "oc-abc"),
            ("content-type", "application/json"),
        ],
        RawBody::json(serde_json::json!({"messages": []})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("oc-abc"));
    assert!(msg.header("x-opencode-session").is_none(), "原头应被剥除");
    assert_eq!(msg.header("content-type"), Some("application/json"));
}

/// opencode 对非 opencode 提供方发 x-session-affinity + x-session-id（同值）。
#[tokio::test]
async fn opencode_plugin_bridges_non_opencode_provider_headers() {
    let rt = runtime(
        "opencode",
        include_str!("../../../plugins/harness/opencode.lua"),
    );
    let mut msg = inbound(
        vec![("x-session-affinity", "oc-gen"), ("x-session-id", "oc-gen")],
        RawBody::json(serde_json::json!({"messages": []})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("oc-gen"));
    assert!(msg.header("x-session-affinity").is_none());
    assert!(msg.header("x-session-id").is_none());
}

#[tokio::test]
async fn opencode_plugin_passes_through_without_header() {
    let rt = runtime(
        "opencode",
        include_str!("../../../plugins/harness/opencode.lua"),
    );
    let mut msg = inbound(vec![], RawBody::json(serde_json::json!({"messages": []})));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert!(msg.session_id.is_none());
}

/// claude-cli 1.x：扁平串 user_<hash>_account_<uuid>_session_<uuid>。
#[tokio::test]
async fn claude_code_plugin_extracts_session_v1_flat() {
    let rt = runtime(
        "claude-code",
        include_str!("../../../plugins/harness/claude-code.lua"),
    );
    let uid = "user_abc123_account_550e8400-e29b-41d4-a716-446655440000_session_9f8e7d6c-1111-4222-8333-444455556666";
    let body = serde_json::json!({"metadata": {"user_id": uid}, "messages": []});
    let mut msg = inbound(vec![], RawBody::json(body.clone()));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(
        msg.session_id.as_deref(),
        Some("9f8e7d6c-1111-4222-8333-444455556666")
    );
    // metadata 属请求语义，不改动报文
    assert_eq!(msg.body.as_json(), Some(&body));
}

/// claude-cli 2.x：metadata.user_id 为 JSON blob {"device_id":..,"session_id":..}。
#[tokio::test]
async fn claude_code_plugin_extracts_session_v2_json_blob() {
    let rt = runtime(
        "claude-code",
        include_str!("../../../plugins/harness/claude-code.lua"),
    );
    let uid = r#"{"device_id":"dev-1","account_uuid":"acc-1","session_id":"9f8e7d6c-2222-4333-8444-555566667777"}"#;
    let body = serde_json::json!({"metadata": {"user_id": uid}, "messages": []});
    let mut msg = inbound(vec![], RawBody::json(body));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(
        msg.session_id.as_deref(),
        Some("9f8e7d6c-2222-4333-8444-555566667777")
    );
}

#[tokio::test]
async fn claude_code_plugin_falls_back_to_full_user_id() {
    let rt = runtime(
        "claude-code",
        include_str!("../../../plugins/harness/claude-code.lua"),
    );
    let mut msg = inbound(
        vec![],
        RawBody::json(serde_json::json!({"metadata": {"user_id": "plain-user"}, "messages": []})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("plain-user"));
}

#[tokio::test]
async fn claude_code_plugin_ignores_non_json_and_missing_metadata() {
    let rt = runtime(
        "claude-code",
        include_str!("../../../plugins/harness/claude-code.lua"),
    );
    for body in [
        RawBody::text("not-json"),
        RawBody::text(""),
        RawBody::json(serde_json::json!({"messages": []})),
    ] {
        let mut msg = inbound(vec![], body);
        rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
        assert!(msg.session_id.is_none());
    }
}

/// 三级粒度并存时取最稳定的 session-id，三个头一并剥除。
#[tokio::test]
async fn codex_plugin_prefers_session_id_and_strips_all() {
    let rt = runtime("codex", include_str!("../../../plugins/harness/codex.lua"));
    let mut msg = inbound(
        vec![
            ("session-id", "sess-1"),
            ("thread-id", "thr-1"),
            ("x-codex-window-id", "thr-1:3"),
        ],
        RawBody::json(serde_json::json!({})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("sess-1"));
    for h in ["session-id", "thread-id", "x-codex-window-id"] {
        assert!(msg.header(h).is_none(), "{h} 应被剥除");
    }
}

/// 只有窗口头时（旧版/精简客户端）也能产身份。
#[tokio::test]
async fn codex_plugin_falls_back_to_window_id() {
    let rt = runtime("codex", include_str!("../../../plugins/harness/codex.lua"));
    let mut msg = inbound(
        vec![("x-codex-window-id", "win-42")],
        RawBody::json(serde_json::json!({})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("win-42"));
    assert!(msg.header("x-codex-window-id").is_none());
}

/// grok-build：session-id 优先于 conv-id，两级头一并剥除。
#[tokio::test]
async fn grok_build_prefers_session_id() {
    let rt = runtime(
        "grok-build",
        include_str!("../../../plugins/harness/grok-build.lua"),
    );
    let mut msg = inbound(
        vec![
            ("x-grok-session-id", "gs-1"),
            ("x-grok-conv-id", "gc-1"),
            ("x-grok-req-id", "gr-9"),
        ],
        RawBody::json(serde_json::json!({})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("gs-1"));
    assert!(msg.header("x-grok-session-id").is_none());
    assert!(msg.header("x-grok-conv-id").is_none());
    // 请求级头非身份源，不动
    assert_eq!(msg.header("x-grok-req-id"), Some("gr-9"));
}

/// grok-build：只有 conv-id 时（无会话上下文的对话请求）也能产身份。
#[tokio::test]
async fn grok_build_falls_back_to_conv_id() {
    let rt = runtime(
        "grok-build",
        include_str!("../../../plugins/harness/grok-build.lua"),
    );
    let mut msg = inbound(
        vec![("x-grok-conv-id", "gc-2")],
        RawBody::json(serde_json::json!({})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("gc-2"));
}

/// kimi-code：openai 系协议读 prompt_cache_key，anthropic 协议读 metadata.user_id，
/// 字段属请求语义照常转发不剥。
#[tokio::test]
async fn kimi_code_reads_body_session_fields() {
    let rt = runtime(
        "kimi-code",
        include_str!("../../../plugins/harness/kimi-code.lua"),
    );
    let body = serde_json::json!({"prompt_cache_key": "kimi-sess-1", "messages": []});
    let mut msg = inbound(vec![], RawBody::json(body.clone()));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("kimi-sess-1"));
    assert_eq!(msg.body.as_json(), Some(&body), "body 不应被改动");

    let body = serde_json::json!({"metadata": {"user_id": "kimi-sess-2"}, "messages": []});
    let mut msg = inbound(vec![], RawBody::json(body.clone()));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("kimi-sess-2"));
    assert_eq!(msg.body.as_json(), Some(&body));
}

/// kimi-code：无亲和字段时不产身份。
#[tokio::test]
async fn kimi_code_passes_through_without_fields() {
    let rt = runtime(
        "kimi-code",
        include_str!("../../../plugins/harness/kimi-code.lua"),
    );
    let mut msg = inbound(vec![], RawBody::json(serde_json::json!({"messages": []})));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert!(msg.session_id.is_none());
}

/// dsh：会话头优先；缺失时 body 的 dsh_session_log.session.id 回退。
#[tokio::test]
async fn dsh_bridges_header_and_body_fallback() {
    let rt = runtime("dsh", include_str!("../../../plugins/harness/dsh.lua"));

    let mut msg = inbound(
        vec![
            ("x-deepseek-harness-session-id", "dsh-sess-1"),
            ("x-deepseek-harness-user-id", "install-1"),
        ],
        RawBody::json(serde_json::json!({})),
    );
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("dsh-sess-1"));
    assert!(msg.header("x-deepseek-harness-session-id").is_none());
    // 安装级 id 非身份源，不动
    assert_eq!(msg.header("x-deepseek-harness-user-id"), Some("install-1"));

    let body =
        serde_json::json!({"dsh_session_log": {"session": {"id": "dsh-sess-2"}}, "messages": []});
    let mut msg = inbound(vec![], RawBody::json(body.clone()));
    rt.on_client_request_raw(&ctx(), &mut msg).await.unwrap();
    assert_eq!(msg.session_id.as_deref(), Some("dsh-sess-2"));
    assert_eq!(msg.body.as_json(), Some(&body), "dsh_session_log 照常转发");
}
