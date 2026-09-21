//! secrets 表 DAO：按 (scope, key) 隔离的加密小值存储。
//!
//! 值在落库前经 `EncKey` 加密、读出后解密；scope 语义由调用方约定
//! （如 `provider:{key}` 存 OAuth 令牌包、`{plugin}/{scope}` 存插件级秘密），
//! DAO 不解释 scope，只做边界校验与加解密。

use rusqlite::params;

use crate::error::{Result, StoreError};
use crate::Database;

/// scope / key 长度上限（OAuth 令牌包等用途远低于此）。
const MAX_NAME_LEN: usize = 128;
/// value 明文上限（64 KiB 对令牌包已非常宽裕）。
const MAX_VALUE_LEN: usize = 64 * 1024;

fn validate<'a>(scope: &'a str, key: &'a str) -> Result<(&'a str, &'a str)> {
    let scope = scope.trim();
    let key = key.trim();
    if scope.is_empty() || key.is_empty() {
        return Err(StoreError::Other("secret 的 scope/key 不能为空".into()));
    }
    if scope.len() > MAX_NAME_LEN || key.len() > MAX_NAME_LEN {
        return Err(StoreError::Other(format!(
            "secret 的 scope/key 超长（上限 {MAX_NAME_LEN}）"
        )));
    }
    Ok((scope, key))
}

impl Database {
    /// 读取 secret（不存在返回 `Ok(None)`；值已解密）。
    pub fn secret_get(&self, scope: &str, key: &str) -> Result<Option<String>> {
        let (scope, key) = validate(scope, key)?;
        let stored: Option<String> = {
            let conn = self.conn.lock();
            let mut stmt =
                conn.prepare("SELECT value_enc FROM secrets WHERE scope = ?1 AND key = ?2")?;
            let mut rows = stmt.query(params![scope, key])?;
            match rows.next()? {
                Some(row) => Some(row.get(0)?),
                None => None,
            }
        };
        match stored {
            Some(enc) => Ok(Some(self.enc.decrypt(&enc)?)),
            None => Ok(None),
        }
    }

    /// 写入 secret（upsert；值落库前加密）。
    pub fn secret_set(&self, scope: &str, key: &str, value: &str) -> Result<()> {
        let (scope, key) = validate(scope, key)?;
        if value.len() > MAX_VALUE_LEN {
            return Err(StoreError::Other(format!(
                "secret value 超长（上限 {MAX_VALUE_LEN} 字节）"
            )));
        }
        let enc = self.enc.encrypt(value)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO secrets (scope, key, value_enc, updated_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(scope, key) DO UPDATE SET value_enc=excluded.value_enc, updated_at=excluded.updated_at",
            params![scope, key, enc, crate::now_unix()],
        )?;
        Ok(())
    }

    /// 删除 secret（幂等）。
    pub fn secret_delete(&self, scope: &str, key: &str) -> Result<()> {
        let (scope, key) = validate(scope, key)?;
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM secrets WHERE scope = ?1 AND key = ?2",
            params![scope, key],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::AesGcmKey;

    fn enc_db() -> Database {
        Database::open_with_key(":memory:", Box::new(AesGcmKey::new(&[3u8; 32]))).unwrap()
    }

    #[test]
    fn roundtrip_isolated_by_scope() {
        let db = enc_db();
        assert_eq!(db.secret_get("provider:a", "oauth").unwrap(), None);
        db.secret_set("provider:a", "oauth", "token-1").unwrap();
        db.secret_set("plugin:auth-kimi", "device_id", "dev-1")
            .unwrap();
        assert_eq!(
            db.secret_get("provider:a", "oauth").unwrap().as_deref(),
            Some("token-1")
        );
        // scope 隔离：同名 key 互不可见
        assert_eq!(db.secret_get("provider:b", "oauth").unwrap(), None);
        // upsert 覆盖
        db.secret_set("provider:a", "oauth", "token-2").unwrap();
        assert_eq!(
            db.secret_get("provider:a", "oauth").unwrap().as_deref(),
            Some("token-2")
        );
        // 删除幂等
        db.secret_delete("provider:a", "oauth").unwrap();
        db.secret_delete("provider:a", "oauth").unwrap();
        assert_eq!(db.secret_get("provider:a", "oauth").unwrap(), None);
        assert_eq!(
            db.secret_get("plugin:auth-kimi", "device_id")
                .unwrap()
                .as_deref(),
            Some("dev-1")
        );
        // 落库的是密文不是明文
        let conn = db.conn.lock();
        let raw: String = conn
            .query_row(
                "SELECT value_enc FROM secrets WHERE scope='plugin:auth-kimi'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(raw.starts_with("mbk:v1:"), "必须是加密形态: {raw}");
        assert!(!raw.contains("dev-1"), "密文不得含明文");
    }

    #[test]
    fn boundary_validation() {
        let db = enc_db();
        assert!(db.secret_set("", "k", "v").is_err());
        assert!(db.secret_set("s", "  ", "v").is_err());
        assert!(db.secret_set(&"s".repeat(129), "k", "v").is_err());
        assert!(db.secret_set("s", "k", &"v".repeat(65 * 1024)).is_err());
        assert!(db.secret_get(" ", "k").is_err());
    }
}
