//! Plugin 与 PluginBinding DAO。

use rusqlite::params;
use serde_json::Value;

use crate::error::Result;
use crate::models::{PluginBinding, PluginRecord};
use crate::Database;

fn str_vec(s: Option<String>) -> Vec<String> {
    s.and_then(|x| serde_json::from_str::<Vec<String>>(&x).ok())
        .unwrap_or_default()
}

fn row_to_plugin(r: &rusqlite::Row) -> rusqlite::Result<PluginRecord> {
    let config: String = r.get(4)?;
    Ok(PluginRecord {
        name: r.get(0)?,
        source: r.get(1)?,
        script_ref: r.get(2)?,
        enabled: r.get::<_, i32>(3)? != 0,
        config: serde_json::from_str(&config).unwrap_or(Value::Null),
        scopes: str_vec(r.get(5)?),
        capabilities: str_vec(r.get(6)?),
    })
}

fn row_to_binding(r: &rusqlite::Row) -> rusqlite::Result<PluginBinding> {
    let config: String = r.get(4)?;
    Ok(PluginBinding {
        plugin_name: r.get(0)?,
        scope: r.get(1)?,
        scope_key: r.get(2)?,
        enabled: r.get::<_, i32>(3)? != 0,
        config: serde_json::from_str(&config).unwrap_or(Value::Null),
    })
}

impl Database {
    /// 列出全部插件。
    pub fn list_plugins(&self) -> Result<Vec<PluginRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT name,source,script_ref,enabled,config_json,scopes_json,capabilities_json FROM plugins ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_plugin)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按名获取插件。
    pub fn get_plugin(&self, name: &str) -> Result<Option<PluginRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT name,source,script_ref,enabled,config_json,scopes_json,capabilities_json FROM plugins WHERE name = ?1",
        )?;
        let mut rows = stmt.query_map(params![name], row_to_plugin)?;
        match rows.next() {
            Some(Ok(p)) => Ok(Some(p)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 插入或更新插件。
    pub fn upsert_plugin(&self, p: &PluginRecord) -> Result<()> {
        let conn = self.conn.lock();
        let config = if p.config.is_null() {
            "{}".to_string()
        } else {
            p.config.to_string()
        };
        let scopes = serde_json::to_string(&p.scopes)?;
        let caps = serde_json::to_string(&p.capabilities)?;
        conn.execute(
            "INSERT INTO plugins (name,source,script_ref,enabled,config_json,scopes_json,capabilities_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(name) DO UPDATE SET
                source=excluded.source, script_ref=excluded.script_ref, enabled=excluded.enabled,
                config_json=excluded.config_json, scopes_json=excluded.scopes_json, capabilities_json=excluded.capabilities_json",
            params![p.name, p.source, p.script_ref, p.enabled as i32, config, scopes, caps],
        )?;
        Ok(())
    }

    /// 删除插件（级联删除其 bindings）。
    pub fn delete_plugin(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM plugins WHERE name = ?1", params![name])?;
        conn.execute("DELETE FROM plugin_bindings WHERE plugin_name = ?1", params![name])?;
        Ok(())
    }

    /// 列出某插件的作用域绑定。
    pub fn list_bindings(&self, plugin_name: &str) -> Result<Vec<PluginBinding>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT plugin_name,scope,scope_key,enabled,config_json FROM plugin_bindings WHERE plugin_name = ?1",
        )?;
        let rows = stmt.query_map(params![plugin_name], row_to_binding)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 列出全部作用域绑定（供网关装配插件门控表）。
    pub fn list_bindings_all(&self) -> Result<Vec<PluginBinding>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT plugin_name,scope,scope_key,enabled,config_json FROM plugin_bindings")?;
        let rows = stmt.query_map([], row_to_binding)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 列出某一作用域类型的全部绑定（scope：provider/model/route/global）。
    pub fn list_bindings_by_scope(&self, scope: &str) -> Result<Vec<PluginBinding>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT plugin_name,scope,scope_key,enabled,config_json FROM plugin_bindings WHERE scope = ?1",
        )?;
        let rows = stmt.query_map(params![scope], row_to_binding)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 删除绑定（用于把三态开关重置回「跟随全局」）。
    pub fn delete_binding(&self, plugin_name: &str, scope: &str, scope_key: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM plugin_bindings WHERE plugin_name = ?1 AND scope = ?2 AND scope_key = ?3",
            params![plugin_name, scope, scope_key],
        )?;
        Ok(())
    }

    /// 插入或更新绑定。
    pub fn upsert_binding(&self, b: &PluginBinding) -> Result<()> {
        let conn = self.conn.lock();
        let config = if b.config.is_null() {
            "{}".to_string()
        } else {
            b.config.to_string()
        };
        conn.execute(
            "INSERT INTO plugin_bindings (plugin_name,scope,scope_key,enabled,config_json)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(plugin_name,scope,scope_key) DO UPDATE SET
                enabled=excluded.enabled, config_json=excluded.config_json",
            params![b.plugin_name, b.scope, b.scope_key, b.enabled as i32, config],
        )?;
        Ok(())
    }

    /// 解析某插件在给定作用域链上是否启用（route > model > provider > global 就近覆盖）。
    ///
    /// 若无任何绑定记录，则回退到插件自身的 `enabled`。
    pub fn plugin_enabled_in_scope(
        &self,
        plugin_name: &str,
        route: Option<&str>,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<bool> {
        let plugin_default = self
            .get_plugin(plugin_name)?
            .map(|p| p.enabled)
            .unwrap_or(false);
        // 就近作用域优先
        for (scope, key) in [
            ("route", route),
            ("model", model),
            ("provider", provider),
            ("global", Some("")),
        ] {
            if let Some(key) = key {
                let conn = self.conn.lock();
                let found: Option<i32> = conn
                    .query_row(
                        "SELECT enabled FROM plugin_bindings WHERE plugin_name=?1 AND scope=?2 AND scope_key=?3",
                        params![plugin_name, scope, key],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(v) = found {
                    return Ok(v != 0);
                }
            }
        }
        Ok(plugin_default)
    }
}
