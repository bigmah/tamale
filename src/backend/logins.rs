//! The CLI logins tamale can switch between. Claude Code keeps each login in its own
//! config directory (`CLAUDE_CONFIG_DIR`) and Codex in its own home (`CODEX_HOME`), so a
//! login is a CLI plus a directory:
//!
//! - the default one (the CLI's own default, or the variable from tamale's environment),
//! - `~/.claude` or `~/.codex`, when that isn't the default and is signed in,
//! - directories listed in `backend.claude_config_dirs` and `backend.codex_homes`,
//! - accounts added from the `/backend` picker, kept under
//!   `$XDG_DATA_HOME/tamale/<claude-code|codex>/<id>`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{Backend, BackendSettings, ClaudeCode, Codex, Kind};

/// Where accounts of `kind` added from the picker are kept.
pub fn added_dir(kind: Kind) -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".local/share")))?;
    Some(data.join("tamale").join(kind.as_str()))
}

/// Whether `login` is an account added from the picker (and can be removed there).
pub fn is_added(login: &Backend) -> bool {
    match (login.login_dir(), added_dir(login.kind())) {
        (Some(dir), Some(root)) => dir.parent() == Some(root.as_path()),
        _ => false,
    }
}

/// A new, signed-out login of `kind` in an empty directory, about to be signed in.
pub fn new_login(kind: Kind, settings: &BackendSettings) -> Result<Backend> {
    let root = added_dir(kind).context("HOME is not set")?;
    let dir = root.join(crate::session::new_id());
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(match kind {
        Kind::ClaudeCode => Backend::ClaudeCode(ClaudeCode::signed_out(
            settings.claude_path.clone(),
            Some(dir),
        )),
        Kind::Codex => Backend::Codex(Codex::signed_out(settings.codex_path.clone(), Some(dir))),
        Kind::Api => bail!("the API backend has no logins"),
    })
}

/// Signs out an added account and deletes its directory.
pub async fn remove(login: &Backend) -> Result<()> {
    let dir = login
        .login_dir()
        .filter(|_| is_added(login))
        .map(Path::to_path_buf)
        .context("only accounts added in tamale can be removed here")?;
    if login.signed_in() {
        match login {
            Backend::ClaudeCode(cc) => cc.logout().await?,
            Backend::Codex(codex) => codex.logout().await?,
            Backend::Api(_) | Backend::Unset => {}
        }
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(())
}

/// Every known login of both CLIs with its sign-in state, each CLI's default first.
/// Other logins are listed only when signed in, except added ones (so they can be signed
/// in again or removed). A CLI that isn't installed is left out; other failures to run
/// one are returned alongside the logins that were found.
pub async fn list(settings: &BackendSettings) -> (Vec<Backend>, Vec<String>) {
    let (claude, codex) = futures::join!(list_claude_code(settings), list_codex(settings));
    let mut logins = Vec::new();
    let mut errors = Vec::new();
    for (kind, result) in [(Kind::ClaudeCode, claude), (Kind::Codex, codex)] {
        match result {
            Ok(found) => logins.extend(found),
            Err(e) if not_installed(&e) && settings.kind != kind => {}
            Err(e) => errors.push(format!("{}: {e:#}", kind.cli_name())),
        }
    }
    (logins, errors)
}

fn not_installed(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|e| e.kind() == std::io::ErrorKind::NotFound)
}

async fn list_claude_code(settings: &BackendSettings) -> Result<Vec<Backend>> {
    let program = &settings.claude_path;
    let default = ClaudeCode::connect(program.clone(), None).await?;
    let default_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".claude")));
    let dirs = candidates(
        Kind::ClaudeCode,
        default_dir,
        home().map(|h| h.join(".claude")),
        &settings.claude_config_dirs,
    );
    let checks = dirs
        .into_iter()
        .map(|dir| ClaudeCode::connect(program.clone(), Some(dir)));
    let others = futures::future::join_all(checks).await;
    Ok(collect(
        Backend::ClaudeCode(default),
        others.into_iter().flatten().map(Backend::ClaudeCode),
    ))
}

async fn list_codex(settings: &BackendSettings) -> Result<Vec<Backend>> {
    let program = &settings.codex_path;
    let default = Codex::connect(program.clone(), None).await?;
    let default_dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".codex")));
    let dirs = candidates(
        Kind::Codex,
        default_dir,
        home().map(|h| h.join(".codex")),
        &settings.codex_homes,
    );
    let checks = dirs
        .into_iter()
        .map(|dir| Codex::connect(program.clone(), Some(dir)));
    let others = futures::future::join_all(checks).await;
    Ok(collect(
        Backend::Codex(default),
        others.into_iter().flatten().map(Backend::Codex),
    ))
}

/// Directories that may hold other logins of `kind`, besides the default one.
fn candidates(
    kind: Kind,
    default_dir: Option<PathBuf>,
    usual: Option<PathBuf>,
    configured: &[PathBuf],
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(usual);
    dirs.extend(configured.iter().map(|d| expand(d)));
    let added = added_dir(kind)
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
    dirs
}

/// The default login, then the others worth listing, each account once.
fn collect(default: Backend, others: impl Iterator<Item = Backend>) -> Vec<Backend> {
    let mut logins = vec![default];
    for login in others {
        let keep = login.signed_in() || is_added(&login);
        if keep && !logins.iter().any(|known| known.same_account(&login)) {
            logins.push(login);
        }
    }
    logins
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
