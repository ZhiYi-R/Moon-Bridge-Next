//! 模型目录：从 [models.dev](https://models.dev) 拉取公开模型库，供用户搜索并批量导入。
//!
//! 数据源为 `https://models.dev/api.json`（约 4.5MB）：顶层是 provider 字典，每个
//! provider 含 `name` / `api` / `models`，每个模型含 `id / name / limit / modalities /
//! reasoning_options / cost` 等。本模块在**后端**拉取并解析，只把精简后的扁平候选列表
//! 交给前端（避免大 JSON 过 webview，也把字段映射收在一处）。
//!
//! 导入内容分两处落库：模型**元数据**（slug/名称/上下文/输出上限/模态/推理档）→ `models` 表；
//! **定价**（`cost` → `pricing`）→ 对应 provider 的 **offer**（`insert_offer_if_absent`，
//! 并对同 slug 已有空定价的行 `backfill_offer_pricing`）——V7 起定价口径统一在 offers，
//! `models.pricing_json` 已删除。刻意**不触碰** provider 端点配置（端点在 Providers 页单独管理）。

use std::sync::Arc;
use std::time::Duration;

use std::collections::{HashMap, HashSet};

use moonbridge_store::{Endpoint, ModelDef, Offer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::State;

use crate::commands::CmdResult;
use crate::state::ManagedState;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const FETCH_TIMEOUT_SECS: u64 = 30;

/// 实时探测上游模型列表的超时（秒）。
const PROBE_TIMEOUT_SECS: u64 = 15;

/// models.dev 目录缓存 TTL：4.5MB 拉取体感时快时慢，检测/目录页共享 5 分钟窗口。
const CATALOG_CACHE_TTL: Duration = Duration::from_secs(300);

/// 读缓存（TTL 内命中才返回；过期条目就地驱逐，不留 4.5MB 僵尸）。
fn catalog_cache_get(state: &ManagedState) -> Option<Arc<Value>> {
    let mut g = state.catalog_cache.lock().unwrap();
    match g.as_ref() {
        Some(c) if c.fetched_at.elapsed() < CATALOG_CACHE_TTL => Some(c.root.clone()),
        Some(_) => {
            *g = None;
            None
        }
        None => None,
    }
}

/// 写缓存（覆盖旧条目）。
fn catalog_cache_put(state: &ManagedState, root: Arc<Value>) {
    *state.catalog_cache.lock().unwrap() = Some(crate::state::CatalogCacheEntry {
        fetched_at: std::time::Instant::now(),
        root,
    });
}

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

/// 拉取 models.dev 顶层 JSON（约 4.5MB）。TTL 内命中缓存直接返回（检测/目录页共享），
/// 未命中才走网络；`force` 绕过 TTL（手动刷新必须是真实重拉，不是静默 no-op）。
/// 并发拉取经 `catalog_lock` 单飞：目录页与模型检测同时触发时只走一次网络。
/// 返回 (root, 是否来自缓存)。
async fn fetch_models_dev_root(
    state: &ManagedState,
    force: bool,
) -> Result<(Arc<Value>, bool), String> {
    if !force {
        if let Some(cached) = catalog_cache_get(state) {
            return Ok((cached, true));
        }
    }
    let _guard = state.catalog_lock.lock().await;
    // 双检：等锁期间另一调用可能已完成拉取（force 路径已明确要求重拉，仍复查——
    // 连续两次手动刷新间隔毫秒级时，第二次复用刚拉的热数据无害且更快）
    {
        let g = state.catalog_cache.lock().unwrap();
        if let Some(c) = g.as_ref() {
            let fresh = c.fetched_at.elapsed() < CATALOG_CACHE_TTL;
            let just_fetched = c.fetched_at.elapsed() < Duration::from_secs(5);
            if (!force && fresh) || (force && just_fetched) {
                return Ok((c.root.clone(), !force));
            }
        }
    }
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
        return Err(format!("models.dev 返回错误状态: {}", resp.status()));
    }

    let root: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 models.dev 响应失败: {e}"))?;
    let root = Arc::new(root);
    catalog_cache_put(state, root.clone());
    Ok((root, false))
}

/// 从 models.dev 拉取并解析全部候选模型。
///
/// 解析精简后返回扁平列表，避免 4.5MB 原始 JSON 过 webview。
/// `refresh = true` 强制重拉（手动刷新）；返回带缓存命中标记的结果。
#[tauri::command]
pub async fn catalog_fetch(
    state: State<'_, Arc<ManagedState>>,
    refresh: Option<bool>,
) -> CmdResult<CatalogFetchResult> {
    let force = refresh.unwrap_or(false);
    let (root, cached) = fetch_models_dev_root(&state, force).await?;
    Ok(CatalogFetchResult {
        models: parse_catalog(&root),
        cached,
    })
}

/// `catalog_fetch` 的返回：候选列表 + 本次是否命中缓存（前端据此提示「使用缓存」）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFetchResult {
    pub models: Vec<CatalogModel>,
    pub cached: bool,
}

/// 已存在模型的元数据合并：每个字段「有值者胜」（本地非空字段不被目录值覆盖），
/// 本地为空的字段用目录值补齐。用户手改过的字段永远不会被再导入冲掉；目录侧的
/// 新信息（如新增的推理档位）也只在本地缺省时进入。
fn merge_model_def(existing: &ModelDef, incoming: ModelDef) -> ModelDef {
    ModelDef {
        slug: existing.slug.clone(),
        display_name: existing.display_name.clone().or(incoming.display_name),
        context_window: existing.context_window.or(incoming.context_window),
        max_output_tokens: existing.max_output_tokens.or(incoming.max_output_tokens),
        modalities: existing.modalities.clone().or(incoming.modalities),
        reasoning_levels: existing
            .reasoning_levels
            .clone()
            .or(incoming.reasoning_levels),
        extra: if existing.extra.is_null() {
            incoming.extra
        } else {
            existing.extra.clone()
        },
    }
}

/// `catalog_import` 的返回：实际写入数 + 无变化跳过数（前端如实提示，不虚报）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogImportResult {
    pub imported: usize,
    pub skipped: usize,
}

/// 把用户勾选的候选模型批量导入：模型定义按 slug upsert（已存在的按「有值者胜」
/// 合并，不覆盖本地字段）；models.dev 的定价写入对应 provider key 的报价（仅当该
/// 报价不存在时，不覆盖用户已配置的定价），并按 slug 给本地已有同模型报价回填
/// 定价（仅填 `pricing` 为空的行——目录 key 与本地 provider key 命名空间不同，
/// Provider 页绑定又会先建空定价行，不回填就永久没定价）。
///
/// 返回实际变化数与跳过数。任一条写库失败即中断并返回错误（前端可整体重试）。
#[tauri::command]
pub fn catalog_import(
    state: State<'_, Arc<ManagedState>>,
    models: Vec<CatalogModel>,
) -> CmdResult<CatalogImportResult> {
    let mut imported = 0;
    let mut skipped = 0;
    for m in &models {
        let incoming = m.to_model_def();
        let mut changed = match state.db.get_model(&m.id)? {
            Some(existing) => {
                let merged = merge_model_def(&existing, incoming);
                if merged != existing {
                    state.db.upsert_model(&merged)?;
                    true
                } else {
                    false
                }
            }
            None => {
                state.db.upsert_model(&incoming)?;
                true
            }
        };
        if state.db.insert_offer_if_absent(&m.to_offer())? {
            changed = true;
        }
        if let Some(pricing) = m.to_offer().pricing {
            if state.db.backfill_offer_pricing(&m.id, &pricing)? > 0 {
                changed = true;
            }
        }
        if changed {
            imported += 1;
        } else {
            skipped += 1;
        }
    }
    Ok(CatalogImportResult { imported, skipped })
}

// ───────────────────────── 模型检测（实时探测 + 目录 enrich）─────────────────────────

/// 模型检测结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectResult {
    /// 数据来源：`live`（实时探测）/ `catalog`（models.dev 目录回退）。
    pub source: String,
    /// 降级原因提示（live 失败回退目录、目录拉取失败等）；无则为 None。
    pub warning: Option<String>,
    /// 检测到的候选模型（按 id 排序）。
    pub models: Vec<DetectedModel>,
}

/// 检测到的单个模型：目录字段 + 是否已导入本地（该 provider 下已有 offer）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedModel {
    #[serde(flatten)]
    pub model: CatalogModel,
    /// 已导入（该 provider 下存在对应 offer）。
    pub exists: bool,
}

/// 按协议构造模型列表探测 URL；`None` 表示该协议不支持实时探测。
///
/// OpenAI 兼容端点的 baseUrl 通常自带 `/v1` 后缀，模型列表在 `{base}/models`；
/// Anthropic 的 base 不带 `/v1`，列表在 `{base}/v1/models`。
fn probe_url(protocol: &str, base_url: &str) -> Option<String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    match protocol {
        "openai-chat" | "openai-response" => Some(format!("{base}/models")),
        "anthropic" => Some(format!("{base}/v1/models")),
        _ => None,
    }
}

/// 从模型列表响应提取 (id, 展示名)：OpenAI 与 Anthropic 的列表接口同为
/// `{ "data": [{ "id": ... }] }` 形态，Anthropic 另有 `display_name`。
/// 非法/缺字段的条目跳过（模型输出属系统边界，不做全有或全无的失败）。
fn parse_live_model_ids(body: &Value) -> Vec<(String, Option<String>)> {
    let Some(arr) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| {
            let id = it.get("id")?.as_str()?;
            let name = it
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some((id.to_string(), name))
        })
        .collect()
}

/// 实时探测上游模型列表：按协议分派 URL 与认证头。
async fn probe_live(
    ep: &Endpoint,
    version: Option<&str>,
) -> Result<Vec<(String, Option<String>)>, String> {
    let url = probe_url(&ep.protocol, &ep.base_url)
        .ok_or_else(|| format!("协议 {} 不支持实时探测", ep.protocol))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let mut req = client.get(&url);
    match ep.protocol.as_str() {
        "anthropic" => {
            if !ep.api_key.is_empty() {
                req = req.header("x-api-key", &ep.api_key);
            }
            req = req.header("anthropic-version", version.unwrap_or("2023-06-01"));
        }
        _ => {
            if !ep.api_key.is_empty() {
                req = req.bearer_auth(&ep.api_key);
            }
        }
    }
    let resp = req.send().await.map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("上游返回 {}", resp.status()));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("响应不是合法 JSON: {e}"))?;
    Ok(parse_live_model_ids(&body))
}

/// 用目录元数据 enrich 实时探测到的 id 列表，产出归属本地 provider 的候选模型。
/// 目录中查不到的 id 保留为裸条目（只有 id/name），不丢弃。
fn merge_with_catalog(
    ids: Vec<(String, Option<String>)>,
    catalog: &[CatalogModel],
    provider_key: &str,
) -> Vec<CatalogModel> {
    let by_id: HashMap<&str, &CatalogModel> = catalog.iter().map(|m| (m.id.as_str(), m)).collect();
    ids.into_iter()
        .map(|(id, name)| match by_id.get(id.as_str()) {
            // 元数据取自目录，归属改为本地 provider（导入时 offer 落到本地 key 下）
            Some(c) => CatalogModel {
                provider_key: provider_key.to_string(),
                provider_name: provider_key.to_string(),
                id: id.clone(),
                ..(*c).clone()
            },
            None => CatalogModel {
                provider_key: provider_key.to_string(),
                provider_name: provider_key.to_string(),
                id,
                name,
                context_window: None,
                max_output_tokens: None,
                modalities: Vec::new(),
                reasoning_levels: Vec::new(),
                pricing: Map::new(),
            },
        })
        .collect()
}

/// 解析 models.dev 中指定 provider 分区为候选列表（归属记为本地 provider），按 id 排序。
fn catalog_section(root: &Value, models_dev_id: &str, provider_key: &str) -> Vec<CatalogModel> {
    let Some(models) = root
        .get(models_dev_id)
        .and_then(|p| p.get("models"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut out: Vec<CatalogModel> = models
        .iter()
        .map(|(id, raw)| parse_model(provider_key, provider_key, id, raw))
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 给候选列表打上「已导入」标记（该 provider 下已有 offer 的 slug）。
fn mark_exists(models: Vec<CatalogModel>, existing: &HashSet<String>) -> Vec<DetectedModel> {
    models
        .into_iter()
        .map(|m| DetectedModel {
            exists: existing.contains(&m.id),
            model: m,
        })
        .collect()
}

/// 目录回退：目录分区非空则返回目录列表；否则汇总两侧失败原因报错。
fn catalog_fallback(
    reason: String,
    catalog: Option<Vec<CatalogModel>>,
    catalog_err: Option<String>,
    existing: &HashSet<String>,
) -> CmdResult<DetectResult> {
    match catalog {
        Some(list) if !list.is_empty() => Ok(DetectResult {
            source: "catalog".to_string(),
            warning: Some(format!("实时探测失败（{reason}），已回退 models.dev 目录")),
            models: mark_exists(list, existing),
        }),
        _ => match catalog_err {
            Some(ce) => {
                Err(format!("实时探测失败（{reason}），models.dev 目录拉取也失败：{ce}").into())
            }
            None => Err(format!("实时探测失败（{reason}），且该上游无 models.dev 目录映射").into()),
        },
    }
}

/// 模型检测：实时探测 provider 首端点的模型列表，用 models.dev 目录 enrich 元数据；
/// 探测失败（或返回空）且预设带目录映射时回退到目录列表。
///
/// 实时探测走第一个端点（故障转移组的主端点）；目录拉取失败不阻断实时结果，
/// 仅以 warning 告知元数据缺失。
#[tauri::command]
pub async fn provider_detect_models(
    state: State<'_, Arc<ManagedState>>,
    provider_key: String,
) -> CmdResult<DetectResult> {
    let provider = state
        .db
        .get_provider(&provider_key)?
        .ok_or_else(|| format!("上游服务不存在: {provider_key}"))?;
    let ep = provider
        .endpoints
        .first()
        .ok_or_else(|| format!("上游服务 {provider_key} 未配置端点"))?;

    // 预设映射（enrich 与目录回退的数据源）：key 命中优先，再按 baseUrl 找回
    let base_urls: Vec<&str> = provider
        .endpoints
        .iter()
        .map(|e| e.base_url.as_str())
        .collect();
    let models_dev_id =
        crate::presets::find_preset(&provider.key, &base_urls).and_then(|p| p.models_dev_id);

    let live = probe_live(ep, provider.version.as_deref()).await;

    // 目录只在需要时拉取：live 成功时用于 enrich，失败时用于回退
    let mut catalog: Option<Vec<CatalogModel>> = None;
    let mut catalog_err: Option<String> = None;
    if let Some(mid) = models_dev_id {
        match fetch_models_dev_root(&state, false).await {
            Ok((root, _)) => catalog = Some(catalog_section(&root, mid, &provider.key)),
            Err(e) => catalog_err = Some(e),
        }
    }

    let existing: HashSet<String> = state
        .db
        .list_offers(&provider.key)?
        .into_iter()
        .map(|o| o.model_slug)
        .collect();

    // 分流：live 拿到非空列表走 enrich 路径；失败/空列表走目录回退
    let ids = match live {
        Ok(ids) if !ids.is_empty() => ids,
        Ok(_) => {
            return catalog_fallback(
                "上游返回空列表".to_string(),
                catalog,
                catalog_err,
                &existing,
            )
        }
        Err(e) => return catalog_fallback(e, catalog, catalog_err, &existing),
    };

    let mut models = merge_with_catalog(ids, catalog.as_deref().unwrap_or(&[]), &provider.key);
    models.sort_by(|a, b| a.id.cmp(&b.id));
    // live 成功但目录侧有缺失时告知（裸列表/无映射/拉取失败）
    let warning = if models_dev_id.is_none() {
        Some("该上游无 models.dev 目录映射，仅返回实时探测的裸列表".to_string())
    } else if let Some(e) = catalog_err {
        Some(format!(
            "models.dev 目录拉取失败（{e}），仅返回实时探测的裸列表"
        ))
    } else if catalog.as_ref().is_some_and(|c| c.is_empty()) {
        Some("models.dev 目录中无该上游分区，仅返回实时探测的裸列表".to_string())
    } else {
        None
    };
    Ok(DetectResult {
        source: "live".to_string(),
        warning,
        models: mark_exists(models, &existing),
    })
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
        // Anthropic 排在 OpenCode Zen 前（按 provider 名）
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
        // 长上下文分层价目随目录导入保留（计价按 input_tokens 越阈值换挡）
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
    }

    /// 回填只发生在内存库测不了 `catalog_import`（要 Tauri State），但 DAO 语义可测：
    /// 同 slug 多 provider 行里，只有 `pricing IS NULL` 的被回填，手填过的不动。
    #[test]
    fn backfill_only_fills_null_pricing() {
        use moonbridge_store::Database;
        let db = Database::open_in_memory().unwrap();
        let pricing = json!({ "input": 3.0, "output": 15.0 });
        // 用户 key 下的空定价行（Provider 页绑定的产物）+ 目录 key 行 + 手填过的行。
        // 注意顺序：upsert_offer 对 pricing=None 的新行会经 same_slug_pricing 继承
        // 同 slug 的已有定价——空行须先于手填行插入才能保持 NULL。
        for (key, p) in [
            ("mykey", None),
            ("anthropic", None),
            ("mykey2", Some(json!({ "input": 99.0 }))),
        ] {
            db.upsert_offer(&Offer {
                provider_key: key.into(),
                model_slug: "claude-x".into(),
                pricing: p,
                endpoint_protocol: None,
            })
            .unwrap();
        }
        assert_eq!(db.backfill_offer_pricing("claude-x", &pricing).unwrap(), 2);
        let get = |key: &str| {
            db.list_offers(key)
                .unwrap()
                .into_iter()
                .find(|o| o.model_slug == "claude-x")
                .unwrap()
                .pricing
        };
        assert_eq!(get("mykey"), Some(pricing.clone()), "空行应被回填");
        assert_eq!(
            get("mykey2").and_then(|v| v.get("input").and_then(Value::as_f64)),
            Some(99.0),
            "手填定价不得被覆盖"
        );
        assert_eq!(get("anthropic"), Some(pricing), "目录 key 行同样回填");
        // 无空行时返回 0 且无副作用
        assert_eq!(
            db.backfill_offer_pricing("claude-x", &json!({ "input": 1.0 }))
                .unwrap(),
            0
        );
    }

    // ── 模型检测 ──

    #[test]
    fn probe_url_per_protocol() {
        assert_eq!(
            probe_url("openai-chat", "https://api.deepseek.com/").as_deref(),
            Some("https://api.deepseek.com/models"),
            "尾随斜杠应被归一化"
        );
        assert_eq!(
            probe_url("openai-response", "https://opencode.ai/zen/go/v1").as_deref(),
            Some("https://opencode.ai/zen/go/v1/models")
        );
        assert_eq!(
            probe_url("anthropic", "https://api.anthropic.com").as_deref(),
            Some("https://api.anthropic.com/v1/models")
        );
        assert!(
            probe_url("google-genai", "https://x").is_none(),
            "未覆盖协议不支持探测"
        );
        assert!(
            probe_url("openai-chat", "  ").is_none(),
            "空 base 不构造 URL"
        );
    }

    #[test]
    fn parse_live_ids_openai_and_anthropic() {
        let openai = json!({
            "object": "list",
            "data": [ { "id": "deepseek-flash" }, { "id": 7 }, { "name": "no-id" } ]
        });
        assert_eq!(
            parse_live_model_ids(&openai),
            vec![("deepseek-flash".to_string(), None)],
            "非字符串 id / 缺 id 的条目跳过"
        );
        let anthropic = json!({
            "data": [ { "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5", "type": "model" } ]
        });
        assert_eq!(
            parse_live_model_ids(&anthropic),
            vec![(
                "claude-sonnet-5".to_string(),
                Some("Claude Sonnet 5".to_string())
            )]
        );
        assert!(parse_live_model_ids(&json!({ "error": "x" })).is_empty());
    }

    #[test]
    fn merge_enriches_known_ids_and_keeps_unknown_bare() {
        let catalog = vec![CatalogModel {
            provider_key: "deepseek".into(),
            provider_name: "DeepSeek".into(),
            id: "deepseek-flash".into(),
            name: Some("DeepSeek Flash".into()),
            context_window: Some(1_000_000),
            max_output_tokens: None,
            modalities: vec!["text".into()],
            reasoning_levels: vec![],
            pricing: json!({ "input": 0.27 }).as_object().unwrap().clone(),
        }];
        let merged = merge_with_catalog(
            vec![
                ("deepseek-flash".to_string(), None),
                ("mystery-1".to_string(), Some("Mystery".to_string())),
            ],
            &catalog,
            "my-ds",
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].provider_key, "my-ds", "归属改为本地 key");
        assert_eq!(merged[0].context_window, Some(1_000_000));
        assert_eq!(
            merged[0].pricing.get("input").and_then(Value::as_f64),
            Some(0.27)
        );
        assert_eq!(merged[1].id, "mystery-1");
        assert_eq!(merged[1].name.as_deref(), Some("Mystery"));
        assert!(merged[1].context_window.is_none(), "未知 id 保留裸条目");
    }

    #[test]
    fn catalog_section_parses_and_owns_local_key() {
        let root = sample_root();
        let list = catalog_section(&root, "anthropic", "my-ant");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider_key, "my-ant", "归属改为本地 key");
        assert_eq!(list[0].id, "claude-sonnet-4-6");
        assert!(catalog_section(&root, "no-such", "x").is_empty());
    }

    // ── 目录缓存 ──

    fn temp_state(tag: &str) -> (Arc<ManagedState>, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("mbn-catalog-cache-{}-{tag}", std::process::id()));
        let paths = crate::config::AppPaths::resolve(dir.join("config"), dir.join("data"));
        (ManagedState::new(paths).unwrap(), dir)
    }

    #[test]
    fn catalog_cache_hit_within_ttl_and_miss_after() {
        let (st, dir) = temp_state("ttl");
        assert!(catalog_cache_get(&st).is_none(), "空缓存未命中");
        catalog_cache_put(&st, Arc::new(json!({})));
        assert!(catalog_cache_get(&st).is_some(), "TTL 内命中");
        // 手动把条目时间戳回拨到 TTL 之外
        if let Some(c) = st.catalog_cache.lock().unwrap().as_mut() {
            c.fetched_at = std::time::Instant::now()
                .checked_sub(CATALOG_CACHE_TTL + Duration::from_secs(1))
                .unwrap();
        }
        assert!(catalog_cache_get(&st).is_none(), "过期未命中");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 导入合并：本地非空字段「有值者胜」，空字段用目录补齐；extra 同理。
    #[test]
    fn merge_model_def_preserves_local_values() {
        let existing = ModelDef {
            slug: "m".into(),
            display_name: Some("用户改过的名字".into()),
            context_window: Some(131_072),
            max_output_tokens: None,
            modalities: None,
            reasoning_levels: Some(json!(["high"])),
            extra: json!({"note": "手写"}),
        };
        let incoming = ModelDef {
            slug: "m".into(),
            display_name: Some("目录名".into()),
            context_window: Some(200_000),
            max_output_tokens: Some(65_536),
            modalities: Some(json!(["text"])),
            reasoning_levels: Some(json!(["low", "high"])),
            extra: json!({"src": "models.dev"}),
        };
        let merged = merge_model_def(&existing, incoming);
        // 本地非空：不被覆盖
        assert_eq!(merged.display_name.as_deref(), Some("用户改过的名字"));
        assert_eq!(merged.context_window, Some(131_072));
        assert_eq!(merged.reasoning_levels, Some(json!(["high"])));
        assert_eq!(merged.extra["note"], "手写");
        // 本地为空：目录补齐
        assert_eq!(merged.max_output_tokens, Some(65_536));
        assert_eq!(merged.modalities, Some(json!(["text"])));
        // 全有值时合并结果与原值相等（调用方据此判「无变化跳过」）
        let full = ModelDef {
            slug: "m".into(),
            display_name: Some("a".into()),
            context_window: Some(1),
            max_output_tokens: Some(2),
            modalities: Some(json!([])),
            reasoning_levels: Some(json!([])),
            extra: json!({}),
        };
        let merged2 = merge_model_def(&full, incoming_clone());
        assert_eq!(merged2, full);
    }

    fn incoming_clone() -> ModelDef {
        ModelDef {
            slug: "m".into(),
            display_name: Some("目录名".into()),
            context_window: Some(200_000),
            max_output_tokens: Some(65_536),
            modalities: Some(json!(["text"])),
            reasoning_levels: Some(json!(["low"])),
            extra: json!({"src": "models.dev"}),
        }
    }
}
