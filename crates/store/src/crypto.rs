//! API Key 落库加密与主密钥文件。

#[cfg(not(windows))]
use std::path::Path;

use aes_gcm::aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};

use crate::error::{Result, StoreError};

pub trait EncKey: Send + Sync {
    fn encrypt(&self, plaintext: &str) -> Result<String>;
    fn decrypt(&self, stored: &str) -> Result<String>;

    /// 持久化格式标识；自定义实现应提供稳定且唯一的值。
    fn scheme(&self) -> &str {
        "custom-v1"
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PlaintextKey;

impl EncKey for PlaintextKey {
    fn encrypt(&self, plaintext: &str) -> Result<String> {
        Ok(plaintext.to_string())
    }

    fn decrypt(&self, stored: &str) -> Result<String> {
        Ok(stored.to_string())
    }

    fn scheme(&self) -> &str {
        "plaintext"
    }
}

pub struct AesGcmKey(Aes256Gcm);

impl AesGcmKey {
    pub fn new(key: &[u8; 32]) -> Self {
        Self(Aes256Gcm::new(key.into()))
    }
}

const PREFIX: &str = "mbk:v1:";
const AAD: &[u8] = b"moonbridge-store:api-key:v1";

impl EncKey for AesGcmKey {
    fn encrypt(&self, plaintext: &str) -> Result<String> {
        let mut nonce = [0u8; 12];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| StoreError::Encryption("系统随机数不可用".into()))?;
        let ciphertext = self
            .0
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: AAD,
                },
            )
            .map_err(|_| StoreError::Encryption("加密失败".into()))?;
        let mut bytes = nonce.to_vec();
        bytes.extend_from_slice(&ciphertext);
        Ok(format!("{PREFIX}{}", STANDARD.encode(bytes)))
    }

    fn decrypt(&self, stored: &str) -> Result<String> {
        let encoded = stored
            .strip_prefix(PREFIX)
            .ok_or_else(|| StoreError::Encryption("不支持的密文格式".into()))?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| StoreError::Encryption("密文编码无效".into()))?;
        if bytes.len() < 28 {
            return Err(StoreError::Encryption("密文长度无效".into()));
        }
        let plaintext = self
            .0
            .decrypt(
                Nonce::from_slice(&bytes[..12]),
                Payload {
                    msg: &bytes[12..],
                    aad: AAD,
                },
            )
            .map_err(|_| StoreError::Encryption("密文认证失败".into()))?;
        String::from_utf8(plaintext).map_err(|_| StoreError::Encryption("密文明文编码无效".into()))
    }

    fn scheme(&self) -> &str {
        "aes256-gcm-v1"
    }
}

#[cfg(unix)]
pub(crate) fn load_key_file(path: &Path, allow_create: bool) -> Result<AesGcmKey> {
    use std::fs::{self, File, Metadata, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    fn validate(meta: &Metadata) -> Result<()> {
        if !meta.is_file() || meta.mode() & 0o7177 != 0 || meta.nlink() != 1 {
            return Err(StoreError::Encryption(
                "主密钥必须是权限不宽于 0600 的独立普通文件".into(),
            ));
        }
        Ok(())
    }

    fn same_file(a: &Metadata, b: &Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }

    let original_parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(original_parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| StoreError::Encryption("主密钥文件名无效".into()))?;
    let resolved_path = parent.join(name);
    let parent_before = fs::symlink_metadata(&parent)?;
    if !parent_before.is_dir() {
        return Err(StoreError::Encryption("主密钥父路径不是目录".into()));
    }
    let check_parent = || -> Result<()> {
        let current = fs::symlink_metadata(&parent)?;
        if !current.is_dir()
            || !same_file(&parent_before, &current)
            || fs::canonicalize(original_parent)? != parent
        {
            return Err(StoreError::Encryption("主密钥父路径发生变化".into()));
        }
        Ok(())
    };
    check_parent()?;
    let mut created_key = None;
    let mut file = match fs::symlink_metadata(&resolved_path) {
        Ok(before) => {
            validate(&before)?;
            let file = File::open(&resolved_path)?;
            let opened = file.metadata()?;
            validate(&opened)?;
            if !same_file(&before, &opened) {
                return Err(StoreError::Encryption("打开主密钥时文件发生变化".into()));
            }
            file
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && allow_create => {
            let mut key = [0u8; 32];
            OsRng
                .try_fill_bytes(&mut key)
                .map_err(|_| StoreError::Encryption("系统随机数不可用".into()))?;
            let mut file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&resolved_path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    check_parent()?;
                    return load_key_file(path, false);
                }
                Err(e) => return Err(e.into()),
            };
            validate(&file.metadata()?)?;
            let named = fs::symlink_metadata(&resolved_path)?;
            validate(&named)?;
            check_parent()?;
            if !same_file(&file.metadata()?, &named) {
                return Err(StoreError::Encryption("创建主密钥时路径发生变化".into()));
            }
            file.write_all(&key)?;
            file.sync_all()?;
            File::open(&parent)?.sync_all()?;
            file.seek(SeekFrom::Start(0))?;
            created_key = Some(key);
            file
        }
        Err(e) => return Err(e.into()),
    };
    let check_file = |file: &File| -> Result<()> {
        let opened = file.metadata()?;
        let after = fs::symlink_metadata(&resolved_path)?;
        validate(&opened)?;
        validate(&after)?;
        check_parent()?;
        if !same_file(&opened, &after) {
            return Err(StoreError::Encryption("主密钥路径发生变化".into()));
        }
        Ok(())
    };
    check_file(&file)?;
    let mut bytes = Vec::new();
    (&mut file).take(33).read_to_end(&mut bytes)?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::Encryption("主密钥长度必须为 32 字节".into()))?;
    check_file(&file)?;
    if created_key.is_some_and(|expected| expected != key) {
        return Err(StoreError::Encryption("主密钥读回校验失败".into()));
    }
    Ok(AesGcmKey::new(&key))
}

#[cfg(windows)]
#[path = "crypto_windows.rs"]
mod windows;

#[cfg(windows)]
pub(crate) use windows::load_key_file;

#[cfg(not(any(unix, windows)))]
pub(crate) fn load_key_file(_path: &Path, _allow_create: bool) -> Result<AesGcmKey> {
    Err(StoreError::Encryption(
        "当前平台无法保证主密钥文件权限；请显式提供 EncKey".into(),
    ))
}
