//! 会话状态存储：跨请求保存插件的 per-session 数据。
//!
//! 以 `(plugin_name, session_id, key)` 三元组隔离，避免插件间串扰；不同会话
//! （由 `session_id` 或 `X-Codex-Window-Id` 标识）互不可见。同步 `Mutex` 足够，
//! 因为访问仅发生在 Lua 同步函数内、不跨 await。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

type Key = (String, Option<String>, String);

/// 会话状态存储句柄（可克隆共享）。
#[derive(Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<Key, Value>>>,
}

impl SessionStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取某插件在某会话下的键值。
    pub fn get(&self, plugin: &str, session: Option<&str>, key: &str) -> Option<Value> {
        let guard = self.inner.lock().ok()?;
        guard
            .get(&(plugin.to_string(), session.map(|s| s.to_string()), key.to_string()))
            .cloned()
    }

    /// 写入某插件在某会话下的键值。
    pub fn set(&self, plugin: &str, session: Option<&str>, key: &str, value: Value) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(
                (plugin.to_string(), session.map(|s| s.to_string()), key.to_string()),
                value,
            );
        }
    }

    /// 清除某会话的全部状态（会话结束时调用）。
    pub fn clear_session(&self, session: &str) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.retain(|(_, sid, _), _| sid.as_deref() != Some(session));
        }
    }
}
