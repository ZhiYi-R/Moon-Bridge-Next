//! Google Generative AI (Gemini) 入口 Adapter —— 非流式部分。
//!
//! `to_core_request`：Gemini `generateContent` 请求 → CoreRequest
//! （contents/systemInstruction/tools/toolConfig/generationConfig 解析）。
//! `from_core_response`：CoreResponse → Gemini 响应对象（candidates 结构）。
//!
//! 与上游侧（[`super::provider`]）共享 [`super::dto`] 的转换，保证「Gemini 入口 →
//! Gemini 上游」等链路语义一致。
//!
//! 注：Gemini 的模型名在真实 API 中位于 URL 路径（`/models/{model}:generateContent`），
//! 请求体不含 `model`；此处优先取体内 `model` 字段，回退到 `ctx.model_alias`。

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse, Protocol, Reasoning, Result};
use serde_json::{json, Value};

use super::dto;
use super::GoogleGenAiAdapter;
use crate::adapters::effort_from_budget;
use crate::adapter::ClientAdapter;
use crate::context::ReqCtx;

#[async_trait]
impl ClientAdapter for GoogleGenAiAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::GoogleGenai
    }

    async fn to_core_request(&self, ctx: &ReqCtx, raw: Value) -> Result<CoreRequest> {
        let model = raw
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| ctx.model_alias.clone());
        let mut req = CoreRequest::new(model.clone());
        req.model_alias = model;

        // system：systemInstruction（兼容 snake_case 别名）
        let si = raw
            .get("systemInstruction")
            .or_else(|| raw.get("system_instruction"));
        req.system = dto::system_instruction_to_core(si);

        // messages：contents
        if let Some(contents) = raw.get("contents").and_then(|v| v.as_array()) {
            req.messages = dto::contents_to_core(contents);
        }

        // tools
        if let Some(tools) = raw.get("tools").and_then(|v| v.as_array()) {
            req.tools = dto::tools_to_core(tools);
        }
        // tool_choice：toolConfig
        req.tool_choice = dto::tool_config_to_core(raw.get("toolConfig").or_else(|| raw.get("tool_config")));

        // generationConfig
        if let Some(gc) = raw.get("generationConfig").or_else(|| raw.get("generation_config")) {
            req.max_tokens = gc.get("maxOutputTokens").and_then(|v| v.as_u64()).map(|v| v as u32);
            req.temperature = gc.get("temperature").and_then(|v| v.as_f64()).map(|v| v as f32);
            req.top_p = gc.get("topP").and_then(|v| v.as_f64()).map(|v| v as f32);
            if let Some(stops) = gc.get("stopSequences").and_then(|v| v.as_array()) {
                req.stop = stops.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            // thinkingConfig → req.reasoning：thinkingLevel 直接映射；
            // thinkingBudget 经档位表逆推 effort（与上游出站方向互逆）
            if let Some(tc) = gc.get("thinkingConfig") {
                let effort = tc
                    .get("thinkingLevel")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| {
                        tc.get("thinkingBudget")
                            .and_then(|v| v.as_u64())
                            .map(|b| effort_from_budget(b as u32).to_string())
                    });
                if let Some(effort) = effort.filter(|s| !s.is_empty()) {
                    req.reasoning = Some(Reasoning {
                        effort: Some(effort),
                        summary: None,
                    });
                }
            }
        }

        // Gemini 由端点（:generateContent / :streamGenerateContent）区分流式；
        // 入口体内如显式带 stream 字段则采纳，便于测试与兼容网关。
        req.stream = raw
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(ctx.stream);

        // 未解析顶层字段透传（Gemini→Gemini 保真）：safetySettings、
        // cachedContent、labels、session 等经 Core 无字段位，收进 meta 回传。
        // generationConfig 内未映射字段（responseSchema/responseMimeType/
        // thinkingConfig.includeThoughts 等）按子对象同样透传。
        if let Some(obj) = raw.as_object() {
            const KNOWN: &[&str] = &[
                "model", "systemInstruction", "system_instruction", "contents",
                "tools", "toolConfig", "tool_config", "generationConfig",
                "generation_config", "stream",
            ];
            let mut extra: serde_json::Map<String, Value> = obj
                .iter()
                .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if let Some(gc) = raw
                .get("generationConfig")
                .or_else(|| raw.get("generation_config"))
                .and_then(|v| v.as_object())
            {
                const GC_KNOWN: &[&str] = &[
                    "maxOutputTokens", "temperature", "topP", "stopSequences",
                    "thinkingConfig",
                ];
                let gc_extra: serde_json::Map<String, Value> = gc
                    .iter()
                    .filter(|(k, _)| !GC_KNOWN.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !gc_extra.is_empty() {
                    extra.insert("generationConfig".to_string(), Value::Object(gc_extra));
                }
            }
            if !extra.is_empty() {
                req.meta
                    .insert("gemini.extra".to_string(), Value::Object(extra));
            }
        }

        Ok(req)
    }

    async fn from_core_response(&self, _ctx: &ReqCtx, resp: CoreResponse) -> Result<Value> {
        let parts = dto::core_to_parts(&resp.content);
        let finish = dto::unmap_stop_reason(resp.stop_reason);
        Ok(json!({
            "candidates": [{
                "content": { "role": "model", "parts": parts },
                "finishReason": finish,
                "index": 0,
            }],
            "usageMetadata": dto::usage_object(&resp.usage),
            "modelVersion": resp.model,
            "responseId": resp.id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{ContentBlock, StopReason, Usage};

    #[tokio::test]
    async fn parses_gemini_request() {
        let raw = json!({
            "model": "gemini-2.0-flash",
            "systemInstruction": { "parts": [{ "text": "Be brief" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "Hi" }] },
                { "role": "model", "parts": [{ "functionCall": { "name": "get_time", "args": {"tz":"UTC"} } }] },
                { "role": "user", "parts": [{ "functionResponse": { "name": "get_time", "response": {"result":"12:00"} } }] }
            ],
            "tools": [{ "functionDeclarations": [{ "name": "get_time", "parameters": {"type":"object"} }] }],
            "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } },
            "generationConfig": { "maxOutputTokens": 256, "temperature": 0.7, "stopSequences": ["END"] }
        });
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::GoogleGenai);
        let req = adapter.to_core_request(&ctx, raw).await.unwrap();

        assert_eq!(req.model, "gemini-2.0-flash");
        assert_eq!(req.system.len(), 1);
        assert_eq!(req.messages.len(), 3);
        assert!(matches!(req.messages[1].content[0], ContentBlock::ToolUse { .. }));
        assert!(matches!(req.messages[2].content[0], ContentBlock::ToolResult { .. }));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.max_tokens, Some(256));
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.stop, vec!["END".to_string()]);
    }

    #[tokio::test]
    async fn builds_gemini_response() {
        let resp = CoreResponse {
            id: "resp-9".into(),
            model: "gemini-2.0-flash".into(),
            content: vec![ContentBlock::text("Hello")],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage { input_tokens: 6, output_tokens: 2, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 },
            ext: Default::default(),
        };
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::GoogleGenai);
        let out = adapter.from_core_response(&ctx, resp).await.unwrap();

        assert_eq!(out["candidates"][0]["content"]["role"], "model");
        assert_eq!(out["candidates"][0]["content"]["parts"][0]["text"], "Hello");
        assert_eq!(out["candidates"][0]["finishReason"], "STOP");
        assert_eq!(out["usageMetadata"]["promptTokenCount"], 6);
        assert_eq!(out["modelVersion"], "gemini-2.0-flash");
    }

    #[tokio::test]
    async fn parses_thinking_config_to_reasoning() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::GoogleGenai);
        let req = adapter.to_core_request(&ctx, json!({
            "model": "gemini",
            "generationConfig": { "thinkingConfig": { "thinkingLevel": "high" } },
            "contents": [{ "role": "user", "parts": [{ "text": "Hi" }] }]
        })).await.unwrap();
        assert_eq!(req.reasoning.as_ref().and_then(|r| r.effort.as_deref()), Some("high"));
    }
}
