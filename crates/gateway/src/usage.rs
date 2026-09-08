//! 用量统计与落库。

use std::sync::Arc;
use std::time::Instant;

use moonbridge_core::Usage;
use moonbridge_protocol::ReqCtx;
use moonbridge_store::{Database, UsageRecord};

/// 累积流式 usage：Anthropic 在 `message_start` 给出 input，在 `message_delta`
/// 给出最终 output，故各字段取最大值即最终值。
pub fn accumulate(acc: &mut Usage, u: &Usage) {
    acc.input_tokens = acc.input_tokens.max(u.input_tokens);
    acc.output_tokens = acc.output_tokens.max(u.output_tokens);
    acc.cache_read_tokens = acc.cache_read_tokens.max(u.cache_read_tokens);
    acc.cache_write_tokens = acc.cache_write_tokens.max(u.cache_write_tokens);
    acc.reasoning_tokens = acc.reasoning_tokens.max(u.reasoning_tokens);
}

/// 记录一次请求的用量（同步落库；失败仅告警，不影响主链路）。
///
/// `ttft_ms` 为首字延迟（流式请求才有值；非流式/错误传 None）。
/// TODO(pricing): cost 目前恒为 0，后续按 provider offers 的 pricing 计算。
#[allow(clippy::too_many_arguments)]
pub fn record(
    db: &Arc<Database>,
    ctx: &ReqCtx,
    upstream_model: &str,
    usage: &Usage,
    status: &str,
    error: Option<&str>,
    start: Instant,
    ttft_ms: Option<i64>,
) {
    let rec = UsageRecord {
        id: ctx.request_id.clone(),
        session_id: ctx.session_id.clone(),
        model: Some(ctx.model_alias.clone()),
        upstream_model: Some(upstream_model.to_string()),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cost: 0.0,
        status: Some(status.to_string()),
        error: error.map(|s| s.to_string()),
        latency_ms: start.elapsed().as_millis() as i64,
        ttft_ms,
        created_at: 0,
    };
    if let Err(e) = db.insert_usage(&rec) {
        tracing::warn!(error = %e, "usage 落库失败");
    }
}
