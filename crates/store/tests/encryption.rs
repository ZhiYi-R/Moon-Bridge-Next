use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use moonbridge_store::{
    AesGcmKey, BalanceCard, Database, EncKey, Endpoint, PlaintextKey, Provider, StoreError,
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

fn seed(db: &Database) -> (Provider, Vec<BalanceCard>) {
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
        created_at: 0,
        updated_at: 0,
    };
    db.upsert_provider(&provider).unwrap();
    let cards = [
        "sk-balance-single-secret",
        "",
        "sk-balance-first-secret\n\n sk-balance-last-secret \nsk-balance-first-secret",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, api_key)| BalanceCard {
        key: format!("card-{index}"),
        provider_key: Some(provider.key.clone()),
        display_mode: "auto".into(),
        api_key: api_key.into(),
        base_url: "https://example.invalid/balance".into(),
        provider_label: "Example".into(),
        script_ref: String::new(),
        interval_secs: 60,
        enabled: true,
        extra: json!({}),
        position: index as i64,
        created_at: 1,
        updated_at: 0,
    })
    .collect::<Vec<_>>();
    for card in &cards {
        db.upsert_balance_card(card).unwrap();
    }
    (provider, cards)
}

fn assert_roundtrip(db: &Database, provider: &Provider, cards: &[BalanceCard]) {
    assert_eq!(
        db.get_provider(&provider.key).unwrap().unwrap().endpoints,
        provider.endpoints
    );
    assert_eq!(
        db.list_endpoints(&provider.key).unwrap(),
        provider.endpoints
    );
    assert_eq!(
        db.list_providers().unwrap()[0].endpoints,
        provider.endpoints
    );
    let listed = db.list_balance_cards().unwrap();
    let due = db.list_balance_cards_due(1_000).unwrap();
    assert_eq!(listed.len(), cards.len());
    assert_eq!(due.len(), cards.len());
    for (index, card) in cards.iter().enumerate() {
        assert_eq!(
            db.get_balance_card(&card.key).unwrap().unwrap().api_key,
            card.api_key
        );
        assert_eq!(listed[index].key, card.key);
        assert_eq!(listed[index].api_key, card.api_key);
        assert_eq!(due[index].key, card.key);
        assert_eq!(due[index].api_key, card.api_key);
    }
}

fn stored_keys(conn: &Connection) -> (Vec<String>, Vec<String>) {
    fn query(conn: &Connection, sql: &str) -> Vec<String> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }
    (
        query(
            conn,
            "SELECT api_key_enc FROM provider_endpoints ORDER BY provider_key, idx",
        ),
        query(
            conn,
            "SELECT api_key FROM balance_cards ORDER BY position, key",
        ),
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

fn assert_encrypted(path: &Path, provider: &Provider, cards: &[BalanceCard]) {
    let conn = Connection::open(path).unwrap();
    let (endpoints, balances) = stored_keys(&conn);
    assert_eq!(endpoints.len(), provider.endpoints.len());
    assert_eq!(balances.len(), cards.len());
    for (stored, original) in endpoints.iter().zip(&provider.endpoints) {
        assert_ne!(stored, &original.api_key);
        assert!(stored.starts_with("mbk:v1:"));
    }
    for (stored, original) in balances.iter().zip(cards) {
        assert_ne!(stored, &original.api_key);
        assert!(stored.starts_with("mbk:v1:"));
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
            .chain(cards.iter().flat_map(|card| card.api_key.lines()))
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

#[test]
fn default_disk_database_encrypts_both_key_kinds_and_reopens_in_order() {
    let dir = TestDir::new();
    let db = Database::open(dir.db()).unwrap();
    let (provider, cards) = seed(&db);
    assert_roundtrip(&db, &provider, &cards);
    assert_encrypted(&dir.db(), &provider, &cards);
    let key = fs::read(dir.key()).unwrap();
    #[cfg(unix)]
    assert_eq!(key.len(), 32);
    #[cfg(windows)]
    assert!(key.len() > 32);
    drop(db);
    assert_encrypted(&dir.db(), &provider, &cards);
    let reopened = Database::open(dir.db()).unwrap();
    assert_roundtrip(&reopened, &provider, &cards);
    assert_eq!(fs::read(dir.key()).unwrap(), key);
}

#[test]
fn explicit_key_file_is_used_without_creating_default_sidecar() {
    let dir = TestDir::new();
    let key_path = dir.0.join("master.key");
    let db = Database::open_with_key_file(&dir.db(), &key_path).unwrap();
    let (provider, cards) = seed(&db);
    let key = fs::read(&key_path).unwrap();
    #[cfg(unix)]
    assert_eq!(key.len(), 32);
    #[cfg(windows)]
    assert!(key.len() > 32);
    assert!(!dir.key().exists());
    drop(db);
    let reopened = Database::open_with_key_file(&dir.db(), &key_path).unwrap();
    assert_roundtrip(&reopened, &provider, &cards);
    assert_encrypted(&dir.db(), &provider, &cards);
    assert_eq!(fs::read(&key_path).unwrap(), key);
    assert!(!dir.key().exists());
}

#[test]
fn v12_plaintext_database_migrates_without_guessing_ciphertext_from_prefix() {
    let dir = TestDir::new();
    let db = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
    let (mut provider, mut cards) = seed(&db);
    provider.endpoints[0].api_key = "mbk:v1:not-actually-encrypted".into();
    cards[0].api_key = AesGcmKey::new(&[37; 32])
        .encrypt("still-a-plaintext-key")
        .unwrap();
    db.upsert_provider(&provider).unwrap();
    db.upsert_balance_card(&cards[0]).unwrap();
    assert_roundtrip(&db, &provider, &cards);
    drop(db);
    let conn = Connection::open(dir.db()).unwrap();
    conn.execute_batch(
        "DROP TABLE encryption_metadata; DELETE FROM schema_version WHERE version = 13;",
    )
    .unwrap();
    assert_eq!(
        moonbridge_store::schema::current_version(&conn).unwrap(),
        12
    );
    drop(conn);

    let migrated = Database::open(dir.db()).unwrap();
    assert_eq!(migrated.version().unwrap(), 13);
    assert_roundtrip(&migrated, &provider, &cards);
    assert_encrypted(&dir.db(), &provider, &cards);
    drop(migrated);
    let reopened = Database::open(dir.db()).unwrap();
    assert_roundtrip(&reopened, &provider, &cards);
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

fn seed_v12_custom_key(dir: &TestDir, old: &dyn EncKey) -> (Provider, Vec<BalanceCard>) {
    let db = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
    let (provider, cards) = seed(&db);
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
    conn.execute_batch(
        "DROP TABLE encryption_metadata; DELETE FROM schema_version WHERE version = 13;",
    )
    .unwrap();
    assert_eq!(
        moonbridge_store::schema::current_version(&conn).unwrap(),
        12
    );
    let (endpoints, balances) = stored_keys(&conn);
    for (stored, endpoint) in endpoints.iter().zip(&provider.endpoints) {
        assert_ne!(stored, &endpoint.api_key);
        assert!(stored.starts_with("mbk:v1:"));
    }
    assert_eq!(
        balances,
        cards
            .iter()
            .map(|card| card.api_key.clone())
            .collect::<Vec<_>>()
    );
    (provider, cards)
}

#[test]
fn v12_custom_provider_source_is_required_and_only_used_once() {
    let dir = TestDir::new();
    let old = CountingLegacyKey {
        key: AesGcmKey::new(&[41; 32]),
        decrypt_calls: AtomicUsize::new(0),
    };
    let (provider, cards) = seed_v12_custom_key(&dir, &old);
    let conn = Connection::open(dir.db()).unwrap();
    let original = stored_keys(&conn);
    drop(conn);

    let error = Database::open_with_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32])))
        .err()
        .expect("legacy provider source must not be guessed");
    assert!(error.to_string().contains("来源不明确"), "{error}");
    let conn = Connection::open(dir.db()).unwrap();
    assert_eq!(stored_keys(&conn), original);
    assert_eq!(state(&conn), ("plaintext".into(), String::new()));
    drop(conn);
    assert_eq!(old.decrypt_calls.load(Ordering::Relaxed), 0);

    let migrated =
        Database::open_with_legacy_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32])), &old)
            .unwrap();
    assert_eq!(migrated.version().unwrap(), 13);
    assert_roundtrip(&migrated, &provider, &cards);
    assert_eq!(
        old.decrypt_calls.load(Ordering::Relaxed),
        provider.endpoints.len()
    );
    drop(migrated);
    assert_encrypted(&dir.db(), &provider, &cards);
    let conn = Connection::open(dir.db()).unwrap();
    let migrated_keys = stored_keys(&conn);
    let migrated_state = state(&conn);
    assert_ne!(migrated_keys.0, original.0);
    assert_ne!(migrated_keys.1, original.1);
    drop(conn);

    let reopened =
        Database::open_with_legacy_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32])), &old)
            .unwrap();
    assert_roundtrip(&reopened, &provider, &cards);
    assert_eq!(
        old.decrypt_calls.load(Ordering::Relaxed),
        provider.endpoints.len()
    );
    drop(reopened);
    let reopened = Database::open_with_key(dir.db(), Box::new(AesGcmKey::new(&[42; 32]))).unwrap();
    assert_roundtrip(&reopened, &provider, &cards);
    let conn = Connection::open(dir.db()).unwrap();
    assert_eq!(stored_keys(&conn), migrated_keys);
    assert_eq!(state(&conn), migrated_state);
}

#[test]
fn v12_wrong_custom_provider_source_rolls_back_keys_and_encryption_metadata() {
    let dir = TestDir::new();
    let old = AesGcmKey::new(&[51; 32]);
    let (provider, cards) = seed_v12_custom_key(&dir, &old);
    let conn = Connection::open(dir.db()).unwrap();
    let original = stored_keys(&conn);
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
    assert_eq!(stored_keys(&conn), original);
    assert_eq!(state(&conn), ("plaintext".into(), String::new()));
    drop(conn);

    let migrated =
        Database::open_with_legacy_key(dir.db(), Box::new(AesGcmKey::new(&[52; 32])), &old)
            .unwrap();
    assert_roundtrip(&migrated, &provider, &cards);
    drop(migrated);
    assert_encrypted(&dir.db(), &provider, &cards);
}

#[test]
fn encrypted_database_rejects_missing_wrong_and_malformed_keys_without_replacement() {
    let dir = TestDir::new();
    let db = Database::open(dir.db()).unwrap();
    let (provider, cards) = seed(&db);
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
    assert_roundtrip(&reopened, &provider, &cards);
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
            "SELECT api_key FROM balance_cards WHERE key = 'card-0'",
            "UPDATE balance_cards SET api_key = ?1 WHERE key = 'card-0'",
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
    for fail_after in [1, 5, 7] {
        let dir = TestDir::new();
        let db = Database::open_with_key(dir.db(), Box::new(PlaintextKey)).unwrap();
        let (provider, cards) = seed(&db);
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
        assert_roundtrip(&reopened, &provider, &cards);
    }
}
