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
use std::sync::{Arc, Mutex};
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

pub use crate::balance_http::BalanceNetworkPolicy;
use crate::parse_script_ref;
use crate::ScriptRef;

/// 单次查询的整体超时（含脚本加载 + `MB.query` 执行）。
const QUERY_TIMEOUT: Duration = Duration::from_secs(45);

/// 调度器扫描周期：每轮挑出到期卡片串行执行。
pub const SCHEDULE_TICK: Duration = Duration::from_secs(30);

/// 当前 unix 时间戳（秒）。口径与 store 的 `created_at` 一致。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 余额脚本的宿主桥：只提供受网络策略约束的 `mb.http.request`。
struct BalanceBridge {
    network_policy: BalanceNetworkPolicy,
    base_url: String,
    network_error: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl HostBridge for BalanceBridge {
    async fn http_request(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        let result = self.network_policy.request(&self.base_url, req).await;
        if let Err(error) = &result {
            if let Ok(mut network_error) = self.network_error.lock() {
                *network_error = Some(error.clone());
            }
        }
        result
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

/// 余额查询引擎：持库句柄、脚本根目录与出站网络策略。
#[derive(Clone)]
pub struct BalanceEngine {
    db: Arc<Database>,
    /// 脚本根目录：`script_ref` 里相对 `.lua` 归一到此处，绝对路径必须落在其内。
    /// `None` 时不做包含性校验（CLI/测试场景，与插件加载侧同语义）。
    plugins_dir: Option<PathBuf>,
    network_policy: BalanceNetworkPolicy,
}

impl BalanceEngine {
    pub fn new(db: Arc<Database>, plugins_dir: Option<PathBuf>) -> Self {
        BalanceEngine {
            db,
            plugins_dir,
            network_policy: BalanceNetworkPolicy::from_environment(None),
        }
    }

    pub fn with_network_policy(mut self, network_policy: BalanceNetworkPolicy) -> Self {
        self.network_policy = network_policy;
        self
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
    pub async fn run_card(
        &self,
        card: &BalanceCard,
    ) -> moonbridge_store::Result<Vec<BalanceKeyResult>> {
        let now = now_unix();
        let (keys, provider_name) = match self.resolve_keys(card)? {
            Ok(v) => v,
            Err(message) => {
                let result = self.engine_error_result(&card.key, 0, message, now)?;
                let kr = BalanceKeyResult {
                    key_index: 0,
                    key_label: String::new(),
                    result,
                };
                self.store_key_result(&card.key, &kr)?;
                self.db.prune_balance_results(&card.key, 1)?;
                return Ok(vec![kr]);
            }
        };
        let mut out = Vec::with_capacity(keys.len());
        for (idx, key) in keys.iter().enumerate() {
            out.push(
                self.run_key(card, idx as i64, key, &provider_name, now)
                    .await?,
            );
        }
        self.db
            .prune_balance_results(&card.key, keys.len() as i64)?;
        Ok(out)
    }

    /// 刷新一张卡片并回传视图（供 REST/IPC）。`key_index` 为 `Some` 时只重跑该 key
    /// （越界或 Provider 引用失效时不动作，直接回读现状）；数据库失败始终上抛。
    pub async fn refresh_card(
        &self,
        card: &BalanceCard,
        key_index: Option<i64>,
    ) -> moonbridge_store::Result<BalanceCardView> {
        match key_index {
            None => {
                self.run_card(card).await?;
            }
            Some(idx) => {
                if let Ok((keys, provider_name)) = self.resolve_keys(card)? {
                    if idx >= 0 {
                        if let Some(key) = keys.get(idx as usize).cloned() {
                            self.run_key(card, idx, &key, &provider_name, now_unix())
                                .await?;
                        }
                    }
                }
            }
        }
        Ok(
            view_of(&self.db, &card.key)?.unwrap_or_else(|| BalanceCardView {
                card: card.clone(),
                results: Vec::new(),
            }),
        )
    }

    /// 试运行一张卡片：不写库、不读历史结果，所有失败仅返回预览结果。
    pub async fn test_card(&self, card: &BalanceCard) -> Vec<BalanceKeyResult> {
        let now = now_unix();
        let resolved = self
            .resolve_keys(card)
            .map_err(|e| e.to_string())
            .and_then(|v| v);
        let (keys, provider_name) = match resolved {
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

    /// 串行刷新全部启用卡片，返回全部卡片视图；数据库失败中断刷新并上抛。
    pub async fn refresh_all(&self) -> moonbridge_store::Result<Vec<BalanceCardView>> {
        for card in self
            .db
            .list_balance_cards()?
            .into_iter()
            .filter(|c| c.enabled)
        {
            self.run_card(&card).await?;
        }
        list_views(&self.db)
    }

    async fn run_key(
        &self,
        card: &BalanceCard,
        key_index: i64,
        key: &str,
        provider_name: &str,
        now: i64,
    ) -> moonbridge_store::Result<BalanceKeyResult> {
        let ctx = Self::ctx_for(card, key, provider_name);
        let result = match self.query(card, &ctx).await {
            Ok(ret) => Self::from_script_return(&ret, now),
            Err(message) => self.engine_error_result(&card.key, key_index, message, now)?,
        };
        let kr = BalanceKeyResult {
            key_index,
            key_label: mask_key(key),
            result,
        };
        self.store_key_result(&card.key, &kr)?;
        Ok(kr)
    }

    /// 引擎级失败的结果：`payload` 保留该 key 上一次落库的值。
    fn engine_error_result(
        &self,
        card_key: &str,
        key_index: i64,
        message: String,
        now: i64,
    ) -> moonbridge_store::Result<BalanceResult> {
        let previous = self.db.get_balance_key_result(card_key, key_index)?;
        Ok(BalanceResult {
            status: "error".to_string(),
            payload: previous.and_then(|p| p.result.payload),
            error: Some(message),
            queried_at: now,
        })
    }

    fn store_key_result(
        &self,
        card_key: &str,
        kr: &BalanceKeyResult,
    ) -> moonbridge_store::Result<()> {
        self.db.upsert_balance_key_result(card_key, kr)
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
    fn resolve_keys(
        &self,
        card: &BalanceCard,
    ) -> moonbridge_store::Result<Result<(Vec<String>, String), String>> {
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
            return Ok(Ok((keys, provider_name)));
        }

        if let Some(pk) = provider_key {
            let Some(provider) = self.db.get_provider(pk)? else {
                return Ok(Err(format!("上游服务 {pk} 不存在")));
            };
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
        Ok(Ok((keys, provider_name)))
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
        let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
        let query = async {
            let engine = self.clone();
            let script_ref = card.script_ref.clone();
            let script = tokio::task::spawn_blocking(move || engine.read_script(&script_ref))
                .await
                .map_err(|_| "读取余额脚本失败".to_string())??;
            let network_error = Arc::new(Mutex::new(None));
            let bridge = Arc::new(BalanceBridge {
                network_policy: self.network_policy.clone(),
                base_url: card.base_url.clone(),
                network_error: network_error.clone(),
            });
            let limits = SandboxLimits {
                call_timeout: deadline.saturating_duration_since(tokio::time::Instant::now()),
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
            .map_err(|_| "余额脚本加载失败".to_string())?;
            if tokio::time::Instant::now() >= deadline {
                return Err("余额脚本查询超时".to_string());
            }
            runtime.call_mb_once("query", ctx).await.map_err(|_| {
                network_error
                    .lock()
                    .ok()
                    .and_then(|mut error| error.take())
                    .unwrap_or_else(|| "余额脚本执行失败".to_string())
            })
        };
        tokio::time::timeout_at(deadline, query)
            .await
            .map_err(|_| format!("查询超时（{} 秒）", QUERY_TIMEOUT.as_secs()))?
    }

    /// 只有对象中的显式 `status = "ok"` 视为成功；业务错误更新 payload。
    /// `quotas` 按 [`BalanceQuota`] 归一化，空数组也是有效结果。
    fn from_script_return(ret: &Value, now: i64) -> BalanceResult {
        let valid_status = ret
            .as_object()
            .and_then(|obj| obj.get("status"))
            .and_then(Value::as_str);
        let status = if valid_status == Some("ok") {
            "ok"
        } else {
            "error"
        };
        let error = match valid_status {
            Some("ok") => None,
            Some("error") => Some(
                ret.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("脚本返回错误状态")
                    .to_string(),
            ),
            _ => Some("余额脚本必须返回包含有效 status（ok/error）的对象".to_string()),
        };
        let payload = ret.is_object().then(|| normalize_quotas(ret));
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
    if quotas.as_object().is_some_and(|object| object.is_empty()) {
        let mut out = ret.clone();
        out["quotas"] = json!([]);
        return out;
    }
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
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(async move { engine.run_card(&card).await });
            match tasks.join_next().await {
                Some(Ok(Ok(_))) | None => {}
                Some(Ok(Err(e))) => {
                    tracing::error!(card = %key, error = %e, "余额卡片存储失败");
                }
                Some(Err(e)) => {
                    tracing::error!(card = %key, error = %e, "余额卡片执行失败");
                }
            }
        }
    }
}

#[cfg(test)]
mod db_failure_tests {
    use super::*;
    use moonbridge_store::{PlaintextKey, StoreError};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct DatabaseDirectory(PathBuf);

    impl DatabaseDirectory {
        fn execute(&self, sql: &str) {
            rusqlite::Connection::open(self.0.join("balance.sqlite"))
                .unwrap()
                .execute_batch(sql)
                .unwrap();
        }

        fn reject_result_writes(&self) {
            self.execute(
                "CREATE TRIGGER reject_results BEFORE INSERT ON balance_results
                 BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;",
            );
        }
    }

    impl Drop for DatabaseDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn file_database() -> (DatabaseDirectory, Arc<Database>, BalanceCard) {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "moonbridge-balance-db-failure-{}-{timestamp}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = DatabaseDirectory(directory);
        let db =
            Database::open_with_key(directory.0.join("balance.sqlite"), Box::new(PlaintextKey))
                .unwrap();
        let card = BalanceCard {
            key: "stored-card".into(),
            provider_key: None,
            display_mode: "auto".into(),
            api_key: "manual-test-key".into(),
            base_url: String::new(),
            provider_label: String::new(),
            script_ref:
                "MB = {}\nfunction MB.query(ctx) return { status = 'ok', summary = 'queried' } end"
                    .into(),
            interval_secs: 60,
            enabled: true,
            extra: json!({}),
            position: 0,
            created_at: 0,
            updated_at: 0,
        };
        db.upsert_balance_card(&card).unwrap();
        let stored = db.get_balance_card(&card.key).unwrap().unwrap();
        assert_eq!(stored.api_key, card.api_key);
        assert_eq!(stored.script_ref, card.script_ref);
        assert!(stored.enabled);
        assert_eq!(db.list_balance_cards().unwrap().len(), 1);
        assert!(db.list_balance_results(&card.key).unwrap().is_empty());
        (directory, Arc::new(db), card)
    }

    fn assert_sqlite_error(error: StoreError, expected: &str) {
        match error {
            StoreError::Db(error) => assert_eq!(error.to_string(), expected),
            error => panic!("expected SQLite error {expected:?}, got {error:?}"),
        }
    }

    async fn assert_query_succeeds(engine: &BalanceEngine, card: &BalanceCard) {
        let preview = engine.test_card(card).await;
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].result.status, "ok");
        assert_eq!(preview[0].result.error, None);
        assert_eq!(
            preview[0].result.payload.as_ref().unwrap()["summary"],
            "queried"
        );
    }

    #[tokio::test]
    async fn refresh_all_propagates_card_list_failure() {
        let (directory, db, card) = file_database();
        let engine = BalanceEngine::new(db.clone(), None);
        assert_query_succeeds(&engine, &card).await;
        directory.execute("DROP TABLE balance_cards;");
        assert_sqlite_error(
            db.list_balance_cards().unwrap_err(),
            "no such table: balance_cards",
        );

        assert_sqlite_error(
            engine.refresh_all().await.unwrap_err(),
            "no such table: balance_cards",
        );
        assert!(db.list_balance_results(&card.key).unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_card_propagates_card_read_failure() {
        let (directory, db, card) = file_database();
        let engine = BalanceEngine::new(db.clone(), None);
        assert_query_succeeds(&engine, &card).await;
        directory.execute("DROP TABLE balance_cards;");
        assert_sqlite_error(
            db.get_balance_card(&card.key).unwrap_err(),
            "no such table: balance_cards",
        );

        assert_sqlite_error(
            engine.refresh_card(&card, Some(-1)).await.unwrap_err(),
            "no such table: balance_cards",
        );
        assert!(db.list_balance_results(&card.key).unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_card_propagates_result_list_failure() {
        let (directory, db, card) = file_database();
        let engine = BalanceEngine::new(db.clone(), None);
        assert_query_succeeds(&engine, &card).await;
        directory.execute("DROP TABLE balance_results;");
        assert_sqlite_error(
            db.list_balance_results(&card.key).unwrap_err(),
            "no such table: balance_results",
        );

        assert_sqlite_error(
            engine.refresh_card(&card, Some(-1)).await.unwrap_err(),
            "no such table: balance_results",
        );
        assert!(db.get_balance_card(&card.key).unwrap().is_some());
    }

    #[tokio::test]
    async fn run_card_propagates_result_write_failure() {
        let (directory, db, card) = file_database();
        let engine = BalanceEngine::new(db.clone(), None);
        assert_query_succeeds(&engine, &card).await;
        directory.reject_result_writes();

        assert_sqlite_error(
            engine.run_card(&card).await.unwrap_err(),
            "synthetic write failure",
        );
        assert!(db.list_balance_results(&card.key).unwrap().is_empty());

        directory.execute("DROP TRIGGER reject_results;");
        let results = engine.run_card(&card).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].result.status, "ok");
        assert_eq!(db.list_balance_results(&card.key).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn refresh_card_propagates_result_write_failure_for_full_and_single_key_refresh() {
        let (directory, db, card) = file_database();
        let engine = BalanceEngine::new(db.clone(), None);
        assert_query_succeeds(&engine, &card).await;
        directory.reject_result_writes();

        for key_index in [None, Some(0)] {
            assert_sqlite_error(
                engine.refresh_card(&card, key_index).await.unwrap_err(),
                "synthetic write failure",
            );
            assert!(db.list_balance_results(&card.key).unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn refresh_all_propagates_result_write_failure() {
        let (directory, db, card) = file_database();
        let engine = BalanceEngine::new(db.clone(), None);
        assert_query_succeeds(&engine, &card).await;
        directory.reject_result_writes();

        assert_sqlite_error(
            engine.refresh_all().await.unwrap_err(),
            "synthetic write failure",
        );
        assert!(db.list_balance_results(&card.key).unwrap().is_empty());
        assert_eq!(db.list_balance_cards().unwrap().len(), 1);
    }
}
