//! Route DAO：别名 → (provider, model) 映射。

use rusqlite::params;
use serde_json::Value;

use crate::error::Result;
use crate::models::Route;
use crate::Database;

fn row_to_route(r: &rusqlite::Row) -> rusqlite::Result<Route> {
    let extra: Option<String> = r.get(3)?;
    Ok(Route {
        alias: r.get(0)?,
        model_slug: r.get(1)?,
        provider_key: r.get(2)?,
        extra: extra
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or(Value::Null),
    })
}

impl Database {
    /// 列出全部路由。
    pub fn list_routes(&self) -> Result<Vec<Route>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT alias,model_slug,provider_key,extra_json FROM routes ORDER BY alias")?;
        let rows = stmt.query_map([], row_to_route)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 解析别名（等价于按 alias 获取路由）。
    pub fn resolve_route(&self, alias: &str) -> Result<Option<Route>> {
        self.get_route(alias)
    }

    /// 按 alias 获取路由。
    pub fn get_route(&self, alias: &str) -> Result<Option<Route>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT alias,model_slug,provider_key,extra_json FROM routes WHERE alias = ?1",
        )?;
        let mut rows = stmt.query_map(params![alias], row_to_route)?;
        match rows.next() {
            Some(Ok(rt)) => Ok(Some(rt)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 插入或更新路由。
    pub fn upsert_route(&self, rt: &Route) -> Result<()> {
        let conn = self.conn.lock();
        let extra = if rt.extra.is_null() {
            "{}".to_string()
        } else {
            rt.extra.to_string()
        };
        conn.execute(
            "INSERT INTO routes (alias,model_slug,provider_key,extra_json) VALUES (?1,?2,?3,?4)
             ON CONFLICT(alias) DO UPDATE SET
                model_slug=excluded.model_slug, provider_key=excluded.provider_key, extra_json=excluded.extra_json",
            params![rt.alias, rt.model_slug, rt.provider_key, extra],
        )?;
        Ok(())
    }

    /// 删除路由（连带删除该别名的 route 维度插件绑定）。
    pub fn delete_route(&self, alias: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM routes WHERE alias = ?1", params![alias])?;
        tx.execute(
            "DELETE FROM plugin_bindings WHERE scope = 'route' AND scope_key = ?1",
            params![alias],
        )?;
        tx.commit()?;
        Ok(())
    }
}
