//! Provider 与 Endpoint DAO。

use rusqlite::params;
use serde_json::Value;

use crate::error::Result;
use crate::models::{Endpoint, Provider};
use crate::Database;

const SELECT_COLS: &str =
    "key,version,user_agent,web_search_json,extra_json,enabled,created_at,updated_at";

fn row_to_provider(r: &rusqlite::Row) -> rusqlite::Result<Provider> {
    let extra_json: String = r.get(4)?;
    let ws: Option<String> = r.get(3)?;
    Ok(Provider {
        key: r.get(0)?,
        endpoints: Vec::new(),
        version: r.get(1)?,
        user_agent: r.get(2)?,
        web_search: ws.and_then(|s| serde_json::from_str(&s).ok()),
        extra: serde_json::from_str(&extra_json).unwrap_or(Value::Null),
        enabled: r.get::<_, i32>(5)? != 0,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
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
            e.api_key = self.enc.decrypt(&e.api_key);
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
            Some(Ok(mut p)) => {
                drop(rows);
                drop(stmt);
                drop(conn);
                p.endpoints = self.list_endpoints(&p.key)?;
                Ok(Some(p))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 插入或更新 provider（按 key upsert），并整体重写其端点列表。
    /// 各端点 api_key 落库前加密。
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
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO providers (key,version,user_agent,web_search_json,extra_json,enabled,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(key) DO UPDATE SET
                version=excluded.version, user_agent=excluded.user_agent,
                web_search_json=excluded.web_search_json, extra_json=excluded.extra_json,
                enabled=excluded.enabled, updated_at=excluded.updated_at",
            params![p.key, p.version, p.user_agent, ws, extra, p.enabled as i32, created, now],
        )?;
        tx.execute(
            "DELETE FROM provider_endpoints WHERE provider_key = ?1",
            params![p.key],
        )?;
        for (idx, e) in p.endpoints.iter().enumerate() {
            let enc = self.enc.encrypt(&e.api_key);
            tx.execute(
                "INSERT INTO provider_endpoints (provider_key,idx,protocol,base_url,api_key_enc) VALUES (?1,?2,?3,?4,?5)",
                params![p.key, idx as i64, e.protocol, e.base_url, enc],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 删除 provider（级联删除其端点与 offers）。
    pub fn delete_provider(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM providers WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM provider_endpoints WHERE provider_key = ?1", params![key])?;
        conn.execute("DELETE FROM offers WHERE provider_key = ?1", params![key])?;
        Ok(())
    }
}
