//! Anthropic Messages API Adapter（入口 + 上游）。
//!
//! 作为 Provider 侧，把 Core IR 转换为 Anthropic Messages 请求，并把
//! Anthropic 响应/SSE 转换回 Core IR；作为 Client 侧，把 Anthropic Messages
//! 入口请求转为 Core IR，并把 Core 响应/SSE 转回 Anthropic 格式。四个象限
//! 均实现：[`crate::adapter::ClientAdapter`] / [`crate::adapter::ClientStreamAdapter`] /
//! [`crate::adapter::ProviderAdapter`] / [`crate::adapter::ProviderStreamAdapter`]。

pub mod client;
pub mod dto;
pub mod provider;
pub mod stream;

/// Anthropic Adapter（无状态；同时作为入口与上游）。
#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicAdapter;
