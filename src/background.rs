//! Sessions that outlive their terminal, like a small `dtach`.
//!
//! `tamale` starts a server, itself run again with `--server` in a session of its own, and
//! attaches to it over a Unix socket. The server owns the conversation, the agent and the
//! tools, and draws the TUI to a [`crate::browser::FrameBackend`]; the client only paints
//! frames and forwards input. So leaving the client leaves the work running, and
//! `tamale attach` picks it up again. One client is attached at a time.
//!
//! Each server keeps three files in [`dir`], named after its pid: `<id>.sock`, `<id>.json`
//! (its [`Status`], for `tamale ls` and the like) and `<id>.log` (its stderr, which the
//! client shows if the server dies). A server that's gone is noticed by nothing listening
//! on its socket, and swept up by [`list`].

pub mod client;
mod server;
#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tamale_web_protocol::{self as protocol, Frame};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub use server::{Host, HostEvent, redirect_stderr};

/// Bump when the messages below (or `Frame`, or crossterm's `Event`) change, so a client
/// and a server from different builds refuse each other instead of misreading bytes.
const PROTOCOL: u32 = 2;

/// Largest message read from the socket.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// From client to server. `Hello` and `Quit` stay the first two variants, so they read the
/// same across protocol versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToServer {
    /// The first message of an attaching client, with its terminal's size.
    Hello {
        protocol: u32,
        cols: u16,
        rows: u16,
    },
    /// Ends the session, attached or not (`tamale kill`).
    Quit,
    Resize {
        cols: u16,
        rows: u16,
    },
    Input(crossterm::event::Event),
    /// How a [`ToClient::Login`] went.
    LoginDone {
        success: bool,
        status: String,
    },
}

/// From server to client. `Refused` stays the first variant; see [`ToServer`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToClient {
    Refused(String),
    Frame(Frame),
    /// Turns mouse capture on or off.
    Mouse(bool),
    /// Suspends the TUI to run an interactive sign-in, which needs the real terminal.
    Login(LoginCommand),
    Bye(Bye),
}

/// Why the server let a client go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Bye {
    /// The user detached; the session goes on.
    Detached,
    /// Another terminal attached; the session goes on.
    Replaced,
    /// The session ended, with an error if it failed.
    Exited(Option<String>),
    /// The user picked another session, with this id, to go to.
    Switch(u32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Variables to set, or with `None` to remove.
    pub envs: Vec<(String, Option<String>)>,
}

/// What a server is up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Running,
    /// A turn is waiting for someone to answer an approval prompt.
    NeedsApproval,
    Idle,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::NeedsApproval => "needs approval",
            Self::Idle => "idle",
        }
    }
}

/// A server's `<id>.json`, rewritten whenever any of this changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub id: u32,
    pub cwd: PathBuf,
    pub session_id: String,
    /// The conversation's first message; empty until there is one.
    pub title: String,
    pub state: State,
    pub attached: bool,
    /// When `state` last changed, in Unix seconds.
    pub since: u64,
}

/// Where servers keep their files. Only this user can enter it, which is what guards the
/// sockets: whoever connects can drive the agent.
pub fn dir() -> Result<PathBuf> {
    let dir = crate::session::dir()
        .context("couldn't find a data directory (HOME isn't set)")?
        .join(".servers");
    ensure_private(&dir)?;
    Ok(dir)
}

fn ensure_private(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("securing {}", dir.display()))
}

fn socket_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("{id}.sock"))
}

fn status_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn log_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("{id}.log"))
}

fn remove_files(dir: &Path, id: u32, log: bool) {
    let _ = std::fs::remove_file(socket_path(dir, id));
    let _ = std::fs::remove_file(status_path(dir, id));
    if log {
        let _ = std::fs::remove_file(log_path(dir, id));
    }
}

/// The servers still alive, most recently active first.
pub fn list() -> Vec<Status> {
    dir().map(|dir| list_in(&dir)).unwrap_or_default()
}

fn list_in(dir: &Path) -> Vec<Status> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut servers: Vec<Status> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|entry| {
            let status: Status = serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()?;
            // A connection without a `Hello` doesn't disturb the server.
            if std::os::unix::net::UnixStream::connect(socket_path(dir, status.id)).is_ok() {
                Some(status)
            } else {
                remove_files(dir, status.id, true);
                None
            }
        })
        .collect();
    servers.sort_by(|a, b| b.since.cmp(&a.since).then(b.id.cmp(&a.id)));
    servers
}

/// The other server that has this conversation open, if any. Two writers would overwrite
/// each other's saves.
pub fn running_session(session_id: &str) -> Option<Status> {
    list()
        .into_iter()
        .find(|s| s.session_id == session_id && s.id != std::process::id())
}

async fn write_message<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &T,
) -> io::Result<()> {
    let bytes = protocol::encode(message);
    let mut buffer = Vec::with_capacity(bytes.len() + 4);
    buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buffer.extend_from_slice(&bytes);
    writer.write_all(&buffer).await?;
    writer.flush().await
}

/// The next message, or `None` at the end of the stream. Not cancel safe.
async fn read_message<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> io::Result<Option<T>> {
    let mut length = [0; 4];
    match reader.read_exact(&mut length).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    protocol::decode(&bytes)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unreadable message"))
}
