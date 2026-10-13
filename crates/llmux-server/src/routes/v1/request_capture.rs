//! Encrypted, one-shot capture of a selected Messages → Responses request.
//!
//! An operator arms it by creating `request-captures/next.json` under LOG_DIR:
//! `{ "alias": "of", "model": "muse-spark-1.3-contributor" }`.
//! The next matching request consumes the marker and writes one encrypted
//! `.enc` artifact. This keeps prompts out of the plaintext NAS log files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

const CAPTURE_SUBDIR: &str = "request-captures";
const ARM_FILE: &str = "next.json";
const DEFAULT_RETAIN_DAYS: u64 = 7;

static CLAIM_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Deserialize, Serialize)]
struct CaptureArm {
    alias: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct CaptureArtifact {
    captured_at: String,
    request_id: String,
    alias: String,
    requested_model: String,
    upstream_model: String,
    upstream_method: String,
    upstream_url: String,
    upstream_headers: BTreeMap<String, String>,
    /// Exact UTF-8 JSON bytes received by `/v1/messages`.
    inbound_body: String,
    /// Exact JSON serialization passed to reqwest's `.json()` for the provider.
    upstream_body: String,
    upstream_status: Option<u16>,
    upstream_response_body: Option<String>,
    transport_error: Option<String>,
}

pub struct RequestCapture {
    capture_dir: PathBuf,
    claimed_marker: PathBuf,
    master_key: String,
    artifact: CaptureArtifact,
}

impl RequestCapture {
    #[allow(clippy::too_many_arguments)]
    pub async fn claim(
        log_dir: &Path,
        master_key: &str,
        alias: &str,
        requested_model: &str,
        upstream_model: &str,
        inbound_body: &[u8],
        upstream_method: &str,
        upstream_url: &str,
        upstream_headers: &BTreeMap<String, String>,
        upstream_body: &Value,
    ) -> Option<Self> {
        let capture_dir = log_dir.join(CAPTURE_SUBDIR);
        let arm_path = capture_dir.join(ARM_FILE);
        if !arm_path.is_file() {
            return None;
        }

        // Serialize the read/claim sequence within this process; rename is the
        // durable one-shot transition and is atomic on the mounted NAS share.
        let _claim_guard = CLAIM_LOCK.lock().await;
        let arm_text = tokio::fs::read_to_string(&arm_path).await.ok()?;
        let arm: CaptureArm = serde_json::from_str(&arm_text).ok()?;
        if arm.alias != alias || arm.model != upstream_model {
            return None;
        }

        let inbound_body = String::from_utf8(inbound_body.to_vec()).ok()?;
        let upstream_body = serde_json::to_string(upstream_body).ok()?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let claimed_marker = capture_dir.join(format!("claimed-{request_id}.json"));
        tokio::fs::rename(&arm_path, &claimed_marker).await.ok()?;

        Some(Self {
            capture_dir,
            claimed_marker,
            master_key: master_key.to_string(),
            artifact: CaptureArtifact {
                captured_at: time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| "unknown".to_string()),
                request_id,
                alias: alias.to_string(),
                requested_model: requested_model.to_string(),
                upstream_model: upstream_model.to_string(),
                upstream_method: upstream_method.to_string(),
                upstream_url: redact_url(upstream_url),
                upstream_headers: redact_headers(upstream_headers),
                inbound_body,
                upstream_body,
                upstream_status: None,
                upstream_response_body: None,
                transport_error: None,
            },
        })
    }

    pub async fn finish(
        mut self,
        upstream_status: Option<u16>,
        upstream_response_body: Option<String>,
        transport_error: Option<String>,
    ) {
        self.artifact.upstream_status = upstream_status;
        self.artifact.upstream_response_body = upstream_response_body;
        self.artifact.transport_error = transport_error;

        let result = async {
            tokio::fs::create_dir_all(&self.capture_dir)
                .await
                .map_err(|error| error.to_string())?;
            let plaintext = serde_json::to_string(&self.artifact).map_err(|error| error.to_string())?;
            let secret = self.master_key.clone();
            let encrypted = tokio::task::spawn_blocking(move || {
                llmux_core::crypto::encrypt_api_key(&plaintext, &secret)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;

            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let final_path = self
                .capture_dir
                .join(format!("capture-{stamp}-{}.enc", self.artifact.request_id));
            let temp_path = self
                .capture_dir
                .join(format!(".capture-{}.tmp", self.artifact.request_id));
            tokio::fs::write(&temp_path, encrypted.as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            tokio::fs::rename(&temp_path, &final_path)
                .await
                .map_err(|error| error.to_string())?;
            Ok::<PathBuf, String>(final_path)
        }
        .await;

        match result {
            Ok(path) => {
                tracing::warn!(
                    "[request-capture] encrypted one-shot capture saved: {} (alias={}, model={}, inbound_bytes={}, upstream_bytes={})",
                    path.display(),
                    self.artifact.alias,
                    self.artifact.upstream_model,
                    self.artifact.inbound_body.len(),
                    self.artifact.upstream_body.len(),
                );
                cleanup_expired(&self.capture_dir);
            }
            Err(error) => tracing::error!(
                "[request-capture] failed to save encrypted capture for alias={} model={}: {error}",
                self.artifact.alias,
                self.artifact.upstream_model
            ),
        }
        let _ = tokio::fs::remove_file(&self.claimed_marker).await;
    }
}

/// Remove expired captures and abandoned claim markers. Capture files are
/// encrypted at rest because CIFS may ignore chmod and expose mode 0755.
pub fn cleanup_expired(log_dir: &Path) {
    cleanup_expired_in(&log_dir.join(CAPTURE_SUBDIR));
}

fn cleanup_expired_in(capture_dir: &Path) {
    let retain_days = std::env::var("REQUEST_CAPTURE_RETAIN_DAYS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_RETAIN_DAYS);
    let retention = Duration::from_secs(retain_days.saturating_mul(86_400));
    let Ok(entries) = std::fs::read_dir(capture_dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_capture = name.starts_with("capture-") && name.ends_with(".enc");
        let is_stale_claim = name.starts_with("claimed-") && name.ends_with(".json");
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default();
        if (is_capture && age > retention) || (is_stale_claim && age > Duration::from_secs(86_400)) {
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!("[request-capture] failed to remove expired file {}: {error}", path.display());
            }
        }
    }
}

fn redact_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let lower = name.to_ascii_lowercase();
            let sensitive = lower == "authorization"
                || lower == "cookie"
                || lower.contains("api-key")
                || lower.contains("apikey")
                || lower.contains("token")
                || lower.contains("secret");
            (name.clone(), if sensitive { "[REDACTED]".to_string() } else { value.clone() })
        })
        .collect()
}

fn redact_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return "[unparseable upstream URL]".to_string();
    };
    if !url.username().is_empty() {
        let _ = url.set_username("[REDACTED]");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("[REDACTED]"));
    }
    let query = url.query_pairs().map(|(key, value)| {
        let lower = key.to_ascii_lowercase();
        let sensitive = lower.contains("key")
            || lower.contains("token")
            || lower.contains("auth")
            || lower.contains("secret")
            || lower.contains("signature");
        (key.into_owned(), if sensitive { "[REDACTED]".to_string() } else { value.into_owned() })
    }).collect::<Vec<_>>();
    if !query.is_empty() {
        url.query_pairs_mut().clear().extend_pairs(query);
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_is_one_shot_encrypted_and_preserves_both_json_bodies() {
        let root = std::env::temp_dir().join(format!("llmux-capture-{}", uuid::Uuid::new_v4()));
        let capture_dir = root.join(CAPTURE_SUBDIR);
        tokio::fs::create_dir_all(&capture_dir).await.unwrap();
        tokio::fs::write(
            capture_dir.join(ARM_FILE),
            r#"{"alias":"of","model":"muse-spark-1.3-contributor"}"#,
        )
        .await
        .unwrap();

        let raw = br#"{ "model" : "of", "messages" : [] }"#;
        let outbound = serde_json::json!({"model":"muse-spark-1.3-contributor","stream":true});
        let headers = BTreeMap::from([
            ("authorization".to_string(), "Bearer do-not-capture".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        let capture = RequestCapture::claim(
            &root,
            "test-master-key",
            "of",
            "of",
            "muse-spark-1.3-contributor",
            raw,
            "POST",
            "https://example.test/v1/responses?api_key=private",
            &headers,
            &outbound,
        )
        .await
        .expect("matching armed request should be captured");

        assert!(RequestCapture::claim(
            &root,
            "test-master-key",
            "of",
            "of",
            "muse-spark-1.3-contributor",
            raw,
            "POST",
            "https://example.test/v1/responses",
            &headers,
            &outbound,
        )
        .await
        .is_none());

        capture
            .finish(Some(400), Some("invalid request".to_string()), None)
            .await;

        let mut files = tokio::fs::read_dir(&capture_dir).await.unwrap();
        let mut encrypted_path = None;
        while let Some(entry) = files.next_entry().await.unwrap() {
            if entry.path().extension().and_then(|v| v.to_str()) == Some("enc") {
                encrypted_path = Some(entry.path());
            }
        }
        let encrypted = tokio::fs::read_to_string(encrypted_path.unwrap()).await.unwrap();
        assert!(!encrypted.contains("do-not-capture"));
        let plaintext = llmux_core::crypto::decrypt_api_key(&encrypted, "test-master-key").unwrap();
        let artifact: Value = serde_json::from_str(&plaintext).unwrap();
        assert_eq!(artifact["inbound_body"], std::str::from_utf8(raw).unwrap());
        assert_eq!(artifact["upstream_body"], outbound.to_string());
        assert_eq!(artifact["upstream_status"], 400);
        assert_eq!(artifact["upstream_headers"]["authorization"], "[REDACTED]");
        assert!(artifact["upstream_url"].as_str().unwrap().contains("api_key=%5BREDACTED%5D"));

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
