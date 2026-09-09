//! OpenAI Responses API 的 DTO 辅助：SSE 事件名常量与 output item 构造。

use serde_json::{json, Value};

/// Responses 对象的 `object` 字段值。
pub const OBJECT_RESPONSE: &str = "response";

/// Responses API 路径（作为上游时拼接到 base_url 之后）。
pub const RESPONSES_PATH: &str = "/v1/responses";

/// 未指定 max_tokens 时的兜底值（Responses 用 `max_output_tokens`）。
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;

/// Responses 流式 SSE 事件名（本实现覆盖的最小子集）。
pub mod event {
    pub const CREATED: &str = "response.created";
    pub const IN_PROGRESS: &str = "response.in_progress";
    pub const OUTPUT_ITEM_ADDED: &str = "response.output_item.added";
    pub const OUTPUT_ITEM_DONE: &str = "response.output_item.done";
    pub const CONTENT_PART_ADDED: &str = "response.content_part.added";
    pub const CONTENT_PART_DONE: &str = "response.content_part.done";
    pub const TEXT_DELTA: &str = "response.output_text.delta";
    pub const TEXT_DONE: &str = "response.output_text.done";
    pub const REASONING_SUMMARY_TEXT_DELTA: &str = "response.reasoning_summary_text.delta";
    pub const REASONING_SUMMARY_TEXT_DONE: &str = "response.reasoning_summary_text.done";
    pub const FUNC_ARGS_DELTA: &str = "response.function_call_arguments.delta";
    pub const FUNC_ARGS_DONE: &str = "response.function_call_arguments.done";
    pub const COMPLETED: &str = "response.completed";
}

/// 构造一个 response 骨架对象，用于 `response.created` / `response.completed`
/// 事件中的 `response` 字段。
pub fn response_skeleton(id: &str, model: &str, status: &str) -> Value {
    json!({
        "id": id,
        "object": OBJECT_RESPONSE,
        "created_at": 0,
        "status": status,
        "model": model,
        "output": [],
        "usage": null,
    })
}

/// 构造一个 assistant message output item。
pub fn message_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "role": "assistant",
        "status": status,
        "content": [
            { "type": "output_text", "text": text, "annotations": [] }
        ],
    })
}

/// 构造一个 function_call output item。
pub fn function_call_item(call_id: &str, name: &str, arguments: &str, status: &str) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
        "status": status,
    })
}

/// 构造 usage 对象（Responses 使用 input_tokens/output_tokens/total_tokens）。
pub fn usage_object(input_tokens: u32, output_tokens: u32) -> Value {
    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
    })
}
