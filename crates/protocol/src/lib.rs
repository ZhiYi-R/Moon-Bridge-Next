//! Moon Bridge Next 协议层。
//!
//! 本层位于 `core` 之上、`service/gateway` 之下，职责是「协议转换」：
//! - [`adapter`]：入口/上游 × 非流式/流式 四象限 Adapter 契约，以及
//!   [`adapter::ProviderEndpoint`] / [`adapter::UpstreamRequest`] 等数据载体。
//! - [`raw`]：报文层数据载体（[`raw::RawMessage`] / [`raw::RawChunk`] /
//!   [`raw::RawVerdict`]），供 Lua 插件直接读写出入站 HTTP 报文。
//! - [`hooks`]：[`hooks::PluginHooks`] trait —— 插件扩展点（Core 层 + 报文层），
//!   protocol 通过它调用插件，从而与 plugin crate 解耦（不反向依赖）。
//! - [`registry`]：[`registry::Registry`] 按协议索引 Adapter；
//!   [`registry::builtin_registry`] 装配内置 Adapter。
//! - [`adapters`]：内置协议实现（openai_responses / anthropic / openai_chat /
//!   google_genai，均含入口 + 上游四象限）。
//! - [`context`]：[`context::ReqCtx`] 请求上下文。
//!
//! 依赖方向：protocol → core（不依赖 plugin/store/gateway）。

pub mod adapter;
pub mod adapters;
pub mod context;
pub mod hooks;
pub mod raw;
pub mod registry;

pub use adapter::{
    ClientAdapter, ClientStreamAdapter, ProviderAdapter, ProviderEndpoint, ProviderStreamAdapter,
    UpstreamRequest,
};
pub use context::ReqCtx;
pub use hooks::{NoopHooks, PluginHooks};
pub use raw::{ChunkStage, ChunkVerdict, RawBody, RawChunk, RawMessage, RawStage, RawVerdict};
pub use registry::{builtin_registry, Registry};

// 便于下游直接引用内置 Adapter 类型
pub use adapters::anthropic::AnthropicAdapter;
pub use adapters::google_genai::GoogleGenAiAdapter;
pub use adapters::openai_chat::OpenAiChatAdapter;
pub use adapters::openai_responses::OpenAiResponsesAdapter;
