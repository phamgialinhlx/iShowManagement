//! Git IPC.
//!
//! Thin, like the rest of this layer: the reading and parsing live in
//! `rmux-git`, which is testable without a GUI, and this only resolves a target
//! and hands the result over.
//!
//! Every call takes the project *folder* and resolves the repository root
//! itself. A project is frequently a subdirectory of a checkout, and pinning
//! each command to the same root is what stops the change list and the history
//! describing two different trees.

use rmux_git::{Against, Change, Commit, FileDiff, Status};
use rmux_ssh::SshTarget;
use rmux_transport::{LocalTarget, Target, TargetId};
use serde::Serialize;

use crate::terminal::TargetRef;

async fn resolved(target: &TargetRef) -> Result<Box<dyn Target>, String> {
    match target.id() {
        TargetId::Local => Ok(Box::new(LocalTarget::new())),
        TargetId::Ssh(host) => {
            let ssh = SshTarget::new(host);
            ssh.connect().await.map_err(|e| e.to_string())?;
            Ok(Box::new(ssh))
        }
    }
}

/// The repository, or the reason there is not one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    /// Absolute path of the work tree root, absent when this is not a checkout.
    pub root: Option<String>,
}

#[tauri::command]
pub async fn git_repo(target: TargetRef, folder: String) -> Result<RepoInfo, String> {
    let t = resolved(&target).await?;
    let root = rmux_git::repo_root(t.as_ref(), &folder).await.map_err(|e| e.to_string())?;
    Ok(RepoInfo { root })
}

#[tauri::command]
pub async fn git_status(target: TargetRef, root: String) -> Result<Status, String> {
    let t = resolved(&target).await?;
    rmux_git::status(t.as_ref(), &root).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn git_log(target: TargetRef, root: String, limit: usize) -> Result<Vec<Commit>, String> {
    let t = resolved(&target).await?;
    rmux_git::log(t.as_ref(), &root, limit).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn git_commit_files(
    target: TargetRef,
    root: String,
    sha: String,
) -> Result<Vec<Change>, String> {
    let t = resolved(&target).await?;
    rmux_git::commit_files(t.as_ref(), &root, &sha).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn git_file_diff(
    target: TargetRef,
    root: String,
    path: String,
    against: Against,
) -> Result<FileDiff, String> {
    let t = resolved(&target).await?;
    rmux_git::file_diff(t.as_ref(), &root, &path, &against).await.map_err(|e| e.to_string())
}
