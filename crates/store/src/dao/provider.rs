//! Provider 与 Endpoint DAO。

use rusqlite::params;
use serde_json::Value;

use crate::error::Result;
use crate::models::{Endpoint, Provider};
use crate::Database;

const SELECT_COLS: &str =
    "key,version,user_agent,web_search_json,extra_json,enabled,created_at,updated_at,quota_plugin_ref,quota_interval_secs,quota_enabled,quota_config_enc";

/// `quota_config_enc` 需要 `self.enc` 解密，行映射只能先把密文带出去——
/// 返回 `(Provider, quota_config_enc)`，由调用方解密后写回 `quota_config`。
fn row_to_provider(r: &rusqlite::Row) -> rusqlite::Result<(Provider, String)> {
    let extra_json: String = r.get(4)?;
    let ws: Option<String> = r.get(3)?;
    Ok((
        Provider {
            key: r.get(0)?,
            endpoints: Vec::new(),
            version: r.get(1)?,
            user_agent: r.get(2)?,
            web_search: ws.and_then(|s| serde_json::from_str(&s).ok()),
            extra: serde_json::from_str(&extra_json).unwrap_or(Value::Null),
            enabled: r.get::<_, i32>(5)? != 0,
            quota_plugin_ref: r.get(8)?,
            quota_interval_secs: r.get(9)?,
            quota_enabled: r.get::<_, i32>(10)? != 0,
            quota_config: Value::Null,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
        },
        r.get(11)?,
    ))
}

impl Database {
    /// 解密 `quota_config_enc` 为 JSON；空串/解密失败按空对象处理（新行尚无配置）。
    fn decode_quota_config(&self, enc: &str) -> Result<Value> {
        if enc.is_empty() {
            return Ok(Value::Null);
        }
        let plain = self.enc.decrypt(enc)?;
        Ok(serde_json::from_str(&plain).unwrap_or(Value::Null))
    }
}

impl Database {
    /// 读取 provider 的端点列表（按 idx 升序；api_key 已解密）。
    pub fn list_endpoints(&self, provider_key: &str) -> Result<Vec<Endpoint>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT protocol,base_url,api_key_enc FROM provider_endpoints WHERE provider_key = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map(params![provider_key], |r| {
            Ok(Endpoint {
                protocol: r.get(0)?,
                base_url: r.get(1)?,
                // 此处为密文，出函数前统一解密
                api_key: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            let mut e = r?;
            e.api_key = self.enc.decrypt(&e.api_key)?;
            out.push(e);
        }
        Ok(out)
    }

    /// 列出全部 provider（含端点，api_key 已解密）。
    pub fn list_providers(&self) -> Result<Vec<Provider>> {
        let keys: Vec<String> = {
            let conn = self.conn.lock();
            let mut stmt = conn.prepare("SELECT key FROM providers ORDER BY key")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
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

    /// 按 key 获取 provider（含端点，api_key 已解密）。
    pub fn get_provider(&self, key: &str) -> Result<Option<Provider>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM providers WHERE key = ?1"
        ))?;
        let mut rows = stmt.query_map(params![key], row_to_provider)?;
        match rows.next() {
            Some(Ok((mut p, quota_enc))) => {
                drop(rows);
                drop(stmt);
                drop(conn);
                p.quota_config = self.decode_quota_config(&quota_enc)?;
                p.endpoints = self.list_endpoints(&p.key)?;
                Ok(Some(p))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 插入或更新 provider（按 key upsert），并整体重写其端点列表。
    /// 各端点 api_key 写入数据库前加密。
    pub fn upsert_provider(&self, p: &Provider) -> Result<()> {
        let mut conn = self.conn.lock();
        let now = crate::now_unix();
        let ws = p.web_search.as_ref().map(|v| v.to_string());
        let extra = if p.extra.is_null() {
            "{}".to_string()
        } else {
            p.extra.to_string()
        };
        let created = if p.created_at > 0 { p.created_at } else { now };
        // 配额配置整段加密写入数据库（含密钥类字段）；空配置存空串，读回按空对象处理。
        let quota_config_enc = if p.quota_config.is_null()
            || p.quota_config.as_object().is_some_and(|o| o.is_empty())
        {
            String::new()
        } else {
            self.enc.encrypt(&p.quota_config.to_string())?
        };
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO providers (key,version,user_agent,web_search_json,extra_json,enabled,created_at,updated_at,quota_plugin_ref,quota_interval_secs,quota_enabled,quota_config_enc)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(key) DO UPDATE SET
                version=excluded.version, user_agent=excluded.user_agent,
                web_search_json=excluded.web_search_json, extra_json=excluded.extra_json,
                enabled=excluded.enabled, updated_at=excluded.updated_at,
                quota_plugin_ref=excluded.quota_plugin_ref,
                quota_interval_secs=excluded.quota_interval_secs,
                quota_enabled=excluded.quota_enabled,
                quota_config_enc=excluded.quota_config_enc",
            params![
                p.key,
                p.version,
                p.user_agent,
                ws,
                extra,
                p.enabled as i32,
                created,
                now,
                p.quota_plugin_ref,
                crate::quota::clamp_interval_secs(p.quota_interval_secs),
                p.quota_enabled as i32,
                quota_config_enc,
            ],
        )?;
        tx.execute(
            "DELETE FROM provider_endpoints WHERE provider_key = ?1",
            params![p.key],
        )?;
        for (idx, e) in p.endpoints.iter().enumerate() {
            let enc = self.enc.encrypt(&e.api_key)?;
            tx.execute(
                "INSERT INTO provider_endpoints (provider_key,idx,protocol,base_url,api_key_enc) VALUES (?1,?2,?3,?4,?5)",
                params![p.key, idx as i64, e.protocol, e.base_url, enc],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 删除 provider（级联删除其端点、offers、指向它的 routes 与 provider 维度
    /// 插件绑定——不留悬挂引用）。多步删除包在一个事务里，中途失败整体回滚。
    pub fn delete_provider(&self, key: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM providers WHERE key = ?1", params![key])?;
        tx.execute(
            "DELETE FROM provider_endpoints WHERE provider_key = ?1",
            params![key],
        )?;
        tx.execute("DELETE FROM offers WHERE provider_key = ?1", params![key])?;
        tx.execute("DELETE FROM routes WHERE provider_key = ?1", params![key])?;
        tx.execute(
            "DELETE FROM plugin_bindings WHERE scope = 'provider' AND scope_key = ?1",
            params![key],
        )?;
        tx.execute(
            "DELETE FROM quota_results WHERE provider_key = ?1",
            params![key],
        )?;
        tx.commit()?;
        Ok(())
    }
}
