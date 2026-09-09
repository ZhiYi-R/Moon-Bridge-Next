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
                ContentBlock::Reasoning { .. } => {
                    // reasoning item：收尾（output_item.done）在 BlockStop 按
                    // block 类型分派，这里只发 item 起始
                    out.push(sse(
                        event::OUTPUT_ITEM_ADDED,
                        json!({
                            "type": event::OUTPUT_ITEM_ADDED,
                            "output_index": index,
                            "item": { "type": "reasoning", "id": item_id(*index), "summary": [] },
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
                StreamDelta::Reasoning { text } => {
                    // 摘要明文增量：客户端累积后随 output_item.done 的 item 一致
                    out.push(sse(
                        event::REASONING_SUMMARY_TEXT_DELTA,
                        json!({
                            "type": event::REASONING_SUMMARY_TEXT_DELTA,
                            "item_id": item_id(*index),
                            "output_index": index,
                            "delta": text,
                        }),
                    ));
                }
                StreamDelta::ReasoningSignature { .. } => {
                    // 凭据不经增量事件下发：随 BlockStop 的 reasoning item
                    // （output_item.done）原样携带
                }
            },
            CoreStreamEvent::BlockStop { index, block } => {
                // reasoning 块：收尾事件用 reasoning item 形态（含 encrypted_content 凭据），
                // 客户端累积后下一轮原样回传；其余块维持 message/output_text 形态。
                if let Some(ContentBlock::Reasoning { text, signature }) = block {
                    let mut item = json!({
                        "type": "reasoning",
                        "id": item_id(*index),
                        "summary": [{ "type": "summary_text", "text": text }],
                    });
                    if let Some(enc) = signature {
                        if !enc.is_empty() {
                            item["encrypted_content"] = json!(enc);
                        }
                    }
                    out.push(sse(
                        event::OUTPUT_ITEM_DONE,
                        json!({
                            "type": event::OUTPUT_ITEM_DONE,
                            "output_index": index,
                            "item": item,
                        }),
                    ));
                } else {
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
                        signature: None,
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
                // reasoning item 的明文/凭据在 done 事件的 item 上：组装进收尾块，
                // encode 端据此发 reasoning 形态的收尾事件（凭据原样带回客户端）。
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                let mut block = None;
                if item.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
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
                    // 无明文且无凭据：不发 reasoning 形态收尾（encode 回退 message 形态）
                    if !text.is_empty() || signature.is_some() {
                        block = Some(ContentBlock::Reasoning { text, signature });
                    }
                }
                out.push(CoreStreamEvent::BlockStop {
                    index: output_index(&data),
                    block,
                });
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

    /// 加密 CoT 流式 round-trip：上游 output_item.done 携带 encrypted_content →
    /// BlockStop 的 reasoning 块凭据 → 入口 encode 以 reasoning item 形态原样下发。
    #[test]
    fn reasoning_encrypted_content_roundtrips_via_stop() {
        let adapter = OpenAiResponsesAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiResponse);

        // 上游：reasoning item 只带 encrypted_content（无任何明文摘要）
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({
                    "type": "response.output_item.done", "output_index": 0,
                    "item": { "type": "reasoning", "summary": [], "encrypted_content": "ENC" }
                })),
            )
            .unwrap();
        match &evs[0] {
            CoreStreamEvent::BlockStop {
                block: Some(ContentBlock::Reasoning { text, signature: Some(enc) }),
                ..
            } => {
                assert!(text.is_empty(), "encrypted 不是展示文本");
                assert_eq!(enc, "ENC");
            }
            other => panic!("expected reasoning stop block, got {other:?}"),
        }

        // 入口 encode：凭据随 reasoning item 收尾事件原样下发
        let cchunks = adapter.encode(&ctx, &evs[0]).unwrap();
        let raw = serde_json::to_string(
            &cchunks[0]
                .data
                .as_json()
                .expect("reasoning stop 应为 JSON 事件"),
        )
        .unwrap();
        assert!(raw.contains("encrypted_content"));
        assert!(raw.contains("ENC"));

        // 无明文且无凭据：回退 message 形态收尾，不发空 reasoning item
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({
                    "type": "response.output_item.done", "output_index": 0,
                    "item": { "type": "reasoning", "summary": [] }
                })),
            )
            .unwrap();
        assert!(matches!(&evs[0], CoreStreamEvent::BlockStop { block: None, .. }));
    }
}
