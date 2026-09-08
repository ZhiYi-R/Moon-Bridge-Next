//! OpenAI Responses API Adapter（入口 + 上游）。
//!
//! 作为入口对外暴露 `/v1/responses`（Codex CLI 等客户端）；作为上游可调用 OpenAI
//! 官方或兼容 Responses API 的服务。四象限均实现：[`crate::adapter::ClientAdapter`]
//! / [`crate::adapter::ClientStreamAdapter`] / [`crate::adapter::ProviderAdapter`]
//! / [`crate::adapter::ProviderStreamAdapter`]。

pub mod client;
pub mod dto;
pub mod provider;
pub mod stream;

/// OpenAI Responses Adapter（无状态；同时作为入口与上游）。
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiResponsesAdapter;
