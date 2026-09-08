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

/// 已加载 Lua 插件的注册表。
pub struct LuaPluginRegistry {
    plugins: Vec<Arc<LuaRuntime>>,
    /// Provider 维度三态门控表：(plugin_name, provider_key) → enabled。
    /// 存在表项则以表项为准（启用/禁用），否则回落插件全局 `enabled`
    /// （即「跟随全局」）。由 store 的 `plugin_bindings(scope='provider')` 装配。
    overrides: HashMap<(String, String), bool>,
}

impl LuaPluginRegistry {
    /// 由已加载的插件运行时与 provider 维度三态覆盖表构造。
    pub fn new(plugins: Vec<Arc<LuaRuntime>>, overrides: HashMap<(String, String), bool>) -> Self {
        Self { plugins, overrides }
    }
    /// 空注册表。
    pub fn empty() -> Self {
        Self {
            plugins: Vec::new(),
            overrides: HashMap::new(),
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
    /// 三态语义：provider 维度 binding 存在 → 以 binding 为准；否则跟随插件
    /// 全局 `enabled`。注意：客户端阶段 RAW 钩子在路由解析前触发，此时
    /// `ctx.provider_key` 为 None，按全局开关执行。
    fn effective(&self, p: &LuaRuntime, ctx: &ReqCtx) -> bool {
        match &ctx.provider_key {
            Some(pk) => self
                .overrides
                .get(&(p.name.clone(), pk.clone()))
                .copied()
                .unwrap_or(p.enabled),
            None => p.enabled,
        }
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

    fn enabled_for_model(&self, _model: &str) -> bool {
        // 加载入注册表的插件均视为启用；细粒度 scope（含 provider 维度三态）
        // 由各钩子内 `effective()` 依据门控表逐请求判定。
        true
    }
}
