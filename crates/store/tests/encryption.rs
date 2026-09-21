use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use moonbridge_store::{
    AesGcmKey, Database, EncKey, Endpoint, PlaintextKey, Provider, StoreError,
};
use rusqlite::{params, Connection};
use serde_json::json;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = fs::canonicalize(std::env::temp_dir()).unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!(
            "moonbridge-encryption-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn db(&self) -> PathBuf {
        self.0.join("store.sqlite")
    }

    fn key(&self) -> PathBuf {
        self.0.join("store.sqlite.key")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 配额配置的密钥类字段（断言其不以明文出现在数据库文件中）。
const QUOTA_SECRET: &str = "sk-quota-mgmt-secret";

fn seed(db: &Database) -> Provider {
    let provider = Provider {
        key: "provider".into(),
        endpoints: [
            "sk-provider-first-secret",
            "",
            "sk-provider-last-secret",
            "sk-provider-first-secret",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, api_key)| Endpoint {
            protocol: "anthropic".into(),
            base_url: format!("https://example.invalid/{index}"),
            api_key: api_key.into(),
        })
        .collect(),
        version: None,
        user_agent: None,
        web_search: None,
        extra: json!({}),
        enabled: true,
        quota_plugin_ref: "quota/test".into(),
        quota_interval_secs: 60,
        quota_enabled: true,
        quota_config: json!({"management_token": QUOTA_SECRET, "unit": "$"}),
        created_at: 0,
        updated_at: 0,
    };
    db.upsert_provider(&provider).unwrap();
    provider
}

fn assert_roundtrip(db: &Database, provider: &Provider) {
    assert_eq!(
        db.get_provider(&provider.key).unwrap().unwrap().endpoints,
        provider.endpoints
    );
    assert_eq!(
        db.list_endpoints(&provider.key).unwrap(),
        provider.endpoints
    );
    let listed = db.list_providers().unwrap();
    assert_eq!(listed[0].endpoints, provider.endpoints);
    assert_eq!(listed[0].quota_config, provider.quota_config);
    // 绑定且启用才到期；v12 降级场景配额字段被剥掉，不到期
    assert_eq!(
        db.list_quota_due(1_000_000)
            .unwrap()
            .iter()
            .any(|p| p.key == provider.key),
        provider.quota_enabled && provider.quota_interval_secs > 0
    );
}

fn stored_endpoint_keys(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT api_key_enc FROM provider_endpoints ORDER BY provider_key, idx")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn stored_keys(conn: &Connection) -> (Vec<String>, Vec<String>) {
    (
        stored_endpoint_keys(conn),
        conn.prepare("SELECT quota_config_enc FROM providers ORDER BY key")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap(),
    )
}

fn state(conn: &Connection) -> (String, String) {
    conn.query_row(
        "SELECT scheme, verifier FROM encryption_metadata WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

fn assert_encrypted(path: &Path, provider: &Provider) {
    let conn = Connection::open(path).unwrap();
    let (endpoints, quota_encs) = stored_keys(&conn);
    assert_eq!(endpoints.len(), provider.endpoints.len());
    assert_eq!(quota_encs.len(), 1, "一个 provider 一行 quota_config_enc");
    for (stored, original) in endpoints.iter().zip(&provider.endpoints) {
        assert_ne!(stored, &original.api_key);
        assert!(stored.starts_with("mbk:v1:"));
    }
    if !provider.quota_config.is_null() {
        assert!(quota_encs[0].starts_with("mbk:v1:"), "配额配置应加密写入数据库");
    }
    let (scheme, verifier) = state(&conn);
    assert_eq!(scheme, "aes256-gcm-v1");
    assert!(verifier.starts_with("mbk:v1:"));
    drop(conn);

    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        if sidecar.exists() {
            paths.push(sidecar);
        }
    }
    for file in paths {
        let bytes = fs::read(&file).unwrap();
        for secret in provider
            .endpoints
            .iter()
            .map(|endpoint| endpoint.api_key.as_str())
            .chain([QUOTA_SECRET])
            .filter(|secret| !secret.is_empty())
        {
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "plaintext secret remains in {}",
                file.display()
            );
        }
    }
}

/// V12 库没有配额列：断言用的 Provider 剥掉配额字段（降级后数据真实丢失）。
fn strip_quota(p: &Provider) -> Provider {
    let mut p = p.clone();
    p.quota_plugin_ref.clear();
    p.quota_interval_secs = 0;
    p.quota_enabled = false;
    p.quota_config = serde_json::Value::Null;
    p
}

/// 把当前数据库伪装成 V12：encryption_metadata 与 V14 新增列一并回滚。
fn downgrade_to_v12(conn: &Connection) {
    conn.execute_batch(
        "DROP TABLE encryption_metadata;
         ALTER TABLE providers DROP COLUMN quota_plugin_ref;
         ALTER TABLE providers DROP COLUMN quota_interval_secs;
         ALTER TABLE providers DROP COLUMN quota_enabled;
         ALTER TABLE providers DROP COLUMN quota_config_enc;
         ALTER TABLE plugins DROP COLUMN category;
         ALTER TABLE plugins DROP COLUMN config_schema_json;
         DROP TABLE quota_results;
         DELETE FROM schema_version WHERE version >= 13;",
    )
    .unwrap();
    assert_eq!(
        moonbridge_store::schema::current_version(&conn).unwrap(),
        12
    );
}

#[test]
fn default_disk_database_encrypts_both_key_kinds_and_reopens_in_order() {
    let dir = TestDir::new();
    let db = Database::open(dir.db()).unwrap();
    let provider = seed(&db);
    assert_roundtrip(&db, &provider);
    assert_encrypted(&dir.db(), &provider);
    let key = fs::read(dir.key()).unwrap();
    #[cfg(unix)]
    assert_eq!(key.len(), 32);
    #[cfg(windows)]
    assert!(key.len() > 32);
    drop(db);
    assert_encrypted(&dir.db(), &provider);
    let reopened = Database::open(dir.db()).unwrap();
    assert_roundtrip(&reopened, &provider);
    assert_eq!(fs::read(dir.key()).unwrap(), key);
}

#[test]
fn explicit_key_file_is_used_without_creating_default_sidecar() {
    let dir = TestDir::new();
    let key_path = dir.0.join("master.key");
    let db = Database::open_with_key_file(&dir.db(), &key_path).unwrap();
    let provider = seed(&db);
    let key = fs::read(&key_path).unwrap();
    #[cfg(unix)]
    assert_eq!(key.len(), 32);
    #[cfg(windows)]
    assert!(key.len() > 32);
    assert!(!dir.key().exists());
    drop(db);
    let reopened = Database::open_with_key_file(&dir.db(), &key_path).unwrap();
    assert_roundtrip(&reopened, &provider);
    assert_encrypted(&dir.db(), &provider);
    assert_eq!(fs::read(&key_path).unwrap(), key);
    assert!(!dir.key().exists());
}

#[test]
fn v12_plaintext_database_migrates_without_guessing_ciphertext_from_prefix() {
    let dir = TestDir::new();
    let db = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
    let mut provider = seed(&db);
    provider.endpoints[0].api_key = "mbk:v1:not-actually-encrypted".into();
    provider.quota_config["management_token"] = json!("mbk:v1:not-actually-encrypted");
    db.upsert_provider(&provider).unwrap();
    assert_roundtrip(&db, &provider);
    drop(db);
    let conn = Connection::open(dir.db()).unwrap();
    downgrade_to_v12(&conn);
    drop(conn);

    let migrated = Database::open(dir.db()).unwrap();
    assert_eq!(migrated.version().unwrap(), 14);
    let provider = strip_quota(&provider);
    assert_roundtrip(&migrated, &provider);
    assert_encrypted(&dir.db(), &provider);
    drop(migrated);
    let reopened = Database::open(dir.db()).unwrap();
    assert_roundtrip(&reopened, &provider);
    assert!(Database::open_with_key(dir.db(), Box::new(PlaintextKey)).is_err());
}

struct CountingLegacyKey {
    key: AesGcmKey,
    decrypt_calls: AtomicUsize,
}

impl EncKey for CountingLegacyKey {
    fn encrypt(&self, plaintext: &str) -> moonbridge_store::Result<String> {
        self.key.encrypt(plaintext)
    }

    fn decrypt(&self, stored: &str) -> moonbridge_store::Result<String> {
        self.decrypt_calls.fetch_add(1, Ordering::Relaxed);
        self.key.decrypt(stored)
    }

    fn scheme(&self) -> &str {
        self.key.scheme()
    }
}

fn seed_v12_custom_key(dir: &TestDir, old: &dyn EncKey) -> Provider {
    let db = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
    let provider = seed(&db);
    drop(db);
    let conn = Connection::open(dir.db()).unwrap();
    for (index, endpoint) in provider.endpoints.iter().enumerate() {
        conn.execute(
            "UPDATE provider_endpoints SET api_key_enc = ?1 WHERE provider_key = ?2 AND idx = ?3",
            params![
                old.encrypt(&endpoint.api_key).unwrap(),
                provider.key,
                index as i64
            ],
        )
        .unwrap();
    }
    let (endpoints, quota_encs) = stored_keys(&conn);
    for (stored, endpoint) in endpoints.iter().zip(&provider.endpoints) {
        assert_ne!(stored, &endpoint.api_key);
        assert!(stored.starts_with("mbk:v1:"));
    }
    // 配额配置在 plaintext scheme 下是明文 JSON（与旧版 api_key 口径一致）
    assert!(
        quota_encs[0].contains(QUOTA_SECRET),
        "plaintext scheme 下配额配置明文写入数据库"
    );
    downgrade_to_v12(&conn);
    provider
}

#[test]
fn v12_custom_provider_source_is_required_and_only_used_once() {
    let dir = TestDir::new();
    let old = CountingLegacyKey {
        key: AesGcmKey::new(&[41; 32]),
        decrypt_calls: AtomicUsize::new(0),
    };
    let provider = strip_quota(&seed_v12_custom_key(&dir, &old));
    let conn = Connection::open(dir.db()).unwrap();
    let original = stored_endpoint_keys(&conn);
    drop(conn);

    let error = Database::open_with_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32])))
        .err()
        .expect("legacy provider source must not be guessed");
    assert!(error.to_string().contains("来源不明确"), "{error}");
    let conn = Connection::open(dir.db()).unwrap();
    assert_eq!(stored_endpoint_keys(&conn), original);
    assert_eq!(state(&conn), ("plaintext".into(), String::new()));
    drop(conn);
    assert_eq!(old.decrypt_calls.load(Ordering::Relaxed), 0);

    let migrated =
        Database::open_with_legacy_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32])), &old)
            .unwrap();
    assert_eq!(migrated.version().unwrap(), 14);
    assert_roundtrip(&migrated, &provider);
    assert_eq!(
        old.decrypt_calls.load(Ordering::Relaxed),
        provider.endpoints.len()
    );
    drop(migrated);
    assert_encrypted(&dir.db(), &provider);
    let conn = Connection::open(dir.db()).unwrap();
    let migrated_keys = stored_endpoint_keys(&conn);
    let migrated_state = state(&conn);
    assert_ne!(migrated_keys, original);
    drop(conn);

    let reopened =
        Database::open_with_legacy_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32])), &old)
            .unwrap();
    assert_roundtrip(&reopened, &provider);
    assert_eq!(
        old.decrypt_calls.load(Ordering::Relaxed),
        provider.endpoints.len()
    );
    drop(reopened);
    let reopened = Database::open_with_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32]))).unwrap();
    assert_roundtrip(&reopened, &provider);
    let conn = Connection::open(dir.db()).unwrap();
    assert_eq!(stored_endpoint_keys(&conn), migrated_keys);
    assert_eq!(state(&conn), migrated_state);
}

#[test]
fn v12_wrong_custom_provider_source_rolls_back_keys_and_encryption_metadata() {
    let dir = TestDir::new();
    let old = AesGcmKey::new(&[51; 32]);
    let provider = strip_quota(&seed_v12_custom_key(&dir, &old));
    let conn = Connection::open(dir.db()).unwrap();
    let original = stored_endpoint_keys(&conn);
    drop(conn);

    let error = Database::open_with_legacy_key(
        dir.db(),
        Box::new(AesGcmKey::new(&[52; 32])),
        &AesGcmKey::new(&[53; 32]),
    )
    .err()
    .expect("wrong legacy source must fail authentication");
    assert!(matches!(error, StoreError::Encryption(_)), "{error}");
    let conn = Connection::open(dir.db()).unwrap();
    assert_eq!(stored_endpoint_keys(&conn), original);
    assert_eq!(state(&conn), ("plaintext".into(), String::new()));
    drop(conn);

    let migrated =
        Database::open_with_legacy_key(dir.db(), Box::new(AesGcmKey::new(&[52; 32])), &old)
            .unwrap();
    assert_roundtrip(&migrated, &provider);
    drop(migrated);
    assert_encrypted(&dir.db(), &provider);
}

#[test]
fn encrypted_database_rejects_missing_wrong_and_malformed_keys_without_replacement() {
    let dir = TestDir::new();
    let db = Database::open(dir.db()).unwrap();
    let provider = seed(&db);
    drop(db);
    let key = fs::read(dir.key()).unwrap();
    let conn = Connection::open(dir.db()).unwrap();
    let original = stored_keys(&conn);
    drop(conn);

    let mut wrong = key.clone();
    wrong[0] ^= 1;
    for invalid in [wrong, vec![0; 31], vec![0; 33], Vec::new()] {
        fs::write(dir.key(), &invalid).unwrap();
        assert!(Database::open(dir.db()).is_err());
        assert_eq!(fs::read(dir.key()).unwrap(), invalid);
        let conn = Connection::open(dir.db()).unwrap();
        assert_eq!(stored_keys(&conn), original);
    }
    fs::write(dir.key(), &key).unwrap();
    let reopened = Database::open(dir.db()).unwrap();
    assert_roundtrip(&reopened, &provider);
    drop(reopened);
    fs::remove_file(dir.key()).unwrap();
    assert!(Database::open(dir.db()).is_err());
    assert!(!dir.key().exists(), "missing key must not be regenerated");
    let conn = Connection::open(dir.db()).unwrap();
    assert_eq!(stored_keys(&conn), original);
}

#[test]
fn altered_ciphertext_or_verifier_is_rejected_on_open() {
    for (select, update) in [
        (
            "SELECT api_key_enc FROM provider_endpoints WHERE idx = 0",
            "UPDATE provider_endpoints SET api_key_enc = ?1 WHERE idx = 0",
        ),
        (
            "SELECT quota_config_enc FROM providers WHERE key = 'provider'",
            "UPDATE providers SET quota_config_enc = ?1 WHERE key = 'provider'",
        ),
        (
            "SELECT verifier FROM encryption_metadata WHERE id = 1",
            "UPDATE encryption_metadata SET verifier = ?1 WHERE id = 1",
        ),
    ] {
        let dir = TestDir::new();
        let db = Database::open(dir.db()).unwrap();
        seed(&db);
        drop(db);
        let key = fs::read(dir.key()).unwrap();
        let conn = Connection::open(dir.db()).unwrap();
        let original: String = conn.query_row(select, [], |row| row.get(0)).unwrap();
        let mut bytes = original.into_bytes();
        let index = "mbk:v1:".len() + 16;
        bytes[index] = if bytes[index] == b'A' { b'B' } else { b'A' };
        let altered = String::from_utf8(bytes).unwrap();
        conn.execute(update, params![altered]).unwrap();
        drop(conn);
        assert!(
            Database::open(dir.db()).is_err(),
            "accepted alteration: {update}"
        );
        assert_eq!(fs::read(dir.key()).unwrap(), key);
        let conn = Connection::open(dir.db()).unwrap();
        assert_eq!(
            conn.query_row(select, [], |row| row.get::<_, String>(0))
                .unwrap(),
            altered
        );
    }
}

#[cfg(unix)]
#[test]
fn key_file_permissions_and_symlinks_are_rejected_without_rewriting() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let dir = TestDir::new();
    drop(Database::open(dir.db()).unwrap());
    let key = fs::read(dir.key()).unwrap();
    assert_eq!(
        fs::metadata(dir.key()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(dir.key(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Database::open(dir.db()).is_err());
    assert_eq!(fs::read(dir.key()).unwrap(), key);
    assert_eq!(
        fs::metadata(dir.key()).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::set_permissions(dir.key(), fs::Permissions::from_mode(0o600)).unwrap();
    let target = dir.0.join("original.key");
    fs::rename(dir.key(), &target).unwrap();
    symlink(&target, dir.key()).unwrap();
    assert!(Database::open(dir.db()).is_err());
    assert!(fs::symlink_metadata(dir.key())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&target).unwrap(), key);
    fs::remove_file(dir.key()).unwrap();
    fs::hard_link(&target, dir.key()).unwrap();
    assert!(Database::open(dir.db()).is_err());
    assert_eq!(fs::read(&target).unwrap(), key);
}

struct FailingKey {
    calls: AtomicUsize,
    fail_after: usize,
}

impl EncKey for FailingKey {
    fn encrypt(&self, plaintext: &str) -> moonbridge_store::Result<String> {
        if self.calls.fetch_add(1, Ordering::Relaxed) == self.fail_after {
            return Err(StoreError::Encryption("injected migration failure".into()));
        }
        Ok(format!("test-encrypted:{plaintext}"))
    }

    fn decrypt(&self, stored: &str) -> moonbridge_store::Result<String> {
        stored
            .strip_prefix("test-encrypted:")
            .map(str::to_owned)
            .ok_or_else(|| StoreError::Encryption("invalid test ciphertext".into()))
    }

    fn scheme(&self) -> &str {
        "test-failing-v1"
    }
}

#[test]
fn migration_failure_rolls_back_all_keys_and_metadata() {
    // 加密调用顺序：4 个端点 + 1 条配额配置 + verifier——0/3/5 覆盖首/中/尾。
    for fail_after in [0, 3, 5] {
        let dir = TestDir::new();
        let db = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
        let provider = seed(&db);
        drop(db);
        let conn = Connection::open(dir.db()).unwrap();
        let original_keys = stored_keys(&conn);
        let original_state = state(&conn);
        assert_eq!(original_state, ("plaintext".into(), String::new()));
        drop(conn);
        let error = Database::open_with_legacy_key(
            dir.db(),
            Box::new(FailingKey {
                calls: AtomicUsize::new(0),
                fail_after,
            }),
            &PlaintextKey,
        )
        .err()
        .expect("migration must propagate encryption failure");
        assert!(
            error.to_string().contains("injected migration failure"),
            "{error}"
        );
        let conn = Connection::open(dir.db()).unwrap();
        assert_eq!(stored_keys(&conn), original_keys);
        assert_eq!(state(&conn), original_state);
        drop(conn);
        let reopened = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
        assert_roundtrip(&reopened, &provider);
    }
}
