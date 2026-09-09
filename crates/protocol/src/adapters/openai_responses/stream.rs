//! OpenAI Responses 入口 Adapter —— 流式编码。
//!
//! 把协议中立的 [`CoreStreamEvent`] 编码为 Responses SSE 事件序列。采用无状态
//! 映射：`item_id` 由块 index 稳定派生，文本增量经 `response.output_text.delta`
//! 转发，结束时发 `response.completed`（携带 usage）。Codex CLI 依据 delta 累积
//! 文本，因此 `*.done` 事件的 text 字段可为空。

use async_trait::async_trait;
use moonbridge_core::{ContentBlock, CoreStreamEvent, Protocol, Result, StopReason, StreamDelta};
use serde_json::{json, Value};

use super::client::usage_from_value;
use super::dto::{self, event};
use super::OpenAiResponsesAdapter;
use crate::adapter::{ClientStreamAdapter, ProviderStreamAdapter};
use crate::context::ReqCtx;
use crate::raw::{ChunkStage, RawChunk};

/// 构造一个客户端方向的 SSE chunk。
fn sse(event_name: &str, data: Value) -> RawChunk {
    RawChunk::json(
        ChunkStage::ClientChunk,
        Protocol::OpenAiResponse,
        Some(event_name.to_string()),
        data,
    )
}

fn item_id(index: usize) -> String {
    format!("msg_{index}")
}

#[async_trait]
impl ClientStreamAdapter for OpenAiResponsesAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiResponse
    }

    fn encode(&self, ctx: &ReqCtx, ev: &CoreStreamEvent) -> Result<Vec<RawChunk>> {
        let mut out = Vec::new();
        match ev {
            CoreStreamEvent::MessageStart { id, model } => {
                out.push(sse(
                    event::CREATED,
                    json!({
                        "type": event::CREATED,
                        "response": dto::response_skeleton(id, model, "in_progress"),
                    }),
                ));
                out.push(sse(
                    event::IN_PROGRESS,
                    json!({
                        "type": event::IN_PROGRESS,
                        "response": dto::response_skeleton(id, model, "in_progress"),
                    }),
                ));
            }
            CoreStreamEvent::BlockStart { index, block } => match block {
                ContentBlock::ToolUse { id, name, .. } => {
                    out.push(sse(
                        event::OUTPUT_ITEM_ADDED,
                        json!({
                            "type": event::OUTPUT_ITEM_ADDED,
                            "output_index": index,
                            "item": dto::function_call_item(id, name, "", "in_progress"),
                        }),
                    ));
                }
                _ => {
                    out.push(sse(
                        event::OUTPUT_ITEM_ADDED,
                        json!({
                            "type": event::OUTPUT_ITEM_ADDED,
                            "output_index": index,
                            "item": dto::message_item(&item_id(*index), "", "in_progress"),
                        }),
                    ));
                    out.push(sse(
                        event::CONTENT_PART_ADDED,
                        json!({
                            "type": event::CONTENT_PART_ADDED,
                            "item_id": item_id(*index),
                            "output_index": index,
                            "content_index": 0,
                            "part": { "type": "output_text", "text": "", "annotations": [] },
                        }),
                    ));
                }
            },
            CoreStreamEvent::BlockDelta { index, delta } => match delta {
                StreamDelta::Text { text } => {
                    out.push(sse(
                        event::TEXT_DELTA,
                        json!({
                            "type": event::TEXT_DELTA,
                            "item_id": item_id(*index),
                            "output_index": index,
                            "content_index": 0,
                            "delta": text,
                        }),
                    ));
                }
                StreamDelta::ToolInput { partial_json } => {
                    out.push(sse(
                        event::FUNC_ARGS_DELTA,
                        json!({
                            "type": event::FUNC_ARGS_DELTA,
                            "item_id": item_id(*index),
                            "output_index": index,
                            "delta": partial_json,
                        }),
                    ));
                }
                StreamDelta::Reasoning { .. } => {
                    // 简化：Responses 入口不映射 reasoning 增量事件。输出 item 的
                    // done 收尾由 BlockStop 统一按 message 形态发出（encode 无状态、
                    // 无法区分块类型），若发 reasoning item 会与收尾事件不一致；
                    // Responses 入口的推理展示暂缺，chat/anthropic 入口不受影响。
                }
                StreamDelta::ReasoningSignature { .. } => {
                    // 同上：reasoning item 流式形态未映射，凭据随非流式路径透传
                }
            },
            CoreStreamEvent::BlockStop { index } => {
                out.push(sse(
                    event::TEXT_DONE,
                    json!({
                        "type": event::TEXT_DONE,
                        "item_id": item_id(*index),
                        "output_index": index,
                        "content_index": 0,
                        "text": "",
                    }),
                ));
                out.push(sse(
                    event::CONTENT_PART_DONE,
                    json!({
                        "type": event::CONTENT_PART_DONE,
                        "item_id": item_id(*index),
                        "output_index": index,
                        "content_index": 0,
                        "part": { "type": "output_text", "text": "", "annotations": [] },
                    }),
                ));
                out.push(sse(
                    event::OUTPUT_ITEM_DONE,
                    json!({
                        "type": event::OUTPUT_ITEM_DONE,
                        "output_index": index,
                        "item": dto::message_item(&item_id(*index), "", "completed"),
                    }),
                ));
            }
            CoreStreamEvent::MessageDelta { usage, .. } => {
                let usage = match usage {
                    Some(u) => dto::usage_object(u.input_tokens, u.output_tokens),
                    None => Value::Null,
                };
                let mut resp = dto::response_skeleton("", &ctx.model_alias, "completed");
                if let Some(obj) = resp.as_object_mut() {
                    obj.insert("usage".to_string(), usage);
                }
                out.push(sse(
                    event::COMPLETED,
                    json!({ "type": event::COMPLETED, "response": resp }),
                ));
            }
            CoreStreamEvent::MessageStop => {}
            CoreStreamEvent::Error { message } => {
                out.push(sse(
                    "response.failed",
                    json!({
                        "type": "response.failed",
                        "response": { "status": "failed", "error": { "message": message } },
                    }),
                ));
            }
        }
        Ok(out)
    }
}

/// 从 chunk 取出 JSON data（优先已解析的 data，回退解析 raw 文本）。
fn chunk_json(chunk: &RawChunk) -> Option<Value> {
    if let Some(v) = chunk.data.as_json() {
        return Some(v.clone());
    }
    serde_json::from_str::<Value>(&chunk.raw).ok()
}

fn output_index(data: &Value) -> usize {
    data.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize
}

#[async_trait]
impl ProviderStreamAdapter for OpenAiResponsesAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::OpenAiResponse
    }

    /// Responses SSE → Core 流事件（与入口 encode 互为逆向）。
    fn decode(&self, _ctx: &ReqCtx, chunk: &RawChunk) -> Result<Vec<CoreStreamEvent>> {
        let Some(data) = chunk_json(chunk) else {
            return Ok(Vec::new());
        };
        let ty = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let mut out = Vec::new();

        match ty {
            "response.created" => {
                let resp = data.get("response").cloned().unwrap_or(Value::Null);
                let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let model = resp.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                out.push(CoreStreamEvent::MessageStart { id, model });
            }
            "response.output_item.added" => {
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                let block = match item.get("type").and_then(|t| t.as_str()) {
                    Some("function_call") => ContentBlock::ToolUse {
                        id: item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        name: item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        namespace: None,
                        input: json!({}),
                    },
                    // reasoning item：后续 summary/reasoning 文本增量挂同一 output_index
                    Some("reasoning") => ContentBlock::Reasoning { text: String::new(), signature: None },
                    _ => ContentBlock::text(""),
                };
                out.push(CoreStreamEvent::BlockStart { index: output_index(&data), block });
            }
            // 推理增量：官方摘要（summary_text）与原文（reasoning_text，第三方兼容实现）
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let text = data.get("delta").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if !text.is_empty() {
                    out.push(CoreStreamEvent::BlockDelta {
                        index: output_index(&data),
                        delta: StreamDelta::Reasoning { text },
                    });
                }
            }
            "response.output_text.delta" => {
                let text = data.get("delta").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                out.push(CoreStreamEvent::BlockDelta {
                    index: output_index(&data),
                    delta: StreamDelta::Text { text },
                });
            }
            "response.function_call_arguments.delta" => {
                let partial = data.get("delta").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                out.push(CoreStreamEvent::BlockDelta {
                    index: output_index(&data),
                    delta: StreamDelta::ToolInput { partial_json: partial },
                });
            }
            "response.output_item.done" => {
                // reasoning item 的加密 CoT 凭据在 done 事件的 item 上：
                // 转为凭据增量流出，供入口协议透传（chat 搭 <mb-cot>、anthropic 发 signature_delta）
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
                    if let Some(enc) = item
                        .get("encrypted_content")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        out.push(CoreStreamEvent::BlockDelta {
                            index: output_index(&data),
                            delta: StreamDelta::ReasoningSignature { signature: enc.to_string() },
                        });
                    }
                }
                out.push(CoreStreamEvent::BlockStop { index: output_index(&data) });
            }
            "response.completed" => {
                let usage = data
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .map(usage_from_value);
                out.push(CoreStreamEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage,
                });
                out.push(CoreStreamEvent::MessageStop);
            }
            "response.failed" => {
                let message = data
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("response failed")
                    .to_string();
                out.push(CoreStreamEvent::Error { message });
            }
            _ => {}
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(v: Value) -> RawChunk {
        RawChunk::json(ChunkStage::UpstreamChunk, Protocol::OpenAiResponse, None, v)
    }

    #[test]
    fn decodes_reasoning_item_and_deltas() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);

        // reasoning item 开始：块类型为 Reasoning（而非默认 text 块）
        let evs = adapter
            .decode(&ctx, &chunk(json!({
                "type": "response.output_item.added", "output_index": 0,
                "item": { "type": "reasoning", "summary": [] }
            })))
            .unwrap();
        assert!(matches!(
            &evs[0],
            CoreStreamEvent::BlockStart { block: ContentBlock::Reasoning { .. }, .. }
        ));

        // 官方摘要与第三方原文两种增量都解析为 Reasoning
        for ty in ["response.reasoning_summary_text.delta", "response.reasoning_text.delta"] {
            let evs = adapter
                .decode(&ctx, &chunk(json!({ "type": ty, "output_index": 0, "delta": "hmm" })))
                .unwrap();
            match &evs[0] {
                CoreStreamEvent::BlockDelta { delta: StreamDelta::Reasoning { text }, .. } => {
                    assert_eq!(text, "hmm")
                }
                other => panic!("expected reasoning delta for {ty}, got {other:?}"),
            }
        }
    }

    #[test]
    fn decodes_responses_stream() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"response.created","response":{"id":"resp_1","model":"gpt-x"}})))
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageStart { .. }));

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"response.output_text.delta","output_index":0,"delta":"Hi"})))
            .unwrap();
        match &evs[0] {
            CoreStreamEvent::BlockDelta { delta: StreamDelta::Text { text }, .. } => {
                assert_eq!(text, "Hi")
            }
            _ => panic!("expected text delta"),
        }

        let evs = adapter
            .decode(&ctx, &chunk(json!({"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":3}}})))
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageDelta { .. }));
        assert!(matches!(evs[1], CoreStreamEvent::MessageStop));
    }
}
