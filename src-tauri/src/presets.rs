//! 上游预设表：Providers 页「从预设添加」的静态数据源。
//!
//! 预设是**代码内嵌**的精选清单（随代码版本演进，与 opencodex registry 的做法一致），
//! 字段对齐其 registry 后裁剪到 MBN 口径：`protocol` 只取 MBN 四协议串。
//! `category = "account"` 的 OAuth 预设为后做占位（`enabled = false`），前端禁用展示、
//! 不进入表单。

use serde::Serialize;

/// 上游预设（DTO，camelCase 供前端）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    /// 预设 id（= 建议的 provider key）。
    pub id: &'static str,
    /// 展示名。
    pub label: &'static str,
    /// 分组：`api`（API Key 直连）/ `account`（OAuth 账户，后做）。
    pub category: &'static str,
    /// MBN 协议串（`openai-chat` 等）；account 占位为空串（不进入表单）。
    pub protocol: &'static str,
    /// 预填 Base URL。
    pub base_url: &'static str,
    /// 官方取 Key 页面。
    pub dashboard_url: Option<&'static str>,
    /// Key 可空（本地/免密服务）。
    pub key_optional: bool,
    /// 说明（展示在预设行与表单横幅）。
    pub note: Option<&'static str>,
    /// models.dev 目录中的 provider key：实时探测失败时的回退数据源，也用于元数据
    /// enrich。`None` 表示 models.dev 无对应目录（如本地运行时）。
    pub models_dev_id: Option<&'static str>,
    /// 可用状态：`false` = 后做占位（前端禁用展示）。
    pub enabled: bool,
}

/// 全部预设。顺序即前端展示顺序（组内）。
pub const PRESETS: &[ProviderPreset] = &[
    // ---- API Key 直连 ----
    ProviderPreset {
        id: "ollama",
        label: "Ollama",
        category: "api",
        protocol: "openai-chat",
        base_url: "http://localhost:11434/v1",
        dashboard_url: None,
        key_optional: true,
        note: Some("本地 Ollama 服务（OpenAI 兼容端点），通常无需 Key"),
        models_dev_id: None,
        enabled: true,
    },
    ProviderPreset {
        id: "vllm",
        label: "vLLM",
        category: "api",
        protocol: "openai-chat",
        base_url: "http://localhost:8000/v1",
        dashboard_url: None,
        key_optional: true,
        note: Some("本地 vLLM 服务（OpenAI 兼容端点），通常无需 Key"),
        models_dev_id: None,
        enabled: true,
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://api.deepseek.com",
        dashboard_url: Some("https://platform.deepseek.com/api_keys"),
        key_optional: false,
        note: None,
        models_dev_id: Some("deepseek"),
        enabled: true,
    },
    ProviderPreset {
        id: "kimi",
        label: "Kimi",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://api.kimi.com/coding/v1",
        dashboard_url: Some("https://platform.moonshot.cn/console/api-keys"),
        key_optional: false,
        note: Some("Kimi Code Plan 的 API Key 形态"),
        models_dev_id: Some("kimi-for-coding"),
        enabled: true,
    },
    ProviderPreset {
        id: "alibaba-token-plan",
        label: "Alibaba Token Plan",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        dashboard_url: Some("https://bailian.console.aliyun.com/cn-beijing?tab=plan"),
        key_optional: false,
        note: Some("Token Plan 个人版 · 北京"),
        models_dev_id: Some("alibaba-token-plan-cn"),
        enabled: true,
    },
    ProviderPreset {
        id: "commandcode",
        label: "Command Code",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://api.commandcode.ai/provider/v1",
        dashboard_url: Some("https://commandcode.ai/studio/"),
        key_optional: false,
        note: Some("Provider API（OpenAI 兼容）；需 Provider plan"),
        models_dev_id: None,
        enabled: true,
    },
    ProviderPreset {
        id: "opencode-go",
        label: "opencode go",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://opencode.ai/zen/go/v1",
        dashboard_url: Some("https://opencode.ai/auth"),
        key_optional: false,
        note: Some("GLM / DeepSeek / Kimi / Qwen / MiMo 聚合"),
        models_dev_id: Some("opencode-go"),
        enabled: true,
    },
    ProviderPreset {
        id: "zhipu-bigmodel-coding",
        label: "Zhipu BigModel Coding",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        dashboard_url: Some("https://bigmodel.cn/console/usercenter/apikeys"),
        key_optional: false,
        note: Some("智谱 BigModel Coding Plan 端点"),
        models_dev_id: Some("zhipuai-coding-plan"),
        enabled: true,
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        category: "api",
        protocol: "openai-chat",
        base_url: "https://openrouter.ai/api/v1",
        dashboard_url: Some("https://openrouter.ai/keys"),
        key_optional: false,
        note: Some("聚合商：模型量大，导入时注意勾选"),
        models_dev_id: Some("openrouter"),
        enabled: true,
    },
    // ---- OAuth 账户（后做，占位禁用）----
    ProviderPreset {
        id: "command-code-auth",
        label: "Command Code - Auth",
        category: "account",
        protocol: "",
        base_url: "",
        dashboard_url: None,
        key_optional: false,
        note: Some("Command Code 账户 OAuth 登录（后续版本支持）"),
        models_dev_id: None,
        enabled: false,
    },
    ProviderPreset {
        id: "kimi-oauth",
        label: "Kimi",
        category: "account",
        protocol: "",
        base_url: "",
        dashboard_url: None,
        key_optional: false,
        note: Some("Kimi 账户 OAuth 登录（后续版本支持）"),
        models_dev_id: None,
        enabled: false,
    },
    ProviderPreset {
        id: "devin",
        label: "Devin",
        category: "account",
        protocol: "",
        base_url: "",
        dashboard_url: None,
        key_optional: false,
        note: Some("Devin 账户（CLI 凭据导入 / Auth0 登录，后续版本支持）"),
        models_dev_id: None,
        enabled: false,
    },
];

/// 归一化 Base URL 用于比较：去尾随 `/`。
fn normalize_base_url(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// 解析 provider 对应的预设：先按 key 精确匹配，再按端点 Base URL 匹配（用户改名后
/// 仍能找回目录映射）。
pub fn find_preset(key: &str, base_urls: &[&str]) -> Option<&'static ProviderPreset> {
    if let Some(p) = PRESETS.iter().find(|p| p.id == key) {
        return Some(p);
    }
    base_urls
        .iter()
        .map(|u| normalize_base_url(u))
        .find_map(|u| {
            PRESETS
                .iter()
                .find(|p| p.enabled && !p.base_url.is_empty() && normalize_base_url(p.base_url) == u)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for p in PRESETS {
            assert!(seen.insert(p.id), "预设 id 重复: {}", p.id);
        }
    }

    #[test]
    fn api_presets_are_well_formed() {
        const PROTOCOLS: [&str; 4] = [
            "anthropic",
            "openai-response",
            "openai-chat",
            "google-genai",
        ];
        for p in PRESETS.iter().filter(|p| p.category == "api") {
            assert!(p.enabled, "API 预设应可用: {}", p.id);
            assert!(PROTOCOLS.contains(&p.protocol), "{} 协议非法: {}", p.id, p.protocol);
            assert!(p.base_url.starts_with("http"), "{} base_url 非法", p.id);
            if let Some(mid) = p.models_dev_id {
                assert!(!mid.is_empty(), "{} models_dev_id 为空串", p.id);
            }
        }
    }

    #[test]
    fn account_presets_are_disabled_placeholders() {
        let accounts: Vec<_> = PRESETS.iter().filter(|p| p.category == "account").collect();
        assert_eq!(accounts.len(), 3, "账户占位应为 Command Code-Auth / Kimi / Devin 三个");
        for p in accounts {
            assert!(!p.enabled, "账户预设应为禁用占位: {}", p.id);
        }
    }

    #[test]
    fn find_preset_by_key_then_base_url() {
        // key 命中优先
        assert_eq!(find_preset("deepseek", &[]).map(|p| p.id), Some("deepseek"));
        // key 改名后按 baseUrl 找回（尾随斜杠差异容忍）
        assert_eq!(
            find_preset("my-ds", &["https://api.deepseek.com/"]).map(|p| p.id),
            Some("deepseek")
        );
        // 无映射时返回 None（如自定义 relay）
        assert!(find_preset("custom", &["https://relay.example.com"]).is_none());
        // 禁用占位不参与 baseUrl 匹配（其 base_url 为空，本就不会误中）
        assert!(find_preset("x", &[""]).is_none());
    }
}
