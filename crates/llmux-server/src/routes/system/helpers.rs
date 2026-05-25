use std::fs;

use serde::Deserialize;

pub fn check_tool(name: &str) -> bool {
    if cfg!(windows) {
        std::process::Command::new("where")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::process::Command::new("where")
                .arg(format!("{name}.exe"))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    } else {
        std::process::Command::new("which")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

pub fn prune_backups(dir: &std::path::Path, keep: usize, prefixes: &[&str]) {
    for prefix in prefixes {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .collect();
        if files.len() <= keep {
            continue;
        }
        files.sort_by_key(|e| {
            e.file_name()
                .to_string_lossy()
                .to_string()
        });
        // Remove oldest (smallest names, since they're timestamp-based)
        for entry in &files[..files.len() - keep] {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct BackupQuery {
    pub name: Option<String>,
}

/// Generate a local-time timestamp string for backup filenames.
pub fn local_now_str() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.format(
        &time::format_description::parse("[year]-[month]-[day]-[hour]-[minute]-[second]")
            .unwrap(),
    )
    .unwrap_or_else(|_| "unknown".to_string())
}

/// Convert a SystemTime to local-time formatted string.
pub fn format_local_time(t: std::time::SystemTime) -> String {
    let dur = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let utc = time::OffsetDateTime::from_unix_timestamp(dur.as_secs() as i64)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    utc
        .format(
            &time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]")
                .unwrap(),
        )
        .unwrap_or_else(|_| "unknown".to_string())
}
