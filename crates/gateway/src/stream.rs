//! 流式请求编排：把上游 SSE 经「报文层 chunk 钩子 → 解码 → Core 层事件钩子 →
//! 编码 → 报文层 chunk 钩子」链路转换为客户端协议的 SSE 输出流。
//!
//! 输出采用 axum 原生 [`Sse`]，流项为 `Result<Event, Infallible>`。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use moonbridge_core::{CoreStreamEvent, Protocol, Usage};
use moonbridge_protocol::{ChunkVerdict, RawBody, RawChunk, ReqCtx};

use crate::state::AppState;
use crate::trace::TraceRecord;
use crate::upstream;
use crate::usage;

/// 把客户端方向的 RawChunk 转为 axum SSE [`Event`]。
fn chunk_to_event(cc: &RawChunk) -> Event {
    let mut ev = Event::default();
    if let Some(name) = &cc.event {
        ev = ev.event(name.clone());
    }
    let data = match &cc.data {
        RawBody::Json { value } => value.to_string(),
        RawBody::Text { text } => text.clone(),
        RawBody::Binary { data } => String::from_utf8_lossy(data).to_string(),
        RawBody::Empty => cc.raw.clone(),
    };
    ev.data(data)
}

/// 构造一个带内 error 事件（流已开始、HTTP 200 已发出，错误只能带内传递）。
fn error_event(msg: &str) -> Event {
    Event::default()
        .event("error")
        .data(serde_json::json!({ "error": { "message": msg } }).to_string())
}

/// 构建流式响应。`resp` 为已发送并收到头的上游响应。
pub fn build_stream_response(
    state: Arc<AppState>,
    ctx: ReqCtx,
    resp: reqwest::Response,
    upstream_model: String,
    start: Instant,
    trace: TraceRecord,
) -> Response {
    let hooks = state.hooks.clone();
    let db = state.db.clone();
    let trace_dir = state.config.trace_dir.clone();
    let upstream_protocol = ctx.upstream_protocol.unwrap_or(Protocol::Anthropic);
    let provider = ctx.provider_key.clone();
    let provider_stream = state.registry.provider_stream(upstream_protocol).cloned();
    let client_stream = state.registry.client_stream(ctx.client_protocol).cloned();

    let body = async_stream::stream! {
        let mut trace = trace;
        let (Some(provider_stream), Some(client_stream)) = (provider_stream, client_stream) else {
            yield Ok::<Event, Infallible>(error_event("缺少流式 Adapter"));
            return;
        };
        let mut chunks = Box::pin(upstream::response_to_chunks(resp, upstream_protocol, provider));
        let mut acc = Usage::default();
        // 首字延迟：第一个上游 chunk 到达时刻（相对请求起点）
        let mut ttft_ms: Option<i64> = None;

        while let Some(item) = chunks.next().await {
            let mut chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    yield Ok(error_event(&e.to_string()));
                    break;
                }
            };
            if ttft_ms.is_none() {
                ttft_ms = Some(start.elapsed().as_millis() as i64);
            }

            // [RAW] 上游 chunk 钩子（可改写/丢弃）
            match hooks.on_upstream_chunk_raw(&ctx, &mut chunk).await {
                Ok(ChunkVerdict::Drop) => continue,
                Err(e) => tracing::warn!(error = %e, "on_upstream_chunk_raw 失败，放行"),
                Ok(ChunkVerdict::Forward) => {}
            }

            // [CORE] 解码为 Core 流事件
            let events = match provider_stream.decode(&ctx, &chunk) {
                Ok(ev) => ev,
                Err(e) => {
                    yield Ok(error_event(&e.to_string()));
                    break;
                }
            };

            for mut ev in events {
                if let CoreStreamEvent::MessageDelta { usage: Some(u), .. } = &ev {
                    usage::accumulate(&mut acc, u);
                }
                // [CORE] 流事件钩子（返回 true 丢弃）
                match hooks.on_stream_event(&ctx, &mut ev).await {
                    Ok(true) => continue,
                    Err(e) => tracing::warn!(error = %e, "on_stream_event 失败，放行"),
                    Ok(false) => {}
                }
                // [CORE] 编码为客户端 SSE chunk
                let cchunks = match client_stream.encode(&ctx, &ev) {
                    Ok(c) => c,
                    Err(e) => {
                        yield Ok(error_event(&e.to_string()));
                        break;
                    }
                };
                for mut cc in cchunks {
                    // [RAW] 客户端 chunk 钩子（可改写/丢弃）
                    if let Ok(ChunkVerdict::Drop) = hooks.on_client_chunk_raw(&ctx, &mut cc).await {
                        continue;
                    }
                    yield Ok(chunk_to_event(&cc));
                }
            }
        }

        usage::record(&db, &ctx, &upstream_model, &acc, "ok", None, start, ttft_ms);
        trace.usage = acc;
        trace.latency_ms = start.elapsed().as_millis() as u64;
        crate::trace::write(trace_dir.as_deref(), &trace);
    };

    Sse::new(body).keep_alive(KeepAlive::default()).into_response()
}
