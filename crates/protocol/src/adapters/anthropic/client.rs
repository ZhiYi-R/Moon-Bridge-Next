//! Anthropic Messages 入口 Adapter —— 非流式部分。
//!
//! `to_core_request`：Anthropic Messages 请求 → CoreRequest（system/messages/tools/
//! tool_choice/参数解析）。
//! `from_core_response`：CoreResponse → Anthropic Messages 响应对象。
//!
//! 与上游侧（[`super::provider`]）共享 block 转换函数，保证「Anthropic 入口 →
//! Anthropic 上游」等链路语义一致。

use async_trait::async_trait;
use moonbridge_core::{
    ContentBlock, CoreRequest, CoreResponse, Message, Protocol, Reasoning, Result, Role, Tool,
    ToolChoice,
};
use serde_json::{json, Value};

use super::provider::{
    anthropic_to_block, anthropic_usage_out, block_to_anthropic_client, unmap_stop_reason,
};
use super::AnthropicAdapter;
use crate::adapter::ClientAdapter;
use crate::adapters::effort_from_budget;
use crate::context::ReqCtx;

/// `msg.ext`/`req.meta` 中记录块级 `cache_control` 位置的键：
/// `{"<content 内块序号>": <cache_control 原始对象>}`。
/// Anthropic 的 prompt caching 断点挂在块上，Core 块没有字段位，
/// 以 ext 旁挂位置表保真回传。
pub(crate) const CACHE_EXT_KEY: &str = "anthropic.cache_control";
/// `req.meta` 中 system 块 cache_control 位置表的键。
pub(crate) const SYSTEM_CACHE_META_KEY: &str = "anthropic.system_cache_control";
/// `req.meta` 中未解析顶层字段透传对象的键（Anthropic→Anthropic 保真）。
pub(crate) const EXTRA_META_KEY: &str = "anthropic.extra";

/// 解析 Anthropic message 的 content（字符串或 block 数组）为 Core 内容块，
/// 同时把各块的 `cache_control` 记入位置表。
fn parse_content(v: Option<&Value>) -> (Vec<ContentBlock>, Value) {
    match v {
        Some(Value::String(s)) => (vec![ContentBlock::text(s.clone())], Value::Null),
        Some(Value::Array(arr)) => {
            let mut cache = json!({});
            let mut blocks = Vec::new();
            for raw in arr {
                if let Some(blk) = anthropic_to_block(raw) {
                    if let Some(cc) = raw.get("cache_control") {
                        cache[blocks.len().to_string()] = cc.clone();
                    }
                    blocks.push(blk);
                }
            }
            (blocks, cache)
        }
        _ => (Vec::new(), Value::Null),
    }
}

/// 解析 Anthropic tool_choice。
fn parse_tool_choice(v: &Value) -> Option<ToolChoice> {
    match v.get("type").and_then(|t| t.as_str()) {
        Some("auto") => Some(ToolChoice::Auto),
        Some("any") => Some(ToolChoice::Required),
        Some("none") => Some(ToolChoice::None),
        Some("tool") => v
            .get("name")
            .and_then(|n| n.as_str())
            .map(|n| ToolChoice::Tool { name: n.to_string() }),
        _ => None,
    }
}

#[async_trait]
impl ClientAdapter for AnthropicAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    async fn to_core_request(&self, _ctx: &ReqCtx, raw: Value) -> Result<CoreRequest> {
        let model = raw
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mut req = CoreRequest::new(model.clone());
        req.model_alias = model;

        // system：字符串或 block 数组（块的 cache_control 记入 meta 位置表）
        match raw.get("system") {
            Some(Value::String(s)) if !s.is_empty() => req.system.push(ContentBlock::text(s.clone())),
            Some(Value::Array(arr)) => {
                let mut cache = json!({});
                for b in arr {
                    if let Some(blk) = anthropic_to_block(b) {
                        if let Some(cc) = b.get("cache_control") {
                            cache[req.system.len().to_string()] = cc.clone();
                        }
                        req.system.push(blk);
                    }
                }
                if let Some(obj) = cache.as_object().filter(|o| !o.is_empty()) {
                    req.meta.insert(
                        SYSTEM_CACHE_META_KEY.to_string(),
                        Value::Object(obj.clone()),
                    );
                }
            }
            _ => {}
        }

        // messages
        if let Some(Value::Array(msgs)) = raw.get("messages") {
            for m in msgs {
                let role = match m.get("role").and_then(|r| r.as_str()) {
                    Some("assistant") => Role::Assistant,
                    Some("system") => Role::System,
                    _ => Role::User,
                };
                let (content, cache) = parse_content(m.get("content"));
                let mut msg = Message {
                    role,
                    content,
                    ext: Default::default(),
                };
                if let Some(obj) = cache.as_object().filter(|o| !o.is_empty()) {
                    msg.ext.insert(CACHE_EXT_KEY.to_string(), Value::Object(obj.clone()));
                }
                req.messages.push(msg);
            }
        }

        // tools（工具级 cache_control 记进 Tool.ext）
        if let Some(Value::Array(tools)) = raw.get("tools") {
            for t in tools {
                let Some(name) = t.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                let mut tool = Tool {
                    name: name.to_string(),
                    description: t.get("description").and_then(|v| v.as_str()).map(String::from),
                    input_schema: t
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                    ext: Default::default(),
                };
                if let Some(cc) = t.get("cache_control") {
                    tool.ext.insert("cache_control".to_string(), cc.clone());
                }
                req.tools.push(tool);
            }
        }

        if let Some(tc) = raw.get("tool_choice") {
            req.tool_choice = parse_tool_choice(tc);
        }

        // thinking 配置 → req.reasoning（官方形态：enabled+budget_tokens /
        // adaptive+output_config.effort；不传则上游按默认行为处理）
        if let Some(t) = raw.get("thinking").and_then(|v| v.as_object()) {
            let effort = match t.get("type").and_then(|v| v.as_str()) {
                Some("enabled") => t
                    .get("budget_tokens")
                    .and_then(|v| v.as_u64())
                    .map(|b| effort_from_budget(b as u32)),
                Some("adaptive") => Some(
                    raw.get("output_config")
                        .and_then(|o| o.get("effort"))
                        .and_then(|e| e.as_str())
                        .unwrap_or("high"),
                ),
                _ => None, // disabled 或未知：不下发，交由上游默认
            };
            if let Some(effort) = effort {
                req.reasoning = Some(Reasoning {
                    effort: Some(effort.to_string()),
                    summary: None,
                });
            }
        }

        req.max_tokens = raw.get("max_tokens").and_then(|v| v.as_u64()).map(|v| v as u32);
        req.temperature = raw.get("temperature").and_then(|v| v.as_f64()).map(|v| v as f32);
        req.top_p = raw.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32);
        if let Some(Value::Array(stops)) = raw.get("stop_sequences") {
            req.stop = stops
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        req.stream = raw.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

        // 未解析顶层字段透传（Anthropic→Anthropic 保真）：top_k/metadata/
        // service_tier/mcp_servers/context_management 等经 Core 无字段位，
        // 整体收进 meta，出站时合并回请求体（网关已解析字段优先）。
        if let Some(obj) = raw.as_object() {
            const KNOWN: &[&str] = &[
                "model", "system", "messages", "tools", "tool_choice", "thinking",
                "output_config", "max_tokens", "temperature", "top_p",
                "stop_sequences", "stream",
            ];
            let extra: serde_json::Map<String, Value> = obj
                .iter()
                .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !extra.is_empty() {
                req.meta
                    .insert(EXTRA_META_KEY.to_string(), Value::Object(extra));
            }
        }

        Ok(req)
    }

    async fn from_core_response(&self, _ctx: &ReqCtx, resp: CoreResponse) -> Result<Value> {
        // 客户端回程：本家签名还原原文、异源凭据带标记透传（回传后由
        // 归属协议上游解标）——不走 block_to_anthropic 的上游降级语义。
        let content: Vec<Value> = resp
            .content
            .iter()
            .filter_map(block_to_anthropic_client)
            .collect();
        let stop_reason = resp.stop_reason.map(unmap_stop_reason).unwrap_or("end_turn");
        Ok(json!({
            "id": resp.id,
            "type": "message",
            "role": "assistant",
            "model": resp.model,
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": anthropic_usage_out(&resp.usage),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{StopReason, Usage};

    #[tokio::test]
    async fn parses_anthropic_request() {
        let raw = json!({
            "model": "claude-sonnet-4",
            "system": "You are helpful",
            "max_tokens": 1024,
            "messages": [
                { "role": "user", "content": "Hi" },
                { "role": "assistant", "content": [{ "type": "text", "text": "Hello" }] },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }] }
            ],
            "tools": [{ "name": "get_time", "description": "get tz time", "input_schema": {"type":"object"} }],
            "tool_choice": { "type": "auto" },
            "stream": true
        });
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let req = adapter.to_core_request(&ctx, raw).await.unwrap();

        assert_eq!(req.model, "claude-sonnet-4");
        assert_eq!(req.system.len(), 1);
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert!(matches!(req.messages[2].content[0], ContentBlock::ToolResult { .. }));
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "get_time");
        assert!(matches!(req.tool_choice, Some(ToolChoice::Auto)));
        assert_eq!(req.max_tokens, Some(1024));
        assert!(req.stream);
    }

    /// thinking 配置应传递到 req.reasoning（否则上游推理模型不会思考）。
    #[tokio::test]
    async fn parses_thinking_config_to_reasoning() {
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);

        // enabled + budget_tokens → 档位逆推 effort
        let req = adapter.to_core_request(&ctx, json!({
            "model": "claude", "max_tokens": 16000,
            "thinking": { "type": "enabled", "budget_tokens": 16384 },
            "messages": [{ "role": "user", "content": "Hi" }]
        })).await.unwrap();
        assert_eq!(req.reasoning.as_ref().and_then(|r| r.effort.as_deref()), Some("high"));

        // adaptive → output_config.effort 或默认 high
        let req = adapter.to_core_request(&ctx, json!({
            "model": "claude", "max_tokens": 16000,
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": "low" },
            "messages": [{ "role": "user", "content": "Hi" }]
        })).await.unwrap();
        assert_eq!(req.reasoning.as_ref().and_then(|r| r.effort.as_deref()), Some("low"));
    }

    #[tokio::test]
    async fn builds_anthropic_response() {
        let resp = CoreResponse {
            id: "msg_1".into(),
            model: "claude-sonnet-4".into(),
            content: vec![ContentBlock::text("Hi there")],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            ext: Default::default(),
        };
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let out = adapter.from_core_response(&ctx, resp).await.unwrap();

        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "Hi there");
        assert_eq!(out["usage"]["input_tokens"], 10);
    }
}
