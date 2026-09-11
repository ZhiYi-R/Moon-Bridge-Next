//! Lua 插件注册表：串联多个已启用插件，实现 [`PluginHooks`]。
//!
//! 门控策略：按插件 manifest 声明的 capability 决定是否触发某类钩子——
//! 未声明 `raw_stream` 的插件在流式每-chunk 完全不产生 Lua 调用（零开销）。
//! 容错策略：单个插件钩子出错只记 warn 并跳过，不拖垮整条请求链路。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use moonbridge_core::{
    ContentBlock, CoreRequest, CoreResponse, CoreStreamEvent, Result as CoreResult, Tool,
};
use moonbridge_protocol::{
    ChunkVerdict, PluginHooks, RawChunk, RawMessage, RawVerdict, ReqCtx,
};

use crate::manifest::{CAP_CORE, CAP_RAW_REQUEST, CAP_RAW_RESPONSE, CAP_RAW_STREAM};
use crate::runtime::LuaRuntime;
use crate::session::SessionStore;

/// 某插件的全部作用域绑定（就近覆盖：route > model > provider > global）。
///
/// 与 `store` 的 `plugin_enabled_in_scope` 口径一致；此处做成零分配的嵌套表，
/// 因为流式每 chunk 钩子都会逐插件查询。
#[derive(Debug, Default)]
pub struct ScopeOverrides {
    /// scope='route'：scope_key（routes 表别名）→ enabled。
    pub route: HashMap<String, bool>,
    /// scope='model'：scope_key（上游模型名/别名）→ enabled。
    pub model: HashMap<String, bool>,
    /// scope='provider'：scope_key（provider key）→ enabled。
    pub provider: HashMap<String, bool>,
    /// scope='global'（scope_key 恒为空串）→ enabled。
    pub global: Option<bool>,
}

impl ScopeOverrides {
    /// 记录一条 binding。
    pub fn insert(&mut self, scope: &str, scope_key: String, enabled: bool) {
        match scope {
            "route" => {
                self.route.insert(scope_key, enabled);
            }
            "model" => {
                self.model.insert(scope_key, enabled);
            }
            "provider" => {
                self.provider.insert(scope_key, enabled);
            }
            "global" => self.global = Some(enabled),
            _ => {}
        }
    }

    /// 是否存在任一维度 enabled 的绑定（决定停用插件是否仍需加载）。
    pub fn any_enabled(&self) -> bool {
        self.global == Some(true)
            || self.route.values().any(|v| *v)
            || self.model.values().any(|v| *v)
            || self.provider.values().any(|v| *v)
    }
}

/// 已加载 Lua 插件的注册表。
pub struct LuaPluginRegistry {
    plugins: Vec<Arc<LuaRuntime>>,
    /// 作用域三态门控表：plugin_name → 各维度绑定。存在表项则以「就近作用域
    /// （route > model > provider > global）」的表项为准，否则回落插件全局
    /// `enabled`（即「跟随全局」）。由 store 的 `plugin_bindings` 装配。
    overrides: HashMap<String, ScopeOverrides>,
    /// 与所有插件运行时共享的会话状态存储，供会话淘汰时回收其桶。
    sessions: SessionStore,
}

impl LuaPluginRegistry {
    /// 由已加载的插件运行时、作用域绑定覆盖表与共享会话存储构造。
    pub fn new(
        plugins: Vec<Arc<LuaRuntime>>,
        overrides: HashMap<String, ScopeOverrides>,
        sessions: SessionStore,
    ) -> Self {
        Self {
            plugins,
            overrides,
            sessions,
        }
    }
    /// 空注册表。
    pub fn empty() -> Self {
        Self {
            plugins: Vec::new(),
            overrides: HashMap::new(),
            sessions: SessionStore::new(),
        }
    }
    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
    /// 插件数量。
    pub fn len(&self) -> usize {
        self.plugins.len()
    }
    /// 访问插件列表。
    pub fn plugins(&self) -> &[Arc<LuaRuntime>] {
        &self.plugins
    }

    /// 按 capability 过滤插件。
    fn by_cap(&self, cap: &'static str) -> impl Iterator<Item = &Arc<LuaRuntime>> + '_ {
        self.plugins.iter().filter(move |p| p.manifest.has(cap))
    }

    /// 判定插件在当前请求上是否生效。
    ///
    /// 就近作用域覆盖：route（routes 别名）> model（上游模型名）> provider >
    /// global > 插件全局 `enabled`。注意：客户端阶段 RAW 钩子在路由解析前
    /// 触发，此时 route/model/provider 均为 None，只能命中 global 或全局开关。
    fn effective(&self, p: &LuaRuntime, ctx: &ReqCtx) -> bool {
        let Some(t) = self.overrides.get(&p.name) else {
            return p.enabled;
        };
        for (map, key) in [
            (&t.route, ctx.route_alias.as_deref()),
            (
                &t.model,
                ctx.upstream_model.as_deref().or(Some(ctx.model_alias.as_str())),
            ),
            (&t.provider, ctx.provider_key.as_deref()),
        ] {
            if let Some(k) = key {
                if let Some(&v) = map.get(k) {
                    return v;
                }
            }
        }
        t.global.unwrap_or(p.enabled)
    }
    /// 按 capability + provider 三态门控过滤插件。
    fn eligible<'a>(
        &'a self,
        cap: &'static str,
        ctx: &'a ReqCtx,
    ) -> impl Iterator<Item = &'a Arc<LuaRuntime>> {
        self.by_cap(cap).filter(move |p| self.effective(p, ctx))
    }
}

#[async_trait]
impl PluginHooks for LuaPluginRegistry {
    // ── Core IR 语义层 ──────────────────────────────────────────────

    async fn on_request(&self, ctx: &ReqCtx, req: &mut CoreRequest) -> CoreResult<()> {
        for p in self.eligible(CAP_CORE, ctx) {
            if let Err(e) = p.on_request(ctx, req).await {
                tracing::warn!(plugin = %p.name, error = %e, "on_request 失败，跳过");
            }
        }
        Ok(())
    }

    async fn inject_tools(&self, ctx: &ReqCtx) -> CoreResult<Vec<Tool>> {
        let mut tools = Vec::new();
        for p in self.eligible(CAP_CORE, ctx) {
            match p.inject_tools(ctx).await {
                Ok(mut t) => tools.append(&mut t),
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "inject_tools 失败"),
            }
        }
        Ok(tools)
    }

    async fn on_response(&self, ctx: &ReqCtx, resp: &mut CoreResponse) -> CoreResult<()> {
        for p in self.eligible(CAP_CORE, ctx) {
            if let Err(e) = p.on_response(ctx, resp).await {
                tracing::warn!(plugin = %p.name, error = %e, "on_response 失败，跳过");
            }
        }
        Ok(())
    }

    async fn on_stream_event(&self, ctx: &ReqCtx, ev: &mut CoreStreamEvent) -> CoreResult<bool> {
        for p in self.eligible(CAP_CORE, ctx) {
            match p.on_stream_event(ctx, ev).await {
                Ok(true) => return Ok(true), // 丢弃
                Ok(false) => continue,
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "on_stream_event 失败"),
            }
        }
        Ok(false)
    }

    async fn filter_content(&self, ctx: &ReqCtx, block: &mut ContentBlock) -> CoreResult<bool> {
        for p in self.eligible(CAP_CORE, ctx) {
            match p.filter_content(ctx, block).await {
                Ok(true) => return Ok(true), // 跳过
                Ok(false) => continue,
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "filter_content 失败"),
            }
        }
        Ok(false)
    }

    async fn transform_error(&self, ctx: &ReqCtx, msg: &str) -> CoreResult<String> {
        let mut cur = msg.to_string();
        for p in self.eligible(CAP_CORE, ctx) {
            match p.transform_error(ctx, &cur).await {
                Ok(s) => cur = s,
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "transform_error 失败"),
            }
        }
        Ok(cur)
    }

    // ── 出入站原始报文层 ────────────────────────────────────────────

    async fn on_client_request_raw(
        &self,
        ctx: &ReqCtx,
        m: &mut RawMessage,
    ) -> CoreResult<RawVerdict> {
        for p in self.eligible(CAP_RAW_REQUEST, ctx) {
            match p.on_client_request_raw(ctx, m).await {
                Ok(RawVerdict::Pass) => continue,
                Ok(v) => return Ok(v), // ShortCircuit / Abort 立即生效
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "on_client_request_raw 失败"),
            }
        }
        Ok(RawVerdict::Pass)
    }

    async fn on_upstream_request_raw(
        &self,
        ctx: &ReqCtx,
        m: &mut RawMessage,
    ) -> CoreResult<RawVerdict> {
        for p in self.eligible(CAP_RAW_REQUEST, ctx) {
            match p.on_upstream_request_raw(ctx, m).await {
                Ok(RawVerdict::Pass) => continue,
                Ok(v) => return Ok(v),
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "on_upstream_request_raw 失败"),
            }
        }
        Ok(RawVerdict::Pass)
    }

    async fn on_upstream_response_raw(
        &self,
        ctx: &ReqCtx,
        m: &mut RawMessage,
    ) -> CoreResult<RawVerdict> {
        for p in self.eligible(CAP_RAW_RESPONSE, ctx) {
            match p.on_upstream_response_raw(ctx, m).await {
                Ok(RawVerdict::Pass) => continue,
                Ok(v) => return Ok(v),
                Err(e) => {
                    tracing::warn!(plugin = %p.name, error = %e, "on_upstream_response_raw 失败")
                }
            }
        }
        Ok(RawVerdict::Pass)
    }

    async fn on_client_response_raw(
        &self,
        ctx: &ReqCtx,
        m: &mut RawMessage,
    ) -> CoreResult<RawVerdict> {
        for p in self.eligible(CAP_RAW_RESPONSE, ctx) {
            match p.on_client_response_raw(ctx, m).await {
                Ok(RawVerdict::Pass) => continue,
                Ok(v) => return Ok(v),
                Err(e) => {
                    tracing::warn!(plugin = %p.name, error = %e, "on_client_response_raw 失败")
                }
            }
        }
        Ok(RawVerdict::Pass)
    }

    async fn on_upstream_chunk_raw(
        &self,
        ctx: &ReqCtx,
        c: &mut RawChunk,
    ) -> CoreResult<ChunkVerdict> {
        for p in self.eligible(CAP_RAW_STREAM, ctx) {
            match p.on_upstream_chunk_raw(ctx, c).await {
                Ok(ChunkVerdict::Forward) => continue,
                Ok(ChunkVerdict::Drop) => return Ok(ChunkVerdict::Drop),
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "on_upstream_chunk_raw 失败"),
            }
        }
        Ok(ChunkVerdict::Forward)
    }

    async fn on_client_chunk_raw(
        &self,
        ctx: &ReqCtx,
        c: &mut RawChunk,
    ) -> CoreResult<ChunkVerdict> {
        for p in self.eligible(CAP_RAW_STREAM, ctx) {
            match p.on_client_chunk_raw(ctx, c).await {
                Ok(ChunkVerdict::Forward) => continue,
                Ok(ChunkVerdict::Drop) => return Ok(ChunkVerdict::Drop),
                Err(e) => tracing::warn!(plugin = %p.name, error = %e, "on_client_chunk_raw 失败"),
            }
        }
        Ok(ChunkVerdict::Forward)
    }

    async fn init_all(&self) {
        for p in self.plugins.iter() {
            if let Err(e) = p.init().await {
                tracing::warn!(plugin = %p.name, error = %e, "MB.init 失败，插件继续加载");
            }
        }
    }

    async fn shutdown_all(&self) {
        // 逆序：后加载的插件先收尾，与其建立资源的顺序相反。
        for p in self.plugins.iter().rev() {
            if let Err(e) = p.shutdown().await {
                tracing::warn!(plugin = %p.name, error = %e, "MB.shutdown 失败");
            }
        }
    }

    async fn forget_session(&self, session_id: &str) {
        // 一次性清掉所有插件在该会话下的桶：SessionStore 内部按 (plugin, session, key)
        // 组织，clear_session 自行跨插件过滤，无需逐插件循环。
        self.sessions.clear_session(session_id);
    }
}
