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

    /// 沙箱硬化：`require` / `package` 必须与 `os`/`io` 一并移除。
    ///
    /// 历史缺陷：mlua `Lua::new()` 打开 ALL_SAFE，`io`/`os` 被登记在 `package.loaded`
    /// 里，插件 `require("io")` 可取回活表并 `io.open(path,"w")` 任意读写文件。
    #[tokio::test]
    async fn sandbox_blocks_module_require_escape() {
        let rt = load(
            "escape",
            r#"
            MB = { capabilities = { "raw_request" },
              on_client_request_raw = function(ctx, msg)
                local function probe(name, fn)
                  local ok, res = pcall(fn)
                  if ok and res ~= nil then return name .. " 仍可获取" end
                  return nil
                end
                local leaks = {}
                for _, v in ipairs({
                  "require", "package", "io", "os", "loadfile", "dofile",
                }) do
                  if type(_G[v]) == "function" or type(_G[v]) == "table" then
                    leaks[#leaks + 1] = v .. " 全局仍可达"
                  end
                end
                local r = probe("require('io')", function() return require("io") end)
                if r then leaks[#leaks + 1] = r end
                local r2 = probe("package.loaded.io", function() return package.loaded.io end)
                if r2 then leaks[#leaks + 1] = r2 end
                local r3 = probe("package.searchers", function() return package.searchers[3] end)
                if r3 then leaks[#leaks + 1] = r3 end
                if #leaks > 0 then
                  return { action = "abort", message = table.concat(leaks, "; ") }
                end
                return msg
              end }
            "#,
        );
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut msg = RawMessage {
            stage: RawStage::ClientRequest,
            protocol: Protocol::OpenAiResponse,
            provider: None,
            method: Some("POST".into()),
            url: None,
            status: None,
            headers: vec![],
            body: RawBody::json(json!({"model": "m"})),
        };
        let v = rt.on_client_request_raw(&ctx, &mut msg).await.unwrap();
        assert!(
            matches!(v, RawVerdict::Pass),
            "沙箱应已切断模块加载逃逸路: {v:?}"
        );
    }

    /// 沙箱硬化：死循环放进协程也必须被指令配额掐断。
    ///
    /// 历史缺陷：Lua debug hook 按线程生效且不被新建协程继承，
    /// `coroutine.wrap(function() while true do end end)()` 绕过 `max_instructions`
    /// 与 `call_timeout`，把该插件的 `Mutex<Lua>`（守卫跨 await 持有）永久卡死。
    ///
    /// 判定方式：每个用例产出一个「返回 (ok, err)」的表达式来起协程死循环。
    /// 若配额生效，必然 `ok == false` 且 err 含配额文案；循环真的跑完则为 SURVIVED，
    /// 出错但不是配额（说明是被别的路径掐断）则为 OTHER。注意 `coroutine.resume`
    /// 本身吞错误、以 (false, err) 返回，故不能靠 pcall 一律包裹。
    #[tokio::test]
    async fn coroutine_loop_aborted_by_instruction_limit() {
        let spin = "local i = 0 while true do i = i + 1 end";
        for (label, expr) in [
            (
                "coroutine.wrap",
                &format!("pcall(function() coroutine.wrap(function() {spin} end)() end)"),
            ),
            (
                "coroutine.create+resume",
                &format!("coroutine.resume(coroutine.create(function() {spin} end))"),
            ),
            (
                "嵌套两层协程",
                &format!(
                    "pcall(function() coroutine.wrap(function() \
                     coroutine.wrap(function() {spin} end)() end)() end)"
                ),
            ),
            (
                "抹掉宿主 binder 后仍须被约束",
                &format!(
                    "pcall(function() __mb_bind_thread = nil \
                     coroutine.wrap(function() {spin} end)() end)"
                ),
            ),
            (
                "覆写 coroutine.create 后 wrap 仍受约束",
                &format!(
                    "pcall(function() coroutine.create = function(f) return {{}} end \
                     coroutine.wrap(function() {spin} end)() end)"
                ),
            ),
            (
                "pcall 内死循环（同线程，对照组）",
                &format!("pcall(function() {spin} end)"),
            ),
        ] {
            let script = format!(
                r#"
                MB = {{ capabilities = {{ "core" }},
                  on_request = function(ctx, req)
                    local ok, e = {expr}
                    if ok then
                      req.model = "SURVIVED"
                    elseif tostring(e):find("指令数") or tostring(e):find("超时") then
                      req.model = "ABORTED"
                    else
                      req.model = "OTHER:" .. tostring(e)
                    end
                    return req
                  end }}
                "#,
            );
            let limits = SandboxLimits {
                max_instructions: 500_000,
                instruction_step: 100,
                ..SandboxLimits::default()
            };
            let rt = LuaRuntime::new_with_limits(
                "coloop",
                &script,
                &json!({}),
                bridge(),
                SessionStore::new(),
                limits,
            )
            .unwrap();
            let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
            let mut req = CoreRequest::new("m");
            // 用例内部已 pcall 兜住配额错误，故钩子本身应返回 Ok；判定看 req.model
            rt.on_request(&ctx, &mut req)
                .await
                .expect("钩子内已捕获配额错误，不应冒泡");
            assert_eq!(
                req.model, "ABORTED",
                "{label}: 协程内死循环应被指令/超时配额中止"
            );
        }
    }

    /// 协程补丁必须保留合法语义：yield/resume 传值、多返回值、错误传播。
    #[tokio::test]
    async fn coroutine_patch_preserves_legit_semantics() {
        let rt = load(
            "colegit",
            r#"
            MB = { capabilities = { "core" },
              on_request = function(ctx, req)
                -- create + 双次 resume 传值
                local co = coroutine.create(function(a)
                  local b = coroutine.yield(a + 1)
                  return b + 100
                end)
                local _, v1 = coroutine.resume(co, 1)
                local _, v2 = coroutine.resume(co, 50)
                -- wrap 多返回值
                local w = coroutine.wrap(function()
                  coroutine.yield("x", "y")
                  return "z"
                end)
                local a, b = w()
                local c = w()
                -- wrap 错误传播
                local bad = coroutine.wrap(function() error("boom") end)
                local ok, e = pcall(bad)
                req.model = string.format("%d/%d/%s%s-%s/%s/%s",
                  v1, v2, a, b, c, tostring(ok), tostring(e):find("boom") and "boom" or "无boom")
                return req
              end }
            "#,
        );
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut req = CoreRequest::new("m");
        rt.on_request(&ctx, &mut req).await.unwrap();
        assert_eq!(req.model, "2/150/xy-z/false/boom", "协程语义应未被补丁破坏");
    }

    /// 生命周期钩子接线回归：`MB.init` / `MB.shutdown` 必须分别由
    /// `init_all` / `shutdown_all` 触发（此前二者在整仓零调用点，是死钩子）。
    #[tokio::test]
    async fn registry_lifecycle_hooks_are_invoked() {
        use moonbridge_protocol::PluginHooks;
        let rt = Arc::new(load(
            "lifecycle",
            r#"
            MB = { version = "0.1.0", capabilities = { "core" } }
            function MB.init() INIT_N = (INIT_N or 0) + 1 end
            function MB.shutdown() DOWN_N = (DOWN_N or 0) + 1 end
            function MB.on_request(ctx, req)
              req.model = tostring(INIT_N or -1) .. "/" .. tostring(DOWN_N or -1)
              return req
            end
            "#,
        ));
        let reg = LuaPluginRegistry::new(vec![rt], Default::default(), SessionStore::new());
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        // 用 on_request 读回插件内计数（闭包返回借用 reg 的 future 过不了生命周期检查）
        macro_rules! probe {
            () => {{
                let mut r = CoreRequest::new("m");
                reg.on_request(&ctx, &mut r).await.unwrap();
                r.model
            }};
        }

        assert_eq!(probe!(), "-1/-1", "未调用生命周期时两个计数都应为初值");
        reg.init_all().await;
        assert_eq!(probe!(), "1/-1", "init_all 必须触发 MB.init，且只触发一次");
        reg.shutdown_all().await;
        assert_eq!(probe!(), "1/1", "shutdown_all 必须触发 MB.shutdown");
        reg.shutdown_all().await;
        assert_eq!(probe!(), "1/2", "shutdown 可重复调用，计数如实反映");
    }

    /// 单个插件 `MB.init` 抛错只应记 warn，不得阻断其余插件初始化（与钩子容错一致）。
    #[tokio::test]
    async fn init_all_survives_failing_plugin() {
        use moonbridge_protocol::PluginHooks;
        let boom = Arc::new(load(
            "boom",
            r#"
            MB = { version = "0.1.0", capabilities = { "core" } }
            function MB.init() error("init 失败") end
            "#,
        ));
        let ok = Arc::new(load(
            "ok",
            r#"
            MB = { version = "0.1.0", capabilities = { "core" } }
            function MB.init() INITED = true end
            function MB.on_request(ctx, req)
              req.model = tostring(INITED or false)
              return req
            end
            "#,
        ));
        let reg = LuaPluginRegistry::new(vec![boom, ok], Default::default(), SessionStore::new());
        reg.init_all().await;

        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let mut req = CoreRequest::new("m");
        reg.on_request(&ctx, &mut req).await.unwrap();
        assert_eq!(req.model, "true", "前一个插件 init 抛错不得影响后续插件");
    }

    async fn model_for(reg: &LuaPluginRegistry, ctx: &ReqCtx) -> String {
        use moonbridge_protocol::PluginHooks;
        let mut req = CoreRequest::new("m");
        reg.on_request(ctx, &mut req).await.unwrap();
        req.model
    }

    /// 会话淘汰的回收路径：`forget_session` 必须清空该会话在插件侧的桶，
    /// 且不得波及别的会话（含 `None` 桶）。
    #[tokio::test]
    async fn forget_session_clears_only_that_session() {
        use moonbridge_protocol::PluginHooks;
        let store = SessionStore::new();
        let rt = Arc::new(
            LuaRuntime::new(
                "counter",
                r#"
                MB = { version = "0.1.0", capabilities = { "core" } }
                function MB.on_request(ctx, req)
                  local n = (mb.session.get("n") or 0) + 1
                  mb.session.set("n", n)
                  req.model = tostring(n)
                  return req
                end
                "#,
                &json!({}),
                bridge(),
                store.clone(),
            )
            .unwrap(),
        );
        let reg = LuaPluginRegistry::new(vec![rt], Default::default(), store);

        let mut s1 = ReqCtx::new("r1", Protocol::Anthropic);
        s1.session_id = Some("S1".into());
        let mut s2 = ReqCtx::new("r2", Protocol::Anthropic);
        s2.session_id = Some("S2".into());
        let none = ReqCtx::new("r3", Protocol::Anthropic);

        assert_eq!(model_for(&reg, &s1).await, "1");
        assert_eq!(model_for(&reg, &s1).await, "2", "同会话内应累积");
        assert_eq!(model_for(&reg, &s2).await, "1", "另一会话应独立计数");
        assert_eq!(model_for(&reg, &none).await, "1", "无会话也自成一桶");

        reg.forget_session("S1").await;
        assert_eq!(model_for(&reg, &s1).await, "1", "被 forget 的会话状态应清零");
        assert_eq!(model_for(&reg, &s2).await, "2", "其它会话不得被波及");
        assert_eq!(model_for(&reg, &none).await, "2", "None 桶不得被波及");
    }
}
