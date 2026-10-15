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

/// token 预算 → effort 档位（`thinking_budget` 的逆映射，取不超过预算的最大档）。
/// 供以预算表达思考强度的入口（anthropic/gemini）解析客户端配置。
pub(crate) fn effort_from_budget(budget: u32) -> &'static str {
    THINKING_BUDGETS
        .iter()
        .rev()
        .find(|(_, v)| budget >= *v)
        .map(|(k, _)| *k)
        .unwrap_or("minimal")
}

/// effort 对应的 token 预算；未知 effort 回落到 `medium` 档（保守：不因此关掉思考）。
fn thinking_budget(effort: &str) -> u32 {
    THINKING_BUDGETS
        .iter()
        .find(|(k, _)| effort.eq_ignore_ascii_case(k))
        .map(|(_, v)| *v)
        .unwrap_or(8192)
}

/// 换算并收紧到严格小于 `max_tokens` 的思考预算；收不出合法值时返回 `None`
/// （宁可不发，也不构造出必被上游拒绝的请求）。
pub(crate) fn clamped_thinking_budget(effort: &str, max_tokens: u32) -> Option<u32> {
    let budget = thinking_budget(effort).min(max_tokens.saturating_sub(1));
    (budget >= MIN_THINKING_BUDGET).then_some(budget)
}

// ============================================================================
// 推理凭据的来源标记
// ============================================================================
//
// `ContentBlock::Reasoning.signature` 与 `ToolUse.signature` 存放各协议的不透明回传凭据
// （Anthropic thinking signature / OpenAI encrypted_content / Gemini thoughtSignature）。
// 跨协议转发时，入站凭据打上来源前缀，出站时仅还原同源凭据，异源凭据按无凭据降级或原样透传给客户端。

pub(crate) const SIG_ANTHROPIC: &str = "ant:";
pub(crate) const SIG_OPENAI: &str = "oai:";
pub(crate) const SIG_GEMINI: &str = "gem:";
/// OpenAI Chat 上游无独立凭据字段，思考模式要求在历史消息中回传 `reasoning_content` 原文。
/// 解码时为无签名的推理文本附加 `chat:` 前缀作为自凭据，使客户端（如 Responses）可保存并回传，
/// 同时避免该文本被其他协议上游误认为原生签名。
pub(crate) const SIG_CHAT: &str = "chat:";

/// 入站时为凭据附加来源前缀（空串返回 `None`，已有已知前缀则保持幂等）。
pub(crate) fn tag_signature(prefix: &str, sig: Option<String>) -> Option<String> {
    sig.filter(|s| !s.is_empty()).map(|s| {
        if [SIG_ANTHROPIC, SIG_OPENAI, SIG_GEMINI, SIG_CHAT]
            .iter()
            .any(|p| s.starts_with(p))
        {
            s
        } else {
            format!("{prefix}{s}")
        }
    })
}

/// 出站：取回属于 `prefix` 的凭据原文。异源凭据返回 `None`（按无凭据降级）；
/// 无前缀凭据视为未知来源透传。
pub(crate) fn untag_signature<'a>(prefix: &str, sig: Option<&'a str>) -> Option<&'a str> {
    let s = sig?;
    if s.is_empty() {
        return None;
    }
    for p in [SIG_ANTHROPIC, SIG_OPENAI, SIG_GEMINI, SIG_CHAT] {
        if let Some(rest) = s.strip_prefix(p) {
            return (p == prefix).then_some(rest);
        }
    }
    Some(s)
}

/// 客户端方向凭据透传：本家凭据（`prefix`）还原原文；异源凭据**带标记**
/// 原样下发——客户端把它当不透明串存入历史，回传入站时幂等打标记，
/// 最终回到归属协议上游才由 `untag_signature` 解除标记。丢弃异源凭据会让
/// 「Gemini 上游 → 非 Gemini 客户端 → 历史回传 → Gemini 上游」断链。
pub(crate) fn emit_signature<'a>(prefix: &str, sig: &'a str) -> &'a str {
    untag_signature(prefix, Some(sig)).unwrap_or(sig)
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
        // 收紧到 max_tokens - 1
        assert_eq!(clamped_thinking_budget("max", 4000), Some(3999));
        // 收不出合法值则不发思考配置
        assert_eq!(clamped_thinking_budget("high", 1024), None);
        assert_eq!(clamped_thinking_budget("high", 0), None);
    }
}
