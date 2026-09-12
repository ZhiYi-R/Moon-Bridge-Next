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
    ContentBlock, CoreRequest, DocSource, Message, Role, StopReason, Tool, ToolChoice, Usage,
};
use serde_json::{json, Value};

/// 未显式指定协议版本时使用的 API 版本段。
pub const DEFAULT_VERSION: &str = "v1beta";

/// Gemini 服务端代码执行（`executableCode`/`codeExecutionResult` part）在
/// Core 侧的工具名承载：以 ToolUse/ToolResult 表达调用与结果，本常量作
/// 名字哨兵，回传 Gemini 时还原为原生 part 形态。真实工具若恰好同名会
/// 误判，属可接受的边角冲突。
pub(crate) const EXEC_CODE: &str = "executable_code";

/// `functionCall` 无 `id` 字段时 ToolUse.id 的占位哨兵；`functionResponse`
/// 无 `id` 时 ToolResult.tool_use_id 用 `{FR_NAME_PREFIX}{name}` 占位——
/// contents_to_core 收齐全部消息后按「调用按名入队、结果按名出队」配对
/// 并分配唯一 id。此前结果直接以函数名作 tool_use_id，与调用侧合成的
/// `{name}-call` 永不匹配，转 Anthropic 等强校验上游会因悬空 tool_result
/// 整请求 400；同名函数多次调用时合成 id 还会重复。
const FCALL_PENDING: &str = "__gemini_fcall__";
const FR_NAME_PREFIX: &str = "__gemini_fres__:";

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
        Some(StopReason::ContentFilter) | Some(StopReason::Refusal) => "SAFETY",
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

/// 载波凭据落位：并入前一 part（无签名时补写 thoughtSignature）；
/// 无可并入 part 时落成独立 `{"thought":true,"thoughtSignature":sig}`。
fn flush_pending_sig(parts: &mut Vec<Value>, pending: &mut Option<String>) {
    let Some(sig) = pending.take() else {
        return;
    };
    if let Some(last) = parts.last_mut() {
        if last.get("thoughtSignature").is_none() {
            last["thoughtSignature"] = json!(sig);
            return;
        }
    }
    parts.push(json!({ "thought": true, "thoughtSignature": sig }));
}

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
        // 「仅凭据」载波（空文本 Reasoning{sig}）是 FC/text 签名跨协议的
        // 传输形态：紧邻 ToolUse 之前时迁移到 functionCall part（绑定目标），
        // 其余情况并入前一 part（末块/文本 part 签名原位），无位置可并时
        // 落成独立 thought part。
        let mut pending_sig: Option<String> = None;
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => {
                    flush_pending_sig(&mut parts, &mut pending_sig);
                    parts.push(json!({ "text": text }));
                }
                ContentBlock::Image { data, media_type } => {
                    flush_pending_sig(&mut parts, &mut pending_sig);
                    if media_type != "url" && media_type != "file" {
                        parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
                    }
                }
                ContentBlock::Document {
                    source,
                    media_type,
                    data,
                    name,
                } => {
                    flush_pending_sig(&mut parts, &mut pending_sig);
                    match source {
                        // fileData 是 Gemini 侧文件引用的标准通道；displayName
                        // 承载文件名。
                        DocSource::Url => {
                            let mut fd = json!({ "fileUri": data, "mimeType": media_type });
                            if let Some(n) = name {
                                fd["displayName"] = json!(n);
                            }
                            parts.push(json!({ "fileData": fd }));
                        }
                        // base64 文档走 inlineData（mimeType 任意）；File 源
                        // （平台 file_id）跨协议不可解，不下发。
                        DocSource::Base64 => parts.push(
                            json!({ "inlineData": { "mimeType": media_type, "data": data } }),
                        ),
                        DocSource::Text => parts.push(json!({ "text": data })),
                        DocSource::File => {}
                    }
                }
                ContentBlock::ToolUse { id, name, input, signature, .. } => {
                    // 自带签名优先，否则消费紧邻的前置载波凭据
                    let sig = crate::adapters::untag_signature(
                        crate::adapters::SIG_GEMINI,
                        signature.as_deref(),
                    )
                    .map(String::from)
                    .or_else(|| pending_sig.take());
                    // 服务端代码执行还原为 executableCode 原生 part（区别于
                    // 普通 functionCall：该 part 不参与工具声明校验）。
                    if name == EXEC_CODE {
                        let mut part = json!({ "executableCode": input });
                        if let Some(sig) = sig.filter(|s| !s.is_empty()) {
                            part["thoughtSignature"] = json!(sig);
                        }
                        parts.push(part);
                        continue;
                    }
                    // id 一并下发：Gemini v1beta 支持 functionCall.id，
                    // 客户端原样回传历史时入站可直接按 id 精确配对。
                    let mut fc = json!({ "name": name, "args": input, "id": id });
                    if let Some(sig) = sig.filter(|s| !s.is_empty()) {
                        fc["thoughtSignature"] = json!(sig);
                    }
                    parts.push(json!({ "functionCall": fc }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    flush_pending_sig(&mut parts, &mut pending_sig);
                    // 未配对的占位 id（`__gemini_fres__:{name}`）剥前缀取函数名——
                    // 结果本就只有函数名语义，占位串不能作为 Gemini 函数名下发。
                    let name = id_to_name
                        .get(tool_use_id.as_str())
                        .copied()
                        .unwrap_or_else(|| {
                            tool_use_id
                                .strip_prefix(FR_NAME_PREFIX)
                                .unwrap_or(tool_use_id.as_str())
                        });
                    // 代码执行结果还原为 codeExecutionResult 原生 part；
                    // outcome 只有 ok/fail 粒度可从 is_error 还原。
                    if name == EXEC_CODE || tool_use_id.starts_with(EXEC_CODE) {
                        parts.push(json!({
                            "codeExecutionResult": {
                                "outcome": if *is_error { "OUTCOME_FAILED" } else { "OUTCOME_OK" },
                                "output": text_of(content),
                            }
                        }));
                        continue;
                    }
                    // is_error 无原生通道，写入 response 保留位（同协议回传
                    // 时读回；对模型而言是结果对象里的显式错误标记）。
                    let mut resp = json!({ "result": text_of(content) });
                    if *is_error {
                        resp["is_error"] = json!(true);
                    }
                    // 已配对的真实调用 id 一并下发（v1beta 支持），客户端原样
                    // 回传时入站可精确配对；未配对的占位/名串不作 id 下发。
                    let mut fr = json!({ "name": name, "response": resp });
                    if id_to_name.contains_key(tool_use_id.as_str()) {
                        fr["id"] = json!(tool_use_id);
                    }
                    parts.push(json!({ "functionResponse": fr }));
                    // functionResponse 只承载 JSON 文本：内嵌图片/文档提升为同一
                    // user 回合的 inlineData/fileData part（Gemini 侧工具媒体
                    // 结果的标准通道），附一行来源标注保持与 functionCall 的
                    // 关联——否则经 text_of 静默丢弃后模型只看到空结果。
                    let mut hoisted = false;
                    for b in content {
                        let part = match b {
                            ContentBlock::Image { data, media_type } => {
                                if media_type == "url" || media_type == "file" {
                                    continue; // 与顶层分支同口径：引用形态不下发
                                }
                                json!({ "inlineData": { "mimeType": media_type, "data": data } })
                            }
                            ContentBlock::Document {
                                source,
                                media_type,
                                data,
                                name: doc_name,
                            } => match source {
                                DocSource::Url => {
                                    let mut fd =
                                        json!({ "fileUri": data, "mimeType": media_type });
                                    if let Some(n) = doc_name {
                                        fd["displayName"] = json!(n);
                                    }
                                    json!({ "fileData": fd })
                                }
                                DocSource::Base64 => {
                                    json!({ "inlineData": { "mimeType": media_type, "data": data } })
                                }
                                DocSource::Text => json!({ "text": data }),
                                DocSource::File => continue,
                            },
                            _ => continue,
                        };
                        if !hoisted {
                            parts.push(json!({
                                "text": format!("[media content returned by function call {name}]")
                            }));
                            hoisted = true;
                        }
                        parts.push(part);
                    }
                }
                ContentBlock::Reasoning { text, signature, .. } => {
                    // 历史 thought 回传：Gemini 2.5 多轮 function calling 要求
                    // thoughtSignature 原样带回（缺失会被拒或退化）。
                    let sig = crate::adapters::untag_signature(
                        crate::adapters::SIG_GEMINI,
                        signature.as_deref(),
                    );
                    if text.is_empty() {
                        // 仅凭据载波：不独立成 part，留给后继 ToolUse 或前一 part
                        if let Some(s) = sig {
                            if !s.is_empty() {
                                pending_sig = Some(s.to_string());
                            }
                        }
                        continue;
                    }
                    flush_pending_sig(&mut parts, &mut pending_sig);
                    let mut part = json!({ "thought": true, "text": text });
                    if let Some(s) = sig.filter(|s| !s.is_empty()) {
                        part["thoughtSignature"] = json!(s);
                    }
                    parts.push(part);
                }
            }
        }
        flush_pending_sig(&mut parts, &mut pending_sig);
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
///
/// 与 `core_to_contents` 同一套载波规则：仅凭据 Reasoning 前绑后继
/// ToolUse，其余并入前一 part。
pub fn core_to_parts(content: &[ContentBlock]) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    let mut pending_sig: Option<String> = None;
    for b in content {
        match b {
            ContentBlock::Text { text } => {
                flush_pending_sig(&mut parts, &mut pending_sig);
                parts.push(json!({ "text": text }));
            }
            ContentBlock::ToolUse { id, name, input, signature, .. } => {
                let sig = crate::adapters::untag_signature(
                    crate::adapters::SIG_GEMINI,
                    signature.as_deref(),
                )
                .map(String::from)
                .or_else(|| pending_sig.take());
                if name == EXEC_CODE {
                    let mut part = json!({ "executableCode": input });
                    if let Some(sig) = sig.filter(|s| !s.is_empty()) {
                        part["thoughtSignature"] = json!(sig);
                    }
                    parts.push(part);
                    continue;
                }
                let mut fc = json!({ "name": name, "args": input, "id": id });
                if let Some(sig) = sig.filter(|s| !s.is_empty()) {
                    fc["thoughtSignature"] = json!(sig);
                }
                parts.push(json!({ "functionCall": fc }))
            }
            ContentBlock::Image { data, media_type } => {
                flush_pending_sig(&mut parts, &mut pending_sig);
                if media_type != "url" && media_type != "file" {
                    parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
                }
            }
            ContentBlock::Document {
                source,
                media_type,
                data,
                name,
            } => {
                flush_pending_sig(&mut parts, &mut pending_sig);
                match source {
                    DocSource::Url => {
                        let mut fd = json!({ "fileUri": data, "mimeType": media_type });
                        if let Some(n) = name {
                            fd["displayName"] = json!(n);
                        }
                        parts.push(json!({ "fileData": fd }));
                    }
                    DocSource::Base64 => parts.push(
                        json!({ "inlineData": { "mimeType": media_type, "data": data } }),
                    ),
                    DocSource::Text => parts.push(json!({ "text": data })),
                    DocSource::File => {}
                }
            }
            ContentBlock::Reasoning { text, signature, .. } => {
                // thought part 回传：凭据（thoughtSignature）随 part 原样带回
                let sig = crate::adapters::untag_signature(
                    crate::adapters::SIG_GEMINI,
                    signature.as_deref(),
                );
                if text.is_empty() {
                    if let Some(s) = sig.filter(|s| !s.is_empty()) {
                        pending_sig = Some(s.to_string());
                    }
                    continue;
                }
                flush_pending_sig(&mut parts, &mut pending_sig);
                let mut part = json!({ "thought": true, "text": text });
                if let Some(s) = sig.filter(|s| !s.is_empty()) {
                    part["thoughtSignature"] = json!(s);
                }
                parts.push(part);
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                flush_pending_sig(&mut parts, &mut pending_sig);
                // 响应侧的代码执行结果（model 回合内）还原原生 part；
                // 其余 ToolResult 在响应中不出现，维持丢弃。
                if tool_use_id.starts_with(EXEC_CODE) {
                    parts.push(json!({
                        "codeExecutionResult": {
                            "outcome": if *is_error { "OUTCOME_FAILED" } else { "OUTCOME_OK" },
                            "output": text_of(content),
                        }
                    }));
                }
            }
        }
    }
    flush_pending_sig(&mut parts, &mut pending_sig);
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
///
/// 凭据传输约定：part 级 `thoughtSignature` 打上 `gem:` 来源标记；签名绑定
/// 目标是「紧邻的后继 ToolUse / 前一 part」，而非任意 reasoning 块——
/// 故带签名的普通 text part 产出 `Text + 仅凭据载波 Reasoning`（后者由
/// `core_to_contents` 的 flush 规则还原回原位）。
pub fn part_to_blocks(p: &Value) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let signature = crate::adapters::tag_signature(
        crate::adapters::SIG_GEMINI,
        p.get("thoughtSignature")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string()),
    );
    let thought = p.get("thought").and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
        if !t.is_empty() {
            if thought {
                // thought part：明文 + thoughtSignature（回传凭据）→ Reasoning 块
                out.push(ContentBlock::Reasoning { text: t.to_string(), signature, redacted: false });
            } else {
                // 普通文本 part 带签名（官方：无 FC 时签名在最后一个 part）：
                // 文本归文本，凭据作载波紧随——回传时并入前一 part 还原
                out.push(ContentBlock::text(t));
                if signature.is_some() {
                    out.push(ContentBlock::Reasoning {
                        text: String::new(),
                        signature,
                        redacted: false,
                    });
                }
            }
        } else if signature.is_some() {
            // 无文本但带凭据的 part：凭据不能丢，否则多轮回传缺失
            out.push(ContentBlock::Reasoning { text: String::new(), signature, redacted: false });
        }
    } else if p.get("functionCall").is_none() && p.get("executableCode").is_none() {
        // 非文本且非调用类 part 带凭据：凭据不能丢；
        // functionCall/executableCode part 的凭据归 ToolUse 块（见下），不在此处理
        if signature.is_some() {
            out.push(ContentBlock::Reasoning { text: String::new(), signature, redacted: false });
        }
    }
    if let Some(fc) = p.get("functionCall") {
        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        // Gemini 的 functionCall 不一定带 id；缺失时用占位哨兵，由
        // contents_to_core 统一分配唯一 id 并与 functionResponse 按名配对。
        let id = fc
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| FCALL_PENDING.to_string());
        let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
        // 加密 CoT 凭据：与 function call 强绑定的 thoughtSignature（part 级字段，
        // 官方形态在 part 上；部分实现放在 functionCall 内，均兼容）。
        // ToolUse.signature 直接携带 + 前置载波块兜底（无凭据位的协议靠载波还原）。
        let signature = crate::adapters::tag_signature(
            crate::adapters::SIG_GEMINI,
            p.get("thoughtSignature")
                .or_else(|| fc.get("thoughtSignature"))
                .and_then(|v| v.as_str())
                .map(String::from),
        );
        if let Some(sig) = &signature {
            out.push(ContentBlock::Reasoning {
                text: String::new(),
                signature: Some(sig.clone()),
                redacted: false,
            });
        }
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
        // id 缺省时记 `{FR_NAME_PREFIX}{name}` 占位——结果本身只有函数名，
        // 配对由 contents_to_core 按序完成；同协议回传时 id_to_name 解析
        // 出的是占位串，须在此剥掉前缀还原为函数名语义。
        let tool_use_id = fr
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{FR_NAME_PREFIX}{name}"));
        out.push(ContentBlock::ToolResult {
            tool_use_id,
            content: vec![ContentBlock::text(resp_to_text(&resp))],
            // response.is_error 是本站出站侧写入的保留位（Gemini 无原生
            // 错误语义），回传时读回保真；外部实现不会带此键。
            is_error: resp.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false),
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
    if let Some(fd) = p.get("fileData") {
        // Files API 引用 → Document（Uri 源）：此前整块丢弃。displayName
        // 承载文件名；mimeType 保留真实类型供回传还原。
        let uri = fd.get("fileUri").and_then(|v| v.as_str()).unwrap_or_default();
        if !uri.is_empty() {
            out.push(ContentBlock::Document {
                source: DocSource::Url,
                media_type: fd
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                data: uri.to_string(),
                name: fd
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            });
        }
    }
    // 服务端代码执行对：executableCode（待执行代码）→ ToolUse、
    // codeExecutionResult（执行输出）→ ToolResult，均以 EXEC_CODE 哨兵名
    // 承载；tool_use_id 用占位值，由 contents_to_core 统一配对编号。
    if let Some(ec) = p.get("executableCode") {
        let signature = crate::adapters::tag_signature(
            crate::adapters::SIG_GEMINI,
            p.get("thoughtSignature")
                .or_else(|| ec.get("thoughtSignature"))
                .and_then(|v| v.as_str())
                .map(String::from),
        );
        if let Some(sig) = &signature {
            out.push(ContentBlock::Reasoning {
                text: String::new(),
                signature: Some(sig.clone()),
                redacted: false,
            });
        }
        out.push(ContentBlock::ToolUse {
            id: EXEC_CODE.to_string(),
            name: EXEC_CODE.to_string(),
            namespace: None,
            input: ec.clone(),
            signature,
        });
    }
    if let Some(er) = p.get("codeExecutionResult") {
        let outcome = er.get("outcome").and_then(|v| v.as_str()).unwrap_or("OUTCOME_OK");
        let output = er.get("output").and_then(|v| v.as_str()).unwrap_or_default();
        out.push(ContentBlock::ToolResult {
            tool_use_id: EXEC_CODE.to_string(),
            content: vec![ContentBlock::text(output)],
            is_error: outcome != "OUTCOME_OK",
        });
    }
    out
}

/// Gemini `contents` → Core messages。
pub fn contents_to_core(contents: &[Value]) -> Vec<Message> {
    let mut msgs: Vec<Message> = contents
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
            // executableCode/codeExecutionResult 成对出现：part_to_blocks 里
            // 用占位 id，此处按序分配唯一 id 并配对——跨上游（Anthropic 等）
            // 要求 tool_use id 全局唯一且结果必须回指对应调用。
            let mut seq = 0usize;
            let mut pending: Option<String> = None;
            for b in &mut content {
                match b {
                    ContentBlock::ToolUse { id, name, .. } if name == EXEC_CODE => {
                        let assigned = format!("{EXEC_CODE}-{seq}");
                        seq += 1;
                        *id = assigned.clone();
                        pending = Some(assigned);
                    }
                    ContentBlock::ToolResult { tool_use_id, .. }
                        if tool_use_id == EXEC_CODE =>
                    {
                        if let Some(id) = pending.take() {
                            *tool_use_id = id;
                        }
                    }
                    _ => {}
                }
            }
            Message {
                role,
                content,
                ext: Default::default(),
            }
        })
        .collect();

    // functionCall/functionResponse 配对：调用无 id 时用占位哨兵，此处分配
    // 唯一 id 并按函数名入队；结果无 id 时（`{FR_NAME_PREFIX}{name}`）按名
    // 出队回指对应调用——Gemini 语义即 user 回合的 functionResponse 按序
    // 对应前序 model 回合的同名 functionCall。带真实 id 的调用同样入队，
    // 使缺 id 的结果仍能按名配对；悬空结果保留函数名（旧行为， outbound
    // 剥前缀还原）。跨上游（Anthropic 等）要求结果 id 回指真实调用 id，
    // 不配对会以「找不到对应 tool_use」整请求 400。
    let mut seq = 0usize;
    let mut pending: HashMap<String, std::collections::VecDeque<String>> = HashMap::new();
    for m in &mut msgs {
        for b in &mut m.content {
            match b {
                ContentBlock::ToolUse { id, name, .. } => {
                    if *id == FCALL_PENDING {
                        *id = format!("fcall-{seq}");
                        seq += 1;
                    }
                    pending.entry(name.clone()).or_default().push_back(id.clone());
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if let Some(name) = tool_use_id.strip_prefix(FR_NAME_PREFIX) {
                        let matched = pending
                            .get_mut(name)
                            .and_then(|q| q.pop_front());
                        *tool_use_id = matched.unwrap_or_else(|| name.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    msgs
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
                assert_eq!(sig, "gem:SIG", "凭据带来源标记，出站时还原");
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
        // 载波在前（供无 ToolUse 凭据位的协议还原），ToolUse 自带签名
        match &content[0] {
            ContentBlock::Reasoning { text, signature: Some(sig), .. } => {
                assert!(text.is_empty());
                assert_eq!(sig, "gem:SIG");
            }
            other => panic!("expected signature carrier, got {other:?}"),
        }
        match &content[1] {
            ContentBlock::ToolUse { signature: Some(sig), .. } => assert_eq!(sig, "gem:SIG"),
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

    /// 普通文本 part 携带 thoughtSignature（官方：无 FC 时签名在最后一个
    /// part）：text 归 Text 块、凭据拆出为紧邻的仅凭据载波 Reasoning——
    /// 不再误挂 reasoning 槽位，回传时载波并入前一 part 还原。
    #[test]
    fn text_part_signature_detaches_to_carrier() {
        let blocks = part_to_blocks(&json!({"text": "Hi", "thoughtSignature": "S"}));
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hi"),
            other => panic!("文本归 Text 块, got {other:?}"),
        }
        match &blocks[1] {
            ContentBlock::Reasoning { text, signature, .. } => {
                assert!(text.is_empty());
                assert_eq!(signature.as_deref(), Some("gem:S"));
            }
            other => panic!("签名应拆出为载波块, got {other:?}"),
        }

        // 回传还原：载波并入前一 part
        let contents = core_to_contents(&[Message {
            role: Role::Assistant,
            content: blocks,
            ext: Default::default(),
        }]);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1, "载波不额外占 part");
        assert_eq!(parts[0]["text"], "Hi");
        assert_eq!(parts[0]["thoughtSignature"], "S");
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

    /// 载波凭据迁移：空文本 Reasoning{sig} 紧邻 ToolUse 之前 → 签名落到
    /// functionCall part（非 Gemini 入口回传的标准形态）；异源凭据不迁移。
    #[test]
    fn signature_carrier_attaches_to_next_tool_use() {
        let msgs = vec![
            Message::text(Role::User, "q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: String::new(),
                        signature: Some("gem:SIG".into()),
                        redacted: false,
                    },
                    ContentBlock::ToolUse {
                        id: "c1".into(),
                        name: "f".into(),
                        namespace: None,
                        input: json!({}),
                        signature: None,
                    },
                ],
                ext: Default::default(),
            },
        ];
        let contents = core_to_contents(&msgs);
        let parts = contents[1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1, "载波不独立成 part");
        assert_eq!(parts[0]["functionCall"]["thoughtSignature"], "SIG");

        // 异源凭据（Anthropic 签名）不得迁移——透传只会让上游 400
        let mut foreign = msgs.clone();
        if let Some(ContentBlock::Reasoning { signature, .. }) =
            foreign[1].content.get_mut(0)
        {
            *signature = Some("ant:FOREIGN".into());
        }
        let contents = core_to_contents(&foreign);
        assert!(contents[1]["parts"][0]["functionCall"].get("thoughtSignature").is_none());
    }

    /// 回归：ToolResult 内嵌图片不得经 `text_of` 丢弃成空 functionResponse——
    /// functionResponse 只承载 JSON 文本，图片提升为同一 user 回合的
    /// inlineData part（Gemini 侧工具图片结果的标准通道），附来源标注；
    /// url 形态与顶层 Image 分支同口径跳过。
    #[test]
    fn tool_result_images_hoist_as_inline_data() {
        let msgs = vec![
            Message::text(Role::User, "q"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "read_file".into(),
                    namespace: None,
                    input: json!({ "path": "a.png" }),
                    signature: None,
                }],
                ext: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![
                        ContentBlock::text("image below"),
                        ContentBlock::Image {
                            data: "AAA".into(),
                            media_type: "image/png".into(),
                        },
                        ContentBlock::Image {
                            data: "https://example.com/x.png".into(),
                            media_type: "url".into(),
                        },
                        ContentBlock::Image {
                            data: "BBB".into(),
                            media_type: "image/jpeg".into(),
                        },
                    ],
                    is_error: false,
                }],
                ext: Default::default(),
            },
        ];
        let contents = core_to_contents(&msgs);
        assert_eq!(contents.len(), 3);
        let parts = contents[2]["parts"].as_array().unwrap();
        // functionResponse 文本保留 + 一条标注 + 两张 base64 图（url 跳过）
        assert_eq!(
            parts[0],
            json!({ "functionResponse": { "name": "read_file", "response": { "result": "image below" }, "id": "c1" } })
        );
        assert_eq!(
            parts[1],
            json!({ "text": "[media content returned by function call read_file]" })
        );
        assert_eq!(
            parts[2],
            json!({ "inlineData": { "mimeType": "image/png", "data": "AAA" } })
        );
        assert_eq!(
            parts[3],
            json!({ "inlineData": { "mimeType": "image/jpeg", "data": "BBB" } })
        );
        assert_eq!(parts.len(), 4, "url 形态图片不下发: {parts:?}");
    }

    /// 回归：fileData part（Files API 引用）不得静默丢弃——映射为 Uri 源
    /// Document（mimeType/displayName 保真），回传 Gemini 还原 fileData。
    #[test]
    fn file_data_part_maps_to_document() {
        let uri = "https://generativelanguage.googleapis.com/v1beta/files/abc";
        let blocks = part_to_blocks(&json!({
            "fileData": { "fileUri": uri, "mimeType": "application/pdf", "displayName": "s.pdf" }
        }));
        let ContentBlock::Document { source, media_type, data, name } = &blocks[0] else {
            panic!("应为 Document 块: {blocks:?}")
        };
        assert_eq!(*source, DocSource::Url);
        assert_eq!((media_type.as_str(), data.as_str()), ("application/pdf", uri));
        assert_eq!(name.as_deref(), Some("s.pdf"));

        // 出站还原为 fileData part
        let contents = core_to_contents(&[Message {
            role: Role::User,
            content: vec![blocks[0].clone()],
            ext: Default::default(),
        }]);
        assert_eq!(
            contents[0]["parts"][0],
            json!({ "fileData": { "fileUri": uri, "mimeType": "application/pdf", "displayName": "s.pdf" } })
        );
    }

    /// 回归：executableCode/codeExecutionResult 入站不再丢弃——映射为
    /// ToolUse/ToolResult（EXEC_CODE 哨兵名，按序配对唯一 id）；回传 Gemini
    /// 还原为原生 part 形态。
    #[test]
    fn code_execution_parts_roundtrip() {
        let msgs = contents_to_core(&[json!({
            "role": "model",
            "parts": [
                { "executableCode": { "language": "PYTHON", "code": "print(1)" } },
                { "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1\n" } },
                { "executableCode": { "language": "PYTHON", "code": "print(2)" } },
                { "codeExecutionResult": { "outcome": "OUTCOME_FAILED", "output": "err" } }
            ]
        })]);
        let content = &msgs[0].content;
        let (ContentBlock::ToolUse { id: i0, name: n0, input, .. },
             ContentBlock::ToolResult { tool_use_id: t0, is_error: e0, .. },
             ContentBlock::ToolUse { id: i1, .. },
             ContentBlock::ToolResult { tool_use_id: t1, is_error: e1, .. }) =
            ( &content[0], &content[1], &content[2], &content[3] )
        else {
            panic!("应为 ToolUse/ToolResult 对: {content:?}")
        };
        assert_eq!(n0, EXEC_CODE);
        assert_eq!(input["code"], "print(1)");
        assert_eq!(t0, i0, "结果须回指配对调用");
        assert_eq!(t1, i1, "第二对结果须各自配对");
        assert_ne!(i0, i1, "id 须唯一");
        assert!(!e0 && *e1, "outcome 映射到 is_error");

        // 出站还原原生 part 形态
        let contents = core_to_contents(&msgs);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0], json!({ "executableCode": { "language": "PYTHON", "code": "print(1)" } }));
        assert_eq!(
            parts[1],
            json!({ "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1\n" } })
        );
        assert_eq!(parts[2], json!({ "executableCode": { "language": "PYTHON", "code": "print(2)" } }));
        assert_eq!(
            parts[3],
            json!({ "codeExecutionResult": { "outcome": "OUTCOME_FAILED", "output": "err" } })
        );
    }

    /// 回归：functionCall/functionResponse 无 id 时按名按序配对——同名函数
    /// 多次调用各得唯一 id，结果回指对应调用（此前结果以函数名为 id 与
    /// 调用侧 `{name}-call` 永不匹配，转 Anthropic 整请求 400）。
    /// 带真实 id 的部分原样保留。
    #[test]
    fn function_call_response_pair_by_name_in_order() {
        let msgs = contents_to_core(&[
            json!({
                "role": "model",
                "parts": [
                    { "functionCall": { "name": "read_file", "args": {"p": "a"} } },
                    { "functionCall": { "name": "read_file", "args": {"p": "b"} } },
                    { "functionCall": { "id": "real-id", "name": "ls", "args": {} } }
                ]
            }),
            json!({
                "role": "user",
                "parts": [
                    { "functionResponse": { "name": "read_file", "response": {"result": "A"} } },
                    { "functionResponse": { "name": "read_file", "response": {"result": "B"} } },
                    { "functionResponse": { "id": "real-id", "name": "ls", "response": {"result": "ok"} } }
                ]
            }),
        ]);

        let calls: Vec<&ContentBlock> = msgs[0]
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .collect();
        let id0 = match calls[0] { ContentBlock::ToolUse { id, .. } => id.clone(), _ => unreachable!() };
        let id1 = match calls[1] { ContentBlock::ToolUse { id, .. } => id.clone(), _ => unreachable!() };
        assert_ne!(id0, id1, "同名调用须分配唯一 id");

        let results: Vec<&ContentBlock> = msgs[1]
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .collect();
        let ContentBlock::ToolResult { tool_use_id: t0, content: c0, .. } = results[0] else { panic!() };
        let ContentBlock::ToolResult { tool_use_id: t1, content: c1, .. } = results[1] else { panic!() };
        let ContentBlock::ToolResult { tool_use_id: t2, .. } = results[2] else { panic!() };
        assert_eq!(t0, &id0, "第一个结果回指第一个同名调用");
        assert_eq!(t1, &id1, "第二个结果回指第二个同名调用");
        assert_eq!(t2, "real-id", "带真实 id 的结果原样保留");
        assert!(matches!(&c0[0], ContentBlock::Text { text } if text == "A"));
        assert!(matches!(&c1[0], ContentBlock::Text { text } if text == "B"));

        // 出站回传：id_to_name 解析回函数名，id 一并还原
        let contents = core_to_contents(&msgs);
        let frs: Vec<&Value> = contents[1]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p.get("functionResponse").is_some())
            .collect();
        assert_eq!(frs[0]["functionResponse"]["name"], "read_file");
        assert_eq!(frs[0]["functionResponse"]["id"], id0.as_str());
        assert_eq!(frs[2]["functionResponse"]["id"], "real-id");
    }

    /// 回归：is_error 经 response.is_error 保留位双向保真。
    #[test]
    fn tool_result_is_error_roundtrips() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "exec".into(),
                    namespace: None,
                    input: json!({}),
                    signature: None,
                }],
                ext: Default::default(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: vec![ContentBlock::text("boom")],
                    is_error: true,
                }],
                ext: Default::default(),
            },
        ];
        let contents = core_to_contents(&msgs);
        let fr = &contents[1]["parts"][0]["functionResponse"];
        assert_eq!(fr["response"]["is_error"], true);
        assert_eq!(fr["response"]["result"], "boom");

        let back = contents_to_core(&contents);
        let ContentBlock::ToolResult { is_error, .. } = &back[1].content[0] else {
            panic!()
        };
        assert!(*is_error, "is_error 回传应保留");
    }

    /// tool_result 内嵌文档随图片一并 hoist 为同回合 part。
    #[test]
    fn tool_result_documents_hoist_as_parts() {
        let msgs = vec![Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "f".into(),
                content: vec![
                    ContentBlock::text("see doc"),
                    ContentBlock::Document {
                        source: DocSource::Url,
                        media_type: "application/pdf".into(),
                        data: "https://x/d.pdf".into(),
                        name: None,
                    },
                    ContentBlock::Document {
                        source: DocSource::Base64,
                        media_type: "application/pdf".into(),
                        data: "PP".into(),
                        name: None,
                    },
                ],
                is_error: false,
            }],
            ext: Default::default(),
        }];
        let contents = core_to_contents(&msgs);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["functionResponse"]["response"]["result"], "see doc");
        assert_eq!(
            parts[1],
            json!({ "text": "[media content returned by function call f]" })
        );
        assert_eq!(
            parts[2],
            json!({ "fileData": { "fileUri": "https://x/d.pdf", "mimeType": "application/pdf" } })
        );
        assert_eq!(
            parts[3],
            json!({ "inlineData": { "mimeType": "application/pdf", "data": "PP" } })
        );
    }

    /// 纯文本 tool_result 不产出额外 part（保持既有结构）。
    #[test]
    fn text_only_tool_result_emits_no_extra_parts() {
        let msgs = vec![Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "f".into(),
                content: vec![ContentBlock::text("ok")],
                is_error: false,
            }],
            ext: Default::default(),
        }];
        let contents = core_to_contents(&msgs);
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionResponse"]["response"]["result"], "ok");
    }

    #[test]
    fn maps_usage() {
        let u = usage_from_gemini(&json!({"promptTokenCount": 7, "candidatesTokenCount": 3, "cachedContentTokenCount": 2}));
        assert_eq!(u.input_tokens, 7);
        assert_eq!(u.output_tokens, 3);
        assert_eq!(u.cache_read_tokens, 2);
    }
}
