//! OpenAI Responses 入口 Adapter —— 非流式部分。
//!
//! `to_core_request`：Responses 请求（instructions/input/tools/...）→ CoreRequest。
//! `from_core_response`：CoreResponse → Responses 响应对象。

use async_trait::async_trait;
use moonbridge_core::{
    ContentBlock, CoreRequest, CoreResponse, Message, Protocol, Reasoning, Result, Role, Tool,
    ToolChoice, Usage,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use super::dto;
use super::OpenAiResponsesAdapter;
use crate::adapter::ClientAdapter;
use crate::context::ReqCtx;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 解析 Responses content 项为 Core 内容块。
pub(super) fn parse_content_item(item: &Value) -> Option<ContentBlock> {
    match item.get("type")?.as_str()? {
        "input_text" | "output_text" | "text" => {
            Some(ContentBlock::text(item.get("text")?.as_str()?.to_string()))
        }
        "input_image" | "image" | "output_image" => parse_image(item),
        _ => None,
    }
}

/// 解析图像项，支持 `data:` URL 与远程 URL。
fn parse_image(item: &Value) -> Option<ContentBlock> {
    let url = item
        .get("image_url")
        .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(|u| u.as_str())))
        .or_else(|| item.get("source").and_then(|s| s.get("data")).and_then(|d| d.as_str()))?;
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        let media_type = meta.split(';').next().unwrap_or("image/png").to_string();
        Some(ContentBlock::Image {
            data: data.to_string(),
            media_type,
        })
    } else {
        Some(ContentBlock::Image {
            data: url.to_string(),
            media_type: "url".to_string(),
        })
    }
}

/// 解析 Responses tool 定义为 CoreTool。
fn parse_tool(item: &Value) -> Option<Tool> {
    // 仅处理 function 类型；Responses 采用扁平结构 {type,name,description,parameters}
    let name = item.get("name").and_then(|v| v.as_str())?;
    let description = item
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let input_schema = item
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object"}));
    Some(Tool {
        name: name.to_string(),
        description,
        input_schema,
        ext: Default::default(),
    })
}

/// 解析 tool_choice。
fn parse_tool_choice(v: &Value) -> Option<ToolChoice> {
    if let Some(s) = v.as_str() {
        return match s {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" => Some(ToolChoice::Required),
            _ => None,
        };
    }
    if v.get("type").and_then(|t| t.as_str()) == Some("function") {
        if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
            return Some(ToolChoice::Tool {
                name: name.to_string(),
            });
        }
    }
    None
}

#[async_trait]
impl ClientAdapter for OpenAiResponsesAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiResponse
    }

    async fn to_core_request(&self, _ctx: &ReqCtx, raw: Value) -> Result<CoreRequest> {
        let model = raw
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let mut req = CoreRequest::new(model.clone());
        req.model_alias = model;

        // instructions → system
        if let Some(instr) = raw.get("instructions").and_then(|v| v.as_str()) {
            if !instr.is_empty() {
                req.system.push(ContentBlock::text(instr.to_string()));
            }
        }

        // input：字符串或 items 数组
        match raw.get("input") {
            Some(Value::String(s)) => {
                req.messages
                    .push(Message::text(Role::User, s.clone()));
            }
            Some(Value::Array(items)) => {
                for item in items {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("message") | None => {
                            let role = match item.get("role").and_then(|r| r.as_str()) {
                                Some("system") | Some("developer") => Role::System,
                                Some("assistant") => Role::Assistant,
                                Some("tool") => Role::Tool,
                                _ => Role::User,
                            };
                            let mut content = Vec::new();
                            if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                                for p in parts {
                                    if let Some(block) = parse_content_item(p) {
                                        content.push(block);
                                    }
                                }
                            } else if let Some(text) = item.get("content").and_then(|c| c.as_str())
                            {
                                content.push(ContentBlock::text(text.to_string()));
                            }
                            if content.is_empty() {
                                continue;
                            }
                            if role == Role::System {
                                req.system.extend(content);
                            } else {
                                req.messages.push(Message {
                                    role,
                                    content,
                                    ext: Default::default(),
                                });
                            }
                        }
                        Some("function_call") => {
                            let id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let input = item
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                                .unwrap_or_else(|| json!({}));
                            req.messages.push(Message {
                                role: Role::Assistant,
                                content: vec![ContentBlock::ToolUse {
                                    id,
                                    name,
                                    namespace: None,
                                    input,
                                    signature: None,
                                }],
                                ext: Default::default(),
                            });
                        }
                        Some("function_call_output") => {
                            let id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let output = match item.get("output") {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => other.to_string(),
                                None => String::new(),
                            };
                            req.messages.push(Message {
                                role: Role::Tool,
                                content: vec![ContentBlock::ToolResult {
                                    tool_use_id: id,
                                    content: vec![ContentBlock::text(output)],
                                    is_error: false,
                                }],
                                ext: Default::default(),
                            });
                        }
                        Some("reasoning") => {
                            // 历史 reasoning：明文取 summary[].text / content[].text，
                            // encrypted_content 存入 signature（Reasoning.signature 语义
                            // 即「不透明回传凭据」，与 Anthropic thinking 签名一致）。
                            // 多轮 function-call 循环中缺失 reasoning item 会 400，
                            // 即便无明文也要保留凭据。
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
                            let signature = item
                                .get("encrypted_content")
                                .and_then(|v| v.as_str())
                                .map(String::from)
                                .filter(|s| !s.is_empty());
                            if text.is_empty() && signature.is_none() {
                                continue;
                            }
                            req.messages.push(Message {
                                role: Role::Assistant,
                                content: vec![ContentBlock::Reasoning { text, signature }],
                                ext: Default::default(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        // tools
        if let Some(tools) = raw.get("tools").and_then(|t| t.as_array()) {
            for t in tools {
                if let Some(tool) = parse_tool(t) {
                    req.tools.push(tool);
                }
            }
        }
        if let Some(tc) = raw.get("tool_choice") {
            req.tool_choice = parse_tool_choice(tc);
        }

        // 采样参数
        req.max_tokens = raw
            .get("max_output_tokens")
            .or_else(|| raw.get("max_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        req.temperature = raw.get("temperature").and_then(|v| v.as_f64()).map(|v| v as f32);
        req.top_p = raw.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32);
        req.stream = raw.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

        // reasoning
        if let Some(r) = raw.get("reasoning") {
            req.reasoning = Some(Reasoning {
                effort: r.get("effort").and_then(|v| v.as_str()).map(|s| s.to_string()),
                summary: r.get("summary").cloned(),
            });
        }

        Ok(req)
    }

    async fn from_core_response(&self, _ctx: &ReqCtx, resp: CoreResponse) -> Result<Value> {
        let mut output: Vec<Value> = Vec::new();
        let mut text_buf = String::new();
        let mut item_seq = 0usize;

        let flush_text = |buf: &mut String, output: &mut Vec<Value>, seq: &mut usize| {
            if !buf.is_empty() {
                let id = format!("msg_{}", seq);
                *seq += 1;
                output.push(dto::message_item(&id, buf, "completed"));
                buf.clear();
            }
        };

        for block in &resp.content {
            match block {
                ContentBlock::Text { text } => {
                    text_buf.push_str(text);
                }
                ContentBlock::Reasoning { text, signature } => {
                    let mut item = json!({
                        "type": "reasoning",
                        "id": format!("rs_{}", { item_seq += 1; item_seq }),
                        "summary": [{ "type": "summary_text", "text": text }],
                    });
                    if let Some(enc) = signature {
                        if !enc.is_empty() {
                            item["encrypted_content"] = json!(enc);
                        }
                    }
                    output.push(item);
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    ..
                } => {
                    flush_text(&mut text_buf, &mut output, &mut item_seq);
                    output.push(dto::function_call_item(
                        id,
                        name,
                        &input.to_string(),
                        "completed",
                    ));
                }
                _ => {}
            }
        }
        flush_text(&mut text_buf, &mut output, &mut item_seq);

        let usage = dto::usage_object(resp.usage.input_tokens, resp.usage.output_tokens);

        Ok(json!({
            "id": resp.id,
            "object": dto::OBJECT_RESPONSE,
            "created_at": now_unix(),
            "status": "completed",
            "model": resp.model,
            "output": output,
            "usage": usage,
        }))
    }
}

/// 供 gateway/测试复用的 usage 提取（从 Responses usage 对象）。
pub fn usage_from_value(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        output_tokens: v.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        cache_read_tokens: v
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cache_write_tokens: 0,
        reasoning_tokens: v
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
    }
}
