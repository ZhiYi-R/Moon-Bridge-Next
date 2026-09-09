//! Anthropic 上游 Adapter —— 非流式部分。
//!
//! `from_core_request`：CoreRequest → Anthropic Messages 请求（含 system/tools/
//! tool_choice/messages 规范化）。
//! `to_core_response`：Anthropic Messages 响应 → CoreResponse。

use async_trait::async_trait;
use http::Method;
use moonbridge_core::{
    ContentBlock, CoreRequest, CoreResponse, Protocol, Result, Role, StopReason, Usage,
};
use serde_json::{json, Value};

use super::dto;
use super::AnthropicAdapter;
use crate::adapter::{ProviderAdapter, ProviderEndpoint, UpstreamRequest};
use crate::adapters::{clamped_thinking_budget, reasoning_effort};
use crate::context::ReqCtx;

/// Core 内容块 → Anthropic content block。
pub(super) fn block_to_anthropic(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { data, media_type } => {
            if media_type == "url" {
                Some(json!({ "type": "image", "source": { "type": "url", "url": data } }))
            } else {
                Some(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data },
                }))
            }
        }
        ContentBlock::ToolUse { id, name, input, .. } => Some(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            // 若结果仅为单段文本，直接用字符串；否则用 block 数组
            let content_value = if content.len() == 1 {
                if let ContentBlock::Text { text } = &content[0] {
                    json!(text)
                } else {
                    Value::Array(
                        content.iter().filter_map(block_to_anthropic).collect(),
                    )
                }
            } else {
                Value::Array(content.iter().filter_map(block_to_anthropic).collect())
            };
            let mut obj = json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content_value,
            });
            if *is_error {
                obj["is_error"] = json!(true);
            }
            Some(obj)
        }
        ContentBlock::Reasoning { text, signature, redacted } => {
            // 两种凭据形态必须按原样还原（Anthropic 要求 thinking/redacted_thinking
            // 块不可修改，混转会 400）：
            //   * redacted_thinking：凭据在 data 字段、无可读 thinking；
            //   * display:"omitted" 的 thinking：空文本 + signature（官方合法形态）。
            if text.is_empty() {
                if let Some(sig) = signature {
                    if !sig.is_empty() {
                        if *redacted {
                            return Some(json!({ "type": "redacted_thinking", "data": sig }));
                        }
                        return Some(json!({
                            "type": "thinking",
                            "thinking": "",
                            "signature": sig,
                        }));
                    }
                }
                return None; // 空推理且无凭据，无回传价值
            }
            let mut obj = json!({ "type": "thinking", "thinking": text });
            if let Some(sig) = signature {
                if !sig.is_empty() {
                    obj["signature"] = json!(sig);
                }
            }
            Some(obj)
        }
    }
}

/// Core usage → Anthropic 响应的 usage 对象。
///
/// Core 承载 OpenAI 口径（input_tokens 为 prompt 总量、含缓存），Anthropic 口径
/// 的 input_tokens **不含**缓存读写，出站必须扣除，否则客户端（按
/// input + cache_read + cache_creation 汇总上下文）会双计。
pub(super) fn anthropic_usage_out(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens.saturating_sub(u.cache_read_tokens + u.cache_write_tokens),
        "output_tokens": u.output_tokens,
        "cache_read_input_tokens": u.cache_read_tokens,
        "cache_creation_input_tokens": u.cache_write_tokens,
    })
}

/// Anthropic content block → Core 内容块。
pub(super) fn anthropic_to_block(v: &Value) -> Option<ContentBlock> {
    match v.get("type")?.as_str()? {
        "text" => Some(ContentBlock::text(
            v.get("text").and_then(|t| t.as_str()).unwrap_or_default(),
        )),
        "image" => {
            let source = v.get("source")?;
            Some(ContentBlock::Image {
                data: source.get("data").and_then(|d| d.as_str()).unwrap_or_default().to_string(),
                media_type: source
                    .get("media_type")
                    .and_then(|m| m.as_str())
                    .unwrap_or("image/png")
                    .to_string(),
            })
        }
        "tool_use" => Some(ContentBlock::ToolUse {
            id: v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            name: v.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            namespace: None,
            input: v.get("input").cloned().unwrap_or_else(|| json!({})),
            signature: None,
        }),
        "tool_result" => {
            let content = match v.get("content") {
                Some(Value::String(s)) => vec![ContentBlock::text(s.clone())],
                Some(Value::Array(arr)) => arr.iter().filter_map(anthropic_to_block).collect(),
                _ => Vec::new(),
            };
            Some(ContentBlock::ToolResult {
                tool_use_id: v
                    .get("tool_use_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                content,
                is_error: v.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false),
            })
        }
        "thinking" => Some(ContentBlock::Reasoning {
            text: v.get("thinking").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
            signature: v.get("signature").and_then(|s| s.as_str()).map(|s| s.to_string()),
            redacted: false,
        }),
        // redacted_thinking：凭据在 data 字段（无可读 thinking）→ signature 承载，
        // 多轮回传时原样还原，丢失会 400
        "redacted_thinking" => Some(ContentBlock::Reasoning {
            text: String::new(),
            signature: v.get("data").and_then(|s| s.as_str()).map(|s| s.to_string()),
            redacted: true,
        }),
        _ => None,
    }
}

/// 把 Core 的 stop_reason 字符串映射为枚举。
pub(super) fn map_stop_reason(s: &str) -> Option<StopReason> {
    match s {
        "end_turn" => Some(StopReason::EndTurn),
        "max_tokens" => Some(StopReason::MaxTokens),
        "stop_sequence" => Some(StopReason::StopSequence),
        "tool_use" => Some(StopReason::ToolUse),
        _ => None,
    }
}

/// Core stop_reason → Anthropic 字符串（用于回程，若需要）。
pub(super) fn unmap_stop_reason(r: StopReason) -> &'static str {
    match r {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::ToolUse => "tool_use",
        StopReason::ContentFilter => "end_turn",
    }
}

/// 构建 Anthropic messages 数组：合并连续同 role、把 tool role 归入 user。
fn build_messages(req: &CoreRequest) -> Vec<Value> {
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    for msg in &req.messages {
        let role = match msg.role {
            Role::Assistant => "assistant",
            Role::System => "user", // system 一般走顶层 system 字段；此处兜底为 user
            Role::User | Role::Tool => "user",
        };
        let blocks: Vec<Value> = msg
            .content
            .iter()
            .filter_map(block_to_anthropic)
            .collect();
        if blocks.is_empty() {
            continue;
        }
        // 合并连续同 role（Anthropic 要求 user/assistant 交替）
        if let Some(last) = out.last_mut() {
            if last.0 == role {
                last.1.extend(blocks);
                continue;
            }
        }
        out.push((role.to_string(), blocks));
    }
    out.into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect()
}

/// 构建 Anthropic 顶层 system 字段。
fn build_system(req: &CoreRequest) -> Option<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    for b in &req.system {
        if let Some(v) = block_to_anthropic(b) {
            blocks.push(v);
        }
    }
    // 消息里 role=System 的文本也并入 system
    for msg in &req.messages {
        if msg.role == Role::System {
            for b in &msg.content {
                if let Some(v) = block_to_anthropic(b) {
                    blocks.push(v);
                }
            }
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(Value::Array(blocks))
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    async fn from_core_request(
        &self,
        _ctx: &ReqCtx,
        req: &CoreRequest,
        endpoint: &ProviderEndpoint,
    ) -> Result<UpstreamRequest> {
        let url = format!(
            "{}{}",
            endpoint.base_url.trim_end_matches('/'),
            dto::MESSAGES_PATH
        );

        let max_tokens = req.max_tokens.unwrap_or(dto::DEFAULT_MAX_TOKENS);
        let mut body = json!({
            "model": req.model,
            "max_tokens": max_tokens,
            "messages": build_messages(req),
            "stream": req.stream,
        });
        let obj = body.as_object_mut().expect("body is object");

        if let Some(system) = build_system(req) {
            obj.insert("system".to_string(), system);
        }
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let mut tool = json!({ "name": t.name, "input_schema": t.input_schema });
                    if let Some(desc) = &t.description {
                        tool["description"] = json!(desc);
                    }
                    tool
                })
                .collect();
            obj.insert("tools".to_string(), Value::Array(tools));
        }
        if let Some(tc) = &req.tool_choice {
            let v = match tc {
                moonbridge_core::ToolChoice::Auto => json!({ "type": "auto" }),
                moonbridge_core::ToolChoice::None => json!({ "type": "none" }),
                moonbridge_core::ToolChoice::Required => json!({ "type": "any" }),
                moonbridge_core::ToolChoice::Tool { name } => {
                    json!({ "type": "tool", "name": name })
                }
            };
            obj.insert("tool_choice".to_string(), v);
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".to_string(), json!(t));
        }
        if let Some(p) = req.top_p {
            obj.insert("top_p".to_string(), json!(p));
        }
        if !req.stop.is_empty() {
            obj.insert("stop_sequences".to_string(), json!(req.stop));
        }
        // 推理强度：Anthropic 用扩展思考的 token 预算表达，且要求
        // 1024 <= budget_tokens < max_tokens，故经 clamped_thinking_budget 夹紧。
        if let Some(effort) = reasoning_effort(req) {
            if let Some(budget) = clamped_thinking_budget(effort, max_tokens) {
                obj.insert(
                    "thinking".to_string(),
                    json!({ "type": "enabled", "budget_tokens": budget }),
                );
            }
        }

        // headers
        let version = endpoint
            .version
            .clone()
            .unwrap_or_else(|| dto::DEFAULT_VERSION.to_string());
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-api-key".to_string(), endpoint.api_key.clone()),
            ("anthropic-version".to_string(), version),
        ];
        if let Some(ua) = &endpoint.user_agent {
            headers.push(("user-agent".to_string(), ua.clone()));
        }

        Ok(UpstreamRequest {
            method: Method::POST,
            url,
            headers,
            body,
            stream: req.stream,
        })
    }

    async fn to_core_response(&self, _ctx: &ReqCtx, raw: Value) -> Result<CoreResponse> {
        let id = raw
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let model = raw
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let content = raw
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| arr.iter().filter_map(anthropic_to_block).collect())
            .unwrap_or_default();

        let stop_reason = raw
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .and_then(map_stop_reason);

        let usage = raw
            .get("usage")
            .map(|u| {
                let cache_read = u
                    .get("cache_read_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as u32;
                let cache_write = u
                    .get("cache_creation_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as u32;
                Usage {
                    // Core 口径与 OpenAI 对齐：input_tokens 为 prompt 总量（含缓存），
                    // anthropic 原生口径不含缓存，此处归一化，出站时再扣除
                    input_tokens: (u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0)
                        as u32)
                        .saturating_add(cache_read)
                        .saturating_add(cache_write),
                    output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                    cache_read_tokens: cache_read,
                    cache_write_tokens: cache_write,
                    // Anthropic 协议无独立 reasoning token 字段（含在 output 内）。
                    reasoning_tokens: 0,
                }
            })
            .unwrap_or_default();

        Ok(CoreResponse {
            id,
            model,
            content,
            stop_reason,
            usage,
            ext: Default::default(),
        })
    }
}

/// 供流式与非流式复用的消息构造（测试可用）。
#[allow(dead_code)]
pub(crate) fn messages_for_test(req: &CoreRequest) -> Vec<Value> {
    build_messages(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{Message, Reasoning};

    /// display:"omitted" 的空文本 thinking 块：回传时必须保持 thinking 形态，
    /// 不得误转 redacted_thinking（Anthropic 要求块不可修改，混转 400）。
    #[test]
    fn omitted_thinking_keeps_thinking_form() {
        let block = json!({ "type": "thinking", "thinking": "", "signature": "SIG" });
        let core = anthropic_to_block(&block).unwrap();
        match &core {
            ContentBlock::Reasoning { text, signature: Some(sig), redacted } => {
                assert_eq!(text, "");
                assert_eq!(sig, "SIG");
                assert!(!redacted);
            }
            other => panic!("expected reasoning, got {other:?}"),
        }
        let back = block_to_anthropic(&core).unwrap();
        assert_eq!(back["type"], "thinking");
        assert_eq!(back["thinking"], "");
        assert_eq!(back["signature"], "SIG");
        assert!(back.get("data").is_none());
    }

    /// redacted_thinking：data 凭据 → signature，回传时还原 redacted 形态。
    #[test]
    fn redacted_thinking_roundtrips() {
        let block = json!({ "type": "redacted_thinking", "data": "ENC" });
        let core = anthropic_to_block(&block).unwrap();
        match &core {
            ContentBlock::Reasoning { text, signature: Some(enc), redacted: true } => {
                assert_eq!(text, "");
                assert_eq!(enc, "ENC");
            }
            other => panic!("expected reasoning with credential, got {other:?}"),
        }
        let back = block_to_anthropic(&core).unwrap();
        assert_eq!(back["type"], "redacted_thinking");
        assert_eq!(back["data"], "ENC");
    }


    fn endpoint() -> ProviderEndpoint {
        ProviderEndpoint {
            key: "anthropic".into(),
            protocol: Protocol::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            api_key: "sk-test".into(),
            version: None,
            user_agent: Some("moonbridge-next/0.1".into()),
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn builds_anthropic_request() {
        let mut req = CoreRequest::new("claude-sonnet-4");
        req.system.push(ContentBlock::text("You are helpful"));
        req.messages
            .push(Message::text(Role::User, "Hello"));
        req.max_tokens = Some(1024);

        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let up = adapter
            .from_core_request(&ctx, &req, &endpoint())
            .await
            .unwrap();

        assert!(up.url.ends_with("/v1/messages"));
        assert_eq!(up.body["model"], "claude-sonnet-4");
        assert_eq!(up.body["max_tokens"], 1024);
        assert_eq!(up.body["system"][0]["text"], "You are helpful");
        assert_eq!(up.body["messages"][0]["role"], "user");
        assert!(up
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == dto::DEFAULT_VERSION));
    }

    #[tokio::test]
    async fn merges_consecutive_same_role() {
        let mut req = CoreRequest::new("claude");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "f".into(),
                namespace: None,
                input: json!({}),
                signature: None,
            }],
            ext: Default::default(),
        });
        req.messages.push(Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![ContentBlock::text("ok")],
                is_error: false,
            }],
            ext: Default::default(),
        });
        let msgs = build_messages(&req);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_result");
    }

    #[tokio::test]
    async fn parses_anthropic_response() {
        let raw = json!({
            "id": "msg_1",
            "model": "claude-sonnet-4",
            "role": "assistant",
            "content": [
                { "type": "text", "text": "Hi there" },
                { "type": "tool_use", "id": "call_9", "name": "get_time", "input": {"tz":"UTC"} }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3 }
        });
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let resp = adapter.to_core_response(&ctx, raw).await.unwrap();
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(resp.usage.input_tokens, 13, "Core 口径：prompt 总量含缓存");
        assert_eq!(resp.usage.cache_read_tokens, 3);
        assert!(matches!(resp.content[1], ContentBlock::ToolUse { .. }));
    }

    /// 回归：`reasoning.effort` 必须翻译成 Anthropic 扩展思考的 token 预算。
    #[tokio::test]
    async fn propagates_reasoning_as_thinking_budget() {
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);

        let mut req = CoreRequest::new("claude-sonnet-4");
        req.messages.push(Message::text(Role::User, "Hi"));
        req.max_tokens = Some(32000);
        req.reasoning = Some(Reasoning {
            effort: Some("high".into()),
            summary: None,
        });
        let up = adapter
            .from_core_request(&ctx, &req, &endpoint())
            .await
            .unwrap();
        assert_eq!(up.body["thinking"]["type"], "enabled");
        assert_eq!(up.body["thinking"]["budget_tokens"], 16384);

        // 预算必须严格小于 max_tokens：小 max_tokens 时被夹紧
        let mut tight = req.clone();
        tight.max_tokens = Some(3000);
        let up2 = adapter
            .from_core_request(&ctx, &tight, &endpoint())
            .await
            .unwrap();
        assert_eq!(up2.body["thinking"]["budget_tokens"], 2999);

        // max_tokens 小到无法容纳合法预算时，宁可不发也不构造必被拒的请求
        let mut tiny = req.clone();
        tiny.max_tokens = Some(500);
        let up3 = adapter
            .from_core_request(&ctx, &tiny, &endpoint())
            .await
            .unwrap();
        assert!(up3.body.get("thinking").is_none(), "不应发出非法 thinking");

        // 未声明 reasoning 时不得凭空长出 thinking
        let plain = CoreRequest::new("claude");
        let up4 = adapter
            .from_core_request(&ctx, &plain, &endpoint())
            .await
            .unwrap();
        assert!(up4.body.get("thinking").is_none());
    }
}
