//! `PluginHooks` trait：插件扩展点，定义在 protocol 层。
//!
//! protocol/gateway 在请求处理链路的固定位置调用这些钩子；plugin crate 用 mlua
//! 实现该 trait（`LuaPluginRegistry`）。protocol 永不依赖 plugin——二者仅通过本
//! trait 解耦（对应 Moon Bridge 的 `CorePluginHooks` 函数结构体思路）。
//!
//! 钩子分两组：
//! - **Core IR 语义层**：操作协议中立的 `CoreRequest`/`CoreResponse`/`CoreStreamEvent`。
//! - **出入站原始报文层**：操作 `RawMessage`/`RawChunk`（headers/body/SSE chunk），
//!   位于协议转换最外层，可改写、短路或中止。

use async_trait::async_trait;
use moonbridge_core::{ContentBlock, CoreRequest, CoreResponse, CoreStreamEvent, Result, Tool};

use crate::context::ReqCtx;
use crate::raw::{ChunkVerdict, RawChunk, RawMessage, RawVerdict};

/// 插件钩子集合。所有方法均有 no-op 默认实现，插件按需覆盖。
#[async_trait]
pub trait PluginHooks: Send + Sync {
    // ── Core IR 语义层 ──────────────────────────────────────────────

    /// CoreRequest 构建后、发往上游前修改请求（对应 MutateCoreRequest）。
    async fn on_request(&self, _ctx: &ReqCtx, _req: &mut CoreRequest) -> Result<()> {
        Ok(())
    }

    /// 注入额外工具（对应 ToolInjector）。
    async fn inject_tools(&self, _ctx: &ReqCtx) -> Result<Vec<Tool>> {
        Ok(Vec::new())
    }

    /// CoreResponse 构建后处理（对应 PostProcessResponse）。
    async fn on_response(&self, _ctx: &ReqCtx, _resp: &mut CoreResponse) -> Result<()> {
        Ok(())
    }

    /// 处理单个流事件；返回 `true` 表示丢弃该事件（对应 StreamInterceptor）。
    async fn on_stream_event(
        &self,
        _ctx: &ReqCtx,
        _ev: &mut CoreStreamEvent,
    ) -> Result<bool> {
        Ok(false)
    }

    /// 过滤/检查单个内容块；返回 `true` 表示跳过（对应 ContentFilter）。
    async fn filter_content(&self, _ctx: &ReqCtx, _block: &mut ContentBlock) -> Result<bool> {
        Ok(false)
    }

    /// 转换上游错误消息（对应 ErrorTransformer）。
    async fn transform_error(&self, _ctx: &ReqCtx, msg: &str) -> Result<String> {
        Ok(msg.to_string())
    }

    // ── 出入站原始报文层（协议转换最外层）──────────────────────────

    /// 入站请求（客户端 → 网关），解析为 Core 之前。
    async fn on_client_request_raw(
        &self,
        _ctx: &ReqCtx,
        _m: &mut RawMessage,
    ) -> Result<RawVerdict> {
        Ok(RawVerdict::Pass)
    }

    /// 出站请求（网关 → 上游），发送之前。
    async fn on_upstream_request_raw(
        &self,
        _ctx: &ReqCtx,
        _m: &mut RawMessage,
    ) -> Result<RawVerdict> {
        Ok(RawVerdict::Pass)
    }

    /// 入站响应（上游 → 网关，非流式），解析为 Core 之前。
    async fn on_upstream_response_raw(
        &self,
        _ctx: &ReqCtx,
        _m: &mut RawMessage,
    ) -> Result<RawVerdict> {
        Ok(RawVerdict::Pass)
    }

    /// 出站响应（网关 → 客户端，非流式），回写之前。
    async fn on_client_response_raw(
        &self,
        _ctx: &ReqCtx,
        _m: &mut RawMessage,
    ) -> Result<RawVerdict> {
        Ok(RawVerdict::Pass)
    }

    /// 上游 SSE chunk（流式），解码为 Core 事件之前。
    async fn on_upstream_chunk_raw(
        &self,
        _ctx: &ReqCtx,
        _c: &mut RawChunk,
    ) -> Result<ChunkVerdict> {
        Ok(ChunkVerdict::Forward)
    }

    /// 回写客户端的 SSE chunk（流式），写出之前。
    async fn on_client_chunk_raw(
        &self,
        _ctx: &ReqCtx,
        _c: &mut RawChunk,
    ) -> Result<ChunkVerdict> {
        Ok(ChunkVerdict::Forward)
    }

    /// 该插件是否对指定模型别名启用。
    fn enabled_for_model(&self, _model: &str) -> bool {
        true
    }

    // ── 生命周期（非请求作用域）───────────────────────────────────

    /// 网关开始服务前通知所有插件执行各自的 `MB.init`。默认 no-op。
    ///
    /// 不经 capability、不经 provider 三态门控：初始化是插件自身的事，与该插件
    /// 对哪些请求生效无关（被 provider 强制启用的插件同样需要 init）。
    async fn init_all(&self) {}

    /// 网关停止时通知所有插件执行各自的 `MB.shutdown`，用于释放插件侧资源。
    ///
    /// 默认 no-op：只有 Lua 注册表需要扇出。与 [`Self::init_all`] 同点成对
    /// （`server::serve_with_shutdown`），故插件看到的 shutdown 次数与 init 次数一致。
    async fn shutdown_all(&self) {}

    /// 某个会话彻底不再活跃时被调用，用于丢弃该会话在插件侧的累积状态。
    ///
    /// 由会话活跃表淘汰（FIFO 超深）触发。默认 no-op。
    async fn forget_session(&self, _session_id: &str) {}
}

/// 空钩子实现：无插件启用时使用，全部走 no-op 默认。
pub struct NoopHooks;

#[async_trait]
impl PluginHooks for NoopHooks {}
