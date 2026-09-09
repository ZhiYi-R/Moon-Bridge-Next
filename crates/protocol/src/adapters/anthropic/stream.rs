//! Anthropic 上游 Adapter —— 流式解码。
//!
//! 把 Anthropic Messages 的 SSE chunk 解码为协议中立的 [`CoreStreamEvent`]。
//! Anthropic 的流事件模型与 Core IR 高度一致（Core IR 即参考其设计），因此映射
//! 直接：`message_start`→MessageStart、`content_block_*`→Block*、
//! `message_delta`→MessageDelta、`message_stop`→MessageStop。

use async_trait::async_trait;
use moonbridge_core::{
    ContentBlock, CoreStreamEvent, Protocol, Result, StopReason, StreamDelta, Usage,
};
use serde_json::{json, Value};

use super::provider::unmap_stop_reason;
use super::AnthropicAdapter;
use crate::adapter::{ClientStreamAdapter, ProviderStreamAdapter};
use crate::context::ReqCtx;
use crate::raw::{ChunkStage, RawChunk};

fn stop_reason(s: &str) -> Option<StopReason> {
    match s {
        "end_turn" => Some(StopReason::EndTurn),
        "max_tokens" => Some(StopReason::MaxTokens),
        "stop_sequence" => Some(StopReason::StopSequence),
        "tool_use" => Some(StopReason::ToolUse),
        _ => None,
    }
}

fn u32_of(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0) as u32
}

/// 从 chunk 取出 JSON data：优先已解析的 data，回退解析 raw 文本。
fn chunk_json(chunk: &RawChunk) -> Option<Value> {
    if let Some(v) = chunk.data.as_json() {
        return Some(v.clone());
    }
    serde_json::from_str::<Value>(&chunk.raw).ok()
}

#[async_trait]
impl ProviderStreamAdapter for AnthropicAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    fn decode(&self, _ctx: &ReqCtx, chunk: &RawChunk) -> Result<Vec<CoreStreamEvent>> {
        let data = match chunk_json(chunk) {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };
        let ty = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let mut out = Vec::new();

        match ty {
            "message_start" => {
                let msg = data.get("message").cloned().unwrap_or_else(|| Value::Null);
                let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let model = msg
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                out.push(CoreStreamEvent::MessageStart { id, model });
                if let Some(u) = msg.get("usage") {
                    out.push(CoreStreamEvent::MessageDelta {
                        stop_reason: None,
                        usage: Some(Usage {
                            input_tokens: u32_of(u, "input_tokens"),
                            output_tokens: u32_of(u, "output_tokens"),
                            cache_read_tokens: u32_of(u, "cache_read_input_tokens"),
                            cache_write_tokens: u32_of(u, "cache_creation_input_tokens"),
                            reasoning_tokens: 0,
                        }),
                    });
                }
            }
            "content_block_start" => {
                let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let cb = data.get("content_block").cloned().unwrap_or_else(|| Value::Null);
                let block = match cb.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => ContentBlock::ToolUse {
                        id: cb.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        name: cb.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        namespace: None,
                        input: cb.get("input").cloned().unwrap_or_else(|| Value::Null),
                        signature: None,
                    },
                    Some("thinking") | Some("redacted_thinking") => ContentBlock::Reasoning {
                        text: cb.get("thinking").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        signature: None,
                    },
                    _ => ContentBlock::text(
                        cb.get("text").and_then(|v| v.as_str()).unwrap_or_default(),
                    ),
                };
                out.push(CoreStreamEvent::BlockStart { index, block });
            }
            "content_block_delta" => {
                let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let delta = data.get("delta").cloned().unwrap_or_else(|| Value::Null);
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(|v| v.as_str()).unwrap_or_default();
                        out.push(CoreStreamEvent::BlockDelta {
                            index,
                            delta: StreamDelta::Text { text: text.to_string() },
                        });
                    }
                    Some("input_json_delta") => {
                        let partial = delta
                            .get("partial_json")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        out.push(CoreStreamEvent::BlockDelta {
                            index,
                            delta: StreamDelta::ToolInput { partial_json: partial.to_string() },
                        });
                    }
                    Some("thinking_delta") => {
                        let thinking = delta
                            .get("thinking")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        out.push(CoreStreamEvent::BlockDelta {
                            index,
                            delta: StreamDelta::Reasoning { text: thinking.to_string() },
                        });
                    }
                    // 加密 CoT 回传凭据：必须在 content_block_stop 前到达客户端，
                    // 否则多轮 thinking+tool_use 回传缺失 signature 会 400
                    Some("signature_delta") => {
                        let sig = delta
                            .get("signature")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if !sig.is_empty() {
                            out.push(CoreStreamEvent::BlockDelta {
                                index,
                                delta: StreamDelta::ReasoningSignature { signature: sig.to_string() },
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                out.push(CoreStreamEvent::BlockStop { index, block: None });
            }
            "message_delta" => {
                let sr = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|v| v.as_str())
                    .and_then(stop_reason);
                let usage = data.get("usage").map(|u| Usage {
                    input_tokens: 0,
                    output_tokens: u32_of(u, "output_tokens"),
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                });
                out.push(CoreStreamEvent::MessageDelta { stop_reason: sr, usage });
            }
            "message_stop" => out.push(CoreStreamEvent::MessageStop),
            "error" => {
                let message = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("upstream stream error")
                    .to_string();
                out.push(CoreStreamEvent::Error { message });
            }
            // ping 及未知事件忽略
            _ => {}
        }

        Ok(out)
    }
}

/// 构造一个客户端方向的 Anthropic SSE chunk。
fn client_sse(event: &str, data: Value) -> RawChunk {
    RawChunk::json(
        ChunkStage::ClientChunk,
        Protocol::Anthropic,
        Some(event.to_string()),
        data,
    )
}

#[async_trait]
impl ClientStreamAdapter for AnthropicAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    /// Core 流事件 → Anthropic Messages SSE（与上游 decode 互为逆向）。
    fn encode(&self, _ctx: &ReqCtx, ev: &CoreStreamEvent) -> Result<Vec<RawChunk>> {
        let mut out = Vec::new();
        match ev {
            CoreStreamEvent::MessageStart { id, model } => {
                out.push(client_sse(
                    "message_start",
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": id, "type": "message", "role": "assistant", "model": model,
                            "content": [], "stop_reason": null, "stop_sequence": null,
                            "usage": { "input_tokens": 0, "output_tokens": 0 },
                        },
                    }),
                ));
            }
            CoreStreamEvent::BlockStart { index, block } => {
                let cb = match block {
                    ContentBlock::ToolUse { id, name, .. } => {
                        json!({ "type": "tool_use", "id": id, "name": name, "input": {} })
                    }
                    ContentBlock::Reasoning { .. } => json!({ "type": "thinking", "thinking": "" }),
                    _ => json!({ "type": "text", "text": "" }),
                };
                out.push(client_sse(
                    "content_block_start",
                    json!({ "type": "content_block_start", "index": index, "content_block": cb }),
                ));
            }
            CoreStreamEvent::BlockDelta { index, delta } => {
                let d = match delta {
                    StreamDelta::Text { text } => json!({ "type": "text_delta", "text": text }),
                    StreamDelta::ToolInput { partial_json } => {
                        json!({ "type": "input_json_delta", "partial_json": partial_json })
                    }
                    StreamDelta::Reasoning { text } => {
                        json!({ "type": "thinking_delta", "thinking": text })
                    }
                    // 加密 CoT 回传凭据：客户端在 content_block_stop 前累积进
                    // thinking 块的 signature，下一轮原样回传
                    StreamDelta::ReasoningSignature { signature } => {
                        json!({ "type": "signature_delta", "signature": signature })
                    }
                };
                out.push(client_sse(
                    "content_block_delta",
                    json!({ "type": "content_block_delta", "index": index, "delta": d }),
                ));
            }
            CoreStreamEvent::BlockStop { index, .. } => {
                out.push(client_sse(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": index }),
                ));
            }
            CoreStreamEvent::MessageDelta { stop_reason, usage } => {
                let sr = stop_reason.map(unmap_stop_reason).unwrap_or("end_turn");
                let mut data = json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": sr, "stop_sequence": null },
                });
                if let Some(u) = usage {
                    data["usage"] = json!({ "output_tokens": u.output_tokens });
                }
                out.push(client_sse("message_delta", data));
            }
            CoreStreamEvent::MessageStop => {
                out.push(client_sse("message_stop", json!({ "type": "message_stop" })));
            }
            CoreStreamEvent::Error { message } => {
                out.push(client_sse(
                    "error",
                    json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游 signature_delta → Core 凭据增量；入口 encode 还原 signature_delta。
    #[test]
    fn signature_delta_roundtrips() {
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::Anthropic);
        let evs = adapter.decode(&ctx, &chunk(json!({
            "type": "content_block_delta", "index": 0,
            "delta": { "type": "signature_delta", "signature": "SIG" }
        }))).unwrap();
        match &evs[0] {
            CoreStreamEvent::BlockDelta { delta: StreamDelta::ReasoningSignature { signature }, .. } => {
                assert_eq!(signature, "SIG");
            }
            other => panic!("expected signature delta, got {other:?}"),
        }
        let mut c = ReqCtx::new("r1", Protocol::Anthropic);
        c.model_alias = "m".into();
        let chunks = adapter.encode(&c, &CoreStreamEvent::BlockDelta {
            index: 0,
            delta: StreamDelta::ReasoningSignature { signature: "SIG".into() },
        }).unwrap();
        let d = &chunks[0].data.as_json().unwrap()["delta"];
        assert_eq!(d["type"], "signature_delta");
        assert_eq!(d["signature"], "SIG");
    }


    fn chunk(v: Value) -> RawChunk {
        RawChunk::json(ChunkStage::UpstreamChunk, Protocol::Anthropic, None, v)
    }

    #[test]
    fn decodes_text_stream() {
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"message_start","message":{"id":"m1","model":"claude","usage":{"input_tokens":7}}})))
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageStart { .. }));

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})))
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::BlockStart { index: 0, .. }));

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}})))
            .unwrap();
        match &evs[0] {
            CoreStreamEvent::BlockDelta { delta: StreamDelta::Text { text }, .. } => {
                assert_eq!(text, "Hi")
            }
            _ => panic!("expected text delta"),
        }

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}})))
            .unwrap();
        match &evs[0] {
            CoreStreamEvent::MessageDelta { stop_reason, usage } => {
                assert_eq!(*stop_reason, Some(StopReason::EndTurn));
                assert_eq!(usage.unwrap().output_tokens, 3);
            }
            _ => panic!("expected message delta"),
        }
    }

    #[test]
    fn decodes_tool_use_delta() {
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}})))
            .unwrap();
        match &evs[0] {
            CoreStreamEvent::BlockDelta { index, delta: StreamDelta::ToolInput { partial_json } } => {
                assert_eq!(*index, 1);
                assert_eq!(partial_json, "{\"a\":");
            }
            _ => panic!("expected tool input delta"),
        }
    }

    #[test]
    fn ignores_ping() {
        let adapter = AnthropicAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);
        let evs = adapter.decode(&ctx, &chunk(json!({"type":"ping"}))).unwrap();
        assert!(evs.is_empty());
    }
}
