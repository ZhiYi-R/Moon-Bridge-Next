//! Google Generative AI (Gemini) Adapter（入口 + 上游）。
//!
//! 作为 Provider 侧，把 Core IR 转换为 Gemini `generateContent` /
//! `streamGenerateContent` 请求，并把 Gemini 响应/SSE 转换回 Core IR；作为
//! Client 侧，把 Gemini 入口请求转为 Core IR，并把 Core 响应/SSE 转回 Gemini
//! 格式。四象限均实现：[`crate::adapter::ClientAdapter`] /
//! [`crate::adapter::ClientStreamAdapter`] / [`crate::adapter::ProviderAdapter`] /
//! [`crate::adapter::ProviderStreamAdapter`]。
//!
//! Gemini 与 Core IR 的主要差异：角色用 `user` / `model`（无独立 assistant），
//! system 走顶层 `systemInstruction`，工具调用用 `functionCall` / `functionResponse`
//! part（按函数名而非 id 关联结果），参数集中在 `generationConfig`。

pub mod client;
pub mod dto;
pub mod provider;
pub mod stream;

/// Google GenAI (Gemini) Adapter（无状态；同时作为入口与上游）。
#[derive(Debug, Default, Clone, Copy)]
pub struct GoogleGenAiAdapter;
