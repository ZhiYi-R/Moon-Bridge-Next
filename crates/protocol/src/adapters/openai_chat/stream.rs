//! OpenAI Chat Completions 流式：Core 事件 ↔ Chat SSE chunk。
//!
//! Chat SSE 形如 `data: {"choices":[{"delta":{...}}]}`，以 `data: [DONE]` 结束。
//! encode 供 Chat 作入口时输出；decode 供 Chat 作上游时解析。

use async_trait::async_trait;
use moonbridge_core::{ContentBlock, CoreStreamEvent, Protocol, Result, StreamDelta};
use serde_json::{json, Value};

use super::dto::{map_finish_reason, unmap_stop_reason, usage_from_chat, usage_object};
use super::OpenAiChatAdapter;
use crate::adapter::{ClientStreamAdapter, ProviderStreamAdapter};
use crate::context::ReqCtx;
use crate::raw::{ChunkStage, RawBody, RawChunk};

/// 构造一个 Chat SSE chunk（客户端方向）。
fn chat_chunk(model: &str, delta: Value, finish: Value) -> RawChunk {
    RawChunk::json(
        ChunkStage::ClientChunk,
        Protocol::OpenAiChat,
        None,
        json!({
            "id": "chatcmpl-moonbridge",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        }),
    )
}

/// 构造带 tool_calls delta 的 chunk。
fn tool_chunk(model: &str, tc: Value) -> RawChunk {
    RawChunk::json(
        ChunkStage::ClientChunk,
        Protocol::OpenAiChat,
        None,
        json!({
            "id": "chatcmpl-moonbridge",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": model,
            "choices": [{ "index": 0, "delta": { "tool_calls": [tc] }, "finish_reason": null }],
        }),
    )
}

/// `data: [DONE]` 终止标记（非 JSON，用 Text body）。
fn done_chunk() -> RawChunk {
    RawChunk {
        stage: ChunkStage::ClientChunk,
        protocol: Protocol::OpenAiChat,
        provider: None,
        event: None,
        data: RawBody::Text { text: "[DONE]".to_string() },
        raw: String::new(),
    }
}

#[async_trait]
impl ClientStreamAdapter for OpenAiChatAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiChat
    }

    fn encode(&self, ctx: &ReqCtx, ev: &CoreStreamEvent) -> Result<Vec<RawChunk>> {
        let model = ctx.model_alias.as_str();
        let mut out = Vec::new();
        match ev {
            CoreStreamEvent::MessageStart { .. } => {
                out.push(chat_chunk(model, json!({ "role": "assistant", "content": "" }), Value::Null));
            }
            CoreStreamEvent::BlockStart { index, block } => {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    out.push(tool_chunk(
                        model,
                        json!({
                            "index": index, "id": id, "type": "function",
                            "function": { "name": name, "arguments": "" },
                        }),
                    ));
                }
            }
            CoreStreamEvent::BlockDelta { index, delta } => match delta {
                StreamDelta::Text { text } => {
                    out.push(chat_chunk(model, json!({ "content": text }), Value::Null));
                }
                StreamDelta::ToolInput { partial_json } => {
                    out.push(tool_chunk(
                        model,
                        json!({ "index": index, "function": { "arguments": partial_json } }),
                    ));
                }
                StreamDelta::Reasoning { .. } => {}
            },
            CoreStreamEvent::BlockStop { .. } => {}
            CoreStreamEvent::MessageDelta { stop_reason, usage } => {
                let finish = json!(unmap_stop_reason(*stop_reason));
                let mut c = chat_chunk(model, json!({}), finish);
                if let Some(u) = usage {
                    if let RawBody::Json { value } = &mut c.data {
                        value["usage"] = usage_object(u);
                    }
                }
                out.push(c);
            }
            CoreStreamEvent::MessageStop => out.push(done_chunk()),
            CoreStreamEvent::Error { message } => {
                out.push(RawChunk::json(
                    ChunkStage::ClientChunk,
                    Protocol::OpenAiChat,
                    None,
                    json!({ "error": { "message": message } }),
                ));
            }
        }
        Ok(out)
    }
}

/// 从 chunk 取出 JSON data（[DONE] 等非 JSON 返回 None）。
fn chunk_json(chunk: &RawChunk) -> Option<Value> {
    if let Some(v) = chunk.data.as_json() {
        return Some(v.clone());
    }
    serde_json::from_str::<Value>(&chunk.raw).ok()
}

#[async_trait]
impl ProviderStreamAdapter for OpenAiChatAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiChat
    }

    fn decode(&self, _ctx: &ReqCtx, chunk: &RawChunk) -> Result<Vec<CoreStreamEvent>> {
        // [DONE] 终止标记
        if let RawBody::Text { text } = &chunk.data {
            if text.trim() == "[DONE]" {
                return Ok(vec![CoreStreamEvent::MessageStop]);
            }
        }
        let Some(data) = chunk_json(chunk) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let choice = data
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

        // 首个带 role 的 delta 视为消息开始
        if delta.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            let id = data.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let model = data.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            out.push(CoreStreamEvent::MessageStart { id, model });
        }

        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                out.push(CoreStreamEvent::BlockDelta {
                    index: 0,
                    delta: StreamDelta::Text { text: text.to_string() },
                });
            }
        }

        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    out.push(CoreStreamEvent::BlockStart {
                        index,
                        block: ContentBlock::ToolUse {
                            id: id.to_string(),
                            name: fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            namespace: None,
                            input: json!({}),
                        },
                    });
                }
                if let Some(args) = fn_obj.get("arguments").and_then(|v| v.as_str()) {
                    if !args.is_empty() {
                        out.push(CoreStreamEvent::BlockDelta {
                            index,
                            delta: StreamDelta::ToolInput { partial_json: args.to_string() },
                        });
                    }
                }
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            let usage = data.get("usage").map(usage_from_chat);
            out.push(CoreStreamEvent::MessageDelta {
                stop_reason: map_finish_reason(fr),
                usage,
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(v: Value) -> RawChunk {
        RawChunk::json(ChunkStage::UpstreamChunk, Protocol::OpenAiChat, None, v)
    }

    #[test]
    fn encodes_text_delta_and_done() {
        let adapter = OpenAiChatAdapter;
        let mut ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        ctx.model_alias = "gpt-4o".to_string();

        let evs = adapter
            .encode(&ctx, &CoreStreamEvent::BlockDelta { index: 0, delta: StreamDelta::Text { text: "Hi".into() } })
            .unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data.as_json().unwrap()["choices"][0]["delta"]["content"], "Hi");

        let evs = adapter.encode(&ctx, &CoreStreamEvent::MessageStop).unwrap();
        match &evs[0].data {
            RawBody::Text { text } => assert_eq!(text, "[DONE]"),
            other => panic!("expected [DONE] text, got {other:?}"),
        }
    }

    #[test]
    fn decodes_chat_stream() {
        let adapter = OpenAiChatAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);

        let evs = adapter
            .decode(&ctx, &chunk(json!({"id":"c1","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]})))
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageStart { .. }));
        match &evs[1] {
            CoreStreamEvent::BlockDelta { delta: StreamDelta::Text { text }, .. } => assert_eq!(text, "Hello"),
            other => panic!("expected text delta, got {other:?}"),
        }

        let evs = adapter
            .decode(&ctx, &chunk(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}})))
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageDelta { .. }));

        // [DONE]
        let done = RawChunk {
            stage: ChunkStage::UpstreamChunk,
            protocol: Protocol::OpenAiChat,
            provider: None,
            event: None,
            data: RawBody::Text { text: "[DONE]".into() },
            raw: String::new(),
        };
        let evs = adapter.decode(&ctx, &done).unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageStop));
    }
}
