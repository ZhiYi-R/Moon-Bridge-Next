//! Moon Bridge Next 协议适配层。
//!
//! 位于 `core` 之上、`gateway` 之下，负责协议转换与插件扩展点定义：
//! - [`adapter`]：入口/上游 × 非流式/流式 四象限 Adapter 接口与请求载体。
//! - [`raw`]：原始报文载体（[`raw::RawMessage`] / [`raw::RawChunk`] / [`raw::RawVerdict`]）。
//! - [`hooks`]：[`hooks::PluginHooks`] trait，定义语义层与报文层插件钩子接口。
//! - [`registry`]：按协议索引 Adapter 的注册表。
//! - [`adapters`]：四种内置协议实现（OpenAI Responses、Anthropic、OpenAI Chat、Google GenAI）。
//! - [`context`]：[`context::ReqCtx`] 请求上下文。

pub mod adapter;
pub mod adapters;
pub mod context;
pub mod hooks;
pub mod raw;
pub mod registry;

pub use adapter::{
    ClientAdapter, ClientStreamAdapter, ProviderAdapter, ProviderEndpoint, ProviderStreamAdapter,
    StreamDecodeState, StreamEncodeState, UpstreamRequest,
};
pub use context::ReqCtx;
pub use hooks::{NoopHooks, PluginHooks};
pub use raw::{ChunkStage, ChunkVerdict, RawBody, RawChunk, RawMessage, RawStage, RawVerdict};
pub use registry::{builtin_registry, Registry};

pub use adapters::anthropic::AnthropicAdapter;
pub use adapters::google_genai::GoogleGenAiAdapter;
pub use adapters::openai_chat::OpenAiChatAdapter;
pub use adapters::openai_responses::OpenAiResponsesAdapter;
