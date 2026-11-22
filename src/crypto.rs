//! AES-256-GCM：整包配置加密 + 旧版单字段密码解密。
//!
//! 密钥由固定口令 SHA-256 派生，和程序一起编译进 exe。这不是「防住逆向」的加密，
//! 目标是让配置在磁盘上不是明文、换台电脑拷走 exe 还能解开（便携性优先）。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

const MASTER_PASSPHRASE: &[u8] = b"rdp-launcher::portable::master-key::v1";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

fn master_key() -> [u8; 32] {
    let digest = Sha256::digest(MASTER_PASSPHRASE);
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// 返回 `nonce(12) || ciphertext+tag`。
pub fn encrypt_blob(plain: &[u8]) -> Result<Vec<u8>, String> {
    let cipher =
        Aes256Gcm::new_from_slice(&master_key()).map_err(|_| "初始化加密器失败".to_string())?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|_| "加密配置失败".to_string())?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt_blob(blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < NONCE_LEN + TAG_LEN {
        return Err("配置数据太短，已损坏".into());
    }
    let cipher =
        Aes256Gcm::new_from_slice(&master_key()).map_err(|_| "初始化解密器失败".to_string())?;
    let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| "配置解密失败，数据可能被修改或损坏".to_string())
}

/// 旧版 `rdp_hosts.json` 里的密码字段：Base64(nonce||密文)。迁移时用。
pub fn decrypt_legacy_password(encoded: &str) -> String {
    if encoded.is_empty() {
        return String::new();
    }
    let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return String::new();
    };
    match decrypt_blob(&blob) {
        Ok(plain) => String::from_utf8(plain).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_roundtrip() {
        let secret = b"hosts with \xe5\xaf\x86\xe7\xa0\x81";
        let blob = encrypt_blob(secret).unwrap();
        assert_ne!(&blob[..], &secret[..]);
        assert_eq!(decrypt_blob(&blob).unwrap(), secret);
    }

    #[test]
    fn tampered_blob_is_rejected() {
        let mut blob = encrypt_blob(b"hello").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(decrypt_blob(&blob).is_err());
    }

    #[test]
    fn short_blob_is_rejected() {
        assert!(decrypt_blob(b"abc").is_err());
    }
}
