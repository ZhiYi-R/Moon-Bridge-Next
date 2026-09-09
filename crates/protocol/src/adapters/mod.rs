//! 内置协议 Adapter（四象限：入口 Client / 上游 Provider × 非流式 / 流式）。
//!
//! - [`openai_responses`]：OpenAI Responses（`/v1/responses`，入口 + 上游）。
//! - [`anthropic`]：Anthropic Messages（`/v1/messages`，入口 + 上游）。
//! - [`openai_chat`]：OpenAI Chat Completions（`/v1/chat/completions`，入口 + 上游）。
//! - [`google_genai`]：Google Generative AI / Gemini（入口 + 上游）。
//!
//! 本模块另放各家上游共用的 reasoning 映射辅助（见下）。

pub mod anthropic;
pub mod google_genai;
pub mod openai_chat;
pub mod openai_responses;

use moonbridge_core::CoreRequest;

/// Anthropic 扩展思考的最小预算（低于此值上游会拒绝请求）。
const MIN_THINKING_BUDGET: u32 = 1024;

/// effort → token 预算档位。
///
/// OpenAI 系用字符串枚举表达推理强度，而 Anthropic (`thinking.budget_tokens`) 与
/// Gemini (`generationConfig.thinkingConfig.thinkingBudget`) 用**token 预算**表达，
/// 故需要一张共用映射表把同一意图翻译成各家形态，避免四个 adapter 各写一份而漂移。
const THINKING_BUDGETS: &[(&str, u32)] = &[
    ("minimal", 1024),
    ("low", 2048),
    ("medium", 8192),
    ("high", 16384),
    ("xhigh", 24576),
    ("max", 32768),
];

/// 取本次请求有效的 reasoning effort；未设置或全空白时返回 `None`。
pub(crate) fn reasoning_effort(req: &CoreRequest) -> Option<&str> {
    req.reasoning
        .as_ref()
        .and_then(|r| r.effort.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// effort 对应的 token 预算；未知 effort 回落到 `medium` 档（保守：不因此关掉思考）。
fn thinking_budget(effort: &str) -> u32 {
    THINKING_BUDGETS
        .iter()
        .find(|(k, _)| effort.eq_ignore_ascii_case(k))
        .map(|(_, v)| *v)
        .unwrap_or(8192)
}

/// 换算并夹紧到严格小于 `max_tokens` 的思考预算；夹不到合法值时返回 `None`
/// （宁可不发，也不构造出必被上游拒绝的请求）。
pub(crate) fn clamped_thinking_budget(effort: &str, max_tokens: u32) -> Option<u32> {
    let budget = thinking_budget(effort).min(max_tokens.saturating_sub(1));
    (budget >= MIN_THINKING_BUDGET).then_some(budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moonbridge_core::Reasoning;

    fn req_with(effort: Option<&str>) -> CoreRequest {
        let mut r = CoreRequest::new("m");
        r.reasoning = effort.map(|e| Reasoning {
            effort: Some(e.to_string()),
            summary: None,
        });
        r
    }

    #[test]
    fn effort_is_trimmed_and_blanks_are_none() {
        assert_eq!(reasoning_effort(&req_with(Some("  high "))), Some("high"));
        assert_eq!(reasoning_effort(&req_with(Some("   "))), None);
        assert_eq!(reasoning_effort(&req_with(None)), None);
        assert_eq!(reasoning_effort(&CoreRequest::new("m")), None);
    }

    #[test]
    fn budget_table_and_clamp() {
        assert_eq!(thinking_budget("low"), 2048);
        assert_eq!(thinking_budget("HIGH"), 16384, "大小写不敏感");
        assert_eq!(thinking_budget("bogus"), 8192, "未知档回落 medium");
        // 正常：预算小于 max_tokens
        assert_eq!(clamped_thinking_budget("low", 4096), Some(2048));
        // 夹紧到 max_tokens - 1
        assert_eq!(clamped_thinking_budget("max", 4000), Some(3999));
        // 夹不到合法值则不发思考配置
        assert_eq!(clamped_thinking_budget("high", 1024), None);
        assert_eq!(clamped_thinking_budget("high", 0), None);
    }
}
