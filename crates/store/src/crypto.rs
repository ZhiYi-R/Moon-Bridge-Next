//! API Key 落库加密抽象。
//!
//! 脚手架阶段默认使用 [`PlaintextKey`]（明文），并以 `EncKey` trait 隔离，
//! 后续可无缝替换为 aes-gcm + 机器绑定密钥或系统 keyring（见计划假设）。

/// 密钥加解密接口。存储层写入 provider 时对 api_key 调用 [`EncKey::encrypt`]，
/// 读取时调用 [`EncKey::decrypt`]。
pub trait EncKey: Send + Sync {
    /// 明文 → 落库密文（或编码）。
    fn encrypt(&self, plaintext: &str) -> String;
    /// 落库密文 → 明文。
    fn decrypt(&self, stored: &str) -> String;
}

/// 明文实现（脚手架默认）。
///
/// TODO(security): 替换为 `aes-gcm` + 机器绑定密钥或系统 keyring。
#[derive(Debug, Default, Clone, Copy)]
pub struct PlaintextKey;

impl EncKey for PlaintextKey {
    fn encrypt(&self, plaintext: &str) -> String {
        plaintext.to_string()
    }
    fn decrypt(&self, stored: &str) -> String {
        stored.to_string()
    }
}
