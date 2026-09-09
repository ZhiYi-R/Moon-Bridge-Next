//! OpenAI Chat Completions 入口 Adapter —— 非流式部分。

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse, Protocol, Reasoning, Result};
use serde_json::{json, Value};

use super::dto;
use super::OpenAiChatAdapter;
use crate::adapter::ClientAdapter;
use crate::context::ReqCtx;

#[async_trait]
impl ClientAdapter for OpenAiChatAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiChat
    }

    async fn to_core_request(&self, _ctx: &ReqCtx, raw: Value) -> Result<CoreRequest> {
        let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let mut req = CoreRequest::new(model.clone());
        req.model_alias = model;

        let messages = raw
            .get("messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let (system, msgs) = dto::extract_system(dto::chat_to_core_messages(&messages));
        req.system = system;
        req.messages = msgs;

        if let Some(tools) = raw.get("tools").and_then(|v| v.as_array()) {
            req.tools = dto::chat_tools_to_core(tools);
        }
        if let Some(tc) = raw.get("tool_choice") {
            req.tool_choice = dto::parse_tool_choice(tc);
        }
        // 推理强度：reasoning_effort（OpenAI Chat 扩展）或 reasoning.effort（兼容写法）
        if let Some(effort) = raw
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .or_else(|| {
                raw.get("reasoning")
                    .and_then(|v| v.get("effort"))
                    .and_then(|v| v.as_str())
            })
            .filter(|s| !s.is_empty())
        {
            req.reasoning = Some(Reasoning {
                effort: Some(effort.to_string()),
                summary: None,
            });
        }
        req.max_tokens = raw
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| raw.get("max_completion_tokens").and_then(|v| v.as_u64()))
            .map(|v| v as u32);
        req.temperature = raw.get("temperature").and_then(|v| v.as_f64()).map(|v| v as f32);
        req.top_p = raw.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32);
        match raw.get("stop") {
            Some(Value::String(s)) => req.stop = vec![s.clone()],
            Some(Value::Array(arr)) => {
                req.stop = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            }
            _ => {}
        }
        req.stream = raw.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

        Ok(req)
    }

    async fn from_core_response(&self, _ctx: &ReqCtx, resp: CoreResponse) -> Result<Value> {
        Ok(json!({
            "id": resp.id,
            "object": "chat.completion",
            "created": 0,
            "model": resp.model,
            "choices": [{
                "index": 0,
                "message": dto::core_to_chat_response_message(&resp.content),
                "finish_reason": dto::unmap_stop_reason(resp.stop_reason),
            }],
            "usage": dto::usage_object(&resp.usage),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{ContentBlock, StopReason, Usage};

    #[tokio::test]
    async fn parses_chat_request() {
        let raw = json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "system", "content": "Be brief" },
                { "role": "user", "content": "Hi" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "c1", "type": "function", "function": { "name": "get_time", "arguments": "{\"tz\":\"UTC\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "c1", "content": "12:00" }
            ],
            "tools": [{ "type": "function", "function": { "name": "get_time", "parameters": {"type":"object"} } }],
            "tool_choice": "auto",
            "max_tokens": 256,
            "stream": false
        });
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let req = adapter.to_core_request(&ctx, raw).await.unwrap();

        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.system.len(), 1, "system 应被抽出");
        assert_eq!(req.messages.len(), 3);
        assert!(matches!(req.messages[1].content[0], ContentBlock::ToolUse { .. }));
        assert!(matches!(req.messages[2].content[0], ContentBlock::ToolResult { .. }));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.max_tokens, Some(256));
    }

    #[tokio::test]
    async fn carries_reasoning_in_response_message() {
        let resp = CoreResponse {
            id: "chatcmpl-2".into(),
            model: "m".into(),
            content: vec![
                ContentBlock::Reasoning { text: "thought".into(), signature: None, redacted: false },
                ContentBlock::text("Hi"),
            ],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            ext: Default::default(),
        };
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let out = adapter.from_core_response(&ctx, resp).await.unwrap();

        let msg = &out["choices"][0]["message"];
        assert_eq!(msg["content"], "Hi");
        assert_eq!(msg["reasoning_content"], "thought", "DeepSeek 约定");
        assert_eq!(msg["reasoning"], "thought", "OpenRouter/vLLM 约定");
    }

    #[tokio::test]
    async fn parses_reasoning_effort() {
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let req = adapter.to_core_request(&ctx, json!({
            "model": "m", "reasoning_effort": "high",
            "messages": [{ "role": "user", "content": "Hi" }]
        })).await.unwrap();
        assert_eq!(req.reasoning.as_ref().and_then(|r| r.effort.as_deref()), Some("high"));
    }

    #[tokio::test]
    async fn builds_chat_response() {
        let resp = CoreResponse {
            id: "chatcmpl-1".into(),
            model: "gpt-4o".into(),
            content: vec![ContentBlock::text("Hello")],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage { input_tokens: 7, output_tokens: 2, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 },
            ext: Default::default(),
        };
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let out = adapter.from_core_response(&ctx, resp).await.unwrap();

        assert_eq!(out["object"], "chat.completion");
        assert_eq!(out["choices"][0]["message"]["role"], "assistant");
        assert_eq!(out["choices"][0]["message"]["content"], "Hello");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["prompt_tokens"], 7);
    }
}
