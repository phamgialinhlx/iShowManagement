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
//!
//! Moved from `src-tauri/src/git.rs` unchanged; the `#[tauri::command]`
//! attributes became the `register` table below, with argument structs carrying
//! the camelCase keys Tauri derived from the old parameter names.

use rmux_git::{Against, BranchSummary, Change, Commit, FileDiff, Status};
use rmux_ssh::SshTarget;
use rmux_transport::{LocalTarget, Target, TargetId};
use serde::{Deserialize, Serialize};

use crate::registry::{Registry, Web};
use crate::target::TargetRef;

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

pub async fn git_repo(target: TargetRef, folder: String) -> Result<RepoInfo, String> {
    let t = resolved(&target).await?;
    let root = rmux_git::repo_root(t.as_ref(), &folder).await.map_err(|e| e.to_string())?;
    Ok(RepoInfo { root })
}

/// The branch line the sidebar shows under a project; `None` outside a repo.
///
/// Takes the project *folder*, not a root: it is one round trip, and `git -C`
/// finds the work tree from any subdirectory itself.
pub async fn git_branch(target: TargetRef, folder: String) -> Result<Option<BranchSummary>, String> {
    let t = resolved(&target).await?;
    rmux_git::branch_summary(t.as_ref(), &folder).await.map_err(|e| e.to_string())
}

pub async fn git_status(target: TargetRef, root: String) -> Result<Status, String> {
    let t = resolved(&target).await?;
    rmux_git::status(t.as_ref(), &root).await.map_err(|e| e.to_string())
}

pub async fn git_log(target: TargetRef, root: String, limit: usize) -> Result<Vec<Commit>, String> {
    let t = resolved(&target).await?;
    rmux_git::log(t.as_ref(), &root, limit).await.map_err(|e| e.to_string())
}

pub async fn git_commit_files(
    target: TargetRef,
    root: String,
    sha: String,
) -> Result<Vec<Change>, String> {
    let t = resolved(&target).await?;
    rmux_git::commit_files(t.as_ref(), &root, &sha).await.map_err(|e| e.to_string())
}

pub async fn git_file_diff(
    target: TargetRef,
    root: String,
    path: String,
    against: Against,
) -> Result<FileDiff, String> {
    let t = resolved(&target).await?;
    rmux_git::file_diff(t.as_ref(), &root, &path, &against).await.map_err(|e| e.to_string())
}

// Argument objects, keyed exactly as `ui/src/lib/api.ts` sends them.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRepoArgs {
    pub target: TargetRef,
    pub folder: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusArgs {
    pub target: TargetRef,
    pub root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLogArgs {
    pub target: TargetRef,
    pub root: String,
    pub limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitFilesArgs {
    pub target: TargetRef,
    pub root: String,
    pub sha: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiffArgs {
    pub target: TargetRef,
    pub root: String,
    pub path: String,
    pub against: Against,
}

pub fn register(r: &mut Registry) {
    r.add("git_repo", Web::Yes, |_, a: GitRepoArgs| git_repo(a.target, a.folder));
    r.add("git_branch", Web::Yes, |_, a: GitRepoArgs| git_branch(a.target, a.folder));
    r.add("git_status", Web::Yes, |_, a: GitStatusArgs| git_status(a.target, a.root));
    r.add("git_log", Web::Yes, |_, a: GitLogArgs| git_log(a.target, a.root, a.limit));
    r.add("git_commit_files", Web::Yes, |_, a: GitCommitFilesArgs| {
        git_commit_files(a.target, a.root, a.sha)
    });
    r.add("git_file_diff", Web::Yes, |_, a: GitFileDiffArgs| {
        git_file_diff(a.target, a.root, a.path, a.against)
    });
}
