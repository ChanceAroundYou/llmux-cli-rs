use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use scrypt::{scrypt, Params};
use std::fs;
use std::path::Path;
use zeroize::Zeroize;

const VERSION_PREFIX: &str = "v1";
const VERSION_PREFIX_V2: &str = "v2";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
/// 新密文的 scrypt 工作因子。master.key 是 32 随机字节高熵，KDF 强度非安全瓶颈，
/// 用 log_n=13 让加解密快约 30 倍（debug ~8s → ~250ms）。
const DEFAULT_LOG_N: u8 = 13;
/// v1 旧密文（Params::recommended）的 scrypt 工作因子，仅用于兼容解密。
const LEGACY_LOG_N: u8 = 15;

/// 加密厂商 api_key：`v2:{log_n}:{salt}:{nonce}:{ciphertext}`。
/// 密文自描述工作因子，未来再调强度也无需迁移旧数据。
pub fn encrypt_api_key(plaintext: &str, secret: &str) -> Result<String> {
    if secret.is_empty() {
        return Err(anyhow!("encryption secret must not be empty"));
    }

    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_bytes);

    let mut key = derive_key(secret, &salt, DEFAULT_LOG_N)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("failed to create AES-GCM cipher"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| anyhow!("failed to encrypt api key"))?;
    key.zeroize();

    Ok(format!(
        "{}:{}:{}:{}:{}",
        VERSION_PREFIX_V2,
        DEFAULT_LOG_N,
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(nonce_bytes),
        STANDARD_NO_PAD.encode(ciphertext)
    ))
}

pub fn decrypt_api_key(encoded: &str, secret: &str) -> Result<String> {
    if secret.is_empty() {
        return Err(anyhow!("encryption secret must not be empty"));
    }

    let mut parts = encoded.split(':');
    let version = parts
        .next()
        .ok_or_else(|| anyhow!("missing ciphertext version"))?;

    let log_n = match version {
        // v1 旧格式：无显式工作因子，用 Params::recommended 的值
        VERSION_PREFIX => LEGACY_LOG_N,
        // v2 格式：第二个字段为 scrypt log_n
        VERSION_PREFIX_V2 => {
            let raw = parts
                .next()
                .ok_or_else(|| anyhow!("missing scrypt log_n"))?;
            raw.parse::<u8>().context("invalid scrypt log_n")?
        }
        _ => return Err(anyhow!("unsupported ciphertext version")),
    };

    let salt = decode_part(parts.next(), "salt")?;
    let nonce = decode_part(parts.next(), "nonce")?;
    let ciphertext = decode_part(parts.next(), "ciphertext")?;
    if parts.next().is_some() {
        return Err(anyhow!("invalid ciphertext format"));
    }
    if salt.len() != SALT_LEN {
        return Err(anyhow!("invalid salt length"));
    }
    if nonce.len() != NONCE_LEN {
        return Err(anyhow!("invalid nonce length"));
    }

    let mut key = derive_key(secret, &salt, log_n)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("failed to create AES-GCM cipher"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("failed to decrypt api key"))?;
    key.zeroize();

    String::from_utf8(plaintext).context("decrypted api key is not valid UTF-8")
}

fn decode_part(part: Option<&str>, name: &str) -> Result<Vec<u8>> {
    let value = part.ok_or_else(|| anyhow!("missing {name}"))?;
    STANDARD_NO_PAD
        .decode(value)
        .with_context(|| format!("invalid {name} encoding"))
}

fn derive_key(secret: &str, salt: &[u8], log_n: u8) -> Result<[u8; KEY_LEN]> {
    let params = Params::new(log_n, 8, 1, KEY_LEN).context("invalid scrypt params")?;
    let mut key = [0u8; KEY_LEN];
    scrypt(secret.as_bytes(), salt, &params, &mut key).context("derive encryption key")?;
    Ok(key)
}

pub fn get_or_create_master_key(data_dir: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(value) = explicit.filter(|v| !v.trim().is_empty()) {
        return Ok(value.to_string());
    }

    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("master.key");
    if path.exists() {
        return Ok(fs::read_to_string(&path)?.trim().to_string());
    }

    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let generated = hex::encode(bytes);
    bytes.zeroize();
    fs::write(&path, &generated)?;
    Ok(generated)
}
