//! OpenAI Chat Completions API Adapter（入口 + 上游）。
//!
//! 对外暴露 `/v1/chat/completions`，亦可作为上游调用 OpenAI 官方或任意兼容
//! Chat Completions 的服务。入口与上游格式一致，故共享一套 Chat ↔ Core 转换
//! （见 [`dto`]）。四象限均实现。

pub mod client;
pub mod dto;
pub mod provider;
pub mod stream;

/// OpenAI Chat Adapter（无状态；同时作为入口与上游）。
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiChatAdapter;
