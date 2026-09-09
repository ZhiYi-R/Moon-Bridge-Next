//! Google Generative AI (Gemini) 流式：Core 事件 ↔ Gemini SSE chunk。
//!
//! Gemini `streamGenerateContent?alt=sse` 的每个 chunk 形如
//! `data: {"candidates":[{"content":{"parts":[{"text":"..."}],"role":"model"}}]}`，
//! 首块携带 `modelVersion`，末块携带 `finishReason` 与 `usageMetadata`；无 `[DONE]`。
//! decode 供 Gemini 作上游时解析，encode 供 Gemini 作入口时输出。
//!
//! Gemini 无 Anthropic 式的显式块边界事件，故 decode 以「首块」合成 MessageStart +
//! 文本块起始（index 0），functionCall 归入 index≥1，末块的 finishReason 触发收尾。

use async_trait::async_trait;
use moonbridge_core::{ContentBlock, CoreStreamEvent, Protocol, Result, StreamDelta};
use serde_json::{json, Value};

use super::dto::{map_finish_reason, unmap_stop_reason, usage_from_gemini, usage_object};
use super::GoogleGenAiAdapter;
use crate::adapter::{ClientStreamAdapter, ProviderStreamAdapter, StreamEncodeState};
use crate::context::ReqCtx;
use crate::raw::{ChunkStage, RawBody, RawChunk};

/// 从 chunk 取出 JSON data（[DONE] 等非 JSON 返回 None）。
fn chunk_json(chunk: &RawChunk) -> Option<Value> {
    if let Some(v) = chunk.data.as_json() {
        return Some(v.clone());
    }
    serde_json::from_str::<Value>(&chunk.raw).ok()
}

#[async_trait]
impl ProviderStreamAdapter for GoogleGenAiAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::GoogleGenai
    }

    fn decode(&self, _ctx: &ReqCtx, chunk: &RawChunk) -> Result<Vec<CoreStreamEvent>> {
        // 兼容网关可能补发 [DONE]
        if let RawBody::Text { text } = &chunk.data {
            if text.trim() == "[DONE]" {
                return Ok(vec![CoreStreamEvent::MessageStop]);
            }
        }
        let Some(data) = chunk_json(chunk) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();

        // 首块标记：Gemini 仅在首个 chunk 携带 modelVersion
        if data.get("modelVersion").is_some() {
            let id = data
                .get("responseId")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let model = data
                .get("modelVersion")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            out.push(CoreStreamEvent::MessageStart { id, model });
            // 打开文本块（index 0）；纯函数调用响应会留下一个空文本块，各入口编码器可容忍
            out.push(CoreStreamEvent::BlockStart {
                index: 0,
                block: ContentBlock::text(""),
            });
        }

        let candidate = data
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        let parts = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();

        let mut fc_index = 0usize;
        for p in &parts {
            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    out.push(CoreStreamEvent::BlockDelta {
                        index: 0,
                        delta: StreamDelta::Text { text: t.to_string() },
                    });
                }
            }
            if let Some(fc) = p.get("functionCall") {
                let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let id = fc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| format!("{name}-call"));
                let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                // 加密 CoT 凭据：与 function call 强绑定的 thoughtSignature（part 级字段）
                let signature = p
                    .get("thoughtSignature")
                    .or_else(|| fc.get("thoughtSignature"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let idx = 1 + fc_index;
                fc_index += 1;
                out.push(CoreStreamEvent::BlockStart {
                    index: idx,
                    block: ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        namespace: None,
                        input: json!({}),
                        signature: signature.clone(),
                    },
                });
                out.push(CoreStreamEvent::BlockDelta {
                    index: idx,
                    delta: StreamDelta::ToolInput { partial_json: args.to_string() },
                });
                out.push(CoreStreamEvent::BlockStop {
                    index: idx,
                    block: Some(ContentBlock::ToolUse {
                        id,
                        name,
                        namespace: None,
                        input: args,
                        signature,
                    }),
                });
            } else {
                // 官方流式形态：thoughtSignature 可能在末块以空 text part 返回
                // （官方文档：解析器必须检查空文本部分）。带非空文本的 part 不在此
                // 处理——凭据属于文本块，无独立承载位，丢弃不影响强校验场景。
                let has_text = p.get("text").and_then(|v| v.as_str()).is_some_and(|t| !t.is_empty());
                if !has_text {
                    if let Some(sig) = p
                        .get("thoughtSignature")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        out.push(CoreStreamEvent::BlockDelta {
                            index: 0,
                            delta: StreamDelta::ReasoningSignature { signature: sig.to_string() },
                        });
                    }
                }
            }
        }

        // 末块：finishReason 触发文本块收尾 + MessageDelta(usage) + MessageStop
        if let Some(fr) = candidate.get("finishReason").and_then(|v| v.as_str()) {
            out.push(CoreStreamEvent::BlockStop { index: 0, block: None });
            let usage = data.get("usageMetadata").map(usage_from_gemini);
            out.push(CoreStreamEvent::MessageDelta {
                stop_reason: map_finish_reason(fr),
                usage,
            });
            out.push(CoreStreamEvent::MessageStop);
        }

        Ok(out)
    }
}

/// 构造一个客户端方向的 Gemini SSE chunk。
fn gemini_chunk(parts: Value, finish: Option<&str>, usage: Option<Value>) -> RawChunk {
    let mut cand = json!({ "content": { "role": "model", "parts": parts }, "index": 0 });
    if let Some(fr) = finish {
        cand["finishReason"] = json!(fr);
    }
    let mut data = json!({ "candidates": [cand] });
    if let Some(u) = usage {
        data["usageMetadata"] = u;
    }
    RawChunk::json(ChunkStage::ClientChunk, Protocol::GoogleGenai, None, data)
}

#[async_trait]
impl ClientStreamAdapter for GoogleGenAiAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::GoogleGenai
    }

    /// Core 流事件 → Gemini SSE。文本增量精确映射；Gemini 无起始/结束标记事件，
    /// 故 MessageStart/MessageStop 不产出 chunk，收尾信息随 MessageDelta 的
    /// finishReason + usageMetadata 下发。
    ///
    /// 局限：入口方向的工具参数增量（ToolInput）无状态难以累积为完整 `args`，
    /// 故 functionCall 在 BlockStart 时以已知 input 一次性下发（Gemini 入口为次要
    /// 链路，未挂载 HTTP 路由，主要用于矩阵完整性与测试）。
    fn encode(
        &self,
        _ctx: &ReqCtx,
        ev: &CoreStreamEvent,
        _st: &mut StreamEncodeState,
    ) -> Result<Vec<RawChunk>> {
        let mut out = Vec::new();
        match ev {
            CoreStreamEvent::MessageStart { .. } => {}
            CoreStreamEvent::BlockStart { block, .. } => {
                if let ContentBlock::ToolUse { name, input, signature, .. } = block {
                    let mut fc = json!({ "name": name, "args": input });
                    if let Some(sig) = signature {
                        if !sig.is_empty() {
                            fc["thoughtSignature"] = json!(sig);
                        }
                    }
                    out.push(gemini_chunk(json!([{ "functionCall": fc }]), None, None));
                }
            }
            CoreStreamEvent::BlockDelta { delta, .. } => match delta {
                StreamDelta::Text { text } => {
                    if !text.is_empty() {
                        out.push(gemini_chunk(json!([{ "text": text }]), None, None));
                    }
                }
                StreamDelta::Reasoning { text } => {
                    if !text.is_empty() {
                        out.push(gemini_chunk(json!([{ "text": text }]), None, None));
                    }
                }
                // Gemini 流式无凭据承载位（thoughtSignature 是非流式 part 级字段）
                StreamDelta::ReasoningSignature { .. } => {}
                StreamDelta::ToolInput { .. } => {}
            },
            CoreStreamEvent::BlockStop { .. } => {}
            CoreStreamEvent::MessageDelta { stop_reason, usage } => {
                let fr = unmap_stop_reason(*stop_reason);
                out.push(gemini_chunk(
                    json!([]),
                    Some(fr),
                    usage.as_ref().map(usage_object),
                ));
            }
            CoreStreamEvent::MessageStop => {}
            CoreStreamEvent::Error { message } => {
                out.push(RawChunk::json(
                    ChunkStage::ClientChunk,
                    Protocol::GoogleGenai,
                    None,
                    json!({ "error": { "message": message } }),
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::{StopReason, Usage};

    fn chunk(v: Value) -> RawChunk {
        RawChunk::json(ChunkStage::UpstreamChunk, Protocol::GoogleGenai, None, v)
    }

    /// 官方流式形态：签名可能在末块以空 text part 返回，需转为凭据增量。
    #[test]
    fn decodes_signature_only_trailing_part() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::GoogleGenai);
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({
                    "candidates": [{ "content": { "role": "model", "parts": [
                        { "text": "", "thoughtSignature": "SIG" }
                    ] } }]
                })),
            )
            .unwrap();
        match &evs[0] {
            CoreStreamEvent::BlockDelta { delta: StreamDelta::ReasoningSignature { signature }, .. } => {
                assert_eq!(signature, "SIG");
            }
            other => panic!("expected signature delta, got {other:?}"),
        }
    }

    #[test]
    fn decodes_first_text_chunk() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({
                    "candidates": [{ "content": { "role": "model", "parts": [{ "text": "Hello" }] }, "index": 0 }],
                    "modelVersion": "gemini-2.0-flash",
                    "responseId": "resp-1"
                })),
            )
            .unwrap();
        assert!(matches!(evs[0], CoreStreamEvent::MessageStart { .. }));
        assert!(matches!(evs[1], CoreStreamEvent::BlockStart { index: 0, .. }));
        match &evs[2] {
            CoreStreamEvent::BlockDelta { index: 0, delta: StreamDelta::Text { text } } => {
                assert_eq!(text, "Hello")
            }
            other => panic!("expected text delta, got {other:?}"),
        }
    }

    #[test]
    fn middle_chunk_only_emits_delta() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": " world" }] }, "index": 0 }] })),
            )
            .unwrap();
        assert_eq!(evs.len(), 1, "非首块不应合成 MessageStart: {evs:?}");
        assert!(matches!(evs[0], CoreStreamEvent::BlockDelta { .. }));
    }

    #[test]
    fn decodes_finish_chunk() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({
                    "candidates": [{ "content": { "role": "model", "parts": [{ "text": "!" }] }, "finishReason": "STOP", "index": 0 }],
                    "usageMetadata": { "promptTokenCount": 8, "candidatesTokenCount": 4, "totalTokenCount": 12 }
                })),
            )
            .unwrap();
        // text delta, BlockStop(0), MessageDelta, MessageStop
        assert!(evs.iter().any(|e| matches!(e, CoreStreamEvent::BlockDelta { .. })));
        assert!(evs.iter().any(|e| matches!(e, CoreStreamEvent::BlockStop { index: 0, .. })));
        let md = evs.iter().find_map(|e| match e {
            CoreStreamEvent::MessageDelta { stop_reason, usage } => Some((stop_reason, usage)),
            _ => None,
        });
        let (sr, usage) = md.expect("应有 MessageDelta");
        assert_eq!(*sr, Some(StopReason::EndTurn));
        assert_eq!(usage.unwrap(), Usage { input_tokens: 8, output_tokens: 4, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 });
        assert!(matches!(evs.last().unwrap(), CoreStreamEvent::MessageStop));
    }

    #[test]
    fn decodes_function_call_chunk() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::OpenAiChat);
        let evs = adapter
            .decode(
                &ctx,
                &chunk(json!({
                    "candidates": [{ "content": { "role": "model", "parts": [{ "functionCall": { "name": "get_time", "args": {"tz":"UTC"} } }] }, "index": 0 }]
                })),
            )
            .unwrap();
        let bs = evs.iter().find_map(|e| match e {
            CoreStreamEvent::BlockStart { index, block: ContentBlock::ToolUse { name, .. } } => Some((*index, name.clone())),
            _ => None,
        });
        let (idx, name) = bs.expect("应有 ToolUse BlockStart");
        assert_eq!(idx, 1, "函数调用应避让文本块 index 0");
        assert_eq!(name, "get_time");
        assert!(evs.iter().any(|e| matches!(e, CoreStreamEvent::BlockDelta { delta: StreamDelta::ToolInput { .. }, .. })));
    }

    #[test]
    fn encodes_text_delta_and_finish() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::GoogleGenai);

        let evs = adapter
            .encode(&ctx, &CoreStreamEvent::BlockDelta { index: 0, delta: StreamDelta::Text { text: "Hi".into() } }, &mut StreamEncodeState::default())
            .unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data.as_json().unwrap()["candidates"][0]["content"]["parts"][0]["text"], "Hi");

        let evs = adapter
            .encode(
                &ctx,
                &CoreStreamEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage { input_tokens: 3, output_tokens: 2, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 }),
                },
                &mut StreamEncodeState::default(),
            )
            .unwrap();
        let data = evs[0].data.as_json().unwrap();
        assert_eq!(data["candidates"][0]["finishReason"], "STOP");
        assert_eq!(data["usageMetadata"]["promptTokenCount"], 3);
    }

    #[test]
    fn encode_message_start_and_stop_are_silent() {
        let adapter = GoogleGenAiAdapter;
        let ctx = ReqCtx::new("r1", Protocol::GoogleGenai);
        assert!(adapter.encode(&ctx, &CoreStreamEvent::MessageStart { id: "x".into(), model: "g".into() }, &mut StreamEncodeState::default()).unwrap().is_empty());
        assert!(adapter.encode(&ctx, &CoreStreamEvent::MessageStop, &mut StreamEncodeState::default()).unwrap().is_empty());
    }
}
