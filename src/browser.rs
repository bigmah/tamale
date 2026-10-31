//! `/browser`: the TUI in a browser tab. A local server sends a page that runs xterm.js;
//! over a WebSocket the page reports its size and input, and tamale sends back the same
//! ANSI output it writes to a terminal. One tab is connected at a time, and either the tab
//! or the terminal is in use; the other shows a notice until a key is pressed in it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

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
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use serde::Deserialize;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;

#[cfg(test)]
mod tests;

/// A terminal whose output is collected for the connected tab.
pub type WebTerminal = Terminal<CrosstermBackend<Vec<u8>>>;

/// Largest size, in cells, accepted from a tab.
const MAX_CELLS: u16 = 1000;

static PAGE: LazyLock<String> = LazyLock::new(|| {
    include_str!("browser/index.html")
        .replace("/*XTERM_CSS*/", include_str!("browser/vendor/xterm.css"))
        .replace("/*XTERM_JS*/", include_str!("browser/vendor/xterm.js"))
        .replace("/*FIT_JS*/", include_str!("browser/vendor/addon-fit.js"))
        .replace("/*WIDTHS*/", &widths())
});

/// The code points `unicode-width` (which ratatui lays out with) makes zero or double width,
/// as JSON `{"zero": [start, end, ...], "wide": [...]}` with inclusive ranges. xterm.js uses
/// older tables, and would otherwise draw emoji like the spinner's one cell wide.
fn widths() -> String {
    let (mut zero, mut wide) = (Vec::new(), Vec::new());
    for c in (0..=0x10FFFF).filter_map(char::from_u32) {
        let ranges: &mut Vec<(u32, u32)> = match c.width() {
            Some(0) => &mut zero,
            Some(2) => &mut wide,
            _ => continue,
        };
        let cp = c as u32;
        match ranges.last_mut() {
            Some((_, end)) if *end + 1 == cp => *end = cp,
            _ => ranges.push((cp, cp)),
        }
    }
    let flat = |ranges: Vec<(u32, u32)>| -> Vec<u32> {
        ranges
            .into_iter()
            .flat_map(|(start, end)| [start, end])
            .collect()
    };
    serde_json::json!({ "zero": flat(zero), "wide": flat(wide) }).to_string()
}

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

    /// Whether a request carries the token and was addressed to this server by its IP,
    /// which rejects DNS-rebinding pages that resolve their own name to 127.0.0.1.
    fn authorized(&self, uri: &Uri, headers: &HeaderMap) -> bool {
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
        let token = format!("t={}", self.token);
        host == Some(self.host.as_str())
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

    /// Sends a terminal's pending output to the connected tab.
    pub fn flush(&self, terminal: &mut WebTerminal) {
        let bytes = std::mem::take(terminal.backend_mut().writer_mut());
        if bytes.is_empty() {
            return;
        }
        if let Some((_, tx)) = self.shared.client.lock().unwrap().as_ref() {
            let _ = tx.send(Out::Frame(bytes));
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
    let viewport = Viewport::Fixed(Rect::new(0, 0, cols, rows));
    // Writing to a Vec can't fail.
    Terminal::with_options(
        CrosstermBackend::new(Vec::new()),
        TerminalOptions { viewport },
    )
    .expect("in-memory terminal")
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
    (headers, PAGE.as_str()).into_response()
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
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(message) = serde_json::from_str::<ClientMessage>(&text) else {
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
            for event in message.events() {
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

/// What the page sends, as JSON tagged with `t`.
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum ClientMessage {
    Size {
        cols: u16,
        rows: u16,
    },
    /// A keydown, named like DOM `KeyboardEvent.key`.
    Key {
        key: String,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        shift: bool,
    },
    /// Text from an input method, typed as if key by key.
    Text {
        text: String,
    },
    Paste {
        text: String,
    },
    Wheel {
        up: bool,
        col: u16,
        row: u16,
    },
    Click {
        col: u16,
        row: u16,
    },
}

impl ClientMessage {
    fn events(self) -> Vec<Event> {
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        match self {
            Self::Size { .. } => Vec::new(),
            Self::Key {
                key,
                ctrl,
                alt,
                shift,
            } => key_event(&key, ctrl, alt, shift)
                .map(Event::Key)
                .into_iter()
                .collect(),
            Self::Text { text } => text
                .chars()
                .filter(|c| !c.is_control())
                .map(|c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))
                .collect(),
            Self::Paste { text } => vec![Event::Paste(text)],
            Self::Wheel { up, col, row } => {
                let kind = if up {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                };
                vec![mouse(kind, col, row)]
            }
            Self::Click { col, row } => {
                vec![mouse(MouseEventKind::Down(MouseButton::Left), col, row)]
            }
        }
    }
}

/// Translates a DOM key name and modifiers into the event a terminal would report.
fn key_event(key: &str, ctrl: bool, alt: bool, shift: bool) -> Option<KeyEvent> {
    let code = match key {
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "Tab" if shift => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        "Escape" => KeyCode::Esc,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                (Some('F'), Some(_)) => KeyCode::F(key[1..].parse().ok()?),
                _ => return None,
            }
        }
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::CONTROL, ctrl);
    modifiers.set(KeyModifiers::ALT, alt);
    modifiers.set(KeyModifiers::SHIFT, shift);
    Some(KeyEvent::new(code, modifiers))
}
