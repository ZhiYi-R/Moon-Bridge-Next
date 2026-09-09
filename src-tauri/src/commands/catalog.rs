//! 模型目录：从 [models.dev](https://models.dev) 拉取公开模型库，供用户搜索并批量
//! 导入到本地 `models` 表（含定价）。
//!
//! 数据源为 `https://models.dev/api.json`（约 4.5MB）：顶层是 provider 字典，每个
//! provider 含 `name` / `api` / `models`，每个模型含 `id / name / limit / modalities /
//! reasoning_options / cost` 等。本模块在**后端**拉取并解析，只把精简后的扁平候选列表
//! 交给前端（避免大 JSON 过 webview，也把字段映射收在一处）。
//!
//! 导入内容：模型定义（slug/名称/上下文/模态/推理档）+ 定价（`cost` → `pricing`），
//! 一并写入 `models` 表。刻意**不触碰** provider 端点配置（端点在 Providers 页单独管理）。

use std::sync::Arc;
use std::time::Duration;

use moonbridge_store::{ModelDef, Offer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const FETCH_TIMEOUT_SECS: u64 = 30;

/// 一个可导入的候选模型（后端解析 models.dev 后的扁平视图）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    /// models.dev 的 provider key（如 `opencode`）。
    pub provider_key: String,
    /// provider 展示名（如 `OpenCode Zen`）。
    pub provider_name: String,
    /// 模型 id（= 导入后的 `slug`）。
    pub id: String,
    /// 模型展示名。
    pub name: Option<String>,
    /// 上下文窗口（token）。
    pub context_window: Option<i64>,
    /// 输入模态（text/image/pdf/video/audio）。
    pub modalities: Vec<String>,
    /// 推理档位（effort 值扁平化）。
    pub reasoning_levels: Vec<String>,
    /// 定价（USD / 1M tokens）：input/output/cache_read/cache_write/reasoning，仅含有值项。
    pub pricing: Map<String, Value>,
}

impl CatalogModel {
    /// 转为可落库的模型定义（仅元数据；定价由 `catalog_import` 写到对应 provider 的 offer）。
    fn to_model_def(&self) -> ModelDef {
        let modalities = if self.modalities.is_empty() {
            None
        } else {
            Some(Value::Array(
                self.modalities.iter().cloned().map(Value::String).collect(),
            ))
        };
        let reasoning_levels = if self.reasoning_levels.is_empty() {
            None
        } else {
            Some(Value::Array(
                self.reasoning_levels
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ))
        };
        ModelDef {
            slug: self.id.clone(),
            display_name: self.name.clone(),
            context_window: self.context_window,
            modalities,
            reasoning_levels,
            extra: Value::Null,
        }
    }

    /// 转为可落库的报价（承载 models.dev 定价；绑定全部端点）。
    fn to_offer(&self) -> Offer {
        Offer {
            provider_key: self.provider_key.clone(),
            model_slug: self.id.clone(),
            pricing: if self.pricing.is_empty() {
                None
            } else {
                Some(Value::Object(self.pricing.clone()))
            },
            endpoint_protocol: None,
        }
    }
}

/// 从 models.dev 的单个模型对象提取候选。`raw` 为该模型的 JSON 值。
fn parse_model(provider_key: &str, provider_name: &str, id: &str, raw: &Value) -> CatalogModel {
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);

    let context_window = raw
        .get("limit")
        .and_then(|l| l.get("context"))
        .and_then(Value::as_i64);

    // modalities.input → 字符串数组
    let modalities = raw
        .get("modalities")
        .and_then(|m| m.get("input"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // reasoning_options: [{type,values:[...]}] → 扁平去重的档位列表
    let mut reasoning_levels: Vec<String> = Vec::new();
    if let Some(opts) = raw.get("reasoning_options").and_then(Value::as_array) {
        for opt in opts {
            if let Some(values) = opt.get("values").and_then(Value::as_array) {
                for v in values.iter().filter_map(Value::as_str) {
                    if !reasoning_levels.iter().any(|x| x == v) {
                        reasoning_levels.push(v.to_string());
                    }
                }
            }
        }
    }

    // cost → pricing（只搬 5 个已知键，与 UI 的定价表单一致；其余长尾字段忽略）
    let mut pricing = Map::new();
    if let Some(cost) = raw.get("cost").and_then(Value::as_object) {
        for key in ["input", "output", "cache_read", "cache_write", "reasoning"] {
            if let Some(num) = cost.get(key).and_then(Value::as_f64) {
                pricing.insert(
                    key.to_string(),
                    serde_json::Number::from_f64(num).map(Value::Number).unwrap_or(Value::Null),
                );
            }
        }
        // 过滤掉解析失败落进来的 Null
        pricing.retain(|_, v| !v.is_null());
    }

    CatalogModel {
        provider_key: provider_key.to_string(),
        provider_name: provider_name.to_string(),
        id: id.to_string(),
        name,
        context_window,
        modalities,
        reasoning_levels,
        pricing,
    }
}

/// 解析 models.dev 顶层 JSON（provider 字典）为扁平候选列表。
fn parse_catalog(root: &Value) -> Vec<CatalogModel> {
    let Some(providers) = root.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (provider_key, pv) in providers {
        let provider_name = pv
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_key);
        let Some(models) = pv.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (id, raw) in models {
            out.push(parse_model(provider_key, provider_name, id, raw));
        }
    }
    // 稳定排序：provider 名 → 模型 id，便于前端展示与去抖
    out.sort_by(|a, b| {
        a.provider_name
            .cmp(&b.provider_name)
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// 从 models.dev 拉取并解析全部候选模型。
///
/// 后端直接用 reqwest 拉取（复用 workspace 的 rustls 客户端），解析精简后返回扁平列表，
/// 避免 4.5MB 原始 JSON 过 webview。网络错误以可读消息返回前端。
#[tauri::command]
pub async fn catalog_fetch(_state: State<'_, Arc<ManagedState>>) -> CmdResult<Vec<CatalogModel>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;

    let resp = client
        .get(MODELS_DEV_URL)
        .send()
        .await
        .map_err(|e| format!("拉取 models.dev 失败: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("models.dev 返回错误状态: {}", resp.status()).into());
    }

    let root: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 models.dev 响应失败: {e}"))?;

    Ok(parse_catalog(&root))
}

/// 把用户勾选的候选模型批量导入：模型定义按 slug upsert；models.dev 的定价写入
/// 对应 provider key 的报价（仅当该报价不存在时，不覆盖用户已配置的定价）。
///
/// 返回成功导入的数量。任一条写库失败即中断并返回错误（前端可整体重试）。
#[tauri::command]
pub fn catalog_import(
    state: State<'_, Arc<ManagedState>>,
    models: Vec<CatalogModel>,
) -> CmdResult<usize> {
    let mut n = 0;
    for m in &models {
        state.db.upsert_model(&m.to_model_def())?;
        state.db.insert_offer_if_absent(&m.to_offer())?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_root() -> Value {
        json!({
            "opencode": {
                "id": "opencode",
                "name": "OpenCode Zen",
                "api": "https://opencode.ai/zen/v1",
                "models": {
                    "muse-spark-1.3-contributor-free": {
                        "id": "muse-spark-1.3-contributor-free",
                        "name": "Muse Spark 1.3 Free",
                        "reasoning": true,
                        "reasoning_options": [
                            { "type": "effort", "values": ["minimal", "low", "medium", "high", "xhigh"] }
                        ],
                        "modalities": { "input": ["text", "image", "video", "pdf", "audio"], "output": ["text"] },
                        "limit": { "context": 1048576, "output": 131072 },
                        "cost": { "input": 0, "output": 0, "cache_read": 0 }
                    }
                }
            },
            "anthropic": {
                "name": "Anthropic",
                "models": {
                    "claude-sonnet-4-6": {
                        "id": "claude-sonnet-4-6",
                        "name": "Claude Sonnet 4.6",
                        "modalities": { "input": ["text", "image"], "output": ["text"] },
                        "limit": { "context": 200000 },
                        "cost": { "input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75 }
                    }
                }
            }
        })
    }

    #[test]
    fn parses_flat_catalog_sorted_by_provider_then_id() {
        let list = parse_catalog(&sample_root());
        assert_eq!(list.len(), 2);
        // Anthropic 排在 OpenCode Zen 前（按 provider 名）
        assert_eq!(list[0].provider_name, "Anthropic");
        assert_eq!(list[0].id, "claude-sonnet-4-6");
        assert_eq!(list[1].provider_key, "opencode");
    }

    #[test]
    fn maps_all_fields_for_muse_spark() {
        let list = parse_catalog(&sample_root());
        let muse = list.iter().find(|m| m.id == "muse-spark-1.3-contributor-free").unwrap();
        assert_eq!(muse.provider_name, "OpenCode Zen");
        assert_eq!(muse.name.as_deref(), Some("Muse Spark 1.3 Free"));
        assert_eq!(muse.context_window, Some(1_048_576));
        assert_eq!(muse.modalities, vec!["text", "image", "video", "pdf", "audio"]);
        assert_eq!(
            muse.reasoning_levels,
            vec!["minimal", "low", "medium", "high", "xhigh"]
        );
        // cost 全 0 也应保留为 0（用户可见其免费），三个键都在
        assert_eq!(muse.pricing.get("input").and_then(Value::as_f64), Some(0.0));
        assert_eq!(muse.pricing.get("cache_read").and_then(Value::as_f64), Some(0.0));
        assert!(!muse.pricing.contains_key("cache_write"), "缺失项不应出现");
    }

    #[test]
    fn to_model_def_carries_metadata_only() {
        let list = parse_catalog(&sample_root());
        let claude = list.iter().find(|m| m.id == "claude-sonnet-4-6").unwrap();
        let def = claude.to_model_def();
        assert_eq!(def.slug, "claude-sonnet-4-6");
        assert_eq!(def.context_window, Some(200_000));
        // 定价走 offer，不落在模型定义上
        let offer = claude.to_offer();
        let pricing = offer.pricing.expect("定价应写入 offer");
        assert_eq!(pricing.get("input").and_then(Value::as_f64), Some(3.0));
        assert_eq!(pricing.get("cache_write").and_then(Value::as_f64), Some(3.75));
        assert_eq!(offer.provider_key, "anthropic");
    }

    #[test]
    fn empty_fields_become_none() {
        let root = json!({
            "p": { "name": "P", "models": { "bare": { "id": "bare" } } }
        });
        let list = parse_catalog(&root);
        let m = &list[0];
        assert!(m.modalities.is_empty());
        assert!(m.reasoning_levels.is_empty());
        assert!(m.pricing.is_empty());
        let def = m.to_model_def();
        assert!(def.modalities.is_none());
        assert!(def.reasoning_levels.is_none());
    }
}
