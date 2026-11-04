//! `/browser`: the TUI in a browser tab. A local server sends a page that runs tamale's
//! WebAssembly client (`web/client`); over a WebSocket the client reports its size and
//! input, and tamale sends the cells each draw changes (see `web/protocol`). One tab is
//! connected at a time, and either the tab or the terminal is in use; the other shows a
//! notice until a key is pressed in it.

mod backend;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::{SinkExt, StreamExt};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tamale_web_protocol::{self as protocol, ClientMessage, ServerMessage};
use tokio::sync::mpsc;

pub use backend::FrameBackend;

#[cfg(test)]
mod tests;

/// A terminal whose changes are sent to the connected tab.
pub type WebTerminal = Terminal<FrameBackend>;

/// Largest size, in cells, accepted from a tab.
pub const MAX_CELLS: u16 = 1000;

const PAGE: &str = include_str!("browser/index.html");
/// Built by `web/build.sh`.
const CLIENT_JS: &str = include_str!("browser/dist/client.js");
const CLIENT_WASM: &[u8] = include_bytes!("browser/dist/client_bg.wasm");

pub enum ClientEvent {
    /// A tab connected (replacing any earlier one); its size in columns and rows.
    Connected(u16, u16),
    Resize(u16, u16),
    Input(Event),
    Disconnected,
}

pub struct Server {
    pub url: String,
    pub events: mpsc::UnboundedReceiver<ClientEvent>,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

enum Out {
    Frame(Vec<u8>),
    Close(&'static str),
}

struct Shared {
    token: String,
    /// The `Host` the page must be loaded from, e.g. `127.0.0.1:52011`.
    host: String,
    events: mpsc::UnboundedSender<ClientEvent>,
    /// The connected tab's id and the channel its socket sends from.
    client: Mutex<Option<(u64, mpsc::UnboundedSender<Out>)>>,
    next_id: AtomicU64,
}

impl Shared {
    fn is_current(&self, id: u64) -> bool {
        self.client
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
    }

    /// Whether a request was addressed to this server by its IP, which rejects
    /// DNS-rebinding pages that resolve their own name to 127.0.0.1.
    fn own_host(&self, headers: &HeaderMap) -> bool {
        headers.get(header::HOST).and_then(|h| h.to_str().ok()) == Some(self.host.as_str())
    }

    /// Whether a request is from this server's page: addressed to it and with the token.
    fn authorized(&self, uri: &Uri, headers: &HeaderMap) -> bool {
        let token = format!("t={}", self.token);
        self.own_host(headers)
            && uri
                .query()
                .is_some_and(|q| q.split('&').any(|pair| pair == token))
    }
}

impl Server {
    /// Starts serving on a free port of 127.0.0.1.
    pub async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("couldn't listen on 127.0.0.1")?;
        let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).context("couldn't generate an access token")?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let url = format!("http://{host}/?t={token}");
        let (tx, events) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            token,
            host,
            events: tx,
            client: Mutex::new(None),
            next_id: AtomicU64::new(0),
        });
        let router = Router::new()
            .route("/", get(page))
            .route("/ws", get(socket))
            .route("/client.js", get(client_js))
            .route("/client_bg.wasm", get(client_wasm))
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url,
            events,
            shared,
            task,
        })
    }

    /// Sends what the terminal's draws changed to the connected tab.
    pub fn flush(&self, terminal: &mut WebTerminal) {
        let Some(frame) = terminal.backend_mut().take_frame() else {
            return;
        };
        if let Some((_, tx)) = self.shared.client.lock().unwrap().as_ref() {
            let _ = tx.send(Out::Frame(protocol::encode(&ServerMessage::Frame(frame))));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        if let Some((_, tx)) = self.shared.client.lock().unwrap().take() {
            let _ = tx.send(Out::Close("tamale stopped serving to the browser."));
        }
    }
}

pub fn terminal(cols: u16, rows: u16) -> WebTerminal {
    match Terminal::new(FrameBackend::new(cols, rows)) {
        Ok(terminal) => terminal,
    }
}

/// Fills the screen that isn't in use with a centered message.
pub fn draw_notice(frame: &mut Frame, text: &str) {
    let area = frame.area();
    let notice = Paragraph::new(text)
        .style(Style::new().fg(Color::DarkGray))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    let height = notice.line_count(area.width.saturating_sub(4)) as u16;
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(notice, middle.inner(ratatui::layout::Margin::new(2, 0)));
}

/// Whether input on the screen that isn't in use should switch to it.
pub fn takes_over(event: &Event) -> bool {
    match event {
        Event::Key(key) => key.kind != KeyEventKind::Release,
        Event::Paste(_) => true,
        Event::Mouse(mouse) => matches!(mouse.kind, MouseEventKind::Down(_)),
        _ => false,
    }
}

/// Opens a URL in the default browser, without waiting for it.
pub fn open(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        tokio::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = tokio::process::Command::new("cmd");
        command.args(["/c", "start", ""]);
        command
    } else {
        tokio::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    tokio::spawn(async move {
        let _ = command.status().await;
    });
}

async fn page(State(shared): State<Arc<Shared>>, uri: Uri, headers: HeaderMap) -> Response {
    if !shared.authorized(&uri, &headers) {
        return (StatusCode::FORBIDDEN, "Run /browser in tamale for a link.").into_response();
    }
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        // Keep the token in the URL from leaking to other sites.
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
    ];
    (headers, PAGE).into_response()
}

/// The client's code holds nothing secret, but is only served to this server's page.
async fn client_js(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ([(header::CONTENT_TYPE, "text/javascript")], CLIENT_JS).into_response()
}

async fn client_wasm(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ([(header::CONTENT_TYPE, "application/wasm")], CLIENT_WASM).into_response()
}

async fn socket(
    State(shared): State<Arc<Shared>>,
    uri: Uri,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Browsers send the page's origin, so other sites can't connect even with the token.
    let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
    if !shared.authorized(&uri, &headers) || origin != Some(&format!("http://{}", shared.host)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade.on_upgrade(move |ws| client(ws, shared))
}

async fn client(ws: WebSocket, shared: Arc<Shared>) {
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let message = match out {
                Out::Frame(bytes) => Message::Binary(bytes.into()),
                Out::Close(reason) => Message::Close(Some(CloseFrame {
                    code: 4000,
                    reason: reason.into(),
                })),
            };
            if sink.send(message).await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });
    // Registered once the tab reports its size.
    let mut tx = Some(tx);
    while let Some(Ok(message)) = stream.next().await {
        let bytes = match message {
            Message::Binary(bytes) => bytes,
            Message::Close(_) => break,
            _ => continue,
        };
        let Some(message) = protocol::decode::<ClientMessage>(&bytes) else {
            continue;
        };
        if let ClientMessage::Size { cols, rows } = message {
            let (cols, rows) = (cols.clamp(1, MAX_CELLS), rows.clamp(1, MAX_CELLS));
            if let Some(tx) = tx.take() {
                let previous = shared.client.lock().unwrap().replace((id, tx));
                if let Some((_, previous)) = previous {
                    let _ = previous.send(Out::Close("tamale was opened in another tab."));
                }
                let _ = shared.events.send(ClientEvent::Connected(cols, rows));
            } else if shared.is_current(id) {
                let _ = shared.events.send(ClientEvent::Resize(cols, rows));
            }
        } else if shared.is_current(id) {
            for event in events(message) {
                let _ = shared.events.send(ClientEvent::Input(event));
            }
        }
    }
    let mut client = shared.client.lock().unwrap();
    if client.as_ref().is_some_and(|(current, _)| *current == id) {
        *client = None;
        let _ = shared.events.send(ClientEvent::Disconnected);
    }
}

/// The terminal events a client message stands for.
fn events(message: ClientMessage) -> Vec<Event> {
    let mouse = |kind, column, row| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    match message {
        ClientMessage::Size { .. } => Vec::new(),
        ClientMessage::Key(key) => vec![Event::Key(key_event(key))],
        ClientMessage::Text(text) => text
            .chars()
            .filter(|c| !c.is_control())
            .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
            .collect(),
        ClientMessage::Paste(text) => vec![Event::Paste(text)],
        ClientMessage::Wheel { up, col, row } => {
            let kind = if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            };
            vec![mouse(kind, col, row)]
        }
        ClientMessage::Click { col, row } => {
            vec![mouse(MouseEventKind::Down(MouseButton::Left), col, row)]
        }
    }
}

fn key_event(key: protocol::Key) -> KeyEvent {
    use protocol::KeyCode as K;
    let code = match key.code {
        K::Char(c) => KeyCode::Char(c),
        K::F(n) => KeyCode::F(n),
        K::Enter => KeyCode::Enter,
        K::Backspace => KeyCode::Backspace,
        K::Tab => KeyCode::Tab,
        K::BackTab => KeyCode::BackTab,
        K::Esc => KeyCode::Esc,
        K::Up => KeyCode::Up,
        K::Down => KeyCode::Down,
        K::Left => KeyCode::Left,
        K::Right => KeyCode::Right,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::Delete => KeyCode::Delete,
        K::Insert => KeyCode::Insert,
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::CONTROL, key.ctrl);
    modifiers.set(KeyModifiers::ALT, key.alt);
    modifiers.set(KeyModifiers::SHIFT, key.shift);
    KeyEvent::new(code, modifiers)
}
