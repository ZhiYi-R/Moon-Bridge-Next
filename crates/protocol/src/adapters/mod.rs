//! 内置协议 Adapter（四象限：入口 Client / 上游 Provider × 非流式 / 流式）。
//!
//! - [`openai_responses`]：OpenAI Responses（`/v1/responses`，入口 + 上游）。
//! - [`anthropic`]：Anthropic Messages（`/v1/messages`，入口 + 上游）。
//! - [`openai_chat`]：OpenAI Chat Completions（`/v1/chat/completions`，入口 + 上游）。
//! - [`google_genai`]：Google Generative AI / Gemini（入口 + 上游）。

pub mod anthropic;
pub mod google_genai;
pub mod openai_chat;
pub mod openai_responses;
