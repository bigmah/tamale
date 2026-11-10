//! The server's side: listens for clients and stands in for the terminal in the event loop.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use crossterm::event::Event;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{
    Bye, LoginCommand, PROTOCOL, State, Status, ToClient, ToServer, ensure_private, log_path,
    read_message, remove_files, socket_path, status_path, write_message,
};
use crate::browser::{self, MAX_CELLS, WebTerminal};

/// How long a server with nothing to do and nobody watching stays around. The conversation
/// is saved, so `--continue` still finds it afterwards.
const IDLE_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);

/// How long the last messages get to reach a client when the server exits.
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(1);

/// Sends this process's stderr, which panics and fatal errors go to, to the server's log.
pub fn redirect_stderr() -> Result<()> {
    let dir = super::dir()?;
    let log = std::fs::File::create(log_path(&dir, std::process::id()))?;
    if unsafe { libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// What happened, as far as the event loop cares.
#[derive(Debug)]
pub enum HostEvent {
    Input(Event),
    /// A client just attached.
    Attached,
    /// Something that needs a redraw, like a resize.
    Changed,
    Quit,
}

/// What the tasks serving connections tell the [`Host`].
enum Incoming {
    Connected {
        connection: u64,
        cols: u16,
        rows: u16,
        out: mpsc::UnboundedSender<ToClient>,
        writer: JoinHandle<()>,
    },
    Message(u64, ToServer),
    Disconnected(u64),
    Quit,
}

struct Client {
    connection: u64,
    out: mpsc::UnboundedSender<ToClient>,
    writer: JoinHandle<()>,
    terminal: WebTerminal,
    /// The mouse capture the client was last told.
    mouse: Option<bool>,
}

pub struct Host {
    dir: PathBuf,
    incoming: mpsc::UnboundedReceiver<Incoming>,
    listener: JoinHandle<()>,
    signals: Option<JoinHandle<()>>,
    tx: mpsc::UnboundedSender<Incoming>,
    client: Option<Client>,
    status: Status,
    /// Writers still sending the last words to clients that have been let go.
    leaving: Vec<JoinHandle<()>>,
    /// Since when nobody has been attached and nothing has been running.
    pub(super) idle_since: Option<Instant>,
    quit: bool,
}

impl Host {
    /// Starts listening as this process, and ends the session on SIGTERM.
    pub fn bind(chef: bool) -> Result<Self> {
        let mut host = Self::bind_in(&super::dir()?, std::process::id())?;
        if chef {
            host.status.chef = true;
            host.write_status();
        }
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let tx = host.tx.clone();
        host.signals = Some(tokio::spawn(async move {
            terminate.recv().await;
            let _ = tx.send(Incoming::Quit);
        }));
        Ok(host)
    }

    pub(super) fn bind_in(dir: &Path, id: u32) -> Result<Self> {
        ensure_private(dir)?;
        let path = socket_path(dir, id);
        // Left by an earlier process with this pid, which must be gone.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("couldn't listen on {}", path.display()))?;
        let (tx, incoming) = mpsc::unbounded_channel();
        let accepting = tx.clone();
        let listener = tokio::spawn(async move {
            let mut next = 0;
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(connection(stream, next, accepting.clone()));
                next += 1;
            }
        });
        let host = Self {
            dir: dir.to_path_buf(),
            incoming,
            listener,
            signals: None,
            tx,
            client: None,
            leaving: Vec::new(),
            status: Status {
                id,
                cwd: std::env::current_dir().unwrap_or_default(),
                session_id: String::new(),
                title: String::new(),
                state: State::Idle,
                attached: false,
                since: crate::session::now(),
                sid: crate::web::sid().to_string(),
                chef: false,
            },
            idle_since: None,
            quit: false,
        };
        host.write_status();
        Ok(host)
    }

    /// The next thing to happen. Cancel safe.
    pub async fn next(&mut self) -> HostEvent {
        if self.quit {
            return HostEvent::Quit;
        }
        let Some(incoming) = self.incoming.recv().await else {
            return std::future::pending().await;
        };
        self.handle(incoming)
    }

    fn handle(&mut self, incoming: Incoming) -> HostEvent {
        match incoming {
            Incoming::Connected {
                connection,
                cols,
                rows,
                out,
                writer,
            } => {
                self.say_goodbye(Bye::Replaced);
                let (cols, rows) = clamp(cols, rows);
                self.client = Some(Client {
                    connection,
                    out,
                    writer,
                    terminal: browser::terminal(cols, rows),
                    mouse: None,
                });
                self.set_attached(true);
                HostEvent::Attached
            }
            Incoming::Message(from, message) => {
                let Some(client) = self.client.as_mut().filter(|c| c.connection == from) else {
                    return HostEvent::Changed;
                };
                match message {
                    ToServer::Input(event) => HostEvent::Input(event),
                    ToServer::Resize { cols, rows } => {
                        let (cols, rows) = clamp(cols, rows);
                        client.terminal.backend_mut().resize(cols, rows);
                        HostEvent::Changed
                    }
                    // `Quit` arrives as its own event, and the rest don't belong here.
                    _ => HostEvent::Changed,
                }
            }
            Incoming::Disconnected(from) => {
                if self.client.as_ref().is_some_and(|c| c.connection == from) {
                    self.client = None;
                    self.set_attached(false);
                }
                HostEvent::Changed
            }
            Incoming::Quit => {
                self.quit = true;
                HostEvent::Quit
            }
        }
    }

    /// Draws to the attached client, if there is one.
    pub fn draw(&mut self, render: impl FnOnce(&mut ratatui::Frame)) -> Result<()> {
        let Some(client) = &mut self.client else {
            return Ok(());
        };
        client.terminal.draw(render)?;
        if let Some(frame) = client.terminal.backend_mut().take_frame() {
            let _ = client.out.send(ToClient::Frame(frame));
        }
        Ok(())
    }

    /// Tells the client whether to capture the mouse, if that's news to it.
    pub fn set_mouse(&mut self, on: bool) {
        if let Some(client) = &mut self.client
            && client.mouse != Some(on)
        {
            client.mouse = Some(on);
            let _ = client.out.send(ToClient::Mouse(on));
        }
    }

    /// Lets the client go, leaving the session running.
    pub fn detach(&mut self) {
        self.detach_with(Bye::Detached);
    }

    fn detach_with(&mut self, bye: Bye) {
        self.say_goodbye(bye);
        self.set_attached(false);
    }

    /// Sends the client on to another session, leaving this one running. Whether there was
    /// a client to send.
    pub fn switch(&mut self, to: u32) -> bool {
        if self.client.is_none() {
            return false;
        }
        self.detach_with(Bye::Switch(to));
        true
    }

    fn say_goodbye(&mut self, bye: Bye) {
        if let Some(client) = self.client.take() {
            // Dropping the sender ends the writer once this has gone out.
            let _ = client.out.send(ToClient::Bye(bye));
            self.leaving.retain(|writer| !writer.is_finished());
            self.leaving.push(client.writer);
        }
    }

    /// Updates what `tamale ls` shows.
    pub fn publish(&mut self, cwd: &Path, session_id: &str, title: String, state: State) {
        let since = if state == self.status.state {
            self.status.since
        } else {
            crate::session::now()
        };
        let status = Status {
            id: self.status.id,
            cwd: cwd.to_path_buf(),
            session_id: session_id.to_string(),
            title,
            state,
            attached: self.client.is_some(),
            since,
            sid: self.status.sid.clone(),
            chef: self.status.chef,
        };
        if status != self.status {
            self.status = status;
            self.write_status();
        }
    }

    fn set_attached(&mut self, attached: bool) {
        self.status.attached = attached;
        self.write_status();
    }

    /// Replaced in one step, so a reader never sees half a file.
    fn write_status(&self) {
        let path = status_path(&self.dir, self.status.id);
        let tmp = path.with_extension("json.tmp");
        let Ok(bytes) = serde_json::to_vec(&self.status) else {
            return;
        };
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }

    /// Whether this server has outstayed its welcome, called now and then. `in_use` means
    /// something's running or someone's looking, in which case the clock starts over.
    pub fn expired(&mut self, in_use: bool) -> bool {
        self.expired_after(in_use, IDLE_EXPIRY)
    }

    pub(super) fn expired_after(&mut self, in_use: bool, expiry: Duration) -> bool {
        if in_use || self.client.is_some() {
            self.idle_since = None;
            return false;
        }
        self.idle_since.get_or_insert_with(Instant::now).elapsed() >= expiry
    }

    /// Runs a sign-in in the attached terminal and returns whether it worked and how it
    /// exited. The session waits meanwhile, like it does for a sign-in in `--foreground`.
    pub async fn login(&mut self, command: LoginCommand) -> Result<(bool, String)> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("Attach to this session in a terminal to sign in."))?;
        let current = client.connection;
        let _ = client.out.send(ToClient::Login(command));
        let lost = || anyhow!("The terminal left during sign-in.");
        loop {
            let incoming = self.incoming.recv().await.ok_or_else(lost)?;
            match incoming {
                Incoming::Message(from, ToServer::LoginDone { success, status })
                    if from == current =>
                {
                    if let Some(client) = &mut self.client {
                        // The client's screen was left behind; repaint all of it.
                        client.terminal.clear()?;
                    }
                    return Ok((success, status));
                }
                Incoming::Message(from, _) if from == current => {}
                Incoming::Message(..) => {}
                Incoming::Disconnected(from) if from != current => {}
                // A new client or a request to quit also ends the wait.
                other => {
                    self.handle(other);
                    return Err(lost());
                }
            }
        }
    }

    /// Says goodbye to the client and cleans up. The log only stays if something went wrong.
    pub async fn finish(mut self, error: Option<String>) {
        let failed = error.is_some();
        self.say_goodbye(Bye::Exited(error));
        let leaving = futures::future::join_all(std::mem::take(&mut self.leaving));
        let _ = tokio::time::timeout(GOODBYE_TIMEOUT, leaving).await;
        remove_files(&self.dir, self.status.id, !failed);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.listener.abort();
        if let Some(signals) = &self.signals {
            signals.abort();
        }
        // The log stays: this also runs when a panic unwinds, and `finish` removed it
        // already if all went well.
        remove_files(&self.dir, self.status.id, false);
    }
}

fn clamp(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, MAX_CELLS), rows.clamp(1, MAX_CELLS))
}

/// Serves one connection: a client attaching, or just someone seeing if the server's alive.
async fn connection(stream: UnixStream, id: u64, host: mpsc::UnboundedSender<Incoming>) {
    let (mut reader, mut writer) = stream.into_split();
    let (cols, rows) = match read_message(&mut reader).await {
        Ok(Some(ToServer::Hello {
            protocol,
            cols,
            rows,
        })) if protocol == PROTOCOL => (cols, rows),
        Ok(Some(ToServer::Hello { .. })) => {
            let refusal = "This session was started by a different version of tamale. Attach \
                           with that version, or end it with `tamale kill`.";
            let _ = write_message(&mut writer, &ToClient::Refused(refusal.into())).await;
            return;
        }
        Ok(Some(ToServer::Quit)) => {
            let _ = host.send(Incoming::Quit);
            // Held open until the server exits, which is how the sender knows it has.
            let _ = read_message::<ToServer>(&mut reader).await;
            drop(writer);
            return;
        }
        _ => return,
    };
    let (out, mut outgoing) = mpsc::unbounded_channel();
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if write_message(&mut writer, &message).await.is_err() {
                break;
            }
        }
    });
    let connected = Incoming::Connected {
        connection: id,
        cols,
        rows,
        out,
        writer,
    };
    if host.send(connected).is_err() {
        return;
    }
    loop {
        match read_message(&mut reader).await {
            Ok(Some(ToServer::Quit)) => {
                let _ = host.send(Incoming::Quit);
            }
            Ok(Some(message)) => {
                if host.send(Incoming::Message(id, message)).is_err() {
                    return;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    let _ = host.send(Incoming::Disconnected(id));
}
