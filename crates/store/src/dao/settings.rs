//! Settings DAO：全局键值设置。

use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use crate::error::Result;
use crate::models::Setting;
use crate::Database;

impl Database {
    /// 读取单个设置项。
    pub fn get_setting(&self, key: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock();
        let raw: Option<String> = conn
            .query_row("SELECT value_json FROM settings WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// 写入单个设置项（upsert）。
    pub fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO settings (key, value_json) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
            params![key, value.to_string()],
        )?;
        Ok(())
    }

    /// 列出全部设置项。
    pub fn list_settings(&self) -> Result<Vec<Setting>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT key, value_json FROM settings ORDER BY key")?;
        let rows = stmt.query_map([], |r| {
            let key: String = r.get(0)?;
            let raw: String = r.get(1)?;
            Ok((key, raw))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (key, raw) = r?;
            out.push(Setting {
                key,
                value: serde_json::from_str(&raw).unwrap_or(Value::Null),
            });
        }
        Ok(out)
    }

    /// 删除设置项。
    pub fn delete_setting(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }
}
