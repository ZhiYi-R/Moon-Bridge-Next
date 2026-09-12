//! OpenAI Responses 入口 Adapter —— 非流式部分。
//!
//! `to_core_request`：Responses 请求（instructions/input/tools/...）→ CoreRequest。
//! `from_core_response`：CoreResponse → Responses 响应对象。

use async_trait::async_trait;
use moonbridge_core::{
    ContentBlock, CoreRequest, CoreResponse, DocSource, Message, Protocol, Reasoning, Result,
    Role, Tool, ToolChoice, Usage,
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
        // computer_screenshot 是 computer_call_output 的截图形态，字段同 input_image
        "input_image" | "image" | "output_image" | "computer_screenshot" => parse_image(item),
        "input_file" | "file" => parse_file(item),
        // refusal 是模型拒答的可见内容，折叠为文本块保住信息（协议无对应块）。
        "refusal" => item
            .get("refusal")
            .and_then(|r| r.as_str())
            .map(|r| ContentBlock::text(r.to_string())),
        _ => None,
    }
}

/// 解析 `input_file` 项：file_id / file_url / file_data（data: URL）三种来源。
fn parse_file(item: &Value) -> Option<ContentBlock> {
    let name = item
        .get("filename")
        .and_then(|n| n.as_str())
        .map(String::from);
    if let Some(fid) = item.get("file_id").and_then(|v| v.as_str()) {
        return Some(ContentBlock::Document {
            source: DocSource::File,
            media_type: "application/octet-stream".into(),
            data: fid.to_string(),
            name,
        });
    }
    if let Some(fu) = item.get("file_url").and_then(|v| v.as_str()) {
        return Some(ContentBlock::Document {
            source: DocSource::Url,
            media_type: "application/octet-stream".into(),
            data: fu.to_string(),
            name,
        });
    }
    let fd = item.get("file_data").and_then(|v| v.as_str())?;
    let (mt, data) = fd
        .strip_prefix("data:")
        .and_then(|r| r.split_once(','))
        .map(|(m, d)| (m.split(';').next().unwrap_or("application/octet-stream"), d))
        .unwrap_or(("application/octet-stream", fd));
    Some(ContentBlock::Document {
        source: DocSource::Base64,
        media_type: mt.to_string(),
        data: data.to_string(),
        name,
    })
}

/// 解析图像项，支持 `data:` URL、远程 URL 与内联的 Anthropic 形态 `source`。
fn parse_image(item: &Value) -> Option<ContentBlock> {
    // Anthropic 形态 source 内联（部分网关把 anthropic 块直接塞进 responses 输入）：
    // 只取 source.data 且记为 "url" 会把 base64 串当 URL 发出——按 source.type 分派；
    // source 解析不出值时回退到 image_url 字段。
    if let Some(src) = item.get("source") {
        let parsed = match src.get("type").and_then(|t| t.as_str()) {
            Some("url") => src.get("url").and_then(|u| u.as_str()).map(|u| {
                ContentBlock::Image {
                    data: u.to_string(),
                    media_type: "url".to_string(),
                }
            }),
            Some("file") => src.get("file_id").and_then(|f| f.as_str()).map(|f| {
                ContentBlock::Image {
                    data: f.to_string(),
                    media_type: "file".to_string(),
                }
            }),
            _ => src.get("data").and_then(|d| d.as_str()).map(|d| {
                ContentBlock::Image {
                    data: d.to_string(),
                    media_type: src
                        .get("media_type")
                        .and_then(|m| m.as_str())
                        .unwrap_or("image/png")
                        .to_string(),
                }
            }),
        };
        if parsed.is_some() {
            return parsed;
        }
    }
    let url = item
        .get("image_url")
        .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(|u| u.as_str())))?;
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

/// `*_call` 类 output item（function_call/custom_tool_call/computer_call/
/// mcp_call/local_shell_call 等）→ 装 ToolUse 的 assistant 消息。
/// 原始 item type 记入 `ToolUse.namespace` 供出站还原；参数字段名随类型
/// 不同（arguments 字符串 / input 字符串 / action 对象），统一收进 input。
fn call_item_to_message(item: &Value, namespace: Option<String>) -> Message {
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| namespace.as_deref().unwrap_or("call"))
        .to_string();
    let input = item
        .get("arguments")
        .map(|a| match a {
            // arguments 规范形态是 JSON 字符串；对象形态（非标准实现）直接收
            Value::String(s) => {
                serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({}))
            }
            Value::Object(_) => a.clone(),
            _ => json!({}),
        })
        .or_else(|| item.get("action").cloned())
        .or_else(|| {
            item.get("input").map(|v| match v {
                Value::String(s) => serde_json::from_str::<Value>(s)
                    .unwrap_or_else(|_| json!({ "input": s })),
                other => other.clone(),
            })
        })
        .unwrap_or_else(|| json!({}));
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id,
            name,
            namespace,
            input,
            signature: None,
        }],
        ext: Default::default(),
    }
}

/// `*_call_output` 类 output item → 装 ToolResult 的 tool 消息。
/// output 可为字符串或 part 数组（input_text/input_image/input_file/
/// computer_screenshot 等）：数组形态按块解析，此前整体 to_string 会把
/// 内嵌图片变成 JSON 噪声文本；对象形态（computer_screenshot）按单项解析。
fn call_output_to_message(item: &Value) -> Message {
    let id = item
        .get("call_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let content = match item.get("output") {
        Some(Value::String(s)) => vec![ContentBlock::text(s.clone())],
        Some(Value::Array(arr)) => {
            let blocks: Vec<ContentBlock> =
                arr.iter().filter_map(parse_content_item).collect();
            if blocks.is_empty() {
                vec![ContentBlock::text(String::new())]
            } else {
                blocks
            }
        }
        Some(v @ Value::Object(_)) => {
            vec![parse_content_item(v)
                .unwrap_or_else(|| ContentBlock::text(v.to_string()))]
        }
        Some(other) => vec![ContentBlock::text(other.to_string())],
        None => vec![ContentBlock::text(String::new())],
    };
    Message {
        role: Role::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id,
            content,
            is_error: false,
        }],
        ext: Default::default(),
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
                            req.messages.push(call_item_to_message(item, None));
                        }
                        Some("function_call_output") => {
                            req.messages.push(call_output_to_message(item));
                        }
                        // custom_tool_call / computer_call / mcp_call /
                        // local_shell_call 等同族 item：此前整块丢弃。
                        Some(ty) if ty.ends_with("_call") => {
                            req.messages
                                .push(call_item_to_message(item, Some(ty.to_string())));
                        }
                        Some(ty) if ty.ends_with("_call_output") => {
                            req.messages.push(call_output_to_message(item));
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
                            let signature = crate::adapters::tag_signature(
                                crate::adapters::SIG_OPENAI,
                                item.get("encrypted_content")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                            );
                            if text.is_empty() && signature.is_none() {
                                continue;
                            }
                            req.messages.push(Message {
                                role: Role::Assistant,
                                content: vec![ContentBlock::Reasoning { text, signature, redacted: false }],
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

        // instructions 是 Responses 的 system 等价物：并入顶层 system，
        // 供 Anthropic/Gemini 上游按各自形态下发。
        if let Some(instr) = raw.get("instructions").and_then(|v| v.as_str()) {
            if !instr.is_empty() {
                req.system.push(ContentBlock::text(instr));
            }
        }

        // 未解析顶层字段透传（Responses→Responses 保真）：previous_response_id、
        // store、background、include、truncation、parallel_tool_calls、metadata 等
        // 经 Core 无字段位，整体收进 meta，出站时合并回请求体。
        if let Some(obj) = raw.as_object() {
            const KNOWN: &[&str] = &[
                "model", "input", "tools", "tool_choice", "reasoning",
                "instructions", "max_output_tokens", "max_tokens", "temperature",
                "top_p", "stream",
            ];
            let extra: serde_json::Map<String, Value> = obj
                .iter()
                .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !extra.is_empty() {
                req.meta
                    .insert("responses.extra".to_string(), Value::Object(extra));
            }
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
                ContentBlock::Reasoning { text, signature, .. } => {
                    let mut item = json!({
                        "type": "reasoning",
                        "id": format!("rs_{}", { item_seq += 1; item_seq }),
                        "summary": [{ "type": "summary_text", "text": text }],
                    });
                    if let Some(enc) = signature
                        .as_deref()
                        .map(|s| crate::adapters::emit_signature(crate::adapters::SIG_OPENAI, s))
                    {
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

        let usage = dto::usage_object(&resp.usage);

        // status 语义：输出因上限/过滤被截断时是 "incomplete" 而非
        // "completed"——客户端据此决定是否能直接使用结果。
        let (status, incomplete) = match resp.stop_reason {
            Some(moonbridge_core::StopReason::MaxTokens) => {
                ("incomplete", Some(json!({ "reason": "max_output_tokens" })))
            }
            Some(moonbridge_core::StopReason::ContentFilter) => {
                ("incomplete", Some(json!({ "reason": "content_filter" })))
            }
            _ => ("completed", None),
        };
        let mut obj = json!({
            "id": resp.id,
            "object": dto::OBJECT_RESPONSE,
            "created_at": now_unix(),
            "status": status,
            "model": resp.model,
            "output": output,
            "usage": usage,
        });
        if let Some(d) = incomplete {
            obj["incomplete_details"] = d;
        }
        Ok(obj)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：function_call_output.output 的 part 数组形态按块解析——内嵌图片
    /// 要进 ToolResult.content，此前整体 `to_string` 会把图片变成 JSON 噪声
    /// 文本；字符串形态保持既有行为。
    #[tokio::test]
    async fn function_call_output_array_parts_parse() {
        let raw = json!({
            "model": "gpt-x",
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "c1",
                    "output": [
                        { "type": "output_text", "text": "shot" },
                        { "type": "input_image", "image_url": "data:image/png;base64,AAA" },
                        { "type": "input_image", "image_url": "https://example.com/x.png" }
                    ]
                },
                {
                    "type": "function_call_output",
                    "call_id": "c2",
                    "output": "plain"
                }
            ]
        });
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let req = adapter.to_core_request(&ctx, raw).await.unwrap();

        assert_eq!(req.messages.len(), 2);
        let ContentBlock::ToolResult { tool_use_id, content, .. } = &req.messages[0].content[0]
        else {
            panic!("应为 ToolResult: {:?}", req.messages[0].content)
        };
        assert_eq!(tool_use_id, "c1");
        assert_eq!(content.len(), 3, "文本与两张图都应保留: {content:?}");
        assert!(matches!(&content[0], ContentBlock::Text { text } if text == "shot"));
        assert!(
            matches!(&content[1], ContentBlock::Image { data, media_type }
                if data == "AAA" && media_type == "image/png"),
            "data: URL 应拆分为 base64+media_type: {:?}",
            content[1]
        );
        assert!(
            matches!(&content[2], ContentBlock::Image { data, media_type }
                if data == "https://example.com/x.png" && media_type == "url"),
            "远程 URL 应保持 url 形态: {:?}",
            content[2]
        );

        let ContentBlock::ToolResult { content, .. } = &req.messages[1].content[0] else {
            panic!("应为 ToolResult: {:?}", req.messages[1].content)
        };
        assert_eq!(content.len(), 1);
        assert!(matches!(&content[0], ContentBlock::Text { text } if text == "plain"));
    }

    /// 回归：Anthropic 形态 source 内联的图像项按 source.type 分派——只取
    /// source.data 且记为 "url" 会把 base64 串当 URL 发出（上游 400/垃圾）。
    #[test]
    fn inline_source_image_dispatches_by_type() {
        let b64 = parse_content_item(&json!({
            "type": "input_image",
            "source": { "type": "base64", "media_type": "image/jpeg", "data": "JJJ" }
        }))
        .unwrap();
        assert!(
            matches!(&b64, ContentBlock::Image { data, media_type }
                if data == "JJJ" && media_type == "image/jpeg"),
            "base64 source 应保媒体类型: {b64:?}"
        );

        let file = parse_content_item(&json!({
            "type": "input_image",
            "source": { "type": "file", "file_id": "file_x" }
        }))
        .unwrap();
        assert!(
            matches!(&file, ContentBlock::Image { data, media_type }
                if data == "file_x" && media_type == "file"),
            "file source 应记为 file 形态: {file:?}"
        );

        // source 解析不出值时回退 image_url 字段
        let fallback = parse_content_item(&json!({
            "type": "input_image",
            "image_url": "https://example.com/f.png",
            "source": {}
        }))
        .unwrap();
        assert!(
            matches!(&fallback, ContentBlock::Image { data, media_type }
                if data == "https://example.com/f.png" && media_type == "url"),
            "空 source 不应吞掉 image_url: {fallback:?}"
        );
    }

    /// 回归：input_file → Document（三种来源），此前整块丢弃。
    #[test]
    fn input_file_parts_parse() {
        let by_id = parse_content_item(&json!({
            "type": "input_file", "file_id": "file-1", "filename": "a.pdf"
        }))
        .unwrap();
        assert!(matches!(&by_id, ContentBlock::Document { source, data, name, .. }
            if *source == DocSource::File && data == "file-1" && name.as_deref() == Some("a.pdf")));

        let by_data = parse_content_item(&json!({
            "type": "input_file", "file_data": "data:application/pdf;base64,PP", "filename": "b.pdf"
        }))
        .unwrap();
        assert!(matches!(&by_data, ContentBlock::Document { source, media_type, data, .. }
            if *source == DocSource::Base64 && media_type == "application/pdf" && data == "PP"));
    }

    /// 回归：custom_tool_call/computer_call 等 *_call 族 item 入站不再丢弃——
    /// 映射为 ToolUse，原始 item type 记入 namespace 供出站还原；
    /// *_call_output 同 function_call_output 解析。
    #[tokio::test]
    async fn non_function_call_items_parse() {
        let raw = json!({
            "model": "gpt-x",
            "input": [
                { "type": "custom_tool_call", "call_id": "ct1", "name": "exec", "input": "{\"c\":\"ls\"}" },
                { "type": "custom_tool_call_output", "call_id": "ct1", "output": "done" },
                { "type": "computer_call", "call_id": "cc1", "action": { "type": "click", "x": 1 } },
                { "type": "computer_call_output", "call_id": "cc1",
                  "output": { "type": "computer_screenshot", "image_url": "https://x/s.png" } }
            ]
        });
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let req = adapter.to_core_request(&ctx, raw).await.unwrap();
        assert_eq!(req.messages.len(), 4);

        let ContentBlock::ToolUse { id, name, namespace, input, .. } = &req.messages[0].content[0]
        else {
            panic!()
        };
        assert_eq!((id.as_str(), name.as_str()), ("ct1", "exec"));
        assert_eq!(namespace.as_deref(), Some("custom_tool_call"), "原始 type 须入 namespace");
        assert_eq!(input["c"], "ls");

        let ContentBlock::ToolResult { tool_use_id, .. } = &req.messages[1].content[0] else {
            panic!()
        };
        assert_eq!(tool_use_id, "ct1");

        let ContentBlock::ToolUse { id, namespace, input, .. } = &req.messages[2].content[0] else {
            panic!()
        };
        assert_eq!((id.as_str(), namespace.as_deref()), ("cc1", Some("computer_call")));
        assert_eq!(input["type"], "click", "action 对象整体收进 input");

        // 对象形态 output（computer_screenshot）按单项解析为图片，非 JSON 噪声
        let ContentBlock::ToolResult { content, .. } = &req.messages[3].content[0] else {
            panic!()
        };
        assert!(matches!(&content[0], ContentBlock::Image { .. }), "截屏应解析为图片: {content:?}");
    }

    /// 回归：function_call 的 arguments 对象形态（非标准实现）不得落 {}。
    #[test]
    fn object_form_arguments_parse() {
        let m = call_item_to_message(
            &json!({ "type": "function_call", "call_id": "c1", "name": "f",
                     "arguments": { "x": 1 } }),
            None,
        );
        let ContentBlock::ToolUse { input, .. } = &m.content[0] else { panic!() };
        assert_eq!(input["x"], 1);
    }
}
