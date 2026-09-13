//! 流式请求编排：把上游 SSE 经「报文层 chunk 钩子 → 解码 → Core 层事件钩子 →
//! 编码 → 报文层 chunk 钩子」链路转换为客户端协议的 SSE 输出流。
//!
//! 输出采用 axum 原生 [`Sse`]，流项为 `Result<Event, Infallible>`。
//!
//! 收尾审计（usage 落库 + trace 落盘）由 [`StreamAudit`] 的 `Drop` 承担，
//! 见其文档——流可能以三种方式结束（正常、中途出错、客户端断开），只有 Drop
//! 能同时覆盖。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use moonbridge_core::{CoreStreamEvent, Protocol, StreamDelta, Usage};
use moonbridge_protocol::{
    ChunkVerdict, RawBody, RawChunk, ReqCtx, StreamDecodeState, StreamEncodeState,
};
use moonbridge_store::Database;
use serde_json::Value;

use crate::state::AppState;
use crate::trace::TraceRecord;
use crate::upstream;
use crate::usage;

/// 取客户端 chunk 应写入 `data:` 字段的文本。
fn chunk_data_text(cc: &RawChunk) -> String {
    match &cc.data {
        RawBody::Json { value } => value.to_string(),
        RawBody::Text { text } => text.clone(),
        // 二进制无法直接进 SSE 文本帧：尽力转 UTF-8（保留可观测性，不 panic）
        RawBody::Binary { data } => String::from_utf8_lossy(data).to_string(),
        RawBody::Empty => cc.raw.clone(),
    }
}

/// 把客户端方向的 RawChunk 转为 axum SSE [`Event`]。
fn chunk_to_event(cc: &RawChunk) -> Event {
    let mut ev = Event::default();
    if let Some(name) = &cc.event {
        ev = ev.event(name.clone());
    }
    ev.data(chunk_data_text(cc))
}

/// 构造一个带内 error 事件（流已开始、HTTP 200 已发出，错误只能带内传递）。
fn error_event(msg: &str) -> Event {
    Event::default()
        .event("error")
        .data(serde_json::json!({ "error": { "message": msg } }).to_string())
}

/// 流事件聚合器：把 Core 事件流还原成「最终响应消息」快照。
///
/// trace 的 `upstream_response` / `client_response` 在流式下用它落盘：
/// 体积上界 = 最终消息体（不含逐 chunk 时序与协议帧开销）。
#[derive(Default)]
struct StreamAssembler {
    id: String,
    model: String,
    /// 按 Core 块索引聚合的内容块；None = 该索引只见增量未见实体。
    blocks: Vec<Option<moonbridge_core::ContentBlock>>,
    /// ToolUse 的 partial_json 逐块累积，finish 时一次性 parse。
    tool_args: std::collections::HashMap<usize, String>,
    stop_reason: Option<moonbridge_core::StopReason>,
    usage: Usage,
}

impl StreamAssembler {
    /// 取 `index` 处槽位，按需扩容。
    fn slot(&mut self, index: usize) -> &mut Option<moonbridge_core::ContentBlock> {
        if self.blocks.len() <= index {
            self.blocks.resize(index + 1, None);
        }
        &mut self.blocks[index]
    }

    /// 取 `index` 处内容块，缺失时建占位块（chat 系上游正文是裸 delta，无 BlockStart）。
    fn block_or(
        &mut self,
        index: usize,
        make: impl FnOnce() -> moonbridge_core::ContentBlock,
    ) -> &mut moonbridge_core::ContentBlock {
        let slot = self.slot(index);
        if slot.is_none() {
            *slot = Some(make());
        }
        slot.as_mut().expect("slot just filled")
    }

    fn reasoning_block() -> moonbridge_core::ContentBlock {
        moonbridge_core::ContentBlock::Reasoning {
            text: String::new(),
            signature: None,
            redacted: false,
        }
    }

    fn feed(&mut self, ev: &CoreStreamEvent) {
        use moonbridge_core::ContentBlock as B;
        match ev {
            CoreStreamEvent::MessageStart { id, model } => {
                self.id.clone_from(id);
                self.model.clone_from(model);
            }
            CoreStreamEvent::BlockStart { index, block } => {
                *self.slot(*index) = Some(block.clone());
            }
            CoreStreamEvent::BlockDelta { index, delta } => {
                let i = *index;
                match delta {
                    StreamDelta::Text { text } => {
                        if let B::Text { text: t } = self.block_or(i, || B::text("")) {
                            t.push_str(text);
                        }
                    }
                    StreamDelta::Reasoning { text } => {
                        if let B::Reasoning { text: t, .. } =
                            self.block_or(i, Self::reasoning_block)
                        {
                            t.push_str(text);
                        }
                    }
                    StreamDelta::ReasoningSignature { signature } => {
                        if let B::Reasoning { signature: s, .. } =
                            self.block_or(i, Self::reasoning_block)
                        {
                            *s = Some(signature.clone());
                        }
                    }
                    StreamDelta::ToolInput { partial_json } => {
                        self.tool_args.entry(i).or_default().push_str(partial_json);
                        self.block_or(i, || B::ToolUse {
                            id: String::new(),
                            name: String::new(),
                            namespace: None,
                            input: Value::Null,
                            signature: None,
                        });
                    }
                }
            }
            CoreStreamEvent::BlockStop { index, block } => {
                // 收尾事件携带的完整块（reasoning 的 signature 还原等）优先于累积值
                if let Some(b) = block {
                    *self.slot(*index) = Some(b.clone());
                }
            }
            CoreStreamEvent::MessageDelta { stop_reason, usage } => {
                if let Some(sr) = stop_reason {
                    self.stop_reason = Some(*sr);
                }
                if let Some(u) = usage {
                    let a = &mut self.usage;
                    a.input_tokens = a.input_tokens.max(u.input_tokens);
                    a.output_tokens = a.output_tokens.max(u.output_tokens);
                    a.cache_read_tokens = a.cache_read_tokens.max(u.cache_read_tokens);
                    a.cache_write_tokens = a.cache_write_tokens.max(u.cache_write_tokens);
                    a.reasoning_tokens = a.reasoning_tokens.max(u.reasoning_tokens);
                }
            }
            CoreStreamEvent::MessageStop | CoreStreamEvent::Error { .. } => {}
        }
    }

    /// 聚合完成 → CoreResponse JSON；流上没有任何内容时返回 Null（不伪造空响应）。
    fn finish(mut self) -> Value {
        use moonbridge_core::ContentBlock as B;
        for (i, raw) in std::mem::take(&mut self.tool_args) {
            if let Some(Some(B::ToolUse { input, .. })) = self.blocks.get_mut(i) {
                *input = serde_json::from_str(&raw).unwrap_or(Value::String(raw));
            }
        }
        if self.blocks.is_empty() && self.id.is_empty() {
            return Value::Null;
        }
        serde_json::to_value(moonbridge_core::CoreResponse {
            id: self.id,
            model: self.model,
            content: self.blocks.into_iter().flatten().collect(),
            stop_reason: self.stop_reason,
            usage: self.usage,
            ext: moonbridge_core::Map::new(),
        })
        .unwrap_or(Value::Null)
    }
}

/// 单个 Core 事件的客户端方向处理管线：流事件钩子 → 内容块过滤 → 协议编码。
/// 返回编码出的客户端 chunk（被钩子/过滤器丢弃时为空）。
/// 错误以 `Err(String)` 返回，由调用方决定如何终结。
async fn process_stream_event(
    hooks: &Arc<dyn moonbridge_protocol::PluginHooks>,
    ctx: &ReqCtx,
    client_stream: &dyn moonbridge_protocol::ClientStreamAdapter,
    enc_state: &mut StreamEncodeState,
    dropped_blocks: &mut std::collections::HashSet<usize>,
    ev: &mut CoreStreamEvent,
) -> Result<Vec<RawChunk>, String> {
    // [CORE] 流事件钩子（返回 true 丢弃）
    match hooks.on_stream_event(ctx, ev).await {
        Ok(true) => return Ok(Vec::new()),
        Err(e) => tracing::warn!(error = %e, "on_stream_event 失败，放行"),
        Ok(false) => {}
    }
    // [CORE] 内容块过滤。只有 BlockStart 携带完整块，故只对它问
    // `filter_content`；被丢的块必须连带压制同 index 的 BlockDelta/BlockStop，
    // 否则客户端会收到一个没有头（也没有尾）的孤立增量。
    match ev {
        CoreStreamEvent::BlockStart { index, block } => {
            match hooks.filter_content(ctx, block).await {
                Ok(true) => {
                    dropped_blocks.insert(*index);
                    return Ok(Vec::new());
                }
                Err(e) => tracing::warn!(error = %e, "filter_content 失败，放行"),
                Ok(false) => {}
            }
        }
        CoreStreamEvent::BlockDelta { index, .. }
        | CoreStreamEvent::BlockStop { index, .. }
            if dropped_blocks.contains(index) =>
        {
            return Ok(Vec::new());
        }
        _ => {}
    }
    // [CORE] 编码为客户端 SSE chunk
    client_stream.encode(ctx, ev, enc_state).map_err(|e| e.to_string())
}

/// 流式请求的收尾审计：析构时落 usage 与 trace。
///
/// 三处历史缺陷一并修掉：
/// 1. 原先把 `usage::record` 写在 `while` 之后，客户端中途断开会 drop 掉整个
///    生成器 ⇒ 该请求既无用量也无 trace。放在 `Drop` 里则三种结束方式都覆盖。
/// 2. 原先无条件记 `status="ok"`，即使刚发完 error 事件就 break ⇒ 失败被记成成功。
///    现在按 `failed` / `completed` 记 `error` / `aborted` / `ok`。
/// 3. trace 的 status 与 error 原先从不回填（恒为构造时的 "ok"）。
struct StreamAudit {
    db: Arc<Database>,
    ctx: ReqCtx,
    upstream_model: String,
    trace_dir: Option<String>,
    /// 请求/响应体可选记录：关闭时落盘前抹掉报文体。
    record_bodies: bool,
    /// trace 保留条数（0 = 不清理）。
    trace_retention: usize,
    trace: Option<TraceRecord>,
    acc: Usage,
    ttft_ms: Option<i64>,
    start: Instant,
    /// 中途失败原因；有值即整条流记为 error。
    failed: Option<String>,
    /// 上游流是否自然读尽（未读尽且无失败 = 客户端断开等提前终止）。
    completed: bool,
    /// 上游侧聚合：decode 后、Core 钩子前喂入——记录上游实际发送的内容。
    up_asm: StreamAssembler,
    /// 客户端侧聚合：仅喂入实际编码下发的事件——记录客户端实际收到的内容
    /// （含插件过滤、会话水印注入的效果，可与上游侧对比插件行为）。
    down_asm: StreamAssembler,
}

impl StreamAudit {
    /// 收尾状态：`ok` / `error` / `aborted`，以及附带的错误消息。
    fn outcome(&self) -> (&'static str, Option<String>) {
        match (&self.failed, self.completed) {
            (Some(m), _) => ("error", Some(m.clone())),
            (None, false) => (
                "aborted",
                Some("流未读尽即结束（客户端断开或上游提前关闭）".to_string()),
            ),
            (None, true) => ("ok", None),
        }
    }
}

impl Drop for StreamAudit {
    fn drop(&mut self) {
        let (status, err) = self.outcome();
        usage::record(
            &self.db,
            &self.ctx,
            &self.upstream_model,
            &self.acc,
            status,
            err.as_deref(),
            self.start,
            self.ttft_ms,
        );
        if let Some(mut t) = self.trace.take() {
            t.usage = self.acc.into();
            t.latency_ms = self.start.elapsed().as_millis() as u64;
            t.ttft_ms = self.ttft_ms.map(|v| v as u64);
            t.status = status.to_string();
            t.error = err;
            // 流式 trace 的两个响应字段由事件聚合回填（非流式在 dispatch 已填）。
            if t.stream {
                t.upstream_response = std::mem::take(&mut self.up_asm).finish();
                t.client_response = std::mem::take(&mut self.down_asm).finish();
            }
            if !self.record_bodies {
                crate::trace::strip_bodies(&mut t);
            }
            crate::trace::write(self.trace_dir.as_deref(), &t, self.trace_retention);
        }
    }
}

/// 构建流式响应。`resp` 为已发送并收到头的上游响应。
//
// `audit.failed` / `audit.completed` 在本函数的生成器内写入、仅由
// `StreamAudit::Drop` 读取。rustc 的 `unused_assignments` 只做字段级数据流分析、
// 不把 Drop glue 算作一次读取，故在此误报「assigned value is never read」。
// `e2e_stream_responses_to_anthropic` 断言正常读尽的流落库为 status="ok"
// （而非 Drop 默认的 "aborted"），实证这些写入确实到达了 Drop。
#[allow(unused_assignments)]
pub fn build_stream_response(
    state: Arc<AppState>,
    ctx: ReqCtx,
    resp: reqwest::Response,
    upstream_model: String,
    start: Instant,
    trace: TraceRecord,
    session_tag: Option<String>,
) -> Response {
    let hooks = state.hooks.clone();
    let upstream_protocol = ctx.upstream_protocol.unwrap_or(Protocol::Anthropic);
    let provider = ctx.provider_key.clone();
    let provider_stream = state.registry.provider_stream(upstream_protocol).cloned();
    let client_stream = state.registry.client_stream(ctx.client_protocol).cloned();

    let mut audit = StreamAudit {
        db: state.db.clone(),
        ctx: ctx.clone(),
        upstream_model,
        trace_dir: state.config.trace_dir.clone(),
        record_bodies: state.config.trace_record_bodies,
        trace_retention: state.config.trace_retention,
        trace: Some(trace),
        acc: Usage::default(),
        ttft_ms: None,
        start,
        failed: None,
        completed: false,
        up_asm: StreamAssembler::default(),
        down_asm: StreamAssembler::default(),
    };

    let body = async_stream::stream! {
        let (Some(provider_stream), Some(client_stream)) = (provider_stream, client_stream) else {
            audit.failed = Some("缺少流式 Adapter".to_string());
            yield Ok::<Event, Infallible>(error_event("缺少流式 Adapter"));
            return;
        };
        let mut chunks = Box::pin(upstream::response_to_chunks(resp, upstream_protocol, provider));
        // `filter_content` 丢弃的块序号：其后续增量一律压制（见下方 [CORE] 内容块过滤）。
        let mut dropped_blocks: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        // 会话水印状态：整轮出现过 tool_use 就不打标（用户要求只标非工具调用输出）；
        // 必须至少有一个文本块，否则这轮没有「输出文本」可承载 marker。
        let mut saw_tool = false;
        let mut saw_text = false;
        let mut next_index = 0usize;
        let mut tagged = false;
        // encode 每流状态（anthropic 入口的推理块惰性开块依赖它）
        let mut enc_state = StreamEncodeState::default();
        // decode 每流状态（Gemini 上游的块索引跨 chunk 分配依赖它）
        let mut dec_state = StreamDecodeState::default();
        // 终结状态：上游协议（Chat 的 [DONE]、Gemini 的 finishReason）可能
        // 不发任何终帧就结束——客户端协议状态机需要一个 MessageStop 兜底。
        let mut saw_stop = false;
        // 是否已见过「真正的收尾 delta」（stop_reason 非空）。usage-only 的
        // MessageDelta 不算——那种帧之后客户端仍等不到 finish_reason。
        let mut saw_finish = false;
        let mut saw_start = false;
        // 上游带内错误帧（协议级 error event，非 decode 失败）：流被上游宣告
        // 失败——兜底收尾只补 MessageStop，不再伪造 completed 组装帧。
        let mut saw_err = false;

        // 'stream 标签：解码/编码任一环节出错都要**真正终止**整条流。
        // 编码错误分支原先的 `break` 落在内层 `for ev in events` 里，只放弃了
        // 当前 chunk 的剩余事件，外层 while 继续消费上游并反复刷 error 事件。
        'stream: while let Some(item) = chunks.next().await {
            let mut chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    let msg = e.to_string();
                    audit.failed = Some(msg.clone());
                    // 带内错误走客户端协议的 Error 编码（而非裸 error 帧），
                    // 并补终结帧让客户端状态机正常收尾。
                    for cc in client_stream
                        .encode(&ctx, &CoreStreamEvent::Error { message: msg.clone() }, &mut enc_state)
                        .unwrap_or_default()
                    {
                        yield Ok(chunk_to_event(&cc));
                    }
                    if !saw_stop {
                        for cc in client_stream
                            .encode(&ctx, &CoreStreamEvent::MessageStop, &mut enc_state)
                            .unwrap_or_default()
                        {
                            yield Ok(chunk_to_event(&cc));
                        }
                    }
                    break 'stream;
                }
            };
            if audit.ttft_ms.is_none() {
                audit.ttft_ms = Some(start.elapsed().as_millis() as i64);
            }

            // [RAW] 上游 chunk 钩子（可改写/丢弃）
            match hooks.on_upstream_chunk_raw(&ctx, &mut chunk).await {
                Ok(ChunkVerdict::Drop) => continue,
                Err(e) => tracing::warn!(error = %e, "on_upstream_chunk_raw 失败，放行"),
                Ok(ChunkVerdict::Forward) => {}
            }

            // [CORE] 解码为 Core 流事件
            let mut events = match provider_stream.decode(&ctx, &mut dec_state, &chunk) {
                Ok(ev) => ev,
                Err(e) => {
                    let msg = e.to_string();
                    audit.failed = Some(msg.clone());
                    for cc in client_stream
                        .encode(&ctx, &CoreStreamEvent::Error { message: msg.clone() }, &mut enc_state)
                        .unwrap_or_default()
                    {
                        yield Ok(chunk_to_event(&cc));
                    }
                    if !saw_stop {
                        for cc in client_stream
                            .encode(&ctx, &CoreStreamEvent::MessageStop, &mut enc_state)
                            .unwrap_or_default()
                        {
                            yield Ok(chunk_to_event(&cc));
                        }
                    }
                    break 'stream;
                }
            };

            // 上游侧聚合：decode 后即喂（先于水印注入与 Core 钩子），
            // 记录上游实际发送的内容。
            for ev in &events {
                audit.up_asm.feed(ev);
            }

            // 先按本批事件更新水印判定状态，再决定注入 —— 顺序反了会漏判
            // 同批里 MessageStop 之前刚出现的 tool_use 块。
            // BlockDelta 也要算：chat 系上游从不为正文发 BlockStart（OpenAI Chat
            // 的 content / reasoning 是裸 delta），只看 BlockStart 会把这类流误判
            // 成「没有输出文本」⇒ 水印永不注入、客户端 transcript 里没有可带回的
            // marker，每轮都被判成新会话。delta 携带的 index 同样要挤占 next_index，
            // 否则 marker 块会与惰性承接正文的 0 号块撞车。
            for ev in &events {
                match ev {
                    CoreStreamEvent::BlockStart { index, block } => {
                        next_index = next_index.max(*index + 1);
                        match block {
                            moonbridge_core::ContentBlock::ToolUse { .. } => saw_tool = true,
                            moonbridge_core::ContentBlock::Text { .. } => saw_text = true,
                            _ => {}
                        }
                    }
                    CoreStreamEvent::BlockDelta { index, delta } => {
                        next_index = next_index.max(*index + 1);
                        match delta {
                            StreamDelta::Text { .. } => saw_text = true,
                            StreamDelta::ToolInput { .. } => saw_tool = true,
                            _ => {}
                        }
                    }
                    CoreStreamEvent::MessageDelta { stop_reason: Some(_), .. } => {
                        saw_finish = true;
                    }
                    CoreStreamEvent::MessageStop => saw_stop = true,
                    CoreStreamEvent::MessageStart { .. } => saw_start = true,
                    CoreStreamEvent::Error { .. } => saw_err = true,
                    _ => {}
                }
            }
            // [CORE] 会话水印：marker 必须插在「收尾组装事件」之前——
            // 第一个带 stop_reason 的 MessageDelta（chat/gemini/responses 的
            // response.completed 都由它触发组装 output），其次才是 MessageStop。
            // 只认 MessageStop 会让 marker 排在 completed 之后：客户端按
            // completed.output 存历史时（OpenCode 等），水印进不了 transcript。
            if let Some(tag) = &session_tag {
                if !tagged && !saw_tool && !saw_err && saw_text {
                    let pos = events
                        .iter()
                        .position(|e| matches!(e, CoreStreamEvent::MessageDelta { stop_reason: Some(_), .. }))
                        .or_else(|| {
                            events
                                .iter()
                                .position(|e| matches!(e, CoreStreamEvent::MessageStop))
                        });
                    if let Some(pos) = pos {
                        events.splice(pos..pos, crate::session::marker_blocks(tag, next_index));
                        tagged = true;
                    }
                }
            }

            for mut ev in events {
                if let CoreStreamEvent::MessageDelta { usage: Some(u), .. } = &ev {
                    usage::accumulate(&mut audit.acc, u);
                }
                let cchunks = match process_stream_event(
                    &hooks,
                    &ctx,
                    client_stream.as_ref(),
                    &mut enc_state,
                    &mut dropped_blocks,
                    &mut ev,
                )
                .await
                {
                    Ok(c) => c,
                    Err(msg) => {
                        audit.failed = Some(msg.clone());
                        for cc in client_stream
                            .encode(&ctx, &CoreStreamEvent::Error { message: msg }, &mut enc_state)
                            .unwrap_or_default()
                        {
                            yield Ok(chunk_to_event(&cc));
                        }
                        if !saw_stop {
                            for cc in client_stream
                                .encode(&ctx, &CoreStreamEvent::MessageStop, &mut enc_state)
                                .unwrap_or_default()
                            {
                                yield Ok(chunk_to_event(&cc));
                            }
                        }
                        break 'stream;
                    }
                };
                let mut sent = false;
                for mut cc in cchunks {
                    // [RAW] 客户端 chunk 钩子（可改写/丢弃）。
                    // 出错时与上游侧对称地记 warn 后放行——原先被 `if let Ok(..)` 静默吞掉。
                    match hooks.on_client_chunk_raw(&ctx, &mut cc).await {
                        Ok(ChunkVerdict::Drop) => continue,
                        Err(e) => tracing::warn!(error = %e, "on_client_chunk_raw 失败，放行"),
                        Ok(ChunkVerdict::Forward) => {}
                    }
                    yield Ok(chunk_to_event(&cc));
                    sent = true;
                }
                // 只有真正产出了客户端字节的事件才算「客户端收到的」。
                if sent {
                    audit.down_asm.feed(&ev);
                }
            }
        }
        // 上游流自然耗尽但未产 MessageStop（协议缺终帧或提前 EOF）：
        // 合成收尾——已开块补 BlockStop、缺 MessageDelta 补一帧、补 MessageStop，
        // 否则客户端的块/消息状态机永远收不了尾。带内错误流只补 MessageStop：
        // 错误帧已是协议终态，再发 completed/delta 反而自相矛盾。
        if !saw_stop && audit.failed.is_none() {
            let mut tail: Vec<CoreStreamEvent> = Vec::new();
            if saw_err {
                // 上游已发过带内错误帧：只补 MessageStop 让客户端状态机收尾，
                // 不再补 completed/delta——错误帧之后再说「完成」自相矛盾。
                tail.push(CoreStreamEvent::MessageStop);
            } else {
                // 无 MessageStart 的兜底上游（罕见）：先补一帧，客户端才有
                // 消息头可挂
                if !saw_start {
                    tail.push(CoreStreamEvent::MessageStart {
                        id: String::new(),
                        model: String::new(),
                    });
                }
                for idx in std::mem::take(&mut dec_state.open_blocks) {
                    tail.push(CoreStreamEvent::BlockStop {
                        index: idx,
                        block: dec_state.blocks.get(&idx).cloned(),
                    });
                }
                // 兜底帧同样是水印挂点：marker 要排在 MessageDelta 之前，
                // 否则 responses 入口的 response.completed 组装不到水印 item。
                if let Some(tag) = &session_tag {
                    if !tagged && !saw_tool && saw_text {
                        tail.extend(crate::session::marker_blocks(tag, next_index));
                        tagged = true;
                    }
                }
                if !saw_finish {
                    // 流已干净读尽（chunk 错误会走 failed 分支到不了这里）——
                    // 上游没报停因按 end_turn 收尾：responses 入口缺它就不发
                    // response.completed，chat 入口缺它就没有 finish_reason，
                    // 客户端会一直挂着等终帧。
                    tail.push(CoreStreamEvent::MessageDelta {
                        stop_reason: Some(moonbridge_core::StopReason::EndTurn),
                        usage: None,
                    });
                }
                tail.push(CoreStreamEvent::MessageStop);
            }
            for mut ev in tail {
                match process_stream_event(
                    &hooks,
                    &ctx,
                    client_stream.as_ref(),
                    &mut enc_state,
                    &mut dropped_blocks,
                    &mut ev,
                )
                .await
                {
                    Ok(cchunks) => {
                        let mut sent = false;
                        for mut cc in cchunks {
                            match hooks.on_client_chunk_raw(&ctx, &mut cc).await {
                                Ok(ChunkVerdict::Drop) => continue,
                                Err(e) => tracing::warn!(error = %e, "on_client_chunk_raw 失败，放行"),
                                Ok(ChunkVerdict::Forward) => {}
                            }
                            yield Ok(chunk_to_event(&cc));
                            sent = true;
                        }
                        if sent {
                            audit.down_asm.feed(&ev);
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "收尾事件编码失败"),
                }
            }
        }
        if audit.failed.is_none() {
            audit.completed = true;
        }
        // usage / trace 由 `audit` 析构时统一落笔
    };

    Sse::new(body).keep_alive(KeepAlive::default()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_protocol::ChunkStage;
    use serde_json::json;

    fn audit(failed: Option<&str>, completed: bool) -> StreamAudit {
        StreamAudit {
            db: Arc::new(Database::open_in_memory().unwrap()),
            ctx: ReqCtx::new("r1", Protocol::OpenAiResponse),
            upstream_model: "m-up".into(),
            trace_dir: None,
            record_bodies: true,
            trace_retention: 0,
            trace: None,
            acc: Usage::default(),
            ttft_ms: None,
            start: Instant::now(),
            failed: failed.map(str::to_string),
            completed,
            up_asm: StreamAssembler::default(),
            down_asm: StreamAssembler::default(),
        }
    }

    /// 收尾状态机：三种流结束方式必须落到三个不同状态。
    /// 历史缺陷是无条件记 "ok"，中途失败被记成成功。
    #[test]
    fn outcome_distinguishes_ok_error_and_aborted() {
        assert!(matches!(audit(None, true).outcome(), ("ok", None)));

        let (s, e) = audit(Some("上游炸了"), false).outcome();
        assert_eq!(s, "error");
        assert_eq!(e.as_deref(), Some("上游炸了"));

        // 失败优先于「已读尽」：即使提前 break 也记 error
        let (s2, _) = audit(Some("编码失败"), true).outcome();
        assert_eq!(s2, "error");

        // 未读尽且无失败 = 客户端断开/上游提前关闭
        let (s3, e3) = audit(None, false).outcome();
        assert_eq!(s3, "aborted");
        assert!(e3.is_some(), "aborted 需带说明");
    }

    /// Drop 必须真正落一行 usage——客户端断开时生成器被 drop，
    /// 旧实现把落库写在循环之后，那种情况下什么都不记。
    #[test]
    fn drop_records_usage_row() {
        let a = audit(None, false);
        let db = a.db.clone();
        assert_eq!(db.usage_summary().unwrap().requests, 0);
        drop(a);
        let sum = db.usage_summary().unwrap();
        assert_eq!(sum.requests, 1, "流被丢弃时也应留下用量记录");
        let rows = db
            .query_usage(&moonbridge_store::UsageQuery {
                limit: 5,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows[0].status.as_deref(), Some("aborted"));
    }

    /// 聚合器把事件流还原成 CoreResponse：裸 delta 建占位块、tool 参数
    /// 拼串后 parse、usage 取逐帧最大值。
    #[test]
    fn assembler_merges_deltas_into_response() {
        use moonbridge_core::StopReason;
        let mut a = StreamAssembler::default();
        a.feed(&CoreStreamEvent::MessageStart {
            id: "m1".into(),
            model: "gpt-x".into(),
        });
        // chat 系正文是裸 delta：无 BlockStart 也要聚出 Text 块
        a.feed(&CoreStreamEvent::BlockDelta {
            index: 0,
            delta: StreamDelta::Text { text: "Hel".into() },
        });
        a.feed(&CoreStreamEvent::BlockDelta {
            index: 0,
            delta: StreamDelta::Text { text: "lo".into() },
        });
        a.feed(&CoreStreamEvent::BlockStart {
            index: 1,
            block: moonbridge_core::ContentBlock::ToolUse {
                id: "c1".into(),
                name: "f".into(),
                namespace: None,
                input: Value::Null,
                signature: None,
            },
        });
        a.feed(&CoreStreamEvent::BlockDelta {
            index: 1,
            delta: StreamDelta::ToolInput {
                partial_json: "{\"a\":".into(),
            },
        });
        a.feed(&CoreStreamEvent::BlockDelta {
            index: 1,
            delta: StreamDelta::ToolInput {
                partial_json: "1}".into(),
            },
        });
        a.feed(&CoreStreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(Usage {
                input_tokens: 5,
                output_tokens: 3,
                ..Usage::default()
            }),
        });
        a.feed(&CoreStreamEvent::MessageStop);

        let v = a.finish();
        assert_eq!(v["id"], "m1");
        assert_eq!(v["stop_reason"], "tool_use");
        assert_eq!(v["content"][0]["text"], "Hello");
        assert_eq!(v["content"][1]["input"], json!({"a": 1}));
        assert_eq!(v["usage"]["input_tokens"], 5);
    }

    /// BlockStop 携带的完整块优先于累积值；空流不伪造响应。
    #[test]
    fn assembler_block_stop_wins_and_empty_is_null() {
        let mut a = StreamAssembler::default();
        a.feed(&CoreStreamEvent::BlockDelta {
            index: 0,
            delta: StreamDelta::Reasoning { text: "思".into() },
        });
        a.feed(&CoreStreamEvent::BlockStop {
            index: 0,
            block: Some(moonbridge_core::ContentBlock::Reasoning {
                text: "思考".into(),
                signature: Some("sig".into()),
                redacted: false,
            }),
        });
        let v = a.finish();
        assert_eq!(v["content"][0]["text"], "思考");
        assert_eq!(v["content"][0]["signature"], "sig");

        assert!(StreamAssembler::default().finish().is_null());
    }

    #[test]
    fn chunk_data_text_covers_every_body_kind() {
        let mk = |data: RawBody, raw: &str| RawChunk {
            stage: ChunkStage::ClientChunk,
            protocol: Protocol::OpenAiChat,
            provider: None,
            event: None,
            data,
            raw: raw.to_string(),
        };
        assert_eq!(
            chunk_data_text(&mk(RawBody::json(json!({"a": 1})), "")),
            r#"{"a":1}"#
        );
        assert_eq!(
            chunk_data_text(&mk(RawBody::text("[DONE]"), "")),
            "[DONE]",
            "Text 原样透传"
        );
        assert_eq!(
            chunk_data_text(&mk(RawBody::Empty, "fallback")),
            "fallback",
            "Empty 回落到 raw 文本"
        );
        // 二进制不得 panic
        assert!(!chunk_data_text(&mk(RawBody::Binary { data: vec![0, 255, b'a'] }, "")).is_empty());
    }
}
