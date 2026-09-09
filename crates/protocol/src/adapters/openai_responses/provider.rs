//! OpenAI Responses 上游 Adapter —— 非流式部分。
//!
//! `from_core_request`：CoreRequest → Responses 请求（instructions/input/tools）。
//! `to_core_response`：Responses 响应对象 → CoreResponse（output items 解析）。
//!
//! 与入口侧（[`super::client`]）共享 content 解析与 usage 提取，保证 Responses
//! 既作入口又作上游时语义一致。

use async_trait::async_trait;
use http::Method;
use moonbridge_core::{
    ContentBlock, CoreRequest, CoreResponse, Protocol, Result, Role, StopReason, ToolChoice,
};
use serde_json::{json, Value};

use super::client::{parse_content_item, usage_from_value};
use super::dto;
use super::OpenAiResponsesAdapter;
use crate::adapter::{ProviderAdapter, ProviderEndpoint, UpstreamRequest};
use crate::adapters::reasoning_effort;
use crate::context::ReqCtx;

/// 提取内容块中的纯文本（用于 function_call_output）。
fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Core system（顶层 + system 角色消息）→ Responses `instructions` 字符串。
fn build_instructions(req: &CoreRequest) -> Option<String> {
    let mut texts: Vec<&str> = Vec::new();
    for b in &req.system {
        if let ContentBlock::Text { text } = b {
            texts.push(text);
        }
    }
    for msg in &req.messages {
        if msg.role == Role::System {
            for b in &msg.content {
                if let ContentBlock::Text { text } = b {
                    texts.push(text);
                }
            }
        }
    }
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

/// Core messages → Responses `input` items（system 走 instructions，此处跳过）。
fn build_input(req: &CoreRequest) -> Vec<Value> {
    let mut input: Vec<Value> = Vec::new();
    for msg in &req.messages {
        if msg.role == Role::System {
            continue;
        }
        let role = match msg.role {
            Role::Assistant => "assistant",
            Role::Tool => "user",
            _ => "user",
        };
        let mut parts: Vec<Value> = Vec::new();
        // 遇到工具块时先 flush 已累积的文本 parts
        macro_rules! flush_parts {
            () => {
                if !parts.is_empty() {
                    input.push(json!({ "type": "message", "role": role, "content": std::mem::take(&mut parts) }));
                }
            };
        }
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({ "type": "input_text", "text": text })),
                ContentBlock::Image { data, media_type } => {
                    let url = if media_type == "url" {
                        data.clone()
                    } else {
                        format!("data:{media_type};base64,{data}")
                    };
                    parts.push(json!({ "type": "input_image", "image_url": url }));
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input: tool_input,
                    ..
                } => {
                    flush_parts!();
                    input.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": tool_input.to_string(),
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    flush_parts!();
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": text_of(content),
                    }));
                }
                ContentBlock::Reasoning { signature, .. } => {
                    // 只回传 encrypted_content（上游原始 CoT 的加密形态，缺失会 400
                    // 或退化）；明文 summary 是独立的展示产物，不回传。
                    if let Some(enc) = signature.as_deref().filter(|s| !s.is_empty()) {
                        flush_parts!();
                        input.push(json!({
                            "type": "reasoning",
                            "summary": [],
                            "encrypted_content": enc,
                        }));
                    }
                }
            }
        }
        flush_parts!();
    }
    input
}

/// Core tools → Responses tools（扁平 function 结构）。
fn build_tools(req: &CoreRequest) -> Vec<Value> {
    req.tools
        .iter()
        .map(|t| {
            let mut tool = json!({ "type": "function", "name": t.name, "parameters": t.input_schema });
            if let Some(d) = &t.description {
                tool["description"] = json!(d);
            }
            tool
        })
        .collect()
}

#[async_trait]
impl ProviderAdapter for OpenAiResponsesAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiResponse
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
            dto::RESPONSES_PATH
        );

        let mut body = json!({
            "model": req.model,
            "input": build_input(req),
            "stream": req.stream,
            "max_output_tokens": req.max_tokens.unwrap_or(dto::DEFAULT_MAX_OUTPUT_TOKENS),
            // 无状态网关：推理明细/回传凭据必须随响应带回（否则多轮 function-call
            // 循环中 reasoning item 缺失会 400）。对非推理模型无 reasoning item，
            // 该 include 无副作用。
            "include": ["reasoning.encrypted_content"],
        });
        let obj = body.as_object_mut().expect("body is object");
        if let Some(instr) = build_instructions(req) {
            obj.insert("instructions".to_string(), json!(instr));
        }
        if !req.tools.is_empty() {
            obj.insert("tools".to_string(), json!(build_tools(req)));
        }
        if let Some(tc) = &req.tool_choice {
            let v = match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Tool { name } => json!({ "type": "function", "name": name }),
            };
            obj.insert("tool_choice".to_string(), v);
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".to_string(), json!(t));
        }
        if let Some(p) = req.top_p {
            obj.insert("top_p".to_string(), json!(p));
        }
        // 推理强度：Responses 用 reasoning.{effort,summary} 表达；
        // 未显式指定 summary 时默认 "auto"——网关的 Reasoning 块承载可读推理文本，
        // 主动索要摘要才能拿到明文（否则上游只回 encrypted_content）。
        if let Some(effort) = reasoning_effort(req) {
            let mut r = json!({ "effort": effort });
            match req.reasoning.as_ref().and_then(|x| x.summary.clone()) {
                Some(summary) => r["summary"] = summary,
                None => r["summary"] = json!("auto"),
            }
            obj.insert("reasoning".to_string(), r);
        }

        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("authorization".to_string(), format!("Bearer {}", endpoint.api_key)),
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
        let id = raw.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let model = raw.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string();

        let mut content: Vec<ContentBlock> = Vec::new();
        if let Some(output) = raw.get("output").and_then(|v| v.as_array()) {
            for item in output {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("message") => {
                        if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                            for p in parts {
                                if let Some(b) = parse_content_item(p) {
                                    content.push(b);
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        let args = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .unwrap_or_else(|| json!({}));
                        content.push(ContentBlock::ToolUse {
                            id: item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            name: item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            namespace: None,
                            input: args,
                            signature: None,
                        });
                    }
                    Some("reasoning") => {
                        // summary 与 encrypted_content 是**独立的两个产物**：
                        //   * summary/content 明文 → text，供下游协议展示；
                        //   * encrypted_content（上游原始完整 CoT 的加密形态）→
                        //     signature，**仅用于原样回传**，不是展示文本、不可再生。
                        // 缺了它多轮 function-call 循环会 400（或退化为无推理状态）。
                        let mut text = String::new();
                        for key in ["summary", "content"] {
                            if let Some(arr) = item.get(key).and_then(|s| s.as_array()) {
                                for x in arr {
                                    if let Some(t) = x.get("text").and_then(|t| t.as_str()) {
                                        text.push_str(t);
                                    }
                                }
                            }
                        }
                        if text.is_empty() {
                            text = item
                                .get("reasoning_text")
                                .or_else(|| item.get("reasoning"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                        }
                        let signature = item
                            .get("encrypted_content")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .filter(|s| !s.is_empty());
                        if !text.is_empty() || signature.is_some() {
                            content.push(ContentBlock::Reasoning { text, signature });
                        }
                    }
                    _ => {}
                }
            }
        }

        let usage = raw.get("usage").map(usage_from_value).unwrap_or_default();
        let stop_reason = if content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })) {
            Some(StopReason::ToolUse)
        } else {
            Some(StopReason::EndTurn)
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::ClientAdapter;
    use moonbridge_core::{Message, Reasoning, Tool, Usage};

    /// 推理明文在 content[].text（reasoning_text，第三方兼容实现）也应提取；
    /// 无任何文本时不产生空 Reasoning 块。
    #[tokio::test]
    async fn extracts_reasoning_from_content_and_skips_empty() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let raw = json!({
            "id": "resp_1",
            "model": "m",
            "output": [
                { "type": "reasoning", "summary": [],
                  "content": [ { "type": "reasoning_text", "text": "deep thought" } ] },
                { "type": "message", "role": "assistant",
                  "content": [ { "type": "output_text", "text": "Hi" } ] }
            ],
            "usage": { "input_tokens": 3, "output_tokens": 9 }
        });
        let resp = adapter.to_core_response(&ctx, raw).await.unwrap();
        assert_eq!(resp.content.len(), 2);
        match &resp.content[0] {
            ContentBlock::Reasoning { text, .. } => assert_eq!(text, "deep thought"),
            other => panic!("expected reasoning first, got {other:?}"),
        }

        // 无可读文本（上游未给任何推理载荷）→ 不产空块
        let raw2 = json!({
            "id": "resp_2", "model": "m",
            "output": [ { "type": "reasoning", "summary": [] } ]
        });
        let resp2 = adapter.to_core_response(&ctx, raw2).await.unwrap();
        assert!(resp2.content.is_empty(), "无推理载荷不应产生块");

        // encrypted-only：凭据必须保留（回传用），但不作展示文本
        let raw3 = json!({
            "id": "resp_3", "model": "m",
            "output": [ { "type": "reasoning", "summary": [], "encrypted_content": "ENC" } ]
        });
        let resp3 = adapter.to_core_response(&ctx, raw3).await.unwrap();
        match &resp3.content[0] {
            ContentBlock::Reasoning { text, signature: Some(enc) } => {
                assert_eq!(text, "", "encrypted 不是展示文本");
                assert_eq!(enc, "ENC");
            }
            other => panic!("expected reasoning, got {other:?}"),
        }
    }

    /// 端到端：Responses 入口历史 reasoning item（含 encrypted_content）→ Core →
    /// Responses 上游请求 input，reasoning item 应回传（缺失会 400）。
    #[tokio::test]
    async fn roundtrips_reasoning_item_with_encrypted_content() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let raw = json!({
            "model": "m",
            "input": [
                { "type": "reasoning", "summary": [], "encrypted_content": "ENC" },
                { "type": "function_call", "call_id": "c1", "name": "get_time", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "c1", "output": "12:00" },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "thanks" }] }
            ]
        });
        let req = adapter.to_core_request(&ctx, raw).await.unwrap();
        assert!(
            req.messages.iter().any(|m| m.content.iter().any(|b| matches!(
                b, ContentBlock::Reasoning { signature: Some(enc), .. } if enc == "ENC"
            ))),
            "入口侧应保留 encrypted_content 凭据"
        );

        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();
        let items = up.body["input"].as_array().unwrap();
        let back = items
            .iter()
            .find(|i| i["type"] == "reasoning")
            .expect("reasoning item 应回传上游");
        assert_eq!(back["encrypted_content"], "ENC");
        assert!(back.get("content").is_none(), "明文 summary 是展示产物，不回传");
    }

    /// 无凭据的明文块（如来自其他上游的历史）不回传 reasoning item。
    #[tokio::test]
    async fn plain_text_reasoning_without_signature_is_not_sent_back() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let mut req = CoreRequest::new("m");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning { text: "thought".into(), signature: None }],
            ext: Default::default(),
        });
        req.messages.push(Message::text(Role::User, "go"));
        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();
        let items = up.body["input"].as_array().unwrap();
        assert!(
            !items.iter().any(|i| i["type"] == "reasoning"),
            "无凭据不应合成 reasoning item"
        );
    }

    fn endpoint() -> ProviderEndpoint {
        ProviderEndpoint {
            key: "openai".into(),
            protocol: Protocol::OpenAiResponse,
            base_url: "https://api.openai.com".into(),
            api_key: "sk-test".into(),
            version: None,
            user_agent: None,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn builds_responses_request() {
        let mut req = CoreRequest::new("gpt-x");
        req.system.push(ContentBlock::text("Be helpful"));
        req.messages.push(Message::text(Role::User, "Hi"));
        req.tools.push(Tool {
            name: "get_time".into(),
            description: Some("tz time".into()),
            input_schema: json!({ "type": "object" }),
            ext: Default::default(),
        });
        req.max_tokens = Some(512);

        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let up = adapter.from_core_request(&ctx, &req, &endpoint()).await.unwrap();

        assert!(up.url.ends_with("/v1/responses"));
        assert_eq!(up.body["instructions"], "Be helpful");
        assert_eq!(up.body["input"][0]["role"], "user");
        assert_eq!(up.body["input"][0]["content"][0]["text"], "Hi");
        assert_eq!(up.body["tools"][0]["name"], "get_time");
        assert_eq!(up.body["max_output_tokens"], 512);
        assert!(up.headers.iter().any(|(k, v)| k == "authorization" && v == "Bearer sk-test"));
    }

    #[tokio::test]
    async fn parses_responses_output() {
        let raw = json!({
            "id": "resp_1",
            "object": "response",
            "model": "gpt-x",
            "status": "completed",
            "output": [
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Hello" }] },
                { "type": "function_call", "call_id": "c1", "name": "get_time", "arguments": "{\"tz\":\"UTC\"}" }
            ],
            "usage": { "input_tokens": 8, "output_tokens": 4, "total_tokens": 12 }
        });
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let resp = adapter.to_core_response(&ctx, raw).await.unwrap();

        assert_eq!(resp.id, "resp_1");
        assert_eq!(resp.content.len(), 2);
        assert!(matches!(resp.content[0], ContentBlock::Text { .. }));
        match &resp.content[1] {
            ContentBlock::ToolUse { id, name, input, .. } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "get_time");
                assert_eq!(input["tz"], "UTC");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(resp.usage, Usage { input_tokens: 8, output_tokens: 4, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 });
    }

    /// 回归：`reasoning.{effort,summary}` 必须传导到 Responses 上游。
    #[tokio::test]
    async fn propagates_reasoning_object() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);

        let mut req = CoreRequest::new("gpt-5");
        req.messages.push(Message::text(Role::User, "Hi"));
        req.reasoning = Some(Reasoning {
            effort: Some("medium".into()),
            summary: Some(json!("auto")),
        });
        let up = adapter
            .from_core_request(&ctx, &req, &endpoint())
            .await
            .unwrap();
        assert_eq!(up.body["reasoning"]["effort"], "medium");
        assert_eq!(up.body["reasoning"]["summary"], "auto");

        let plain = CoreRequest::new("gpt-5");
        let up2 = adapter
            .from_core_request(&ctx, &plain, &endpoint())
            .await
            .unwrap();
        assert!(up2.body.get("reasoning").is_none());
    }
}
