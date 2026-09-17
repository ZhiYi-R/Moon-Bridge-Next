//! 余额&健康看板 DAO：卡片配置 + 按 key 拆行的最近一次查询结果。
//!
//! [`BalanceCard`] 是用户配置（脚本引用、查询间隔、自定义参数），一卡一行；
//! [`BalanceKeyResult`] 是引擎落库的**最近一次**结果，按 `(card_key, key_index)`
//! 拆行（卡片有几个有效 key 就有几行）。删除卡片时结果行在同一事务内级联
//! 删除，避免残留孤儿行。

use rusqlite::params;
use serde_json::Value;

use crate::error::Result;
use crate::models::{BalanceCard, BalanceKeyResult, BalanceResult};
use crate::Database;

/// 卡片定时间隔的下限（秒）：`1..=59` 一律抬到 60，避免脚本被高频轮询。
pub const MIN_INTERVAL_SECS: i64 = 60;

/// 归一化定时间隔：负数按 0（禁用）处理，`1..=59` 夹到 [`MIN_INTERVAL_SECS`]。
pub fn clamp_interval_secs(interval_secs: i64) -> i64 {
    match interval_secs {
        v if v <= 0 => 0,
        v if v < MIN_INTERVAL_SECS => MIN_INTERVAL_SECS,
        v => v,
    }
}

fn row_to_card(r: &rusqlite::Row) -> rusqlite::Result<BalanceCard> {
    let extra_json: String = r.get(7)?;
    Ok(BalanceCard {
        key: r.get(0)?,
        api_key: r.get(1)?,
        base_url: r.get(2)?,
        provider_label: r.get(3)?,
        script_ref: r.get(4)?,
        interval_secs: r.get(5)?,
        enabled: r.get::<_, i32>(6)? != 0,
        extra: serde_json::from_str(&extra_json).unwrap_or(Value::Null),
        position: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        provider_key: r.get(11)?,
        display_mode: r.get(12)?,
    })
}

fn row_to_key_result(r: &rusqlite::Row) -> rusqlite::Result<BalanceKeyResult> {
    let payload_json: Option<String> = r.get(3)?;
    Ok(BalanceKeyResult {
        key_index: r.get(0)?,
        key_label: r.get(1)?,
        result: BalanceResult {
            status: r.get(2)?,
            payload: payload_json.and_then(|s| serde_json::from_str(&s).ok()),
            error: r.get(4)?,
            queried_at: r.get(5)?,
        },
    })
}

/// 单 key 结果列（不含 card_key——调用方已知）。
const KEY_RESULT_COLS: &str = "key_index,key_label,status,payload_json,error,queried_at";

const CARD_COLS: &str = "key,api_key,base_url,provider_label,script_ref,interval_secs,enabled,extra_json,position,created_at,updated_at,provider_key,display_mode";

/// 同上，带 `c.` 前缀（到期待查询的查询里与 `balance_results` 联表，需消歧义）。
const CARD_COLS_PREFIXED: &str = "c.key,c.api_key,c.base_url,c.provider_label,c.script_ref,c.interval_secs,c.enabled,c.extra_json,c.position,c.created_at,c.updated_at,c.provider_key,c.display_mode";

/// 归一化显示样式：非法值一律回落 `auto`（引擎不消费此字段，但必须保证落库值合法，
/// 前端才能只做三分支）。
pub fn normalize_display_mode(mode: &str) -> String {
    match mode {
        "percent" | "amount" => mode.to_string(),
        _ => "auto".to_string(),
    }
}

impl Database {
    /// 列出全部卡片（按 position、created_at 升序）。
    pub fn list_balance_cards(&self) -> Result<Vec<BalanceCard>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CARD_COLS} FROM balance_cards ORDER BY position, created_at"
        ))?;
        let rows = stmt.query_map([], row_to_card)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按 key 获取卡片。
    pub fn get_balance_card(&self, key: &str) -> Result<Option<BalanceCard>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CARD_COLS} FROM balance_cards WHERE key = ?1"
        ))?;
        let mut rows = stmt.query_map(params![key], row_to_card)?;
        match rows.next() {
            Some(Ok(c)) => Ok(Some(c)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 列出到期待查询的卡片：已启用、间隔非 0、且距上次查询已达间隔（从未查询过视为到期）。
    ///
    /// 结果按 key 拆行后，卡片的「上次查询」取各行 `queried_at` 的最大值（最近一次
    /// 全量刷新时刻）；无任何结果行时记 -1，使 `now - queried_at >= interval` 恒成立。
    pub fn list_balance_cards_due(&self, now: i64) -> Result<Vec<BalanceCard>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CARD_COLS_PREFIXED} FROM balance_cards c
             LEFT JOIN (
                 SELECT card_key, MAX(queried_at) AS queried_at
                 FROM balance_results GROUP BY card_key
             ) r ON r.card_key = c.key
             WHERE c.enabled = 1 AND c.interval_secs > 0
               AND ?1 - COALESCE(r.queried_at, -1) >= c.interval_secs
             ORDER BY c.position, c.created_at"
        ))?;
        let rows = stmt.query_map(params![now], row_to_card)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 插入或更新卡片：`created_at` 首次写入后固定，`updated_at` 每次刷新。
    ///
    /// `interval_secs` 经 [`clamp_interval_secs`] 归一，保证任何保存路径落库的都是
    /// 合法间隔（`0` 禁用，其余 ≥ 60）。
    pub fn upsert_balance_card(&self, card: &BalanceCard) -> Result<()> {
        let conn = self.conn.lock();
        let now = crate::now_unix();
        let extra = if card.extra.is_null() {
            "{}".to_string()
        } else {
            card.extra.to_string()
        };
        let created = if card.created_at > 0 {
            card.created_at
        } else {
            now
        };
        conn.execute(
            "INSERT INTO balance_cards
                (key,api_key,base_url,provider_label,script_ref,interval_secs,enabled,extra_json,position,created_at,updated_at,provider_key,display_mode)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(key) DO UPDATE SET
                api_key=excluded.api_key, base_url=excluded.base_url,
                provider_label=excluded.provider_label, script_ref=excluded.script_ref,
                interval_secs=excluded.interval_secs, enabled=excluded.enabled,
                extra_json=excluded.extra_json, position=excluded.position,
                provider_key=excluded.provider_key, display_mode=excluded.display_mode,
                updated_at=excluded.updated_at",
            params![
                card.key,
                card.api_key,
                card.base_url,
                card.provider_label,
                card.script_ref,
                clamp_interval_secs(card.interval_secs),
                card.enabled as i32,
                extra,
                card.position,
                created,
                now,
                card.provider_key,
                normalize_display_mode(&card.display_mode),
            ],
        )?;
        Ok(())
    }

    /// 删除卡片，并级联删除其查询结果。事务化，中途失败整体回滚。
    pub fn delete_balance_card(&self, key: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM balance_cards WHERE key = ?1", params![key])?;
        tx.execute(
            "DELETE FROM balance_results WHERE card_key = ?1",
            params![key],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 读取某卡片全部 key 的最近一次查询结果（按 key_index 升序）。
    pub fn list_balance_results(&self, card_key: &str) -> Result<Vec<BalanceKeyResult>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {KEY_RESULT_COLS} FROM balance_results WHERE card_key = ?1 ORDER BY key_index"
        ))?;
        let rows = stmt.query_map(params![card_key], row_to_key_result)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 读取某卡片单个 key 的最近一次查询结果（失败分层时取保留值用）。
    pub fn get_balance_key_result(
        &self,
        card_key: &str,
        key_index: i64,
    ) -> Result<Option<BalanceKeyResult>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {KEY_RESULT_COLS} FROM balance_results WHERE card_key = ?1 AND key_index = ?2"
        ))?;
        let mut rows = stmt.query_map(params![card_key, key_index], row_to_key_result)?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 写入（覆盖）某卡片单个 key 的最近一次查询结果。
    pub fn upsert_balance_key_result(
        &self,
        card_key: &str,
        result: &BalanceKeyResult,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let payload = result
            .result
            .payload
            .as_ref()
            .filter(|v| !v.is_null())
            .map(|v| v.to_string());
        conn.execute(
            "INSERT INTO balance_results (card_key,key_index,key_label,status,payload_json,error,queried_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(card_key, key_index) DO UPDATE SET
                key_label=excluded.key_label, status=excluded.status,
                payload_json=excluded.payload_json, error=excluded.error,
                queried_at=excluded.queried_at",
            params![
                card_key,
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

    /// 剪掉某卡片 key_index >= `keep` 的结果行：Provider 的 key 变少后，旧 key 的
    /// 残留行不应再出现在看板上。
    pub fn prune_balance_results(&self, card_key: &str, keep: i64) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM balance_results WHERE card_key = ?1 AND key_index >= ?2",
            params![card_key, keep],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造一张只关心指定字段的卡片。
    fn card(key: &str, interval: i64, enabled: bool, position: i64) -> BalanceCard {
        BalanceCard {
            key: key.to_string(),
            provider_key: None,
            display_mode: "auto".to_string(),
            api_key: "sk-x".to_string(),
            base_url: "https://example.test".to_string(),
            provider_label: "示例".to_string(),
            script_ref: "return { quotas = {} }".to_string(),
            interval_secs: interval,
            enabled,
            extra: json!({"region": "cn"}),
            position,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn key_result(idx: i64, label: &str, status: &str, payload: Option<Value>) -> BalanceKeyResult {
        BalanceKeyResult {
            key_index: idx,
            key_label: label.to_string(),
            result: BalanceResult {
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
    fn manual_key_list_roundtrips_with_provider_reference() {
        let db = Database::open_in_memory().unwrap();
        let mut c = card("manual-list", 60, true, 0);
        c.provider_key = Some("provider".into());
        c.api_key = " first\r\nsecond\nfirst\n".into();
        db.upsert_balance_card(&c).unwrap();
        let loaded = db.get_balance_card(&c.key).unwrap().unwrap();
        assert_eq!(loaded.api_key, c.api_key);
        assert_eq!(loaded.provider_key, c.provider_key);
        assert_eq!(db.list_balance_cards().unwrap()[0].api_key, c.api_key);
        assert_eq!(
            db.list_balance_cards_due(crate::now_unix()).unwrap()[0].api_key,
            c.api_key
        );
        let json = serde_json::to_value(&loaded).unwrap();
        let decoded: BalanceCard = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.api_key, c.api_key);
        c.api_key.clear();
        db.upsert_balance_card(&c).unwrap();
        let loaded = db.get_balance_card(&c.key).unwrap().unwrap();
        assert!(loaded.api_key.is_empty());
        assert_eq!(loaded.provider_key, c.provider_key);
    }

    #[test]
    fn provider_key_and_display_mode_roundtrip_and_normalize() {
        let db = Database::open_in_memory().unwrap();
        let mut c = card("ref", 300, true, 0);
        c.provider_key = Some("kimi".to_string());
        c.display_mode = "amount".to_string();
        db.upsert_balance_card(&c).unwrap();
        let got = db.get_balance_card("ref").unwrap().unwrap();
        assert_eq!(got.provider_key.as_deref(), Some("kimi"));
        assert_eq!(got.display_mode, "amount");

        // 非法 display_mode 落库即归一；provider_key 可被清回 None（手填模式）
        let mut bad = card("ref", 300, true, 0);
        bad.provider_key = None;
        bad.display_mode = "weird".to_string();
        db.upsert_balance_card(&bad).unwrap();
        let got = db.get_balance_card("ref").unwrap().unwrap();
        assert_eq!(got.provider_key, None, "冲突更新也要能清回手填模式");
        assert_eq!(got.display_mode, "auto", "非法 display_mode 归一为 auto");
    }

    #[test]
    fn card_upsert_get_list_and_conflict_update() {
        let db = Database::open_in_memory().unwrap();
        // 显式固定 created_at，便于断言「首次写入后不再变动」
        let mut a = card("a", 3600, true, 1);
        a.created_at = 1_700_000_000;
        db.upsert_balance_card(&a).unwrap();
        let mut b = card("b", 60, true, 0);
        b.created_at = 1_700_000_001;
        db.upsert_balance_card(&b).unwrap();

        // upsert 总会刷新 updated_at，故只逐字段比对用户配置部分
        let got = db.get_balance_card("a").unwrap().unwrap();
        assert_eq!(got.key, a.key);
        assert_eq!(got.api_key, a.api_key);
        assert_eq!(got.base_url, a.base_url);
        assert_eq!(got.provider_label, a.provider_label);
        assert_eq!(got.script_ref, a.script_ref);
        assert_eq!(got.interval_secs, a.interval_secs);
        assert_eq!(got.enabled, a.enabled);
        assert_eq!(got.extra, a.extra, "extra 应 JSON 往返");
        assert_eq!(got.position, a.position);
        assert_eq!(got.created_at, a.created_at);
        assert!(got.updated_at > 0, "updated_at 由 DAO 刷新");
        assert!(db.get_balance_card("missing").unwrap().is_none());

        let keys: Vec<String> = db
            .list_balance_cards()
            .unwrap()
            .into_iter()
            .map(|c| c.key)
            .collect();
        assert_eq!(keys, vec!["b", "a"], "列表按 position 升序");

        // 冲突更新：created_at 固定、interval 夹逼、updated_at 前进
        let mut changed = card("a", 5, false, 1);
        changed.created_at = 1_234_567; // 冲突路径必须忽略入参 created_at
        changed.script_ref = "return { status = \"ok\" }".to_string();
        db.upsert_balance_card(&changed).unwrap();
        let after = db.get_balance_card("a").unwrap().unwrap();
        assert_eq!(after.created_at, 1_700_000_000, "created_at 首次写入后固定");
        assert_eq!(after.interval_secs, 60, "1..=59 夹到下限");
        assert_eq!(after.script_ref, "return { status = \"ok\" }");
        assert!(!after.enabled);
        assert!(
            after.updated_at >= got.updated_at,
            "updated_at 只前进不后退: {} -> {}",
            got.updated_at,
            after.updated_at
        );
        assert_eq!(
            db.list_balance_cards().unwrap().len(),
            2,
            "upsert 不得新增行"
        );
    }

    #[test]
    fn delete_card_cascades_to_result() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_balance_card(&card("a", 60, true, 0)).unwrap();
        db.upsert_balance_card(&card("b", 60, true, 1)).unwrap();
        db.upsert_balance_key_result(
            "a",
            &key_result(0, "sk-a", "ok", Some(json!({"summary": "s"}))),
        )
        .unwrap();
        db.upsert_balance_key_result(
            "a",
            &key_result(1, "sk-b", "ok", Some(json!({"summary": "s1"}))),
        )
        .unwrap();
        db.upsert_balance_key_result(
            "b",
            &key_result(0, "sk-c", "ok", Some(json!({"summary": "s2"}))),
        )
        .unwrap();

        db.delete_balance_card("a").unwrap();
        assert!(db.get_balance_card("a").unwrap().is_none());
        assert!(
            db.list_balance_results("a").unwrap().is_empty(),
            "结果行应随卡片级联删除"
        );
        assert_eq!(
            db.list_balance_results("b").unwrap().len(),
            1,
            "别卡结果不受影响"
        );
    }

    #[test]
    fn due_cards_cover_five_states() {
        let db = Database::open_in_memory().unwrap();
        let now = 1_000_000;
        // 从未查询（无结果行）→ 到期
        db.upsert_balance_card(&card("never", 60, true, 0)).unwrap();
        // 刚查询（now - queried_at < interval）→ 未到期
        db.upsert_balance_card(&card("fresh", 60, true, 1)).unwrap();
        let mut fresh = key_result(0, "", "ok", Some(json!({})));
        fresh.result.queried_at = now;
        db.upsert_balance_key_result("fresh", &fresh).unwrap();
        // 结果陈旧（now - queried_at >= interval）→ 到期
        db.upsert_balance_card(&card("stale", 60, true, 2)).unwrap();
        let mut stale = key_result(0, "", "ok", Some(json!({})));
        stale.result.queried_at = now - 60;
        db.upsert_balance_key_result("stale", &stale).unwrap();
        // 多 key 卡片按各行 MAX(queried_at) 判定：有一个新鲜行就不到期
        db.upsert_balance_card(&card("multi", 60, true, 3)).unwrap();
        let mut old_row = key_result(0, "k0", "ok", Some(json!({})));
        old_row.result.queried_at = now - 600;
        db.upsert_balance_key_result("multi", &old_row).unwrap();
        let mut new_row = key_result(1, "k1", "ok", Some(json!({})));
        new_row.result.queried_at = now;
        db.upsert_balance_key_result("multi", &new_row).unwrap();
        // 未启用 → 排除
        db.upsert_balance_card(&card("off", 60, false, 4)).unwrap();
        // interval = 0（禁用定时）→ 排除
        db.upsert_balance_card(&card("manual", 0, true, 5)).unwrap();

        let keys: Vec<String> = db
            .list_balance_cards_due(now)
            .unwrap()
            .into_iter()
            .map(|c| c.key)
            .collect();
        assert_eq!(keys, vec!["never", "stale"], "只有从未查询与陈旧的到期");

        // 差一秒未达间隔同样排除，边界上不引入抖动
        assert!(db
            .list_balance_cards_due(now - 1)
            .unwrap()
            .iter()
            .all(|c| c.key != "stale"));
    }

    #[test]
    fn key_results_upsert_get_list_and_prune() {
        let db = Database::open_in_memory().unwrap();
        assert!(
            db.list_balance_results("a").unwrap().is_empty(),
            "未写入时读回空列表"
        );
        assert!(db.get_balance_key_result("a", 0).unwrap().is_none());

        let payload = json!({
            "quotas": [{"label": "5 小时", "usedPercent": 43.0, "leftPercent": 57.0}],
            "summary": "余额正常",
            "nested": {"deep": [1, 2, {"k": null}]}
        });
        db.upsert_balance_key_result("a", &key_result(1, "sk-b", "ok", Some(payload.clone())))
            .unwrap();
        db.upsert_balance_key_result(
            "a",
            &key_result(0, "sk-a", "ok", Some(json!({"summary": "first"}))),
        )
        .unwrap();

        // 列表按 key_index 升序，与写入顺序无关
        let rows = db.list_balance_results("a").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key_index, 0);
        assert_eq!(rows[0].key_label, "sk-a");
        assert_eq!(rows[1].key_index, 1);
        assert_eq!(rows[1].key_label, "sk-b");
        assert_eq!(
            rows[1].result.payload,
            Some(payload),
            "payload 应 JSON 往返不变"
        );

        // 同一 (card_key, key_index) 覆盖写
        let mut failed = key_result(1, "sk-b", "error", None);
        failed.result.error = Some("上游 500".to_string());
        failed.result.queried_at = 200;
        db.upsert_balance_key_result("a", &failed).unwrap();
        let got = db.get_balance_key_result("a", 1).unwrap().unwrap();
        assert_eq!(got, failed, "同一 key_index 覆盖写");
        assert_eq!(got.result.payload, None, "payload 为 None 时不做保留回填");
        assert_eq!(
            db.list_balance_results("a").unwrap().len(),
            2,
            "覆盖写不增行"
        );

        // prune：key 变少后剪掉残留行
        db.upsert_balance_key_result("a", &key_result(2, "sk-c", "ok", None))
            .unwrap();
        db.prune_balance_results("a", 2).unwrap();
        let idxs: Vec<i64> = db
            .list_balance_results("a")
            .unwrap()
            .into_iter()
            .map(|r| r.key_index)
            .collect();
        assert_eq!(idxs, vec![0, 1], "key_index >= keep 的行被剪掉");
        db.prune_balance_results("a", 0).unwrap();
        assert!(
            db.list_balance_results("a").unwrap().is_empty(),
            "keep=0 清空全部"
        );
    }
}
