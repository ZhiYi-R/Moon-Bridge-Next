//! OpenAI Chat Completions 的 DTO 与 Chat ↔ Core 共享转换。
//!
//! 入口（client）与上游（provider）格式一致，故请求/响应的 messages、tools、
//! usage、finish_reason 映射集中于此，供四象限复用。

use moonbridge_core::{ContentBlock, Message, Role, StopReason, Tool, ToolChoice, Usage};
use serde_json::{json, Value};

/// Chat Completions 路径。
pub const CHAT_PATH: &str = "/v1/chat/completions";
/// 未指定 max_tokens 时的兜底。
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// finish_reason → Core StopReason。
pub fn map_finish_reason(s: &str) -> Option<StopReason> {
    match s {
        "stop" => Some(StopReason::EndTurn),
        "length" => Some(StopReason::MaxTokens),
        "tool_calls" | "function_call" => Some(StopReason::ToolUse),
        "content_filter" => Some(StopReason::ContentFilter),
        _ => None,
    }
}

/// Core StopReason → finish_reason。
pub fn unmap_stop_reason(r: Option<StopReason>) -> &'static str {
    match r {
        Some(StopReason::MaxTokens) => "length",
        Some(StopReason::ToolUse) => "tool_calls",
        Some(StopReason::ContentFilter) => "content_filter",
        _ => "stop",
    }
}

/// 提取内容块中的纯文本。
pub fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

/// 解析 Chat message 的 content（字符串或多模态数组）为 Core 内容块。
fn parse_chat_content(v: Option<&Value>) -> Vec<ContentBlock> {
    match v {
        Some(Value::String(txt)) => {
            if txt.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::text(txt.clone())]
            }
        }
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|p| match p.get("type").and_then(|t| t.as_str()) {
                Some("text") => Some(ContentBlock::text(
                    p.get("text").and_then(|t| t.as_str()).unwrap_or_default(),
                )),
                Some("image_url") => {
                    let url = p
                        .get("image_url")
                        .and_then(|i| i.get("url"))
                        .and_then(|u| u.as_str())
                        .unwrap_or_default();
                    Some(ContentBlock::Image {
                        data: url.to_string(),
                        media_type: "url".to_string(),
                    })
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Chat 请求 messages → Core messages（含 tool_calls / tool 结果解析）。
pub fn chat_to_core_messages(msgs: &[Value]) -> Vec<Message> {
    let mut out = Vec::new();
    for m in msgs {
        let role = match m.get("role").and_then(|r| r.as_str()) {
            Some("system") => Role::System,
            Some("assistant") => Role::Assistant,
            Some("tool") | Some("function") => Role::Tool,
            _ => Role::User,
        };
        let mut content = parse_chat_content(m.get("content"));

        // assistant.tool_calls → ToolUse
        if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
                let args = fn_obj
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .and_then(|a| serde_json::from_str::<Value>(a).ok())
                    .unwrap_or_else(|| json!({}));
                content.push(ContentBlock::ToolUse {
                    id: tc.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    name: fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    namespace: None,
                    input: args,
                    signature: None,
                });
            }
        }

        // role=tool 的消息 → ToolResult
        if role == Role::Tool {
            let tid = m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let txt = text_of(&content);
            content = vec![ContentBlock::ToolResult {
                tool_use_id: tid,
                content: vec![ContentBlock::text(txt)],
                is_error: false,
            }];
        }

        // assistant 消息的推理回传：reasoning_content（含 <mb-cot> 凭据标记）
        // → Reasoning 块，凭据还原后供 responses 类上游多轮回传
        if role == Role::Assistant {
            let raw = m
                .get("reasoning_content")
                .or_else(|| m.get("reasoning"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let (reasoning_text, signature) = split_mb_cot(raw);
            if !reasoning_text.is_empty() || signature.is_some() {
                content.insert(
                    0,
                    ContentBlock::Reasoning {
                        text: reasoning_text,
                        signature,
                    },
                );
            }
        }

        out.push(Message {
            role,
            content,
            ext: Default::default(),
        });
    }
    out
}

/// 把 role=System 的消息内容抽出为顶层 system，其余保留。
pub fn extract_system(messages: Vec<Message>) -> (Vec<ContentBlock>, Vec<Message>) {
    let mut system = Vec::new();
    let mut rest = Vec::new();
    for m in messages {
        if m.role == Role::System {
            system.extend(m.content);
        } else {
            rest.push(m);
        }
    }
    (system, rest)
}

/// Core messages（含顶层 system）→ Chat 请求 messages。
pub fn core_to_chat_messages(system: &[ContentBlock], messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    let sys_text = text_of(system);
    if !sys_text.is_empty() {
        out.push(json!({ "role": "system", "content": sys_text }));
    }
    for msg in messages {
        // 分离文本/图像、工具调用、工具结果
        let mut text_parts: Vec<ContentBlock> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        let mut tool_results: Vec<(String, String)> = Vec::new();
        for b in &msg.content {
            match b {
                ContentBlock::ToolUse { id, name, input, .. } => tool_calls.push(json!({
                    "id": id, "type": "function",
                    "function": { "name": name, "arguments": input.to_string() },
                })),
                ContentBlock::ToolResult { tool_use_id, content, .. } => {
                    tool_results.push((tool_use_id.clone(), text_of(content)))
                }
                ContentBlock::Reasoning { .. } => {}
                other => text_parts.push(other.clone()),
            }
        }

        if !tool_results.is_empty() {
            for (tid, txt) in tool_results {
                out.push(json!({ "role": "tool", "tool_call_id": tid, "content": txt }));
            }
            continue;
        }

        let role = match msg.role {
            Role::System => "system",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            Role::User => "user",
        };
        let content_value = if text_parts.len() == 1 {
            if let ContentBlock::Text { text } = &text_parts[0] {
                json!(text)
            } else {
                json!(text_parts.iter().filter_map(content_to_part).collect::<Vec<_>>())
            }
        } else {
            json!(text_parts.iter().filter_map(content_to_part).collect::<Vec<_>>())
        };

        let mut m = json!({ "role": role, "content": content_value });
        if !tool_calls.is_empty() {
            m["tool_calls"] = json!(tool_calls);
        }
        out.push(m);
    }
    out
}

/// Core 内容块 → Chat content part（多模态数组用）。
fn content_to_part(b: &ContentBlock) -> Option<Value> {
    match b {
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { data, media_type } => {
            let url = if media_type == "url" {
                data.clone()
            } else {
                format!("data:{media_type};base64,{data}")
            };
            Some(json!({ "type": "image_url", "image_url": { "url": url } }))
        }
        _ => None,
    }
}

/// Chat 请求 tools → Core tools。
pub fn chat_tools_to_core(tools: &[Value]) -> Vec<Tool> {
    tools
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(Tool {
                name: f.get("name").and_then(|v| v.as_str())?.to_string(),
                description: f.get("description").and_then(|v| v.as_str()).map(String::from),
                input_schema: f
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" })),
                ext: Default::default(),
            })
        })
        .collect()
}

/// Core tools → Chat 请求 tools。
pub fn core_to_chat_tools(tools: &[Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let mut f = json!({ "name": t.name, "parameters": t.input_schema });
            if let Some(d) = &t.description {
                f["description"] = json!(d);
            }
            json!({ "type": "function", "function": f })
        })
        .collect()
}

/// 解析 Chat tool_choice。
pub fn parse_tool_choice(v: &Value) -> Option<ToolChoice> {
    if let Some(str_v) = v.as_str() {
        return match str_v {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" => Some(ToolChoice::Required),
            _ => None,
        };
    }
    match v.get("type").and_then(|t| t.as_str()) {
        Some("none") => Some(ToolChoice::None),
        Some("function") => v
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .map(|n| ToolChoice::Tool { name: n.to_string() }),
        _ => None,
    }
}

/// Core tool_choice → Chat tool_choice。
pub fn unparse_tool_choice(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => json!({ "type": "function", "function": { "name": name } }),
    }
}

/// Chat 响应的 choices[0].message → Core 内容块 + finish_reason。
pub fn chat_choice_to_core(choice: &Value) -> (Vec<ContentBlock>, Option<StopReason>) {
    let msg = choice.get("message").cloned().unwrap_or(Value::Null);
    let mut content = parse_chat_content(msg.get("content"));
    // 推理文本（DeepSeek 系 reasoning_content / OpenRouter 系 reasoning）：
    // 含 <mb-cot> 凭据标记时拆出凭据（嵌套网关场景），否则整体为展示明文；
    // 置于正文之前，缺失时不产生空块
    if let Some(raw) = msg
        .get("reasoning_content")
        .or_else(|| msg.get("reasoning"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let (text, signature) = split_mb_cot(raw);
        if !text.is_empty() || signature.is_some() {
            content.insert(
                0,
                ContentBlock::Reasoning { text, signature },
            );
        }
    }
    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tcs {
            let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
            let args = fn_obj
                .get("arguments")
                .and_then(|a| a.as_str())
                .and_then(|a| serde_json::from_str::<Value>(a).ok())
                .unwrap_or_else(|| json!({}));
            content.push(ContentBlock::ToolUse {
                id: tc.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                name: fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                namespace: None,
                input: args,
                signature: None,
            });
        }
    }
    let finish = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .and_then(map_finish_reason);
    (content, finish)
}

/// 凭据定界标记：凭据（不可读文本）以此包裹拼在 reasoning_content 尾部下发，
/// 客户端回传 assistant 历史时随 reasoning_content 带回，回传侧解析还原。
const MB_COT_OPEN: &str = "<mb-cot>";
const MB_COT_CLOSE: &str = "</mb-cot>";

/// 从 reasoning 字段拆出（展示明文，回传凭据）：凭据以 `<mb-cot>…</mb-cot>`
/// 定界，无标记则整体视为明文（普通上游如 DeepSeek 的 reasoning_content）。
fn split_mb_cot(s: &str) -> (String, Option<String>) {
    let Some(start) = s.find(MB_COT_OPEN) else {
        return (s.to_string(), None);
    };
    let Some(rel) = s[start..].find(MB_COT_CLOSE) else {
        return (s.to_string(), None);
    };
    let enc = s[start + MB_COT_OPEN.len()..start + rel].trim().to_string();
    let mut text = String::with_capacity(s.len());
    text.push_str(&s[..start]);
    text.push_str(&s[start + rel + MB_COT_CLOSE.len()..]);
    let sig = if enc.is_empty() { None } else { Some(enc) };
    (text.trim().to_string(), sig)
}

/// Core 内容块 → Chat 响应的 assistant message 对象。
///
/// Reasoning 下发：`reasoning_content`/`reasoning`（明文展示，双写两种生态约定）；
/// 凭据（不可读文本）以 `<mb-cot>…</mb-cot>` 定界标记拼在字段尾部——chat 协议
/// 无凭据承载字段、tool result 也无法由响应侧闭合，reasoning_content 是
/// assistant 消息上唯一可控的搭车位，客户端回传历史时凭据随之返回。
pub fn core_to_chat_response_message(content: &[ContentBlock]) -> Value {
    let mut tool_calls = Vec::new();
    let mut text_parts: Vec<ContentBlock> = Vec::new();
    let mut reasoning_parts: Vec<&str> = Vec::new();
    let mut credential: Option<&str> = None;
    for b in content {
        match b {
            ContentBlock::ToolUse { id, name, input, .. } => tool_calls.push(json!({
                "id": id, "type": "function",
                "function": { "name": name, "arguments": input.to_string() },
            })),
            ContentBlock::Reasoning { text, signature } => {
                if !text.is_empty() {
                    reasoning_parts.push(text);
                }
                if signature.as_deref().is_some_and(|s| !s.is_empty()) {
                    credential = signature.as_deref();
                }
            }
            other => text_parts.push(other.clone()),
        }
    }
    let mut msg = json!({ "role": "assistant", "content": text_of(&text_parts) });
    let mut reasoning = reasoning_parts.concat();
    if let Some(enc) = credential {
        reasoning.push_str(MB_COT_OPEN);
        reasoning.push_str(enc);
        reasoning.push_str(MB_COT_CLOSE);
    }
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
        msg["reasoning"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }
    msg
}

/// Chat usage → Core Usage（prompt_tokens/completion_tokens）。
pub fn usage_from_chat(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        output_tokens: v.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        cache_read_tokens: v
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cache_write_tokens: 0,
        reasoning_tokens: v
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
    }
}

/// 构造 usage 对象（Chat 命名）。
pub fn usage_object(u: &Usage) -> Value {
    json!({
        "prompt_tokens": u.input_tokens,
        "completion_tokens": u.output_tokens,
        "total_tokens": u.input_tokens + u.output_tokens,
        "completion_tokens_details": { "reasoning_tokens": u.reasoning_tokens },
    })
}

/// 便捷取字段（供 client/provider 复用）。
pub fn str_field(v: &Value, key: &str) -> String {
    s(v, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 上游非流式 message.reasoning_content / reasoning → Reasoning 块（置于正文前）。
    #[test]
    fn chat_choice_to_core_carries_reasoning() {
        let choice = json!({
            "message": {
                "role": "assistant",
                "content": "pong",
                "reasoning_content": "let me think"
            },
            "finish_reason": "stop"
        });
        let (content, finish) = chat_choice_to_core(&choice);
        assert_eq!(finish, Some(StopReason::EndTurn));
        assert_eq!(content.len(), 2);
        match &content[0] {
            ContentBlock::Reasoning { text, signature } => {
                assert_eq!(text, "let me think");
                assert!(signature.is_none());
            }
            other => panic!("expected reasoning first, got {other:?}"),
        }
        match &content[1] {
            ContentBlock::Text { text } => assert_eq!(text, "pong"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn chat_choice_to_core_without_reasoning_is_unchanged() {
        let choice = json!({
            "message": { "role": "assistant", "content": "hi" },
            "finish_reason": "stop"
        });
        let (content, _) = chat_choice_to_core(&choice);
        assert_eq!(content.len(), 1);
        assert!(matches!(&content[0], ContentBlock::Text { text } if text == "hi"));
    }

    /// 凭据搭车：signature 以 <mb-cot> 定界拼入 reasoning_content 下发；
    /// 客户端回传 assistant 历史 → 标记解析拆出凭据（signature）与展示明文。
    #[test]
    fn reasoning_credential_roundtrips_via_mb_cot_marker() {
        // 下发：凭据（不可读文本）拼在 reasoning_content 尾部
        let msg = core_to_chat_response_message(&[
            ContentBlock::Reasoning { text: "thought".into(), signature: Some("ENC".into()) },
            ContentBlock::text("Hi"),
        ]);
        assert_eq!(msg["reasoning_content"], "thought<mb-cot>ENC</mb-cot>");
        assert_eq!(msg["reasoning"], "thought<mb-cot>ENC</mb-cot>");
        // 无凭据时不带标记
        let plain = core_to_chat_response_message(&[
            ContentBlock::Reasoning { text: "pure".into(), signature: None },
        ]);
        assert_eq!(plain["reasoning_content"], "pure");

        // 回传：标记解析拆出凭据与明文
        let msgs = chat_to_core_messages(&[json!({
            "role": "assistant",
            "content": "Hi",
            "reasoning_content": "thought<mb-cot>ENC</mb-cot>"
        })]);
        match &msgs[0].content[0] {
            ContentBlock::Reasoning { text, signature: Some(enc) } => {
                assert_eq!(text, "thought");
                assert_eq!(enc, "ENC");
            }
            other => panic!("expected reasoning with credential, got {other:?}"),
        }

        // 纯凭据（无明文）：凭据还原、展示明文为空
        let msgs2 = chat_to_core_messages(&[json!({
            "role": "assistant",
            "content": "Hi",
            "reasoning_content": "<mb-cot>ENC</mb-cot>"
        })]);
        assert!(matches!(
            &msgs2[0].content[0],
            ContentBlock::Reasoning { text, signature: Some(enc) } if text.is_empty() && enc == "ENC"
        ));

        // 无标记的普通上游明文（如 DeepSeek）→ 整体为展示文本，无凭据
        let msgs3 = chat_to_core_messages(&[json!({
            "role": "assistant",
            "content": "Hi",
            "reasoning_content": "plain upstream thought"
        })]);
        assert!(matches!(
            &msgs3[0].content[0],
            ContentBlock::Reasoning { text, signature: None } if text == "plain upstream thought"
        ));
    }
}
