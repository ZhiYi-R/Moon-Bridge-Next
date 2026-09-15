//! 用量统计与落库。

use std::sync::Arc;
use std::time::Instant;

use moonbridge_core::Usage;
use moonbridge_protocol::ReqCtx;
use moonbridge_store::{Database, UsageRecord};
use serde_json::Value;

/// 累积流式 usage：Anthropic 在 `message_start` 给出 input，在 `message_delta`
/// 给出最终 output，故各字段取最大值即最终值。
pub fn accumulate(acc: &mut Usage, u: &Usage) {
    acc.input_tokens = acc.input_tokens.max(u.input_tokens);
    acc.output_tokens = acc.output_tokens.max(u.output_tokens);
    acc.cache_read_tokens = acc.cache_read_tokens.max(u.cache_read_tokens);
    acc.cache_write_tokens = acc.cache_write_tokens.max(u.cache_write_tokens);
    acc.reasoning_tokens = acc.reasoning_tokens.max(u.reasoning_tokens);
}

/// 长上下文分层价目：返回命中档位的覆盖价表（tier 键逐项覆盖基价）。
///
/// 支持 models.dev 两种形态：
/// - `tiers: [{…价键…, "tier": {"type": "context", "size": N}}]`（权威，可多档）
/// - `context_over_200k: {…价键…}`（旧式单档镜像，等价 size=200_000）
///
/// 触发口径与上游一致：`input_tokens`（Core 口径含缓存）> size——上游语义是
/// 「超过 N token 才加价」（`context_over_200k` 的命名同理）。多档命中取 size
/// 最大者；层内未列出的价键回退基价。
fn tier_overlay(
    p: &serde_json::Map<String, Value>,
    input_tokens: u64,
) -> Option<&serde_json::Map<String, Value>> {
    let mut best: Option<(u64, &serde_json::Map<String, Value>)> = p
        .get("context_over_200k")
        .and_then(Value::as_object)
        .filter(|_| input_tokens > 200_000)
        .map(|m| (200_000, m));
    if let Some(tiers) = p.get("tiers").and_then(Value::as_array) {
        for t in tiers {
            let Some(obj) = t.as_object() else { continue };
            let meta = obj.get("tier");
            if meta.and_then(|m| m.get("type")).and_then(Value::as_str) != Some("context") {
                continue;
            }
            let Some(size) = meta.and_then(|m| m.get("size")).and_then(Value::as_u64) else {
                continue;
            };
            if input_tokens > size && best.is_none_or(|(bs, _)| size > bs) {
                best = Some((size, obj));
            }
        }
    }
    best.map(|(_, m)| m)
}

/// 按 offer 定价计算一次请求的美元成本。
///
/// Core 口径不变量：`input_tokens` 为 prompt 总量（**含**缓存读写），
/// `output_tokens` 为输出总量（**含** reasoning）。pricing 键为 USD/1M tokens
/// （models.dev 口径）：`input` / `output` / `cache_read` / `cache_write` /
/// `reasoning`，另可含长上下文分层（见 [`tier_overlay`]）。
/// 缺项回退：cache_* 按 `input` 价、reasoning 按 `output` 价
/// 计费（上游确实产生了这些 token，按基础价计比按 0 计更接近真实账单）；
/// `input`/`output` 缺省即为 0。pricing 为 None 或全 0 时成本为 0——
/// 与「免费模型」和「未配置价目」同值，调用方不区分。
pub fn cost_of(pricing: Option<&Value>, u: &Usage) -> f64 {
    let Some(p) = pricing.and_then(Value::as_object) else {
        return 0.0;
    };
    let tier = tier_overlay(p, u64::from(u.input_tokens));
    let price = |key: &str| {
        tier.and_then(|t| t.get(key))
            .or_else(|| p.get(key))
            .and_then(Value::as_f64)
    };
    let p_in = price("input").unwrap_or(0.0);
    let p_out = price("output").unwrap_or(0.0);
    let p_cr = price("cache_read").unwrap_or(p_in);
    let p_cw = price("cache_write").unwrap_or(p_in);
    let p_reason = price("reasoning").unwrap_or(p_out);
    // 缓存与推理 token 是 input/output 总量的子集，先拆出按基础价计的部分
    let plain_in = u
        .input_tokens
        .saturating_sub(u.cache_read_tokens.saturating_add(u.cache_write_tokens));
    let plain_out = u.output_tokens.saturating_sub(u.reasoning_tokens);
    (plain_in as f64 * p_in
        + u.cache_read_tokens as f64 * p_cr
        + u.cache_write_tokens as f64 * p_cw
        + plain_out as f64 * p_out
        + u.reasoning_tokens as f64 * p_reason)
        / 1_000_000.0
}

/// 记录一次请求的用量（同步落库；失败仅告警，不影响主链路）。
///
/// `ttft_ms` 为首字延迟（流式请求才有值；非流式/错误传 None）。
/// 成本按 `(provider_key, upstream_model)` 命中的 offer 定价现算——存当时价
/// 而非事后重算，价目变动不会回改历史账单。
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
    // 定价检索失败只告警：成本是统计量，不该让价目缺失丢整条用量记录。
    let pricing = ctx
        .provider_key
        .as_deref()
        .and_then(|pk| match db.get_offer(pk, upstream_model) {
            Ok(offer) => offer.and_then(|o| o.pricing),
            Err(e) => {
                tracing::warn!(error = %e, provider = %pk, model = %upstream_model, "读取报价失败，cost 记 0");
                None
            }
        });
    let rec = UsageRecord {
        id: ctx.request_id.clone(),
        session_id: ctx.session_id.clone(),
        model: Some(ctx.model_alias.clone()),
        upstream_model: Some(upstream_model.to_string()),
        provider_key: ctx.provider_key.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cost: cost_of(pricing.as_ref(), usage),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage() -> Usage {
        Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 200,
            cache_write_tokens: 100,
            reasoning_tokens: 50,
        }
    }

    #[test]
    fn cost_splits_cache_and_reasoning_from_base_tokens() {
        let pricing = json!({
            "input": 3.0, "output": 15.0,
            "cache_read": 0.3, "cache_write": 3.75, "reasoning": 15.0
        });
        // input 1000 含 cache 300 → 基础输入 700；output 500 含 reasoning 50 → 基础输出 450
        let expect =
            (700.0 * 3.0 + 200.0 * 0.3 + 100.0 * 3.75 + 450.0 * 15.0 + 50.0 * 15.0) / 1_000_000.0;
        let got = cost_of(Some(&pricing), &usage());
        assert!((got - expect).abs() < 1e-12, "got {got}, expect {expect}");
    }

    #[test]
    fn missing_prices_fall_back_to_base_rates() {
        // 只有 input/output：cache_* 按 input 价、reasoning 按 output 价
        let pricing = json!({ "input": 2.0, "output": 10.0 });
        let u = usage();
        let expect = (1000.0 * 2.0 + 500.0 * 10.0) / 1_000_000.0;
        assert_eq!(cost_of(Some(&pricing), &u), expect);
    }

    #[test]
    fn no_pricing_is_zero_cost() {
        assert_eq!(cost_of(None, &usage()), 0.0);
        assert_eq!(cost_of(Some(&json!({})), &usage()), 0.0);
        // 全 0 价目（免费模型）同样是 0
        assert_eq!(
            cost_of(Some(&json!({"input": 0.0, "output": 0.0})), &usage()),
            0.0
        );
    }

    #[test]
    fn cache_exceeding_input_does_not_underflow() {
        let pricing = json!({ "input": 3.0, "output": 15.0, "cache_read": 0.3 });
        let u = Usage {
            input_tokens: 10,
            cache_read_tokens: 10,
            ..Usage::default()
        };
        assert!(cost_of(Some(&pricing), &u).is_finite());
    }

    #[test]
    fn long_context_tier_applies_per_key_overlay() {
        // claude-sonnet 形态：≤200K 基价 + >200K 整档加价（含 cache 分项覆盖）
        let pricing = json!({
            "input": 3.0, "output": 15.0, "cache_read": 0.3, "cache_write": 3.75,
            "tiers": [{
                "input": 6.0, "output": 22.5, "cache_read": 0.6, "cache_write": 7.5,
                "tier": { "type": "context", "size": 200_000 }
            }],
            "context_over_200k": { "input": 6.0, "output": 22.5, "cache_read": 0.6, "cache_write": 7.5 }
        });
        // 阈值以下：全基础价
        let under = Usage {
            input_tokens: 199_999,
            output_tokens: 1_000,
            ..Usage::default()
        };
        let expect = (199_999.0 * 3.0 + 1_000.0 * 15.0) / 1e6;
        assert_eq!(cost_of(Some(&pricing), &under), expect);
        // 阈值以上（严格大于）：所有分项都换挡位价
        let over = Usage {
            input_tokens: 200_001,
            output_tokens: 1_000,
            cache_read_tokens: 100_000,
            cache_write_tokens: 10_000,
            reasoning_tokens: 200,
        };
        let expect = ((200_001.0 - 110_000.0) * 6.0
            + 100_000.0 * 0.6
            + 10_000.0 * 7.5
            + 800.0 * 22.5
            + 200.0 * 22.5) // tier 无 reasoning 键 → 回退 tier 的 output
            / 1e6;
        let got = cost_of(Some(&pricing), &over);
        assert!((got - expect).abs() < 1e-9, "got {got}, expect {expect}");
    }

    #[test]
    fn tier_partial_keys_fall_back_to_base() {
        // tier 只覆盖 input/output：cache_* 用基价（Gemini 档常见形态）
        let pricing = json!({
            "input": 1.25, "output": 10.0, "cache_read": 0.31,
            "tiers": [{ "input": 2.5, "output": 15.0, "tier": { "type": "context", "size": 200_000 } }]
        });
        let u = Usage {
            input_tokens: 300_000,
            output_tokens: 1_000,
            cache_read_tokens: 50_000,
            ..Usage::default()
        };
        let expect = (250_000.0 * 2.5 + 50_000.0 * 0.31 + 1_000.0 * 15.0) / 1e6;
        let got = cost_of(Some(&pricing), &u);
        assert!((got - expect).abs() < 1e-12, "got {got}, expect {expect}");
    }

    #[test]
    fn multi_tier_picks_largest_crossed_threshold() {
        let pricing = json!({
            "input": 1.0, "output": 2.0,
            "tiers": [
                { "input": 2.0, "output": 4.0, "tier": { "type": "context", "size": 100_000 } },
                { "input": 3.0, "output": 6.0, "tier": { "type": "context", "size": 500_000 } }
            ]
        });
        let u = Usage {
            input_tokens: 600_000,
            output_tokens: 1_000,
            ..Usage::default()
        };
        assert_eq!(
            cost_of(Some(&pricing), &u),
            (600_000.0 * 3.0 + 1_000.0 * 6.0) / 1e6
        );
        let u = Usage {
            input_tokens: 150_000,
            output_tokens: 1_000,
            ..Usage::default()
        };
        assert_eq!(
            cost_of(Some(&pricing), &u),
            (150_000.0 * 2.0 + 1_000.0 * 4.0) / 1e6
        );
    }

    #[test]
    fn legacy_context_over_200k_alone_works() {
        let pricing = json!({
            "input": 3.0, "output": 15.0,
            "context_over_200k": { "input": 6.0, "output": 22.5 }
        });
        let u = Usage {
            input_tokens: 250_000,
            output_tokens: 1_000,
            ..Usage::default()
        };
        assert_eq!(
            cost_of(Some(&pricing), &u),
            (250_000.0 * 6.0 + 1_000.0 * 22.5) / 1e6
        );
    }

    /// 计价数据流：offer 定价 → record() 落库的 cost。
    #[test]
    fn record_persists_computed_cost_and_provider() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.upsert_offer(&moonbridge_store::Offer {
            provider_key: "mock".into(),
            model_slug: "claude-x".into(),
            pricing: Some(json!({ "input": 3.0, "output": 15.0 })),
            endpoint_protocol: None,
        })
        .unwrap();
        let ctx = ReqCtx::new("r1", moonbridge_core::Protocol::Anthropic)
            .with_route(moonbridge_core::Protocol::Anthropic, "mock");
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Usage::default()
        };
        record(&db, &ctx, "claude-x", &u, "ok", None, Instant::now(), None);

        let rows = db
            .query_usage(&moonbridge_store::UsageQuery {
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows[0].provider_key.as_deref(), Some("mock"));
        assert_eq!(rows[0].cost, 18.0, "1M in × $3 + 1M out × $15");
    }
}
