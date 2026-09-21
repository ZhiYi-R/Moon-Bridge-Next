//! Moon Bridge Next 存储层。
//!
//! 以 SQLite（rusqlite, bundled + WAL）为唯一持久化后端，承载 provider/model/
//! route/plugin/usage/settings 全部配置与运行时数据。手写版本化 migration
//! （见 [`schema`]），每表一个 DAO 模块（见 [`dao`]；配额查询见 [`quota`]），
//! 统一由 [`Database`] 暴露。
//!
//! 并发模型：单连接 + `parking_lot::Mutex` 串行化。本地网关的管理类读写为低并发，
//! 该模型足够且实现简单；如需更高写入并发可平滑替换为连接池。
//!
//! 依赖方向：store → core（不依赖 protocol/plugin/gateway）。

pub mod crypto;
pub mod dao;
pub mod error;
pub mod models;
pub mod quota;
pub mod schema;

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

pub use crypto::{AesGcmKey, EncKey, PlaintextKey};
pub use dao::usage::{ProviderCost, UsageSummary};
pub use error::{Result, StoreError};
pub use models::{
    Endpoint, ModelDef, Offer, PluginBinding, PluginRecord, Provider, ProviderQuotaView,
    QuotaEntry, QuotaKeyResult, QuotaResult, Route, Setting, UsageQuery, UsageRecord,
};
pub use quota::{clamp_interval_secs, MIN_INTERVAL_SECS, QUOTA_SEEDS_DONE};

/// SQLite 存储句柄。可 `Arc` 共享给 gateway 与 app 层。
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) enc: Box<dyn EncKey>,
}

impl Database {
    /// 打开文件数据库，以 `<数据库完整路径>.key` 保存 AES-256 主密钥。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut key_path = path.as_os_str().to_os_string();
        key_path.push(".key");
        Self::open_with_key_file(path, Path::new(&key_path))
    }

    /// 旧库按默认明文来源升级；旧自定义加密库须使用 open_with_legacy_key。
    /// 主密钥仅可为未加密数据库首次生成；已加密库缺失或无法认证时拒绝打开。
    pub fn open_with_key_file(path: &Path, key_path: &Path) -> Result<Self> {
        if path == Path::new(":memory:") || path.as_os_str().is_empty() {
            return Err(StoreError::Encryption(
                "文件加密接口需要持久化数据库路径".into(),
            ));
        }
        Self::create_parent(path)?;
        let mut conn = Connection::open(path)?;
        Self::prepare_conn(&conn)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (scheme, _) = Self::encryption_state(&tx)?;
        let enc = Box::new(crypto::load_key_file(key_path, scheme == "plaintext")?);
        Self::initialize_encryption(&tx, enc.as_ref(), Some(&PlaintextKey))?;
        tx.commit()?;
        Self::checkpoint_encryption(&conn, enc.as_ref())?;
        Ok(Self {
            conn: Mutex::new(conn),
            enc,
        })
    }

    /// 使用显式自定义密钥；旧 provider 数据升级需通过 open_with_legacy_key 指定来源。
    pub fn open_with_key(path: impl AsRef<Path>, enc: Box<dyn EncKey>) -> Result<Self> {
        let path = path.as_ref();
        Self::create_parent(path)?;
        let conn = Connection::open(path)?;
        Self::from_conn(conn, enc, None)
    }

    /// 将旧 provider 密钥按指定来源解密后迁移到目标密钥。
    pub fn open_with_legacy_key(
        path: impl AsRef<Path>,
        target: Box<dyn EncKey>,
        legacy_provider_key: &dyn EncKey,
    ) -> Result<Self> {
        let path = path.as_ref();
        Self::create_parent(path)?;
        let conn = Connection::open(path)?;
        Self::from_conn(conn, target, Some(legacy_provider_key))
    }

    /// 打开内存数据库，显式使用 PlaintextKey（测试用）。
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_conn(conn, Box::new(PlaintextKey), None)
    }

    fn create_parent(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    fn prepare_conn(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        schema::migrate(conn)
    }

    fn from_conn(
        mut conn: Connection,
        enc: Box<dyn EncKey>,
        legacy_provider_key: Option<&dyn EncKey>,
    ) -> Result<Self> {
        Self::prepare_conn(&conn)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::initialize_encryption(&tx, enc.as_ref(), legacy_provider_key)?;
        tx.commit()?;
        Self::checkpoint_encryption(&conn, enc.as_ref())?;
        Ok(Database {
            conn: Mutex::new(conn),
            enc,
        })
    }

    fn checkpoint_encryption(conn: &Connection, enc: &dyn EncKey) -> Result<()> {
        if enc.scheme() != "plaintext" {
            let busy: i32 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
            if busy != 0 {
                return Err(StoreError::Encryption(
                    "无法清理迁移日志，请关闭其他数据库连接后重试".into(),
                ));
            }
        }
        Ok(())
    }

    fn encryption_state(conn: &Connection) -> Result<(String, String)> {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM encryption_metadata", [], |r| r.get(0))?;
        if count != 1 {
            return Err(StoreError::Encryption("加密元数据缺失或损坏".into()));
        }
        let (scheme, verifier): (String, String) = conn.query_row(
            "SELECT scheme, verifier FROM encryption_metadata WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if scheme.is_empty() || (scheme == "plaintext" && !verifier.is_empty()) {
            return Err(StoreError::Encryption("加密元数据无效".into()));
        }
        Ok((scheme, verifier))
    }

    fn initialize_encryption(
        conn: &Connection,
        enc: &dyn EncKey,
        legacy_provider_key: Option<&dyn EncKey>,
    ) -> Result<()> {
        const VERIFIER: &str = "moonbridge-store:key-verifier:v1";
        let (scheme, verifier) = Self::encryption_state(conn)?;
        let legacy = scheme == "plaintext";
        if !legacy && (scheme != enc.scheme() || enc.decrypt(&verifier)? != VERIFIER) {
            return Err(StoreError::Encryption(
                "数据库主密钥不匹配：当前密钥无法解密此库。请确认 --key-file / MOONBRIDGE_KEY_FILE 指向加密该库时所用的同一密钥文件（默认为数据库同目录下的 .key）；若密钥已丢失，或数据库来自他机/旧备份而未带上对应密钥，则无法打开，需用原密钥恢复。"
                    .into(),
            ));
        }
        if legacy && enc.scheme() == "plaintext" && legacy_provider_key.is_none() {
            return Ok(());
        }
        let mut stmt =
            conn.prepare("SELECT provider_key, idx, api_key_enc FROM provider_endpoints")?;
        let endpoints = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        if legacy && legacy_provider_key.is_none() {
            let has_providers: bool =
                conn.query_row("SELECT EXISTS(SELECT 1 FROM providers)", [], |r| r.get(0))?;
            if has_providers || !endpoints.is_empty() {
                return Err(StoreError::Encryption(
                    "旧 provider 密钥来源不明确，请使用 open_with_legacy_key 显式指定来源密钥（明文使用 PlaintextKey）".into(),
                ));
            }
        }
        let source = legacy_provider_key.unwrap_or(&PlaintextKey);
        for (provider, idx, stored) in endpoints {
            if legacy {
                let plaintext = source.decrypt(&stored)?;
                conn.execute(
                    "UPDATE provider_endpoints SET api_key_enc = ?1 WHERE provider_key = ?2 AND idx = ?3",
                    rusqlite::params![enc.encrypt(&plaintext)?, provider, idx],
                )?;
            } else {
                enc.decrypt(&stored)?;
            }
        }
        // quota_config_enc（V14+ 列）同理：plaintext scheme 下存的是明文 JSON，
        // 直接按明文重加密；非 legacy 时逐行解密校验（尽早暴露错误密钥）。
        let quota_rows = {
            let mut stmt = conn
                .prepare("SELECT key, quota_config_enc FROM providers WHERE quota_config_enc <> ''")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (key, stored) in quota_rows {
            if legacy {
                conn.execute(
                    "UPDATE providers SET quota_config_enc = ?1 WHERE key = ?2",
                    rusqlite::params![enc.encrypt(&stored)?, key],
                )?;
            } else {
                enc.decrypt(&stored)?;
            }
        }
        if legacy {
            let verifier = if enc.scheme() == "plaintext" {
                String::new()
            } else {
                let verifier = enc.encrypt(VERIFIER)?;
                if verifier == VERIFIER
                    || enc.decrypt(&verifier)? != VERIFIER
                    || enc.scheme().is_empty()
                {
                    return Err(StoreError::Encryption("EncKey 未提供可验证的加密".into()));
                }
                verifier
            };
            conn.execute(
                "UPDATE encryption_metadata SET scheme = ?1, verifier = ?2 WHERE id = 1",
                rusqlite::params![enc.scheme(), verifier],
            )?;
        }
        Ok(())
    }

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
    use serde_json::{json, Value};

    #[test]
    fn migrates_and_reports_version() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.version().unwrap(), 14);
        assert_eq!(db.enc.scheme(), "plaintext");
        assert_eq!(db.enc.encrypt("memory-secret").unwrap(), "memory-secret");
        let state: (String, String) = db
            .conn
            .lock()
            .query_row(
                "SELECT scheme, verifier FROM encryption_metadata WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("plaintext".into(), String::new()));
    }

    /// 回归：V8 新增 max_output_tokens 列须随模型定义往返（供上游必填
    /// max_tokens 的协议按模型真实输出上限回退）。
    #[test]
    fn model_max_output_tokens_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_model(&ModelDef {
            slug: "m".into(),
            display_name: None,
            context_window: Some(200_000),
            max_output_tokens: Some(64_000),
            modalities: None,
            reasoning_levels: None,
            extra: serde_json::Value::Null,
        })
        .unwrap();
        assert_eq!(
            db.get_model("m").unwrap().unwrap().max_output_tokens,
            Some(64_000)
        );
        // 更新为 None / 未建模型的 slug 均须原样往返
        db.upsert_model(&ModelDef {
            slug: "m".into(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            modalities: None,
            reasoning_levels: None,
            extra: serde_json::Value::Null,
        })
        .unwrap();
        assert_eq!(db.get_model("m").unwrap().unwrap().max_output_tokens, None);
        assert!(db.get_model("absent").unwrap().is_none());
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
            quota_plugin_ref: String::new(),
            quota_interval_secs: 0,
            quota_enabled: false,
            quota_config: serde_json::Value::Null,
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
            provider_key: Some("deepseek".into()),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 3,
            cache_write_tokens: 2,
            reasoning_tokens: 1,
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
        assert_eq!(sum.cache_read_tokens, 3);
        assert_eq!(sum.reasoning_tokens, 1);
        assert_eq!(sum.total_cost, 0.001);

        let rows = db.query_usage(&q_all()).unwrap();
        assert_eq!(rows[0].provider_key.as_deref(), Some("deepseek"));

        let q = UsageQuery {
            limit: 10,
            ..Default::default()
        };
        assert_eq!(db.query_usage(&q).unwrap().len(), 1);
    }

    fn q_all() -> UsageQuery {
        UsageQuery {
            limit: 10,
            ..Default::default()
        }
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
            category: "core".into(),
            config_schema: Value::Null,
        })
        .unwrap();
        // 默认继承插件 enabled
        assert!(db
            .plugin_enabled_in_scope("demo", None, None, None)
            .unwrap());
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
        db.set_setting("listen_addr", &json!("127.0.0.1:38440"))
            .unwrap();
        assert_eq!(
            db.get_setting("listen_addr").unwrap(),
            Some(json!("127.0.0.1:38440"))
        );
        assert_eq!(db.list_settings().unwrap().len(), 1);
    }
}
