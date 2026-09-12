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

// ---------------------------------------------------------------------------
// Admin password hashing
//
// 与 `encrypt_api_key` 不同，这里**不是**为了能解回来：登录只需要「验」，不需要
// 「取」。所以不加 AES，只做 scrypt 加盐哈希 —— 拿到库文件也无法还原密码。
// 复用同一套 `Params::recommended()`（n=2^17），与既有 KDF 强度一致。

const PASSWORD_VERSION: &str = "v1";

/// 密码哈希参数：**故意不用 `Params::recommended()`**。
///
/// `recommended()` 是 n=2^17, r=8 → 每次哈希分配 128MiB。这对跑在 2GB 路由器上、
/// 平时只占 18MiB 的进程是危险的：登录接口公开可达，几个并发请求就能把内存打爆
/// （等于给了一个不用认证的 DoS）。这里取 OWASP 密码存储清单里的低内存档
/// n=2^14, r=8, p=5（16MiB，靠 p 把工作量补回来），内存有上界、强度仍在推荐档内。
///
/// 注意 API key 的 `derive_key` 仍是 `recommended()` —— 那条路径有进程内缓存、
/// 每把 key 只算一次，且不在公开的未认证路径上。
fn password_params() -> Params {
    Params::new(14, 8, 5, KEY_LEN).expect("static scrypt params are valid")
}

/// 生成 `v1:<salt_b64>:<hash_b64>`。每次调用都用新随机盐，同一个密码两次
/// 哈希结果不同（防彩虹表 / 防「两个账号密码相同」被看出来）。
pub fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let mut out = [0u8; KEY_LEN];
    scrypt(password.as_bytes(), &salt, &password_params(), &mut out)
        .context("hash admin password")?;
    let encoded = format!(
        "{}:{}:{}",
        PASSWORD_VERSION,
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(out)
    );
    out.zeroize();
    Ok(encoded)
}

/// 校验密码。任何格式错误都返回 false（不区分「格式坏」与「密码错」，
/// 避免把内部状态透给未认证的调用方）。
pub fn verify_password(password: &str, encoded: &str) -> bool {
    let mut parts = encoded.split(':');
    let (Some(version), Some(salt), Some(expected)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if version != PASSWORD_VERSION || parts.next().is_some() {
        return false;
    }
    let (Ok(salt), Ok(expected)) = (decode_part(Some(salt), "salt"), decode_part(Some(expected), "hash"))
    else {
        return false;
    };
    if salt.len() != SALT_LEN || expected.len() != KEY_LEN {
        return false;
    }
    let mut actual = [0u8; KEY_LEN];
    if scrypt(password.as_bytes(), &salt, &password_params(), &mut actual).is_err() {
        return false;
    }
    let matched = constant_time_eq(&actual, &expected);
    actual.zeroize();
    matched
}

/// 长度无关的定时安全比较（手写以免新引一个依赖）。长度不同直接 false ——
/// 长度本身不是秘密（哈希长度固定）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
