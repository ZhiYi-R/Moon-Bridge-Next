//! 模型目录：从 [models.dev](https://models.dev) 拉取公开模型库，供用户搜索并批量导入。
//!
//! 数据源为 `https://models.dev/api.json`（约 4.5MB）：顶层是 provider 字典，每个
//! provider 含 `name` / `api` / `models`，每个模型含 `id / name / limit / modalities /
//! reasoning_options / cost` 等。本模块在**后端**拉取并解析，只把精简后的扁平候选列表
//! 交给前端（避免大 JSON 过网络，也把字段映射收在一处）。
//!
//! 导入内容分两处落库：模型**元数据**（slug/名称/上下文/输出上限/模态/推理档）→ `models` 表；
//! **定价**（`cost` → `pricing`）→ 对应 provider 的 **offer**（`insert_offer_if_absent`，
//! 并对同 slug 已有空定价的行 `backfill_offer_pricing`）。刻意**不触碰** provider 端点配置。

use axum::extract::State;
use axum::Json;
use moonbridge_store::{ModelDef, Offer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{AdminState, ApiError, ApiResult};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

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
    /// 输出 token 上限（models.dev `limit.output`）。
    pub max_output_tokens: Option<i64>,
    /// 输入模态（text/image/pdf/video/audio）。
    pub modalities: Vec<String>,
    /// 推理档位（effort 值扁平化）。
    pub reasoning_levels: Vec<String>,
    /// 定价（USD / 1M tokens）：input/output/cache_read/cache_write/reasoning，仅含有值项。
    pub pricing: Map<String, Value>,
}

impl CatalogModel {
    /// 转为可落库的模型定义（仅元数据；定价由导入流程写到对应 provider 的 offer）。
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
            max_output_tokens: self.max_output_tokens,
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
    let name = raw.get("name").and_then(Value::as_str).map(str::to_string);

    let context_window = raw
        .get("limit")
        .and_then(|l| l.get("context"))
        .and_then(Value::as_i64);

    let max_output_tokens = raw
        .get("limit")
        .and_then(|l| l.get("output"))
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

    // cost → pricing（搬 5 个扁平价键 + 长上下文分层；其余长尾字段忽略）。
    // 分层的权威形态是 `tiers: [{input, output, …, tier: {type:"context", size}}]`
    // （context 阈值按 prompt/input token 计）；`context_over_200k` 是生成器为旧
    // 消费者同步输出的单档镜像，一并保留——计价时两种形态都认。
    let mut pricing = Map::new();
    if let Some(cost) = raw.get("cost").and_then(Value::as_object) {
        for key in ["input", "output", "cache_read", "cache_write", "reasoning"] {
            if let Some(num) = cost.get(key).and_then(Value::as_f64) {
                pricing.insert(
                    key.to_string(),
                    serde_json::Number::from_f64(num)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                );
            }
        }
        for key in ["tiers", "context_over_200k"] {
            if let Some(v) = cost.get(key) {
                pricing.insert(key.to_string(), v.clone());
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
        max_output_tokens,
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
/// 复用 [`AdminState::catalog`]（30s 超时的直连客户端，不走 egress 代理，与桌面端口径
/// 一致），解析精简后返回扁平列表。网络错误以可读消息返回前端。
pub async fn catalog_fetch(State(state): State<AdminState>) -> ApiResult<Json<Vec<CatalogModel>>> {
    let resp = state
        .catalog
        .get(MODELS_DEV_URL)
        .send()
        .await
        .map_err(|e| ApiError::internal(format!("拉取 models.dev 失败: {e}")))?;

    if !resp.status().is_success() {
        return Err(ApiError::internal(format!(
            "models.dev 返回错误状态: {}",
            resp.status()
        )));
    }

    let root: Value = resp
        .json()
        .await
        .map_err(|e| ApiError::internal(format!("解析 models.dev 响应失败: {e}")))?;

    Ok(Json(parse_catalog(&root)))
}

/// 导入结果。
#[derive(Debug, Serialize)]
pub struct CatalogImportResult {
    pub imported: usize,
}

/// 把用户勾选的候选模型批量导入：模型定义按 slug upsert；models.dev 的定价写入
/// 对应 provider key 的报价（仅当该报价不存在时，不覆盖用户已配置的定价），并按
/// slug 给本地已有同模型报价回填定价（仅填 `pricing` 为空的行——目录 key 与本地
/// provider key 命名空间不同，Provider 页绑定又会先建空定价行，不回填就永久没定价）。
///
/// 返回成功导入的数量。任一条写库失败即中断并返回错误（前端可整体重试）。
pub async fn catalog_import(
    State(state): State<AdminState>,
    Json(models): Json<Vec<CatalogModel>>,
) -> ApiResult<Json<CatalogImportResult>> {
    let mut n = 0;
    for m in &models {
        state.db.upsert_model(&m.to_model_def())?;
        state.db.insert_offer_if_absent(&m.to_offer())?;
        if let Some(pricing) = m.to_offer().pricing {
            state.db.backfill_offer_pricing(&m.id, &pricing)?;
        }
        n += 1;
    }
    Ok(Json(CatalogImportResult { imported: n }))
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
                        "cost": {
                            "input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75,
                            "tiers": [{ "input": 6, "output": 22.5, "cache_read": 0.6, "cache_write": 7.5,
                                        "tier": { "type": "context", "size": 200000 } }],
                            "context_over_200k": { "input": 6, "output": 22.5, "cache_read": 0.6, "cache_write": 7.5 }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn parses_flat_catalog_sorted_by_provider_then_id() {
        let list = parse_catalog(&sample_root());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].provider_name, "Anthropic");
        assert_eq!(list[0].id, "claude-sonnet-4-6");
        assert_eq!(list[1].provider_key, "opencode");
    }

    #[test]
    fn maps_all_fields_for_muse_spark() {
        let list = parse_catalog(&sample_root());
        let muse = list
            .iter()
            .find(|m| m.id == "muse-spark-1.3-contributor-free")
            .unwrap();
        assert_eq!(muse.provider_name, "OpenCode Zen");
        assert_eq!(muse.name.as_deref(), Some("Muse Spark 1.3 Free"));
        assert_eq!(muse.context_window, Some(1_048_576));
        assert_eq!(muse.max_output_tokens, Some(131_072));
        assert_eq!(
            muse.modalities,
            vec!["text", "image", "video", "pdf", "audio"]
        );
        assert_eq!(
            muse.reasoning_levels,
            vec!["minimal", "low", "medium", "high", "xhigh"]
        );
        // cost 全 0 也应保留为 0（用户可见其免费），三个键都在
        assert_eq!(muse.pricing.get("input").and_then(Value::as_f64), Some(0.0));
        assert_eq!(
            muse.pricing.get("cache_read").and_then(Value::as_f64),
            Some(0.0)
        );
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
        assert_eq!(
            pricing.get("cache_write").and_then(Value::as_f64),
            Some(3.75)
        );
        let tiers = pricing.get("tiers").and_then(Value::as_array).unwrap();
        assert_eq!(tiers[0]["tier"]["size"].as_u64(), Some(200_000));
        assert_eq!(tiers[0]["input"].as_f64(), Some(6.0));
        assert!(pricing.get("context_over_200k").is_some(), "旧式镜像也保留");
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
        // 无定价时 offer 不带 pricing（避免把空对象写成定价）
        assert!(m.to_offer().pricing.is_none());
    }

    #[test]
    fn non_object_root_and_provider_without_models_are_skipped() {
        assert!(parse_catalog(&json!([1, 2, 3])).is_empty());
        let list = parse_catalog(&json!({
            "no-models": { "name": "N" },
            "ok": { "name": "O", "models": { "m": { "id": "m" } } }
        }));
        assert_eq!(list.len(), 1);
        // provider 无 name 时回退用 key
        assert_eq!(list[0].provider_name, "O");
        let unnamed = parse_catalog(&json!({
            "k": { "models": { "m": { "id": "m" } } }
        }));
        assert_eq!(unnamed[0].provider_name, "k");
    }
}
