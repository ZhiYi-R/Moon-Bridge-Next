//! Google Generative AI (Gemini) 的 DTO 与 Gemini ↔ Core 共享转换。
//!
//! 入口（client）与上游（provider）格式一致，故请求/响应的 contents、parts、
//! tools、usage、finishReason 映射集中于此，供四象限复用。
//!
//! Gemini 报文要点：
//! - 消息在 `contents`，每条 `{ role: "user"|"model", parts: [...] }`；
//! - system 走顶层 `systemInstruction`；
//! - part 种类：`text` / `inlineData` / `functionCall` / `functionResponse`；
//! - 工具声明在 `tools[].functionDeclarations`，选择策略在 `toolConfig.functionCallingConfig`；
//! - 采样参数集中在 `generationConfig`；用量在 `usageMetadata`。

use std::collections::HashMap;

use crate::adapters::{clamped_thinking_budget, reasoning_effort};
use moonbridge_core::{
    ContentBlock, CoreRequest, Message, Role, StopReason, Tool, ToolChoice, Usage,
};
use serde_json::{json, Value};

/// 未显式指定协议版本时使用的 API 版本段。
pub const DEFAULT_VERSION: &str = "v1beta";

/// finishReason → Core StopReason。
pub fn map_finish_reason(s: &str) -> Option<StopReason> {
    match s {
        "MAX_TOKENS" => Some(StopReason::MaxTokens),
        "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" => {
            Some(StopReason::ContentFilter)
        }
        "STOP" | "LANGUAGE" | "OTHER" | "MALFORMED_FUNCTION_CALL" => Some(StopReason::EndTurn),
        _ => None,
    }
}

/// Core StopReason → Gemini finishReason。
pub fn unmap_stop_reason(r: Option<StopReason>) -> &'static str {
    match r {
        Some(StopReason::MaxTokens) => "MAX_TOKENS",
        Some(StopReason::ContentFilter) => "SAFETY",
        _ => "STOP",
    }
}

/// `generateContent` 路径（非流式）。
pub fn generate_path(version: &str, model: &str) -> String {
    format!(
        "/{}/models/{}:generateContent",
        version.trim_matches('/'),
        model
    )
}

/// `streamGenerateContent` 路径（流式，`alt=sse` 以标准 SSE 返回）。
pub fn stream_path(version: &str, model: &str) -> String {
    format!(
        "/{}/models/{}:streamGenerateContent?alt=sse",
        version.trim_matches('/'),
        model
    )
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

// ============================================================================
// Core → Gemini
// ============================================================================

/// Core messages → Gemini `contents`（system 角色跳过，另走 systemInstruction）。
///
/// Gemini 的 `functionResponse` 按**函数名**关联结果，而 Core 的 `ToolResult` 只有
/// `tool_use_id`；故先扫描全部 `ToolUse` 建立 id→name 映射，再据此回填函数名。
pub fn core_to_contents(messages: &[Message]) -> Vec<Value> {
    let mut id_to_name: HashMap<&str, &str> = HashMap::new();
    for m in messages {
        for b in &m.content {
            if let ContentBlock::ToolUse { id, name, .. } = b {
                id_to_name.insert(id.as_str(), name.as_str());
            }
        }
    }

    let mut out = Vec::new();
    for m in messages {
        if m.role == Role::System {
            continue;
        }
        let role = match m.role {
            Role::Assistant => "model",
            // Tool 结果在 Gemini 里以 user 回合的 functionResponse part 呈现
            _ => "user",
        };
        let mut parts: Vec<Value> = Vec::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => parts.push(json!({ "text": text })),
                ContentBlock::Image { data, media_type } => {
                    if media_type != "url" {
                        parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
                    }
                }
                ContentBlock::ToolUse { name, input, signature, .. } => {
                    let mut fc = json!({ "name": name, "args": input });
                    if let Some(sig) = signature {
                        if !sig.is_empty() {
                            fc["thoughtSignature"] = json!(sig);
                        }
                    }
                    parts.push(json!({ "functionCall": fc }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let name = id_to_name
                        .get(tool_use_id.as_str())
                        .copied()
                        .unwrap_or(tool_use_id.as_str());
                    parts.push(json!({
                        "functionResponse": { "name": name, "response": { "result": text_of(content) } }
                    }));
                }
                ContentBlock::Reasoning { text, signature, .. } => {
                    // 历史 thought 回传：Gemini 2.5 多轮 function calling 要求
                    // thoughtSignature 原样带回（缺失会被拒或退化）。
                    let mut part = json!({ "thought": true });
                    if !text.is_empty() {
                        part["text"] = json!(text);
                    }
                    if let Some(sig) = signature {
                        if !sig.is_empty() {
                            part["thoughtSignature"] = json!(sig);
                        }
                    }
                    parts.push(part);
                }
            }
        }
        if !parts.is_empty() {
            // 连续 Tool 消息合并为单个 user content：官方要求当前回合内
            // 所有 functionResponse 紧跟全部 functionCall，拆成多个 user
            // 回合可能触发严格校验 400
            let merged = m.role == Role::Tool
                && out
                    .last()
                    .and_then(|c: &Value| c.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("user");
            if merged {
                if let Some(arr) = out
                    .last_mut()
                    .and_then(|c| c.get_mut("parts"))
                    .and_then(|p| p.as_array_mut())
                {
                    arr.extend(parts);
                }
            } else {
                out.push(json!({ "role": role, "parts": parts }));
            }
        }
    }
    out
}

/// Core system（顶层 + system 角色消息）→ Gemini `systemInstruction`。
pub fn core_to_system_instruction(system: &[ContentBlock], messages: &[Message]) -> Option<Value> {
    let mut texts: Vec<&str> = Vec::new();
    for b in system {
        if let ContentBlock::Text { text } = b {
            texts.push(text);
        }
    }
    for m in messages {
        if m.role == Role::System {
            for b in &m.content {
                if let ContentBlock::Text { text } = b {
                    texts.push(text);
                }
            }
        }
    }
    if texts.is_empty() {
        None
    } else {
        Some(json!({ "parts": [{ "text": texts.join("\n\n") }] }))
    }
}

/// Core tools → Gemini `tools`（单层 functionDeclarations 数组）。
pub fn core_to_tools(tools: &[Tool]) -> Option<Value> {
    if tools.is_empty() {
        return None;
    }
    let decls: Vec<Value> = tools
        .iter()
        .map(|t| {
            let mut d = json!({ "name": t.name, "parameters": t.input_schema });
            if let Some(desc) = &t.description {
                d["description"] = json!(desc);
            }
            d
        })
        .collect();
    Some(json!([{ "functionDeclarations": decls }]))
}

/// Core tool_choice → Gemini `toolConfig`。
pub fn core_to_tool_config(tc: Option<&ToolChoice>) -> Option<Value> {
    let choice = tc?;
    let mode = match choice {
        ToolChoice::Auto => "AUTO",
        ToolChoice::None => "NONE",
        ToolChoice::Required | ToolChoice::Tool { .. } => "ANY",
    };
    let mut config = json!({ "mode": mode });
    if let ToolChoice::Tool { name } = choice {
        config["allowedFunctionNames"] = json!([name]);
    }
    Some(json!({ "functionCallingConfig": config }))
}

/// Core 采样参数 → Gemini `generationConfig`（全空则返回 None）。
pub fn core_to_generation_config(req: &CoreRequest) -> Option<Value> {
    let mut gc = json!({});
    let obj = gc.as_object_mut().expect("gc is object");
    if let Some(mt) = req.max_tokens {
        obj.insert("maxOutputTokens".to_string(), json!(mt));
    }
    if let Some(t) = req.temperature {
        obj.insert("temperature".to_string(), json!(t));
    }
    if let Some(p) = req.top_p {
        obj.insert("topP".to_string(), json!(p));
    }
    if !req.stop.is_empty() {
        obj.insert("stopSequences".to_string(), json!(req.stop));
    }
    // 推理强度：Gemini 用 generationConfig.thinkingConfig.thinkingBudget 表达
    if let Some(effort) = reasoning_effort(req) {
        let max_tokens = req.max_tokens.unwrap_or(u32::MAX);
        if let Some(budget) = clamped_thinking_budget(effort, max_tokens) {
            obj.insert(
                "thinkingConfig".to_string(),
                json!({ "thinkingBudget": budget, "includeThoughts": true }),
            );
        }
    }
    if obj.is_empty() {
        None
    } else {
        Some(gc)
    }
}

/// Core 内容块 → Gemini 响应 candidate 的 parts（入口 from_core_response 用）。
pub fn core_to_parts(content: &[ContentBlock]) -> Vec<Value> {
    let mut parts = Vec::new();
    for b in content {
        match b {
            ContentBlock::Text { text } => parts.push(json!({ "text": text })),
            ContentBlock::ToolUse { name, input, signature, .. } => {
                let mut fc = json!({ "name": name, "args": input });
                if let Some(sig) = signature {
                    if !sig.is_empty() {
                        fc["thoughtSignature"] = json!(sig);
                    }
                }
                parts.push(json!({ "functionCall": fc }))
            }
            ContentBlock::Image { data, media_type } => {
                if media_type != "url" {
                    parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
                }
            }
            ContentBlock::Reasoning { text, signature, .. } => {
                // thought part 回传：凭据（thoughtSignature）随 part 原样带回
                let mut part = json!({ "thought": true });
                if !text.is_empty() {
                    part["text"] = json!(text);
                }
                if let Some(sig) = signature {
                    if !sig.is_empty() {
                        part["thoughtSignature"] = json!(sig);
                    }
                }
                parts.push(part);
            }
            ContentBlock::ToolResult { .. } => {}
        }
    }
    parts
}

// ============================================================================
// Gemini → Core
// ============================================================================

/// 从 functionResponse.response 提取可读文本。
fn resp_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) => {
            for key in ["result", "output", "content"] {
                if let Some(s) = m.get(key).and_then(|x| x.as_str()) {
                    return s.to_string();
                }
            }
            v.to_string()
        }
        Value::Null => String::new(),
        _ => v.to_string(),
    }
}

/// 单个 Gemini part → 0..n 个 Core 内容块。
pub fn part_to_blocks(p: &Value) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let signature = p
        .get("thoughtSignature")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let thought = p.get("thought").and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
        if !t.is_empty() {
            if thought {
                // thought part：明文 + thoughtSignature（回传凭据）→ Reasoning 块
                out.push(ContentBlock::Reasoning { text: t.to_string(), signature, redacted: false });
            } else {
                out.push(ContentBlock::text(t));
            }
        } else if let Some(sig) = signature {
            // 无文本但带凭据的 part：凭据不能丢，否则多轮回传缺失
            out.push(ContentBlock::Reasoning { text: String::new(), signature: Some(sig), redacted: false });
        }
    } else if p.get("functionCall").is_none() {
        // 非文本且非 functionCall 的 part 带凭据：凭据不能丢；
        // functionCall part 的凭据归 ToolUse 块（见下），不在此处理
        if let Some(sig) = signature {
            out.push(ContentBlock::Reasoning { text: String::new(), signature: Some(sig), redacted: false });
        }
    }
    if let Some(fc) = p.get("functionCall") {
        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        // Gemini 的 functionCall 不一定带 id，缺失时按函数名合成，保证下游可回传结果
        let id = fc
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{name}-call"));
        let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
        // 加密 CoT 凭据：与 function call 强绑定的 thoughtSignature（part 级字段，
        // 官方形态在 part 上；部分实现放在 functionCall 内，均兼容）
        let signature = p
            .get("thoughtSignature")
            .or_else(|| fc.get("thoughtSignature"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        out.push(ContentBlock::ToolUse {
            id,
            name,
            namespace: None,
            input: args,
            signature,
        });
    }
    if let Some(fr) = p.get("functionResponse") {
        let name = fr.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let resp = fr.get("response").cloned().unwrap_or(Value::Null);
        out.push(ContentBlock::ToolResult {
            tool_use_id: name,
            content: vec![ContentBlock::text(resp_to_text(&resp))],
            is_error: false,
        });
    }
    if let Some(inline) = p.get("inlineData") {
        let data = inline.get("data").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let mt = inline
            .get("mimeType")
            .and_then(|v| v.as_str())
            .unwrap_or("image/png")
            .to_string();
        out.push(ContentBlock::Image { data, media_type: mt });
    }
    out
}

/// Gemini `contents` → Core messages。
pub fn contents_to_core(contents: &[Value]) -> Vec<Message> {
    contents
        .iter()
        .map(|c| {
            let role = match c.get("role").and_then(|r| r.as_str()) {
                Some("model") => Role::Assistant,
                _ => Role::User,
            };
            let parts = c
                .get("parts")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default();
            let mut content = Vec::new();
            for p in &parts {
                content.extend(part_to_blocks(p));
            }
            Message {
                role,
                content,
                ext: Default::default(),
            }
        })
        .collect()
}

/// Gemini `systemInstruction` → Core system 内容块。
pub fn system_instruction_to_core(si: Option<&Value>) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    match si {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                out.push(ContentBlock::text(s.clone()));
            }
        }
        Some(v) => {
            if let Some(parts) = v.get("parts").and_then(|p| p.as_array()) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                        if !t.is_empty() {
                            out.push(ContentBlock::text(t));
                        }
                    }
                }
            }
        }
        None => {}
    }
    out
}

/// Gemini `tools` → Core tools。
pub fn tools_to_core(tools: &[Value]) -> Vec<Tool> {
    let mut out = Vec::new();
    for t in tools {
        if let Some(decls) = t.get("functionDeclarations").and_then(|d| d.as_array()) {
            for d in decls {
                let Some(name) = d.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                out.push(Tool {
                    name: name.to_string(),
                    description: d.get("description").and_then(|v| v.as_str()).map(String::from),
                    input_schema: d
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                    ext: Default::default(),
                });
            }
        }
    }
    out
}

/// Gemini `toolConfig` → Core tool_choice。
pub fn tool_config_to_core(tc: Option<&Value>) -> Option<ToolChoice> {
    let fcc = tc?.get("functionCallingConfig")?;
    match fcc.get("mode").and_then(|m| m.as_str()) {
        Some("AUTO") => Some(ToolChoice::Auto),
        Some("NONE") => Some(ToolChoice::None),
        Some("ANY") => {
            if let Some(names) = fcc.get("allowedFunctionNames").and_then(|n| n.as_array()) {
                if names.len() == 1 {
                    if let Some(n) = names[0].as_str() {
                        return Some(ToolChoice::Tool { name: n.to_string() });
                    }
                }
            }
            Some(ToolChoice::Required)
        }
        _ => None,
    }
}

/// Gemini 响应 candidate → Core 内容块 + finishReason。
pub fn candidate_to_core(cand: &Value) -> (Vec<ContentBlock>, Option<StopReason>) {
    let mut content = Vec::new();
    if let Some(parts) = cand
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
    {
        for p in parts {
            content.extend(part_to_blocks(p));
        }
    }
    let finish = cand
        .get("finishReason")
        .and_then(|v| v.as_str())
        .and_then(map_finish_reason);
    (content, finish)
}

/// Gemini `usageMetadata` → Core Usage。
pub fn usage_from_gemini(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("promptTokenCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        output_tokens: v
            .get("candidatesTokenCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cache_read_tokens: v
            .get("cachedContentTokenCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        cache_write_tokens: 0,
        reasoning_tokens: v
            .get("thoughtsTokenCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
    }
}

/// 构造 Gemini `usageMetadata` 对象。
pub fn usage_object(u: &Usage) -> Value {
    json!({
        "promptTokenCount": u.input_tokens,
        "candidatesTokenCount": u.output_tokens,
        "totalTokenCount": u.input_tokens + u.output_tokens,
        "thoughtsTokenCount": u.reasoning_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// thought part 的 thoughtSignature 凭据 round-trip。
    #[test]
    fn thought_signature_roundtrips() {
        let part = json!({ "text": "thinking...", "thought": true, "thoughtSignature": "SIG" });
        let blocks = part_to_blocks(&part);
        match &blocks[0] {
            ContentBlock::Reasoning { text, signature: Some(sig), .. } => {
                assert_eq!(text, "thinking...");
                assert_eq!(sig, "SIG");
            }
            other => panic!("expected reasoning, got {other:?}"),
        }
        // 历史回传：thoughtSignature 原样带回
        let msgs = vec![Message {
            role: Role::Assistant,
            content: blocks,
            ext: Default::default(),
        }];
        let contents = core_to_contents(&msgs);
        let p = &contents[0]["parts"][0];
        assert_eq!(p["thought"], true);
        assert_eq!(p["thoughtSignature"], "SIG");
        assert_eq!(p["text"], "thinking...");
    }


    #[test]
    fn maps_finish_reasons() {
        assert_eq!(map_finish_reason("STOP"), Some(StopReason::EndTurn));
        assert_eq!(map_finish_reason("MAX_TOKENS"), Some(StopReason::MaxTokens));
        assert_eq!(map_finish_reason("SAFETY"), Some(StopReason::ContentFilter));
        assert_eq!(unmap_stop_reason(Some(StopReason::MaxTokens)), "MAX_TOKENS");
        assert_eq!(unmap_stop_reason(Some(StopReason::ToolUse)), "STOP");
    }

    #[test]
    fn builds_paths() {
        assert_eq!(
            generate_path("v1beta", "gemini-2.0-flash"),
            "/v1beta/models/gemini-2.0-flash:generateContent"
        );
        assert!(stream_path("v1", "g").contains(":streamGenerateContent?alt=sse"));
    }

    #[test]
    fn roundtrips_tool_result_by_name() {
        // assistant 发起 get_time（id=c1），随后 user 回合回传结果（tool_use_id=c1）
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "get_time".into(),
                    namespace: None,
                    input: json!({"tz": "UTC"}),
                    signature: None,
                }],
                ext: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![ContentBlock::text("12:00")],
                    is_error: false,
                }],
                ext: Default::default(),
            },
        ];
        let contents = core_to_contents(&msgs);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["functionCall"]["name"], "get_time");
        assert_eq!(contents[1]["role"], "user");
        // functionResponse 按名关联，而非 id
        assert_eq!(contents[1]["parts"][0]["functionResponse"]["name"], "get_time");
        assert_eq!(contents[1]["parts"][0]["functionResponse"]["response"]["result"], "12:00");
    }

    #[test]
    fn parses_candidate_with_function_call() {
        let cand = json!({
            "content": { "role": "model", "parts": [{ "functionCall": { "name": "get_time", "args": {"tz":"UTC"} } }] },
            "finishReason": "STOP"
        });
        let (content, finish) = candidate_to_core(&cand);
        assert_eq!(finish, Some(StopReason::EndTurn));
        match &content[0] {
            ContentBlock::ToolUse { name, input, id, .. } => {
                assert_eq!(name, "get_time");
                assert_eq!(input["tz"], "UTC");
                assert!(!id.is_empty(), "缺失 id 时应合成");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    /// 加密 CoT 凭据与 function call 强绑定：functionCall part 的
    /// thoughtSignature → ToolUse.signature，历史回传时还原到 part。
    #[test]
    fn function_call_thought_signature_roundtrips() {
        let cand = json!({
            "content": { "role": "model", "parts": [{
                "functionCall": { "name": "get_time", "args": {"tz": "UTC"} },
                "thoughtSignature": "SIG"
            }] },
            "finishReason": "STOP"
        });
        let (content, _) = candidate_to_core(&cand);
        match &content[0] {
            ContentBlock::ToolUse { signature: Some(sig), .. } => assert_eq!(sig, "SIG"),
            other => panic!("expected tool_use with signature, got {other:?}"),
        }

        // 历史回传：凭据随 functionCall part 原样带回
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "get_time".into(),
                namespace: None,
                input: json!({"tz": "UTC"}),
                signature: Some("SIG".into()),
            }],
            ext: Default::default(),
        };
        let contents = core_to_contents(&[
            Message {
                role: Role::User,
                content: vec![ContentBlock::text("q")],
                ext: Default::default(),
            },
            msg,
        ]);
        assert_eq!(contents[1]["parts"][0]["functionCall"]["thoughtSignature"], "SIG");
    }

    /// 连续 Tool 消息合并为单个 user content（官方：FR 紧跟全部 FC，
    /// 拆多个 user 回合可能触发严格校验 400）。
    #[test]
    fn consecutive_tool_results_merge_into_one_user_content() {
        let msgs = vec![
            Message::text(Role::User, "q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse { id: "c1".into(), name: "a".into(), namespace: None, input: json!({}), signature: None },
                    ContentBlock::ToolUse { id: "c2".into(), name: "b".into(), namespace: None, input: json!({}), signature: None },
                ],
                ext: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult { tool_use_id: "c1".into(), content: vec![ContentBlock::text("r1")], is_error: false }],
                ext: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult { tool_use_id: "c2".into(), content: vec![ContentBlock::text("r2")], is_error: false }],
                ext: Default::default(),
            },
        ];
        let contents = core_to_contents(&msgs);
        assert_eq!(contents.len(), 3, "两个 Tool 结果应合并为一个 user 回合");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn maps_usage() {
        let u = usage_from_gemini(&json!({"promptTokenCount": 7, "candidatesTokenCount": 3, "cachedContentTokenCount": 2}));
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.output_tokens, 3);
        assert_eq!(u.cache_read_tokens, 2);
    }
}
