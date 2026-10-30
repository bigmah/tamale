//! The Claude Code logins tamale can switch between. Claude Code keeps each login in its
//! own config directory (`CLAUDE_CONFIG_DIR`), so a login is a directory:
//!
//! - the default one (Claude Code's own default, or `CLAUDE_CONFIG_DIR` from tamale's
//!   environment),
//! - `~/.claude`, when that isn't the default and is signed in,
//! - directories listed in `backend.claude_config_dirs`,
//! - accounts added from the `/backend` picker, kept under
//!   `$XDG_DATA_HOME/tamale/claude-code/<id>`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{BackendSettings, ClaudeCode};

/// Where accounts added from the picker are kept.
pub fn added_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".local/share")))?;
    Some(data.join("tamale").join("claude-code"))
}

/// Whether `dir` holds an account added from the picker (and can be removed there).
pub fn is_added(dir: Option<&Path>) -> bool {
    match (dir, added_dir()) {
        (Some(dir), Some(root)) => dir.parent() == Some(root.as_path()),
        _ => false,
    }
}

/// A new, empty directory for an account about to be signed in.
pub fn new_dir() -> Result<PathBuf> {
    let root = added_dir().context("HOME is not set")?;
    let dir = root.join(crate::session::new_id());
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Signs out an added account and deletes its directory.
pub async fn remove(cc: &ClaudeCode) -> Result<()> {
    let dir = cc
        .config_dir
        .clone()
        .filter(|d| is_added(Some(d)))
        .context("only accounts added in tamale can be removed here")?;
    if cc.account.logged_in {
        cc.logout().await?;
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(())
}

/// Every known login with its sign-in state, the default first. Other logins are
/// listed only when signed in, except added ones (so they can be signed in again or
/// removed). Fails only if the CLI itself can't be run.
pub async fn list(settings: &BackendSettings) -> Result<Vec<ClaudeCode>> {
    let program = &settings.claude_path;
    let default = ClaudeCode::connect(program.clone(), None).await?;
    let default_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".claude")));

    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(home().map(|h| h.join(".claude")));
    dirs.extend(settings.claude_config_dirs.iter().map(|d| expand(d)));
    let added = added_dir()
        .and_then(|root| std::fs::read_dir(root).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir());
    let mut added: Vec<PathBuf> = added.collect();
    added.sort();
    dirs.extend(added);

    // The same directory can be reached several ways (symlinks, `~`, the default).
    let mut seen: Vec<PathBuf> = default_dir.iter().map(|d| canonical(d)).collect();
    dirs.retain(|dir| {
        let key = canonical(dir);
        let new = dir.is_dir() && !seen.contains(&key);
        seen.push(key);
        new
    });

    let checks = dirs
        .into_iter()
        .map(|dir| ClaudeCode::connect(program.clone(), Some(dir)));
    let mut logins = vec![default];
    for cc in futures::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
    {
        let keep = cc.account.logged_in || is_added(cc.config_dir.as_deref());
        if keep && !logins.iter().any(|known| known.same_account(&cc)) {
            logins.push(cc);
        }
    }
    Ok(logins)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn expand(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), home()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
