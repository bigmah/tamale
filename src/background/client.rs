//! The client's side: `tamale attach` and friends. Paints the frames a server sends and
//! forwards input, and knows nothing of what's going on in the session.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream,
};
use crossterm::execute;
use futures::StreamExt;
use ratatui::backend::Backend;
use tamale_web_protocol::Frame;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

use super::{
    Bye, LoginCommand, PROTOCOL, Status, ToClient, ToServer, list, log_path, read_message,
    socket_path, write_message,
};
use crate::cli::{Cli, Command, Resume};
use crate::screen::{install_panic_hook, resume_terminal, suspend_terminal};

/// How long a new server gets to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `tamale kill` waits for a session to save and exit.
const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of a dead server's log to show.
const LOG_LINES: usize = 20;

/// `tamale ls`, `attach` and `kill`.
pub async fn run_command(command: &Command) -> Result<()> {
    match command {
        Command::List => {
            let servers = list();
            if servers.is_empty() {
                println!("No sessions are running in the background.");
            } else {
                print_servers(&servers);
            }
            Ok(())
        }
        Command::Attach(Some(id)) => attach(*id).await,
        Command::Attach(None) => {
            let servers = nearest_servers();
            let id = match servers.as_slice() {
                [] => bail!("No sessions are running in the background."),
                [only] => only.id,
                several => match choose(several)? {
                    Some(id) => id,
                    None => return Ok(()),
                },
            };
            attach(id).await
        }
        Command::Kill(id) => {
            stop(*id).await?;
            println!("Ended session {id}. `tamale --continue` can pick it back up.");
            Ok(())
        }
    }
}

/// The running session `--continue` attaches to instead of starting one: the latest from
/// this directory.
pub fn session_to_rejoin(cli: &Cli) -> Result<Option<u32>> {
    // There's one chef: `tamale chef` goes back to it wherever it was started.
    if cli.chef {
        return Ok(list().into_iter().find(|s| s.chef).map(|s| s.id));
    }
    if cli.resume != Some(Resume::Latest) {
        return Ok(None);
    }
    let cwd = crate::workspace::Workspace::current()?.cwd;
    Ok(list().into_iter().find(|s| s.cwd == cwd).map(|s| s.id))
}

/// The servers in this directory, or if there are none, all of them.
fn nearest_servers() -> Vec<Status> {
    let servers = list();
    let cwd = crate::workspace::Workspace::current().map(|w| w.cwd);
    let here: Vec<Status> = servers
        .iter()
        .filter(|s| cwd.as_ref().is_ok_and(|cwd| s.cwd == *cwd))
        .cloned()
        .collect();
    if here.is_empty() { servers } else { here }
}

fn print_servers(servers: &[Status]) {
    for line in server_lines(servers) {
        println!("{line}");
    }
}

fn server_lines(servers: &[Status]) -> Vec<String> {
    servers
        .iter()
        .map(|s| {
            let attached = if s.attached { ", attached" } else { "" };
            let title = if s.title.is_empty() {
                "(new session)"
            } else {
                &s.title
            };
            let line = format!(
                "  {:<7} {} ({}{attached})  {}\n          {title}",
                s.id,
                s.state.label(),
                crate::session::ago(s.since),
                s.cwd.display(),
            );
            crate::secure::printable(&line).into_owned()
        })
        .collect()
}

/// Asks which of these to attach to; `None` means cancel.
fn choose(servers: &[Status]) -> Result<Option<u32>> {
    let count = servers.len();
    if count == 1 {
        println!("A session is still open:\n");
    } else {
        println!("{count} sessions are still open:\n");
    }
    print_servers(servers);
    println!();
    if count == 1 {
        print!("Enter to attach, n to cancel: ");
    } else {
        print!(
            "Session id to attach to (Enter for {}), n to cancel: ",
            servers[0].id
        );
    }
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(parse_choice(answer.trim(), servers))
}

fn parse_choice(answer: &str, servers: &[Status]) -> Option<u32> {
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
        return servers.first().map(|s| s.id);
    }
    let id: u32 = answer.parse().ok()?;
    servers.iter().find(|s| s.id == id).map(|s| s.id)
}

/// Ends a session and waits for it to go.
pub async fn stop(id: u32) -> Result<()> {
    let path = socket_path(&super::dir()?, id);
    let mut stream = UnixStream::connect(&path)
        .await
        .map_err(|_| anyhow::anyhow!("No running session {id}; `tamale ls` lists them."))?;
    write_message(&mut stream, &ToServer::Quit).await?;
    // The connection stays open until the server exits.
    let wait = async {
        let _ = read_message::<ToClient>(&mut stream).await;
    };
    if tokio::time::timeout(KILL_TIMEOUT, wait).await.is_err() {
        bail!("Session {id} hasn't exited yet.");
    }
    Ok(())
}

/// Starts a server with this command line's arguments and attaches to it.
pub async fn start_and_attach() -> Result<()> {
    let id = start_server(std::env::args_os().skip(1)).await?;
    attach(id).await
}

/// Starts a server with `args` (in this directory) and returns its id once it's listening.
pub async fn start_server<I, S>(args: I) -> Result<u32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let dir = super::dir()?;
    let exe = std::env::current_exe().context("couldn't find tamale's executable")?;
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .arg("--server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // In a session of its own, it doesn't get the terminal's hangup when that closes.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("couldn't start tamale's server")?;
    let id = child.id();
    let path = socket_path(&dir, id);
    let deadline = Instant::now() + START_TIMEOUT;
    while UnixStream::connect(&path).await.is_err() {
        if child.try_wait()?.is_some() {
            bail!("tamale's server didn't start.{}", log_tail(id));
        }
        if Instant::now() > deadline {
            bail!("tamale's server didn't start in time.{}", log_tail(id));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(id)
}

/// The end of a server's log, as text to add to a message.
fn log_tail(id: u32) -> String {
    let Some(log) = super::dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(log_path(&dir, id)).ok())
    else {
        return String::new();
    };
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_LINES)..].join("\n");
    if tail.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n{tail}")
    }
}

/// Why the client stopped.
enum Ending {
    Bye(Bye),
    Refused(String),
    /// The connection ended without a goodbye: the server died.
    Lost,
}

/// A connection to a server that has been told about this terminal.
struct Connection {
    writer: OwnedWriteHalf,
    messages: mpsc::UnboundedReceiver<ToClient>,
}

async fn connect(id: u32) -> Result<Connection> {
    let path = socket_path(&super::dir()?, id);
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|_| anyhow::anyhow!("No running session {id}; `tamale ls` lists them."))?;
    let (mut reader, mut writer) = stream.into_split();
    let (cols, rows) = crossterm::terminal::size().context("couldn't read the terminal's size")?;
    let hello = ToServer::Hello {
        protocol: PROTOCOL,
        cols,
        rows,
    };
    write_message(&mut writer, &hello).await?;
    let (tx, messages) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(Some(message)) = read_message::<ToClient>(&mut reader).await {
            if tx.send(message).is_err() {
                return;
            }
        }
    });
    Ok(Connection { writer, messages })
}

/// Attaches this terminal to a running server until it's detached or the session ends.
pub async fn attach(mut id: u32) -> Result<()> {
    let mut connection = connect(id).await?;

    install_panic_hook();
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    terminal.clear()?;
    // Going to another session keeps the terminal, so the screen just changes.
    let ending = loop {
        match session(&mut terminal, &mut connection).await {
            Ok(Ending::Bye(Bye::Switch(to))) => {
                match connect(to).await.and_then(|new| {
                    terminal.clear()?;
                    Ok(new)
                }) {
                    Ok(new) => {
                        connection = new;
                        id = to;
                    }
                    Err(e) => break Err(e),
                }
            }
            other => break other,
        }
    };
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();

    match ending? {
        Ending::Bye(Bye::Detached) => {
            println!("Detached; the session keeps running. Back to it: tamale attach {id}");
        }
        Ending::Bye(Bye::Replaced) => println!("Session {id} was attached from another terminal."),
        Ending::Bye(Bye::Exited(None) | Bye::Switch(_)) => {}
        Ending::Bye(Bye::Exited(Some(error))) => bail!("{error}"),
        Ending::Refused(message) => bail!("{message}"),
        Ending::Lost => bail!("Session {id} stopped unexpectedly.{}", log_tail(id)),
    }
    Ok(())
}

async fn session(
    terminal: &mut ratatui::DefaultTerminal,
    connection: &mut Connection,
) -> Result<Ending> {
    let Connection { writer, messages } = connection;
    let mut events = EventStream::new();
    let mut mouse = false;
    loop {
        tokio::select! {
            event = events.next() => {
                let Some(event) = event else { continue };
                let message = match event? {
                    Event::Resize(cols, rows) => ToServer::Resize { cols, rows },
                    event => ToServer::Input(event),
                };
                if write_message(writer, &message).await.is_err() {
                    return Ok(Ending::Lost);
                }
            }
            message = messages.recv() => match message {
                None => return Ok(Ending::Lost),
                Some(ToClient::Frame(frame)) => paint(terminal, frame)?,
                Some(ToClient::Mouse(on)) => {
                    mouse = on;
                    if on {
                        execute!(std::io::stdout(), EnableMouseCapture)?;
                    } else {
                        execute!(std::io::stdout(), DisableMouseCapture)?;
                    }
                }
                Some(ToClient::Login(command)) => {
                    // The sign-in needs the keyboard to itself.
                    drop(events);
                    suspend_terminal();
                    let (success, status) = run_login(command).await;
                    *terminal = resume_terminal(mouse)?;
                    terminal.clear()?;
                    events = EventStream::new();
                    let done = ToServer::LoginDone { success, status };
                    if write_message(writer, &done).await.is_err() {
                        return Ok(Ending::Lost);
                    }
                }
                Some(ToClient::Bye(bye)) => return Ok(Ending::Bye(bye)),
                Some(ToClient::Refused(message)) => return Ok(Ending::Refused(message)),
            },
        }
    }
}

fn paint(terminal: &mut ratatui::DefaultTerminal, frame: Frame) -> Result<()> {
    let backend = terminal.backend_mut();
    if frame.clear {
        Backend::clear(backend)?;
    }
    let cells: Vec<_> = frame
        .cells
        .iter()
        .map(|cell| (cell.x, cell.y, cell.to_cell()))
        .collect();
    backend.draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))?;
    match frame.cursor {
        Some(position) => {
            backend.set_cursor_position(position)?;
            backend.show_cursor()?;
        }
        None => backend.hide_cursor()?,
    }
    Backend::flush(backend)?;
    Ok(())
}

async fn run_login(login: LoginCommand) -> (bool, String) {
    let mut command = tokio::process::Command::new(&login.program);
    command.args(&login.args);
    for (name, value) in &login.envs {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (false, format!("couldn't run {}: {e}", login.program)),
    };
    // Ctrl+C is meant for the sign-in, which shares this process group.
    loop {
        tokio::select! {
            status = child.wait() => {
                return match status {
                    Ok(status) => (status.success(), status.to_string()),
                    Err(e) => (false, e.to_string()),
                };
            }
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::State;

    #[test]
    fn the_session_list_shows_escape_sequences_as_text() {
        let lines = server_lines(&[Status {
            id: 7,
            cwd: "/tmp/\u{1b}[2Jrepo".into(),
            session_id: "s".into(),
            title: "hi\u{1b}]52;c;aGk=\u{7}".into(),
            state: State::Idle,
            attached: false,
            since: 0,
            sid: String::new(),
            chef: false,
        }]);
        assert!(
            !lines[0].contains('\u{1b}') && !lines[0].contains('\u{7}'),
            "{:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("/tmp/[2Jrepo") && lines[0].contains("hi]52;c;aGk="),
            "{:?}",
            lines[0]
        );
    }
}
