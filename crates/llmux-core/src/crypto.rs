use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use scrypt::{scrypt, Params};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use zeroize::Zeroize;

static KEY_CACHE: OnceLock<Mutex<HashMap<String, [u8; 32]>>> = OnceLock::new();

const VERSION_PREFIX: &str = "v1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

pub fn encrypt_api_key(plaintext: &str, secret: &str) -> Result<String> {
    if secret.is_empty() {
        return Err(anyhow!("encryption secret must not be empty"));
    }

    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce_bytes);

    let mut key = derive_key(secret, &salt)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("failed to create AES-GCM cipher"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| anyhow!("failed to encrypt api key"))?;
    key.zeroize();

    Ok(format!(
        "{}:{}:{}:{}",
        VERSION_PREFIX,
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
    if version != VERSION_PREFIX {
        return Err(anyhow!("unsupported ciphertext version"));
    }
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

    let mut key = derive_key(secret, &salt)?;
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

fn derive_key(secret: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    // ponytail: cache by (salt_hex, secret_hash) — same account decrypted every request; hash avoids retaining secret plaintext in cache key
    let salt_hex = hex::encode(salt);
    let secret_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        secret.hash(&mut h);
        // mix length to reduce collision
        secret.len().hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let cache_key = format!("{salt_hex}:{secret_hash}");
    if let Some(cache) = KEY_CACHE.get() {
        if let Ok(guard) = cache.lock() {
            if let Some(cached) = guard.get(&cache_key) {
                return Ok(*cached);
            }
        }
    }
    let params = Params::recommended();
    let mut key = [0u8; KEY_LEN];
    scrypt(secret.as_bytes(), salt, &params, &mut key).context("derive encryption key")?;
    let cache = KEY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = cache.lock() {
        if guard.len() < 1024 {
            guard.insert(cache_key, key);
        }
    }
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
