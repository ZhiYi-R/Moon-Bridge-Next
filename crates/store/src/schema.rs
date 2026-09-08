//! 数据库 schema 与手写 migration。
//!
//! 采用轻量的版本化 migration：`schema_version` 表记录已应用版本，按序执行各版本
//! 的 DDL。功能等价于 refinery，但更透明、无额外宏依赖。新增版本时追加
//! `MIGRATIONS` 条目即可。

use rusqlite::Connection;

use crate::error::Result;

/// 单个 migration：版本号 + DDL。
struct Migration {
    version: i32,
    sql: &'static str,
}

/// V1：初始 schema。
const V1: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
    key             TEXT PRIMARY KEY,
    protocol        TEXT NOT NULL,
    base_url        TEXT NOT NULL,
    api_key_enc     TEXT NOT NULL DEFAULT '',
    version         TEXT,
    user_agent      TEXT,
    web_search_json TEXT,
    extra_json      TEXT NOT NULL DEFAULT '{}',
    enabled         INTEGER NOT NULL DEFAULT 1,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS models (
    slug                  TEXT PRIMARY KEY,
    display_name          TEXT,
    context_window        INTEGER,
    modalities_json       TEXT,
    reasoning_levels_json TEXT,
    pricing_json          TEXT,
    extra_json            TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS offers (
    provider_key TEXT NOT NULL,
    model_slug   TEXT NOT NULL,
    pricing_json TEXT,
    PRIMARY KEY (provider_key, model_slug)
);

CREATE TABLE IF NOT EXISTS routes (
    alias        TEXT PRIMARY KEY,
    model_slug   TEXT NOT NULL,
    provider_key TEXT NOT NULL,
    extra_json   TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS plugins (
    name              TEXT PRIMARY KEY,
    source            TEXT NOT NULL DEFAULT 'lua',
    script_ref        TEXT NOT NULL,
    enabled           INTEGER NOT NULL DEFAULT 1,
    config_json       TEXT NOT NULL DEFAULT '{}',
    scopes_json       TEXT NOT NULL DEFAULT '[]',
    capabilities_json TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS plugin_bindings (
    plugin_name TEXT NOT NULL,
    scope       TEXT NOT NULL,
    scope_key   TEXT NOT NULL DEFAULT '',
    enabled     INTEGER NOT NULL DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (plugin_name, scope, scope_key)
);

CREATE TABLE IF NOT EXISTS usage_records (
    id                 TEXT PRIMARY KEY,
    session_id         TEXT,
    model              TEXT,
    upstream_model     TEXT,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost               REAL NOT NULL DEFAULT 0,
    status             TEXT,
    error              TEXT,
    latency_ms         INTEGER NOT NULL DEFAULT 0,
    created_at         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_usage_created ON usage_records(created_at);
CREATE INDEX IF NOT EXISTS idx_usage_model   ON usage_records(model);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);
"#;

/// V2：usage_records 增加 reasoning_tokens（推理 token，OpenAI reasoning / Gemini thoughts）。
const V2: &str = "ALTER TABLE usage_records ADD COLUMN reasoning_tokens INTEGER NOT NULL DEFAULT 0;";

/// V3：Provider 多端点。新增 `provider_endpoints` 表（每端点独立 API Key），
/// 并把 providers 表的存量单端点（base_url/api_key_enc）迁入首个端点后删除原列。
const V3: &str = r#"
CREATE TABLE IF NOT EXISTS provider_endpoints (
    provider_key TEXT NOT NULL,
    idx          INTEGER NOT NULL,
    base_url     TEXT NOT NULL,
    api_key_enc  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (provider_key, idx)
);

INSERT INTO provider_endpoints (provider_key, idx, base_url, api_key_enc)
    SELECT key, 0, base_url, api_key_enc FROM providers;

ALTER TABLE providers DROP COLUMN base_url;
ALTER TABLE providers DROP COLUMN api_key_enc;
"#;

/// V4：协议下放到端点。`provider_endpoints` 增加 protocol 列（从 providers 回填），
/// 之后删除 providers.protocol——同一 Provider 可混合不同协议的端点。
const V4: &str = r#"
ALTER TABLE provider_endpoints ADD COLUMN protocol TEXT NOT NULL DEFAULT '';

UPDATE provider_endpoints
SET protocol = (SELECT protocol FROM providers WHERE providers.key = provider_endpoints.provider_key);

ALTER TABLE providers DROP COLUMN protocol;
"#;

/// V5：用量记录增加 ttft_ms（首字延迟，毫秒；流式请求才有值）。
const V5: &str = "ALTER TABLE usage_records ADD COLUMN ttft_ms INTEGER;";

/// 全部 migration，按版本升序。
const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, sql: V1 },
    Migration { version: 2, sql: V2 },
    Migration { version: 3, sql: V3 },
    Migration { version: 4, sql: V4 },
    Migration { version: 5, sql: V5 },
];

/// 对连接执行 migration（幂等）。
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT 0
        );",
    )?;

    let current: i32 =
        conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| {
            r.get(0)
        })?;

    for m in MIGRATIONS {
        if m.version > current {
            conn.execute_batch(m.sql)?;
            conn.execute(
                "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![m.version, now_unix()],
            )?;
            tracing::info!(version = m.version, "已应用数据库 migration");
        }
    }
    Ok(())
}

/// 当前 schema 版本。
pub fn current_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
