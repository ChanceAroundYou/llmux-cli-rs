pub mod claude;
pub mod codex;
pub mod gemini;
pub mod helpers;

use axum::Json;
use serde_json::{json, Value};

use self::helpers::check_tool;

pub use claude::{
    apply_claude_settings, delete_claude_backup, get_claude_settings, list_claude_backups,
    restore_claude_backup,
};
pub use codex::{
    apply_codex_settings, delete_codex_backup, get_codex_settings, list_codex_backups,
    restore_codex_backup,
};
pub use gemini::{
    apply_gemini_settings, delete_gemini_backup, get_gemini_settings, list_gemini_backups,
    restore_gemini_backup,
};

pub async fn get_installed_tools() -> Json<Value> {
    let claude = check_tool("claude");
    let gemini = check_tool("gemini");
    let opencode = check_tool("opencode");
    let vscode = check_tool("code");

    Json(json!({
        "vscode": vscode,
        "claude": claude,
        "gemini": gemini,
        "opencode": opencode,
        "codex": check_tool("codex"),
    }))
}
