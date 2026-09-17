//! 余额&健康看板引擎与定时调度。
//!
//! 每张卡片是一段**一次性** Lua 脚本：宿主加载它（复用 [`moonbridge_plugin::LuaRuntime`]
//! 的沙箱与 `mb.*` 宿主 API），再调用 `MB.query(ctx)`，把返回值经 serde 转为 JSON
//! 落库。脚本不进网关的 [`moonbridge_protocol::PluginHooks`] 注册表，宿主桥也只实现
//! `mb.http.request`——余额查询是旁路能力，不该与网关生命周期互相牵制。
//!
//! 多 key 拆卡：优先使用手动 key 列表，否则引用 Provider 端点，去重保序后引擎对
//! 每个 key 各跑一次脚本（ctx 为单 key 形状，`keys` 是单元素数组），结果按
//! `(card_key, key_index)` 拆行落库，前端逐 key 渲染一张卡片。落库的 `key_label`
//! 是掩码后的展示标签，key 原文不进结果表。
//!
//! 失败分层（按 key 独立）：
//! - **引擎级失败**（脚本读不到 / 无 `MB.query` / 抛错 / 超时 / Provider 引用失效）→
//!   该 key `status = "error"`，`error` 记消息，`payload` **保留该 key 上一次**的值
//!   （前端仍能看到最后已知余额）；
//! - **脚本业务失败**（返回 table 且 `status = "error"`）→ 同样 `status = "error"`，
//!   但 `payload` 更新为该次返回——脚本常在失败时也带回诊断上下文。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moonbridge_core::{CoreRequest, CoreResponse};
use moonbridge_plugin::{
    HostBridge, HttpRequest, HttpResponse, LuaRuntime, SandboxLimits, SessionStore,
};
use moonbridge_store::{
    BalanceCard, BalanceCardView, BalanceKeyResult, BalanceQuota, BalanceResult, Database,
};
use serde_json::{json, Value};

use crate::parse_script_ref;
use crate::ScriptRef;

/// 单次查询的整体超时（含脚本加载 + `MB.query` 执行）。
const QUERY_TIMEOUT: Duration = Duration::from_secs(45);

/// `mb.http.request` 未显式指定超时时的兜底（与网关侧插件子请求同口径）。
const HTTP_FALLBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// 调度器扫描周期：每轮挑出到期卡片串行执行。
pub const SCHEDULE_TICK: Duration = Duration::from_secs(30);

/// 当前 unix 时间戳（秒）。口径与 store 的 `created_at` 一致。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 余额脚本的宿主桥：只提供 `mb.http.request`。
///
/// 刻意不复用 [`crate::bridge::GatewayBridge`]——后者持有 db/registry/上游客户端，与
/// 网关生命周期强耦合。`provider_invoke` 直接拒绝：余额脚本没有跨 provider 编排场景。
struct BalanceBridge {
    client: reqwest::Client,
}

#[async_trait]
impl HostBridge for BalanceBridge {
    async fn http_request(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        let method = reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes())
            .map_err(|e| e.to_string())?;
        let mut builder = self.client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(body) = &req.body {
            builder = builder.json(body);
        }
        // 总是施加超时：脚本未指定时用兜底值，避免单个上游挂起吃满整体预算。
        let timeout_ms = req
            .timeout_ms
            .unwrap_or(HTTP_FALLBACK_TIMEOUT.as_millis() as u64);
        builder = builder.timeout(Duration::from_millis(timeout_ms));
        let resp = builder.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        // 上游返回 JSON 时自动展开为 Lua table，脚本可直接 `r.body.xxx`
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn provider_invoke(
        &self,
        _provider: &str,
        _model: &str,
        _req: CoreRequest,
    ) -> Result<CoreResponse, String> {
        Err("余额脚本不支持".to_string())
    }
}

/// 余额查询共用的出站客户端（进程级单例）。
///
/// 与网关上游客户端分开：余额查询直连（与目录拉取同口径），不套 egress 代理，
/// 也不参与网关启停；单次请求的超时由 [`BalanceBridge`] 逐次施加。
///
/// 两个宿主都在每次请求/每次调度时才构造引擎（管理状态里不持有引擎），故客户端
/// 必须进程级共享——否则每个请求都新建一套连接池，连接无法复用。
fn shared_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build()
                .expect("构建余额查询 HTTP 客户端失败")
        })
        .clone()
}

/// 余额查询引擎：持库句柄、脚本根目录与出站客户端。
#[derive(Clone)]
pub struct BalanceEngine {
    db: Arc<Database>,
    /// 脚本根目录：`script_ref` 里相对 `.lua` 归一到此处，绝对路径必须落在其内。
    /// `None` 时不做包含性校验（CLI/测试场景，与插件加载侧同语义）。
    plugins_dir: Option<PathBuf>,
    client: reqwest::Client,
}

impl BalanceEngine {
    /// 构造引擎（出站客户端取进程级共享实例）。
    pub fn new(db: Arc<Database>, plugins_dir: Option<PathBuf>) -> Self {
        Self::new_with_client(db, plugins_dir, shared_client())
    }

    /// 以指定出站客户端构造引擎（测试可注入）。
    pub fn new_with_client(
        db: Arc<Database>,
        plugins_dir: Option<PathBuf>,
        client: reqwest::Client,
    ) -> Self {
        BalanceEngine {
            db,
            plugins_dir,
            client,
        }
    }

    /// 覆写脚本根目录（宿主在启动时钉住与插件同一套 `plugins_dir`；测试也可用）。
    pub fn with_plugins_dir(mut self, plugins_dir: Option<PathBuf>) -> Self {
        self.plugins_dir = plugins_dir;
        self
    }

    /// 执行一张卡片：逐 key 各跑一次脚本、各自落库，返回本次全部 key 的结果。
    ///
    /// key 列表解析失败（如 Provider 引用失效）时落一行 `key_index = 0` 的错误结果
    /// （label 空），并剪掉其余残留行——看板上呈现为单张错误卡。
    pub async fn run_card(&self, card: &BalanceCard) -> Vec<BalanceKeyResult> {
        let now = now_unix();
        let (keys, provider_name) = match self.resolve_keys(card) {
            Ok(v) => v,
            Err(message) => {
                let result = self.engine_error_result(&card.key, 0, message, now);
                let kr = BalanceKeyResult {
                    key_index: 0,
                    key_label: String::new(),
                    result,
                };
                self.store_key_result(&card.key, &kr);
                if let Err(e) = self.db.prune_balance_results(&card.key, 1) {
                    tracing::error!(card = %card.key, error = %e, "清理余额残留结果失败");
                }
                return vec![kr];
            }
        };
        let mut out = Vec::with_capacity(keys.len());
        for (idx, key) in keys.iter().enumerate() {
            out.push(
                self.run_key(card, idx as i64, key, &provider_name, now)
                    .await,
            );
        }
        // key 变少后剪掉旧 key 的残留行，看板不再展示它们。
        if let Err(e) = self.db.prune_balance_results(&card.key, keys.len() as i64) {
            tracing::error!(card = %card.key, error = %e, "清理余额残留结果失败");
        }
        out
    }

    /// 刷新一张卡片并回传视图（供 REST/IPC）。`key_index` 为 `Some` 时只重跑该 key
    /// （越界或 key 列表解析失败时不动作，直接回读现状）；`None` 时整卡全量重跑。
    pub async fn refresh_card(
        &self,
        card: &BalanceCard,
        key_index: Option<i64>,
    ) -> BalanceCardView {
        match key_index {
            None => {
                self.run_card(card).await;
            }
            Some(idx) => {
                if let Ok((keys, provider_name)) = self.resolve_keys(card) {
                    if idx >= 0 {
                        if let Some(key) = keys.get(idx as usize).cloned() {
                            self.run_key(card, idx, &key, &provider_name, now_unix())
                                .await;
                        }
                    }
                }
            }
        }
        view_of(&self.db, &card.key)
            .ok()
            .flatten()
            .unwrap_or_else(|| BalanceCardView {
                card: card.clone(),
                results: Vec::new(),
            })
    }

    /// 试运行一张卡片（dry-run，供编辑表单的「测试拉取」预览）：逐 key 执行脚本并
    /// 返回本次结果数组，但**不写库、不读历史结果**——引擎级失败时 `payload` 为
    /// None，错误原样呈现给预览面板，不影响线上展示的最后成功值。
    pub async fn test_card(&self, card: &BalanceCard) -> Vec<BalanceKeyResult> {
        let now = now_unix();
        let (keys, provider_name) = match self.resolve_keys(card) {
            Ok(v) => v,
            Err(message) => {
                return vec![BalanceKeyResult {
                    key_index: 0,
                    key_label: String::new(),
                    result: BalanceResult {
                        status: "error".to_string(),
                        payload: None,
                        error: Some(message),
                        queried_at: now,
                    },
                }];
            }
        };
        let mut out = Vec::with_capacity(keys.len());
        for (idx, key) in keys.iter().enumerate() {
            let ctx = Self::ctx_for(card, key, &provider_name);
            let result = match self.query(card, &ctx).await {
                Ok(ret) => Self::from_script_return(&ret, now),
                Err(message) => BalanceResult {
                    status: "error".to_string(),
                    payload: None,
                    error: Some(message),
                    queried_at: now,
                },
            };
            out.push(BalanceKeyResult {
                key_index: idx as i64,
                key_label: mask_key(key),
                result,
            });
        }
        out
    }

    /// 串行刷新全部**启用**卡片，返回刷新后**全部**卡片的视图（含未启用的，顺序同
    /// `list_balance_cards`，前端可直接整体替换列表状态）。
    ///
    /// 串行是刻意的：这些卡片往往共用少数几个上游与 API Key，并发打配额接口
    /// 容易触发限流。
    pub async fn refresh_all(&self) -> Vec<BalanceCardView> {
        for card in self
            .db
            .list_balance_cards()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.enabled)
        {
            self.run_card(&card).await;
        }
        list_views(&self.db).unwrap_or_default()
    }

    /// 执行单个 key 的查询并落库，返回该 key 的结果。
    async fn run_key(
        &self,
        card: &BalanceCard,
        key_index: i64,
        key: &str,
        provider_name: &str,
        now: i64,
    ) -> BalanceKeyResult {
        let ctx = Self::ctx_for(card, key, provider_name);
        // 脚本返回 table 时一律更新 payload（业务 error 也更新）；引擎级失败保留旧值。
        let result = match self.query(card, &ctx).await {
            Ok(ret) => Self::from_script_return(&ret, now),
            Err(message) => self.engine_error_result(&card.key, key_index, message, now),
        };
        let kr = BalanceKeyResult {
            key_index,
            key_label: mask_key(key),
            result,
        };
        self.store_key_result(&card.key, &kr);
        kr
    }

    /// 引擎级失败的结果：`payload` 保留该 key 上一次落库的值。
    fn engine_error_result(
        &self,
        card_key: &str,
        key_index: i64,
        message: String,
        now: i64,
    ) -> BalanceResult {
        let previous = self
            .db
            .get_balance_key_result(card_key, key_index)
            .ok()
            .flatten();
        BalanceResult {
            status: "error".to_string(),
            payload: previous.and_then(|p| p.result.payload),
            error: Some(message),
            queried_at: now,
        }
    }

    /// 落库单个 key 的结果（失败仅记日志，不中断整卡执行）。
    fn store_key_result(&self, card_key: &str, kr: &BalanceKeyResult) {
        if let Err(e) = self.db.upsert_balance_key_result(card_key, kr) {
            tracing::error!(card = %card_key, key_index = kr.key_index, error = %e, "余额结果落库失败");
        }
    }

    /// 读取脚本源码：文件脚本经 [`parse_script_ref`] 收敛到 `plugins_dir` 内。
    fn read_script(&self, script_ref: &str) -> Result<String, String> {
        match parse_script_ref(script_ref, self.plugins_dir.as_deref()) {
            ScriptRef::Inline(src) => Ok(src),
            ScriptRef::Rejected(p) => Err(format!("脚本路径越出插件目录: {p}")),
            ScriptRef::File(path) => std::fs::read_to_string(&path)
                .map_err(|e| format!("读取脚本 {} 失败: {e}", path.display())),
        }
    }

    /// 解析卡片的有效 key 列表与提供商展示名。
    ///
    /// 手动 key 按行解析、去空白并保序去重；非空时不读取 Provider。
    /// 否则从 Provider 端点继承并去重 key，失效引用报错。
    /// 空列表回落为 `[""]`，兼容无需认证的公开查询接口。
    fn resolve_keys(&self, card: &BalanceCard) -> Result<(Vec<String>, String), String> {
        let mut keys: Vec<String> = Vec::new();
        let mut provider_name = card.provider_label.clone();
        let provider_key = card
            .provider_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        for key in card
            .api_key
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !keys.iter().any(|existing| existing == key) {
                keys.push(key.to_string());
            }
        }
        if !keys.is_empty() {
            if provider_name.is_empty() {
                provider_name = provider_key.unwrap_or_default().to_string();
            }
            return Ok((keys, provider_name));
        }

        if let Some(pk) = provider_key {
            let provider = self
                .db
                .get_provider(pk)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("上游服务 {pk} 不存在"))?;
            let mut carry = String::new();
            for ep in &provider.endpoints {
                if !ep.api_key.is_empty() {
                    carry = ep.api_key.clone();
                }
                if !carry.is_empty() && !keys.contains(&carry) {
                    keys.push(carry.clone());
                }
            }
            if provider_name.is_empty() {
                provider_name = provider.key.clone();
            }
        }

        if keys.is_empty() {
            keys.push(String::new());
        }
        Ok((keys, provider_name))
    }

    /// 构造单 key 的脚本 ctx：引擎对每个 key 各跑一次脚本，脚本只需按单 key 编写
    /// （`key` 与 `keys` 同值，`keys` 恒为单元素数组）。`base_url` 取卡片上用户可选
    /// 填写的查询 URL（不从 Provider 端点继承）。
    fn ctx_for(card: &BalanceCard, key: &str, provider_name: &str) -> Value {
        json!({
            "name": card.key,
            "key": key,
            "keys": [key],
            "base_url": card.base_url,
            "provider": provider_name,
            "extra": card.extra,
        })
    }

    /// 一次性执行脚本并返回 `MB.query` 的返回值（JSON）。
    async fn query(&self, card: &BalanceCard, ctx: &Value) -> Result<Value, String> {
        let script = self.read_script(&card.script_ref)?;
        let bridge = Arc::new(BalanceBridge {
            client: self.client.clone(),
        });
        // 卡片 extra 同时作为 `mb.config`（脚本常在顶层读配置），完整 ctx 由
        // `MB.query` 参数传入。沙箱 call_timeout 对齐整体超时，让脚本内的死循环/
        // 长计算由指令计数钩子掐断，外层 tokio 超时兜住加载与锁等待。
        let limits = SandboxLimits {
            call_timeout: QUERY_TIMEOUT,
            ..SandboxLimits::default()
        };
        let runtime = LuaRuntime::new_with_limits(
            &card.key,
            &script,
            &card.extra,
            bridge,
            SessionStore::new(),
            limits,
        )
        .map_err(|e| e.to_string())?;
        match tokio::time::timeout(QUERY_TIMEOUT, runtime.call_mb_once("query", ctx)).await {
            Err(_) => Err(format!("查询超时（{} 秒）", QUERY_TIMEOUT.as_secs())),
            Ok(Err(e)) => Err(e.to_string()),
            Ok(Ok(v)) => Ok(v),
        }
    }

    /// 把脚本返回值转成落库结果。
    ///
    /// `status` 缺省视为 `ok`；`message` 仅在失败时作为 `error`（成功时它是摘要文案，
    /// 留在 payload 里原样透传）。payload 承载脚本返回的**完整** JSON（脚本自定义字段
    /// 全部原样保留），只把 `quotas` 数组按 [`BalanceQuota`] 归一：脚本按 Lua 习惯写
    /// `used_percent`/`left_percent`/`used_amount`/`left_amount`/`reset_at`，归一后前端
    /// 拿到契约里的 camelCase 形状，且 used/left percent 互补值两个都齐（amount 不互补）。
    fn from_script_return(ret: &Value, now: i64) -> BalanceResult {
        let status = match ret.get("status").and_then(Value::as_str) {
            None | Some("ok") => "ok",
            Some(_) => "error",
        };
        let error = match status {
            "ok" => None,
            _ => Some(
                ret.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("脚本返回错误状态")
                    .to_string(),
            ),
        };
        let payload = if ret.is_null() {
            None
        } else {
            Some(normalize_quotas(ret))
        };
        BalanceResult {
            status: status.to_string(),
            payload,
            error,
            queried_at: now,
        }
    }
}

/// key 的展示掩码：空串 → `""`；长度 ≤ 4 → `****`；≤ 10 → 前 2 位 + `…`；
/// 更长 → 前 6 位 + `…` + 后 4 位。结果的 key_label 只保存掩码，不保存完整 key。
fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len == 0 {
        return String::new();
    }
    if len <= 4 {
        return "****".to_string();
    }
    if len <= 10 {
        let prefix: String = chars[..2].iter().collect();
        return format!("{prefix}…");
    }
    let prefix: String = chars[..6].iter().collect();
    let suffix: String = chars[len - 4..].iter().collect();
    format!("{prefix}…{suffix}")
}

/// 归一化 `payload.quotas`：snake_case → [`BalanceQuota`]（camelCase），并补齐 used/left
/// percent 互补值。金额模式字段（`unit`/`used_amount`/`left_amount`）随结构原样透传，
/// amount 之间及与 percent 之间均不做互补互推。
///
/// `quotas` 缺失或形状不符（不是数组、元素缺 `label`、取值类型不对）时**整份 payload
/// 原样返回**——脚本常在失败时把诊断上下文塞进自定义字段，不因一个畸形数组丢掉整份
/// 返回值。
fn normalize_quotas(ret: &Value) -> Value {
    let Some(quotas) = ret.get("quotas") else {
        return ret.clone();
    };
    let Ok(mut parsed) = serde_json::from_value::<Vec<BalanceQuota>>(quotas.clone()) else {
        return ret.clone();
    };
    for q in &mut parsed {
        match (q.used_percent, q.left_percent) {
            (Some(u), None) => q.left_percent = Some((100.0 - u).clamp(0.0, 100.0)),
            (None, Some(l)) => q.used_percent = Some((100.0 - l).clamp(0.0, 100.0)),
            _ => {}
        }
    }
    let Some(value) = serde_json::to_value(parsed).ok() else {
        return ret.clone();
    };
    let mut out = ret.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("quotas".to_string(), value);
    }
    out
}

/// 列出全部卡片视图（按 position、created_at 排序，逐张附上逐 key 的最近一次结果）。
///
/// REST 与 IPC 两个宿主共用，保证 `list` / `refresh_all` 的返回形状一致。
pub fn list_views(db: &Database) -> moonbridge_store::Result<Vec<BalanceCardView>> {
    let mut out = Vec::new();
    for card in db.list_balance_cards()? {
        let results = db.list_balance_results(&card.key)?;
        out.push(BalanceCardView { card, results });
    }
    Ok(out)
}

/// 取单张卡片的视图（卡片不存在时 `Ok(None)`）。
pub fn view_of(db: &Database, key: &str) -> moonbridge_store::Result<Option<BalanceCardView>> {
    let Some(card) = db.get_balance_card(key)? else {
        return Ok(None);
    };
    let results = db.list_balance_results(key)?;
    Ok(Some(BalanceCardView { card, results }))
}

/// 启动余额定时调度（常驻任务：应用存活期间一直跑，不提供停止句柄）。
///
/// 每 [`SCHEDULE_TICK`] 扫一次到期卡片（`enabled && interval_secs > 0 && 距上次查询
/// 已达间隔`），串行执行并落库。单卡失败乃至 panic 都不影响循环——卡片是用户可编辑
/// 的脚本，任何一张坏卡都不该让整个看板停摆。
///
/// `engine` 由调用方提供（与手动刷新共用一个），`plugins_dir` 在此钉到它上面，
/// 避免两个宿主各自维护一份脚本目录来源。
pub fn spawn_balance_scheduler(
    db: Arc<Database>,
    engine: BalanceEngine,
    plugins_dir: Option<PathBuf>,
) -> tokio::task::JoinHandle<()> {
    let engine = engine.with_plugins_dir(plugins_dir);
    tokio::spawn(run_balance_loop(db, engine, SCHEDULE_TICK))
}

/// 调度循环体（`tick` 决定扫描周期，测试可传小值）。
pub async fn run_balance_loop(db: Arc<Database>, engine: BalanceEngine, tick: Duration) {
    loop {
        tokio::time::sleep(tick).await;
        let due = match db.list_balance_cards_due(now_unix()) {
            Ok(cards) => cards,
            Err(e) => {
                tracing::error!(error = %e, "读取到期余额卡片失败");
                continue;
            }
        };
        for card in due {
            // 单卡 panic 不得中断调度循环：交给独立 task，panic 收敛为 JoinError
            let engine = engine.clone();
            let key = card.key.clone();
            let handle = tokio::spawn(async move { engine.run_card(&card).await });
            if let Err(e) = handle.await {
                tracing::error!(card = %key, error = %e, "余额卡片执行失败");
            }
        }
    }
}
