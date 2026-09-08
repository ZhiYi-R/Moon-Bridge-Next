//! Moon Bridge Next 存储层。
//!
//! 以 SQLite（rusqlite, bundled + WAL）为唯一持久化后端，承载 provider/model/
//! route/plugin/usage/settings 全部配置与运行时数据。手写版本化 migration
//! （见 [`schema`]），每表一个 DAO 模块（见 [`dao`]），统一由 [`Database`] 暴露。
//!
//! 并发模型：单连接 + `parking_lot::Mutex` 串行化。本地网关的管理类读写为低并发，
//! 该模型足够且实现简单；如需更高写入并发可平滑替换为连接池。
//!
//! 依赖方向：store → core（不依赖 protocol/plugin/gateway）。

pub mod crypto;
pub mod dao;
pub mod error;
pub mod models;
pub mod schema;

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

pub use crypto::{EncKey, PlaintextKey};
pub use dao::usage::UsageSummary;
pub use error::{Result, StoreError};
pub use models::{
    Endpoint, ModelDef, Offer, PluginBinding, PluginRecord, Provider, Route, Setting, UsageQuery,
    UsageRecord,
};

/// SQLite 存储句柄。可 `Arc` 共享给 gateway 与 app 层。
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) enc: Box<dyn EncKey>,
}

impl Database {
    /// 打开（或创建）指定路径的数据库，启用 WAL 并执行 migration。
    ///
    /// 使用默认 [`PlaintextKey`]（脚手架阶段；生产可换 [`Database::open_with_key`]）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_key(path, Box::new(PlaintextKey))
    }

    /// 使用自定义 [`EncKey`] 打开数据库（用于 api_key 加密）。
    pub fn open_with_key(path: impl AsRef<Path>, enc: Box<dyn EncKey>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        Self::from_conn(conn, enc)
    }

    /// 打开内存数据库（测试用）。
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_conn(conn, Box::new(PlaintextKey))
    }

    fn from_conn(conn: Connection, enc: Box<dyn EncKey>) -> Result<Self> {
        // 内存库设置 WAL 无意义但不报错；忽略 pragma 失败以兼容各平台
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        Ok(Database {
            conn: Mutex::new(conn),
            enc,
        })
    }

    /// 当前 schema 版本。
    pub fn version(&self) -> Result<i32> {
        let conn = self.conn.lock();
        schema::current_version(&conn)
    }
}

/// 当前 unix 时间戳（秒）。
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrates_and_reports_version() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.version().unwrap(), 5);
    }

    #[test]
    fn provider_roundtrip_with_encryption_hook() {
        let db = Database::open_in_memory().unwrap();
        let p = Provider {
            key: "deepseek".into(),
            endpoints: vec![
                Endpoint {
                    protocol: "anthropic".into(),
                    base_url: "https://api.deepseek.com/anthropic".into(),
                    api_key: "sk-secret".into(),
                },
                Endpoint {
                    protocol: "openai-chat".into(),
                    base_url: "https://mirror.deepseek.com/v1".into(),
                    api_key: "sk-secret".into(),
                },
            ],
            version: Some("2023-06-01".into()),
            user_agent: None,
            web_search: Some(json!({"support": "auto"})),
            extra: json!({}),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        db.upsert_provider(&p).unwrap();
        let got = db.get_provider("deepseek").unwrap().unwrap();
        assert_eq!(got.endpoints[0].api_key, "sk-secret"); // 明文密钥经 PlaintextKey 往返不变
        assert_eq!(got.endpoints.len(), 2);
        assert_eq!(got.endpoints[1].protocol, "openai-chat"); // 协议绑定在端点上
        assert_eq!(got.endpoints[1].base_url, "https://mirror.deepseek.com/v1");
        assert_eq!(db.list_providers().unwrap().len(), 1);
        db.delete_provider("deepseek").unwrap();
        assert!(db.get_provider("deepseek").unwrap().is_none());
        assert!(db.list_endpoints("deepseek").unwrap().is_empty());
    }

    #[test]
    fn route_resolve_and_usage_summary() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_route(&Route {
            alias: "moonbridge".into(),
            model_slug: "deepseek-v4-pro".into(),
            provider_key: "deepseek".into(),
            extra: serde_json::Value::Null,
        })
        .unwrap();
        let rt = db.resolve_route("moonbridge").unwrap().unwrap();
        assert_eq!(rt.provider_key, "deepseek");

        db.insert_usage(&UsageRecord {
            id: "".into(),
            session_id: Some("s1".into()),
            model: Some("moonbridge".into()),
            upstream_model: Some("deepseek-v4-pro".into()),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.001,
            status: Some("ok".into()),
            error: None,
            latency_ms: 120,
            ttft_ms: Some(45),
            created_at: 0,
        })
        .unwrap();

        let sum = db.usage_summary().unwrap();
        assert_eq!(sum.requests, 1);
        assert_eq!(sum.input_tokens, 10);
        assert_eq!(sum.output_tokens, 20);

        let q = UsageQuery {
            limit: 10,
            ..Default::default()
        };
        assert_eq!(db.query_usage(&q).unwrap().len(), 1);
    }

    #[test]
    fn plugin_scope_resolution() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_plugin(&PluginRecord {
            name: "demo".into(),
            source: "lua".into(),
            script_ref: "plugins/demo.lua".into(),
            enabled: true,
            config: json!({"prefix": "hi"}),
            scopes: vec!["global".into(), "model".into()],
            capabilities: vec!["core".into()],
        })
        .unwrap();
        // 默认继承插件 enabled
        assert!(db.plugin_enabled_in_scope("demo", None, None, None).unwrap());
        // model 级覆盖为关闭
        db.upsert_binding(&PluginBinding {
            plugin_name: "demo".into(),
            scope: "model".into(),
            scope_key: "gpt".into(),
            enabled: false,
            config: serde_json::Value::Null,
        })
        .unwrap();
        assert!(!db
            .plugin_enabled_in_scope("demo", None, Some("gpt"), None)
            .unwrap());
    }

    #[test]
    fn settings_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        db.set_setting("listen_addr", &json!("127.0.0.1:38440")).unwrap();
        assert_eq!(
            db.get_setting("listen_addr").unwrap(),
            Some(json!("127.0.0.1:38440"))
        );
        assert_eq!(db.list_settings().unwrap().len(), 1);
    }
}
