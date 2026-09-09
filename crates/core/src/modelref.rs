//! 模型引用解析：处理 `model(provider)` 限定名格式。
//!
//! 客户端可用 `deepseek-v4-pro(deepseek)` 直接指定 provider，也可只用
//! `moonbridge` 这样的别名（由路由表解析）。本模块负责前者的解析与回写。

use serde::{Deserialize, Serialize};

/// 解析后的模型引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    /// 模型名（不含 provider 后缀）。
    pub model: String,
    /// 显式指定的 provider；`None` 表示需经路由表解析。
    pub provider: Option<String>,
}

impl ModelRef {
    /// 解析 `model(provider)` 或裸 `model`。
    ///
    /// - `"deepseek-v4-pro(deepseek)"` → model=`deepseek-v4-pro`, provider=`Some("deepseek")`
    /// - `"moonbridge"` → model=`moonbridge`, provider=`None`
    ///
    /// 括号不闭合等异常输入按裸模型名处理，保证健壮性。
    pub fn parse(input: &str) -> Self {
        let input = input.trim();
        if let Some(open) = input.find('(') {
            if input.ends_with(')') && open + 1 < input.len() - 1 {
                let model = input[..open].trim().to_string();
                let provider = input[open + 1..input.len() - 1].trim().to_string();
                if !model.is_empty() && !provider.is_empty() {
                    return ModelRef {
                        model,
                        provider: Some(provider),
                    };
                }
            }
        }
        ModelRef {
            model: input.to_string(),
            provider: None,
        }
    }

    /// 构造一个带 provider 的引用。
    pub fn with_provider(model: impl Into<String>, provider: impl Into<String>) -> Self {
        ModelRef {
            model: model.into(),
            provider: Some(provider.into()),
        }
    }

    /// 构造一个裸模型引用。
    pub fn bare(model: impl Into<String>) -> Self {
        ModelRef {
            model: model.into(),
            provider: None,
        }
    }

    /// 规范化回 `model(provider)` 字符串（无 provider 时仅返回 model）。
    pub fn to_qualified(&self) -> String {
        match &self.provider {
            Some(p) => format!("{}({})", self.model, p),
            None => self.model.clone(),
        }
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_qualified())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_qualified() {
        let r = ModelRef::parse("deepseek-v4-pro(deepseek)");
        assert_eq!(r.model, "deepseek-v4-pro");
        assert_eq!(r.provider.as_deref(), Some("deepseek"));
        assert_eq!(r.to_qualified(), "deepseek-v4-pro(deepseek)");
    }

    #[test]
    fn parse_bare() {
        let r = ModelRef::parse("moonbridge");
        assert_eq!(r.model, "moonbridge");
        assert_eq!(r.provider, None);
        assert_eq!(r.to_qualified(), "moonbridge");
    }

    #[test]
    fn parse_malformed_falls_back() {
        // 括号不闭合按裸名处理
        assert_eq!(ModelRef::parse("model(provider").provider, None);
        // 空 provider 按裸名处理
        assert_eq!(ModelRef::parse("model()").model, "model()");
    }
}
