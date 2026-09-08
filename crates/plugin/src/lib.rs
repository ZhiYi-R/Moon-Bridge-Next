//! Moon Bridge Next 插件层。
//!
//! 以 `mlua`（Lua 5.4 + async + send + serialize）承载插件脚本，实现 protocol 层
//! 定义的 [`moonbridge_protocol::PluginHooks`]。核心组件：
//! - [`runtime::LuaRuntime`]：单插件运行时，加载脚本并按引用语义调用钩子。
//! - [`registry::LuaPluginRegistry`]：串联多插件，按 capability 门控，impl PluginHooks。
//! - [`host`]：`mb.*` 白名单宿主 API 与 Lua 沙箱。
//! - [`quota`]：沙箱配额（指令计数 / 内存上限 / 执行超时 / body 降级）。
//! - [`bridge::HostBridge`]：受控宿主能力契约（HTTP 子请求 / 跨 provider 调用），
//!   由 gateway 实现并注入，维持 plugin 不依赖 gateway 的单向依赖。
//! - [`convert`]：Core IR / Raw 报文 ↔ Lua table 转换。
//! - [`session::SessionStore`]：跨请求会话状态。
//! - [`manifest::Manifest`]：插件清单与能力声明。
//!
//! 依赖方向：plugin → core, protocol（不依赖 store/gateway）。

pub mod bridge;
pub mod convert;
pub mod error;
pub mod host;
pub mod manifest;
pub mod quota;
pub mod registry;
pub mod runtime;
pub mod session;

pub use bridge::{HostBridge, HttpRequest, HttpResponse};
pub use error::{PluginError, Result};
pub use manifest::{Manifest, CAP_CORE, CAP_RAW_REQUEST, CAP_RAW_RESPONSE, CAP_RAW_STREAM};
pub use quota::{ExecutionBudget, SandboxLimits};
pub use registry::LuaPluginRegistry;
pub use runtime::LuaRuntime;
pub use session::SessionStore;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use moonbridge_core::{CoreRequest, Protocol};
    use moonbridge_protocol::{
        ChunkStage, ChunkVerdict, RawBody, RawChunk, RawMessage, RawStage, RawVerdict, ReqCtx,
    };
    use serde_json::json;
    use std::sync::Arc;

    /// 测试用宿主桥：HTTP 恒返回 200，provider_invoke 未实现。
    struct MockBridge;

    #[async_trait]
    impl HostBridge for MockBridge {
        async fn http_request(&self, _req: HttpRequest) -> std::result::Result<HttpResponse, String> {
            Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body: json!({"ok": true}),
            })
        }
        async fn provider_invoke(
            &self,
            _provider: &str,
            _model: &str,
            _req: CoreRequest,
        ) -> std::result::Result<moonbridge_core::CoreResponse, String> {
            Err("provider_invoke 未在测试中实现".to_string())
        }
    }

    fn bridge() -> Arc<dyn HostBridge> {
        Arc::new(MockBridge)
    }

    fn load(name: &str, script: &str) -> LuaRuntime {
        LuaRuntime::new(name, script, &json!({}), bridge(), SessionStore::new()).unwrap()
    }

    #[tokio::test]
    async fn core_on_request_mutates_system() {
        let rt = load(
            "inject",
            r#"
            MB = {
              name = "inject",
              capabilities = { "core" },
              on_request = function(ctx, req)
                req.system[#req.system + 1] = { type = "text", text = "injected-by-lua" }
                return req
              end,
            }
            "#,
        );
        assert!(rt.manifest.needs_core());
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut req = CoreRequest::new("m");
        rt.on_request(&ctx, &mut req).await.unwrap();
        assert_eq!(req.system.len(), 1);
        match &req.system[0] {
            moonbridge_core::ContentBlock::Text { text } => assert_eq!(text, "injected-by-lua"),
            other => panic!("期望 text 块，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn raw_upstream_request_rewrites_headers_and_body() {
        let rt = load(
            "raw",
            r#"
            MB = {
              name = "raw",
              capabilities = { "raw_request" },
              on_upstream_request_raw = function(ctx, msg)
                mb.headers.set(msg.headers, "x-custom", "1")
                msg.body.model = "rewritten"
                return msg
              end,
            }
            "#,
        );
        assert!(rt.manifest.needs_raw_request());
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut msg = RawMessage {
            stage: RawStage::UpstreamRequest,
            protocol: Protocol::Anthropic,
            provider: Some("deepseek".into()),
            method: Some("POST".into()),
            url: Some("https://api.example.com/v1/messages".into()),
            status: None,
            headers: vec![("content-type".into(), "application/json".into())],
            body: RawBody::json(json!({"model": "orig"})),
        };
        let verdict = rt.on_upstream_request_raw(&ctx, &mut msg).await.unwrap();
        assert!(matches!(verdict, RawVerdict::Pass));
        assert!(msg
            .headers
            .iter()
            .any(|(k, v)| k == "x-custom" && v == "1"));
        assert_eq!(msg.body.as_json().unwrap()["model"], "rewritten");
    }

    #[tokio::test]
    async fn raw_short_circuit_verdict() {
        let rt = load(
            "sc",
            r#"
            MB = {
              name = "sc",
              capabilities = { "raw_request" },
              on_client_request_raw = function(ctx, msg)
                return { action = "short_circuit", status = 429, body = { error = "rate limited" } }
              end,
            }
            "#,
        );
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut msg = RawMessage {
            stage: RawStage::ClientRequest,
            protocol: Protocol::OpenAiResponse,
            provider: None,
            method: Some("POST".into()),
            url: Some("/v1/responses".into()),
            status: None,
            headers: vec![],
            body: RawBody::json(json!({})),
        };
        let verdict = rt.on_client_request_raw(&ctx, &mut msg).await.unwrap();
        match verdict {
            RawVerdict::ShortCircuit { status, body, .. } => {
                assert_eq!(status, 429);
                assert_eq!(body.as_json().unwrap()["error"], "rate limited");
            }
            other => panic!("期望 short_circuit，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn async_http_host_call() {
        let rt = load(
            "http",
            r#"
            MB = {
              name = "http",
              capabilities = { "core" },
              on_request = function(ctx, req)
                local resp = mb.http.request({ method = "GET", url = "https://example.com" })
                req.model_alias = "status_" .. tostring(resp.status)
                return req
              end,
            }
            "#,
        );
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut req = CoreRequest::new("m");
        rt.on_request(&ctx, &mut req).await.unwrap();
        assert_eq!(req.model_alias, "status_200");
    }

    #[tokio::test]
    async fn crypto_and_session_host_api() {
        let rt = load(
            "util",
            r#"
            MB = {
              name = "util",
              capabilities = { "core" },
              on_request = function(ctx, req)
                mb.session.set("k", "v")
                req.model_alias = mb.session.get("k") .. ":" .. mb.crypto.sha256("abc"):sub(1, 4)
                return req
              end,
            }
            "#,
        );
        let mut ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        ctx.session_id = Some("sess-1".into());
        let mut req = CoreRequest::new("m");
        rt.on_request(&ctx, &mut req).await.unwrap();
        // sha256("abc") = ba7816bf...
        assert_eq!(req.model_alias, "v:ba78");
    }

    #[tokio::test]
    async fn manifest_capability_parsing() {
        let rt = load(
            "cap",
            r#"MB = { name = "cap", version = "1.2.3", capabilities = { "core", "raw_stream" } }"#,
        );
        assert_eq!(rt.manifest.version, "1.2.3");
        assert!(rt.manifest.needs_core());
        assert!(rt.manifest.needs_raw_stream());
        assert!(!rt.manifest.needs_raw_request());
    }

    /// 加载仓库中真实的示例插件（plugins/examples/*.lua），验证语法、
    /// manifest 解析与钩子执行均正确（交付物可用性回归）。
    #[tokio::test]
    async fn loads_repo_example_plugins() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/examples");

        // ── log_request.lua：core 能力，on_request 注入 system 提示 + 会话计数 ──
        let script = std::fs::read_to_string(dir.join("log_request.lua")).unwrap();
        let rt = LuaRuntime::new(
            "log_request",
            &script,
            &json!({ "reminder": "hello-from-config" }),
            bridge(),
            SessionStore::new(),
        )
        .unwrap();
        assert!(rt.manifest.needs_core());
        assert_eq!(rt.manifest.version, "0.1.0");

        let mut ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        ctx.session_id = Some("sess-1".into());
        let mut req = CoreRequest::new("claude-x");
        rt.on_request(&ctx, &mut req).await.unwrap();
        assert_eq!(req.system.len(), 1, "应注入一条 system 提示");
        match &req.system[0] {
            moonbridge_core::ContentBlock::Text { text } => assert_eq!(text, "hello-from-config"),
            other => panic!("期望 text 块，实际 {other:?}"),
        }
        // 同一会话再次请求：计数递增，再注入一条
        rt.on_request(&ctx, &mut req).await.unwrap();
        assert_eq!(req.system.len(), 2);

        // ── raw_rewrite.lua：raw_request + raw_stream 能力 ──
        let script2 = std::fs::read_to_string(dir.join("raw_rewrite.lua")).unwrap();
        let rt2 = LuaRuntime::new("raw_rewrite", &script2, &json!({}), bridge(), SessionStore::new())
            .unwrap();
        assert!(rt2.manifest.needs_raw_request());
        assert!(rt2.manifest.needs_raw_stream());

        // on_upstream_request_raw：改写头 + body 打补丁 + 收敛 max_tokens
        let mut msg = RawMessage {
            stage: RawStage::UpstreamRequest,
            protocol: Protocol::Anthropic,
            provider: Some("deepseek".into()),
            method: Some("POST".into()),
            url: Some("https://api.example.com/v1/messages".into()),
            status: None,
            headers: vec![("content-type".into(), "application/json".into())],
            body: RawBody::json(json!({ "model": "orig", "max_tokens": 99999 })),
        };
        let v = rt2.on_upstream_request_raw(&ctx, &mut msg).await.unwrap();
        assert!(matches!(v, RawVerdict::Pass));
        assert!(msg
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
        let body = msg.body.as_json().unwrap();
        assert_eq!(body["metadata"]["source"], "moonbridge-next");
        assert_eq!(body["max_tokens"], 8192, "超限的 max_tokens 应被收敛");

        // on_upstream_chunk_raw：丢弃 ping 心跳
        let mut chunk = RawChunk {
            stage: ChunkStage::UpstreamChunk,
            protocol: Protocol::Anthropic,
            provider: None,
            event: Some("ping".into()),
            data: RawBody::Empty,
            raw: String::new(),
        };
        let cv = rt2.on_upstream_chunk_raw(&ctx, &mut chunk).await.unwrap();
        assert!(matches!(cv, ChunkVerdict::Drop), "ping 应被丢弃");
    }

    /// 沙箱硬化：插件 on_request 内死循环应被指令计数 hook 中止（而非挂死）。
    #[tokio::test]
    async fn infinite_loop_aborted_by_instruction_limit() {
        let limits = SandboxLimits {
            max_instructions: 500_000,
            instruction_step: 100,
            ..SandboxLimits::default()
        };
        let rt = LuaRuntime::new_with_limits(
            "loop",
            r#"
            MB = { capabilities = { "core" },
              on_request = function(ctx, req)
                local i = 0
                while true do i = i + 1 end
                return req
              end }
            "#,
            &json!({}),
            bridge(),
            SessionStore::new(),
            limits,
        )
        .unwrap();
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut req = CoreRequest::new("m");
        let err = rt
            .on_request(&ctx, &mut req)
            .await
            .expect_err("死循环应被配额中止");
        let s = err.to_string();
        assert!(
            s.contains("指令数") || s.contains("超时"),
            "应因指令/超时配额中止: {s}"
        );
    }

    /// 沙箱硬化：超过内存上限的分配应报错（不实际占用巨量内存）。
    #[tokio::test]
    async fn memory_limit_aborts_big_allocation() {
        let limits = SandboxLimits {
            max_memory_bytes: 8 * 1024 * 1024,
            ..SandboxLimits::default()
        };
        let rt = LuaRuntime::new_with_limits(
            "mem",
            r#"
            MB = { capabilities = { "core" },
              on_request = function(ctx, req)
                local s = string.rep("x", 64 * 1024 * 1024)
                req.model_alias = #s
                return req
              end }
            "#,
            &json!({}),
            bridge(),
            SessionStore::new(),
            limits,
        )
        .unwrap();
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut req = CoreRequest::new("m");
        assert!(
            rt.on_request(&ctx, &mut req).await.is_err(),
            "超过内存上限应报错"
        );
    }

    /// 沙箱硬化：超大 raw body 降级为 nil + truncated 标记，且回写时保留原始报文。
    #[tokio::test]
    async fn oversized_body_degraded_and_preserved() {
        let limits = SandboxLimits {
            max_body_bytes: 32,
            ..SandboxLimits::default()
        };
        let rt = LuaRuntime::new_with_limits(
            "degrade",
            r#"
            MB = { capabilities = { "raw_request" },
              on_client_request_raw = function(ctx, msg)
                if msg.body ~= nil then return { action = "abort", message = "body 应被降级为 nil" } end
                if msg.body_truncated ~= true then return { action = "abort", message = "缺 truncated 标记" } end
                return msg
              end }
            "#,
            &json!({}),
            bridge(),
            SessionStore::new(),
            limits,
        )
        .unwrap();
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let original = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "this payload is definitely longer than 32 bytes" }]
        });
        let mut msg = RawMessage {
            stage: RawStage::ClientRequest,
            protocol: Protocol::OpenAiResponse,
            provider: None,
            method: Some("POST".into()),
            url: None,
            status: None,
            headers: vec![],
            body: RawBody::json(original.clone()),
        };
        let v = rt.on_client_request_raw(&ctx, &mut msg).await.unwrap();
        assert!(matches!(v, RawVerdict::Pass), "降级后插件应正常放行: {v:?}");
        assert_eq!(
            msg.body.as_json().unwrap(),
            &original,
            "原始 body 应被完整保留，未被截断污染"
        );
    }
}
