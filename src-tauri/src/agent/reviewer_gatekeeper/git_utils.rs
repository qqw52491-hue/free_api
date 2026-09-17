//! git 探测与只读命令执行。

use super::*;
use crate::agent::context::SandwichContext;
use crate::agent::router::{AgentRouter, ModelRoutingConfig, roles};
use crate::agent::tools::role_manager::resolve_enabled_card;
use crate::db::DbState;
use crate::llm::{self, ModelConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;


pub(crate) fn is_git_repo(workspace: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(crate) fn git_cmd(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}
