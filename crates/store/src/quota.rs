//! 配额查询 DAO：Provider 绑定字段 + 按端点拆行的最近一次查询结果。
//!
//! 配额查询绑定直接长在 Provider 上（`quota_plugin_ref`/`quota_interval_secs`/
//! `quota_enabled`/`quota_config_enc`），key 的唯一来源是 provider 端点。
//! [`QuotaKeyResult`] 是引擎落库的**最近一次**结果，按 `(provider_key, key_index)`
//! 拆行（key_index 即 `provider_endpoints.idx`）。删除 Provider 时结果行级联删除
//! （见 `dao/provider.rs` 的 `delete_provider`）。

use rusqlite::params;

use crate::error::Result;
use crate::models::{Provider, ProviderQuotaView, QuotaKeyResult, QuotaResult};
use crate::Database;

/// 配额定时查询间隔的下限（秒）：`1..=59` 一律抬到 60，避免脚本被高频轮询。
pub const MIN_INTERVAL_SECS: i64 = 60;

/// 归一化定时间隔：负数按 0（禁用）处理，`1..=59` 夹到 [`MIN_INTERVAL_SECS`]。
pub fn clamp_interval_secs(interval_secs: i64) -> i64 {
    match interval_secs {
        v if v <= 0 => 0,
        v if v < MIN_INTERVAL_SECS => MIN_INTERVAL_SECS,
        v => v,
    }
}

fn row_to_key_result(r: &rusqlite::Row) -> rusqlite::Result<QuotaKeyResult> {
    let payload_json: Option<String> = r.get(3)?;
    Ok(QuotaKeyResult {
        key_index: r.get(0)?,
        key_label: r.get(1)?,
        result: QuotaResult {
            status: r.get(2)?,
            payload: payload_json.and_then(|s| serde_json::from_str(&s).ok()),
            error: r.get(4)?,
            queried_at: r.get(5)?,
        },
    })
}

/// 单 key 结果列（不含 provider_key——调用方已知）。
const KEY_RESULT_COLS: &str = "key_index,key_label,status,payload_json,error,queried_at";

impl Database {
    /// 列出到期待查询的 Provider：已启用配额查询、绑定了插件、间隔非 0、且距上次
    /// 查询已达间隔（从未查询过视为到期）。
    ///
    /// Provider 的「上次查询」取各结果行 `queried_at` 的最大值（最近一次全量刷新
    /// 时刻）；无任何结果行时记 -1，使 `now - queried_at >= interval` 恒成立。
    pub fn list_quota_due(&self, now: i64) -> Result<Vec<Provider>> {
        let keys: Vec<String> = {
            let conn = self.conn.lock();
            let mut stmt = conn.prepare(
                "SELECT p.key FROM providers p
                 LEFT JOIN (
                     SELECT provider_key, MAX(queried_at) AS queried_at
                     FROM quota_results GROUP BY provider_key
                 ) r ON r.provider_key = p.key
                 WHERE p.quota_enabled = 1 AND p.quota_plugin_ref <> ''
                   AND p.quota_interval_secs > 0
                   AND ?1 - COALESCE(r.queried_at, -1) >= p.quota_interval_secs
                 ORDER BY p.key",
            )?;
            let rows = stmt.query_map(params![now], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut out = Vec::new();
        for key in keys {
            if let Some(p) = self.get_provider(&key)? {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// 列出绑定了配额插件的 Provider 视图（绑定信息 + 逐 key 最近结果，按 key 排序）。
    pub fn list_quota_views(&self) -> Result<Vec<ProviderQuotaView>> {
        let providers = self.list_providers()?;
        let mut out = Vec::new();
        for p in providers {
            if p.quota_plugin_ref.is_empty() {
                continue;
            }
            let results = self.list_quota_results(&p.key)?;
            out.push(ProviderQuotaView {
                provider_key: p.key,
                quota_plugin_ref: p.quota_plugin_ref,
                quota_interval_secs: p.quota_interval_secs,
                quota_enabled: p.quota_enabled,
                key_count: p.endpoints.len() as i64,
                results,
            });
        }
        Ok(out)
    }

    /// 读取某 Provider 全部 key 的最近一次查询结果（按 key_index 升序）。
    pub fn list_quota_results(&self, provider_key: &str) -> Result<Vec<QuotaKeyResult>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {KEY_RESULT_COLS} FROM quota_results WHERE provider_key = ?1 ORDER BY key_index"
        ))?;
        let rows = stmt.query_map(params![provider_key], row_to_key_result)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 读取某 Provider 单个 key 的最近一次查询结果（失败分层时取保留值用）。
    pub fn get_quota_key_result(
        &self,
        provider_key: &str,
        key_index: i64,
    ) -> Result<Option<QuotaKeyResult>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {KEY_RESULT_COLS} FROM quota_results WHERE provider_key = ?1 AND key_index = ?2"
        ))?;
        let mut rows = stmt.query_map(params![provider_key, key_index], row_to_key_result)?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 写入（覆盖）某 Provider 单个 key 的最近一次查询结果。
    pub fn upsert_quota_key_result(
        &self,
        provider_key: &str,
        result: &QuotaKeyResult,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let payload = result
            .result
            .payload
            .as_ref()
            .filter(|v| !v.is_null())
            .map(|v| v.to_string());
        conn.execute(
            "INSERT INTO quota_results (provider_key,key_index,key_label,status,payload_json,error,queried_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(provider_key, key_index) DO UPDATE SET
                key_label=excluded.key_label, status=excluded.status,
                payload_json=excluded.payload_json, error=excluded.error,
                queried_at=excluded.queried_at",
            params![
                provider_key,
                result.key_index,
                result.key_label,
                result.result.status,
                payload,
                result.result.error,
                result.result.queried_at
            ],
        )?;
        Ok(())
    }

    /// 剪掉某 Provider key_index >= `keep` 的结果行：端点变少后，旧 key 的
    /// 残留行不应再出现在看板上。
    pub fn prune_quota_results(&self, provider_key: &str, keep: i64) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM quota_results WHERE provider_key = ?1 AND key_index >= ?2",
            params![provider_key, keep],
        )?;
        Ok(())
    }
}

/// 内置配额插件种子的**播种标记**：首次启动由
/// [`moonbridge_gateway::seed_builtin_quota_plugins`] 播入 `plugins` 表，
/// 已播过种的库不再重播（用户删掉的内置插件不会复活）。
pub const QUOTA_SEEDS_DONE: &str = "quota_seeds_v1";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    use crate::models::{Endpoint, Provider};

    /// 构造一个带配额绑定的 Provider。
    fn provider(key: &str, interval: i64, enabled: bool) -> Provider {
        Provider {
            key: key.to_string(),
            endpoints: vec![
                Endpoint {
                    protocol: "openai-chat".to_string(),
                    base_url: "https://a".to_string(),
                    api_key: "sk-a".to_string(),
                },
                Endpoint {
                    protocol: "openai-chat".to_string(),
                    base_url: "https://b".to_string(),
                    api_key: "sk-b".to_string(),
                },
            ],
            version: None,
            user_agent: None,
            web_search: None,
            extra: Value::Null,
            enabled: true,
            quota_plugin_ref: "quota/test".to_string(),
            quota_interval_secs: interval,
            quota_enabled: enabled,
            quota_config: Value::Null,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn key_result(idx: i64, label: &str, status: &str, payload: Option<Value>) -> QuotaKeyResult {
        QuotaKeyResult {
            key_index: idx,
            key_label: label.to_string(),
            result: QuotaResult {
                status: status.to_string(),
                payload,
                error: None,
                queried_at: 100,
            },
        }
    }

    #[test]
    fn clamp_interval_boundaries() {
        assert_eq!(clamp_interval_secs(-5), 0, "负数按禁用处理");
        assert_eq!(clamp_interval_secs(0), 0, "0 表示禁用定时");
        assert_eq!(clamp_interval_secs(1), MIN_INTERVAL_SECS);
        assert_eq!(clamp_interval_secs(59), MIN_INTERVAL_SECS);
        assert_eq!(clamp_interval_secs(60), 60, "恰好达到下限不改变");
        assert_eq!(clamp_interval_secs(3600), 3600, "高于下限原样保留");
    }

    #[test]
    fn quota_config_encrypts_and_roundtrips() {
        let db = Database::open_in_memory().unwrap();
        let mut p = provider("p", 60, true);
        p.quota_config = json!({"management_token": "secret-tok", "unit": "$"});
        db.upsert_provider(&p).unwrap();

        // 加密落库的覆盖在 tests/encryption.rs（AES）；内存库走 PlaintextKey，
        // 这里只验证经 enc 往返的语义：quota_config_enc 非空且读回解密正确。
        {
            let conn = db.conn.lock();
            let enc: String = conn
                .query_row(
                    "SELECT quota_config_enc FROM providers WHERE key = 'p'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!enc.is_empty(), "有配置应写入 quota_config_enc");
        }

        let got = db.get_provider("p").unwrap().unwrap();
        assert_eq!(got.quota_config["management_token"], json!("secret-tok"));
        assert_eq!(got.quota_plugin_ref, "quota/test");
        assert_eq!(got.quota_interval_secs, 60);
        assert!(got.quota_enabled);

        // 清空配置 → 空串落库、读回 Null
        let mut p2 = provider("p", 0, false);
        p2.quota_config = Value::Null;
        db.upsert_provider(&p2).unwrap();
        {
            let conn = db.conn.lock();
            let enc: String = conn
                .query_row(
                    "SELECT quota_config_enc FROM providers WHERE key = 'p'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(enc.is_empty(), "空配置不落密文");
        }
        let got = db.get_provider("p").unwrap().unwrap();
        assert!(got.quota_config.is_null());
        assert_eq!(got.quota_plugin_ref, "quota/test");
        assert!(!got.quota_enabled);
    }

    #[test]
    fn due_providers_cover_states() {
        let db = Database::open_in_memory().unwrap();
        let now = 1_000_000;
        // 从未查询（无结果行）→ 到期
        db.upsert_provider(&provider("never", 60, true)).unwrap();
        // 刚查询 → 未到期
        db.upsert_provider(&provider("fresh", 60, true)).unwrap();
        let mut fresh = key_result(0, "", "ok", Some(json!({})));
        fresh.result.queried_at = now;
        db.upsert_quota_key_result("fresh", &fresh).unwrap();
        // 结果陈旧 → 到期
        db.upsert_provider(&provider("stale", 60, true)).unwrap();
        let mut stale = key_result(0, "", "ok", Some(json!({})));
        stale.result.queried_at = now - 60;
        db.upsert_quota_key_result("stale", &stale).unwrap();
        // 多 key 按各行 MAX(queried_at) 判定：有一个新鲜行就不到期
        db.upsert_provider(&provider("multi", 60, true)).unwrap();
        let mut old_row = key_result(0, "k0", "ok", Some(json!({})));
        old_row.result.queried_at = now - 600;
        db.upsert_quota_key_result("multi", &old_row).unwrap();
        let mut new_row = key_result(1, "k1", "ok", Some(json!({})));
        new_row.result.queried_at = now;
        db.upsert_quota_key_result("multi", &new_row).unwrap();
        // 未启用 → 排除
        db.upsert_provider(&provider("off", 60, false)).unwrap();
        // interval = 0 → 排除
        db.upsert_provider(&provider("manual", 0, true)).unwrap();
        // 未绑定插件 → 排除
        let mut unbound = provider("unbound", 60, true);
        unbound.quota_plugin_ref.clear();
        db.upsert_provider(&unbound).unwrap();

        let keys: Vec<String> = db
            .list_quota_due(now)
            .unwrap()
            .into_iter()
            .map(|p| p.key)
            .collect();
        assert_eq!(keys, vec!["never", "stale"], "只有从未查询与陈旧的到期");

        assert!(db
            .list_quota_due(now - 1)
            .unwrap()
            .iter()
            .all(|p| p.key != "stale"));
    }

    #[test]
    fn quota_views_only_bound_providers() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_provider(&provider("bound", 60, true)).unwrap();
        let mut unbound = provider("plain", 0, false);
        unbound.quota_plugin_ref.clear();
        db.upsert_provider(&unbound).unwrap();

        db.upsert_quota_key_result(
            "bound",
            &key_result(0, "sk-a", "ok", Some(json!({"summary": "s"}))),
        )
        .unwrap();

        let views = db.list_quota_views().unwrap();
        assert_eq!(views.len(), 1, "未绑定 provider 不进配额视图");
        assert_eq!(views[0].provider_key, "bound");
        assert_eq!(views[0].key_count, 2);
        assert_eq!(views[0].results.len(), 1);
    }

    #[test]
    fn delete_provider_cascades_quota_results() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_provider(&provider("a", 60, true)).unwrap();
        db.upsert_quota_key_result("a", &key_result(0, "sk-a", "ok", Some(json!({}))))
            .unwrap();

        db.delete_provider("a").unwrap();
        assert!(db.list_quota_results("a").unwrap().is_empty());
    }

    #[test]
    fn key_results_upsert_get_list_and_prune() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.list_quota_results("a").unwrap().is_empty());
        assert!(db.get_quota_key_result("a", 0).unwrap().is_none());

        let payload = json!({
            "quotas": [{"type": "percentage", "label": "5 小时", "usedPercent": 43.0, "leftPercent": 57.0}],
            "summary": "余额正常",
        });
        db.upsert_quota_key_result("a", &key_result(1, "sk-b", "ok", Some(payload.clone())))
            .unwrap();
        db.upsert_quota_key_result(
            "a",
            &key_result(0, "sk-a", "ok", Some(json!({"summary": "first"}))),
        )
        .unwrap();

        let rows = db.list_quota_results("a").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key_label, "sk-a");
        assert_eq!(rows[1].result.payload, Some(payload));

        let mut failed = key_result(1, "sk-b", "error", None);
        failed.result.error = Some("上游 500".to_string());
        failed.result.queried_at = 200;
        db.upsert_quota_key_result("a", &failed).unwrap();
        let got = db.get_quota_key_result("a", 1).unwrap().unwrap();
        assert_eq!(got.result.status, "error");

        db.prune_quota_results("a", 1).unwrap();
        let rows = db.list_quota_results("a").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key_index, 0);
    }
}
