//! `/web`: a chat-first browser UI for the session, with a code browser beside it.
//!
//! Unlike `/browser`, which mirrors the terminal's cells, this sends the conversation
//! itself as JSON over a WebSocket: entries (assistant markdown already rendered to
//! HTML), the status line and any pending approval. [`Mirror`] remembers what tabs were
//! sent, so each sync only carries the entries that changed. The page answers with
//! prompts, interrupts and approvals, which the event loop hands to the app, and asks
//! for files, which this server reads itself (see `files`). Any number of tabs can be
//! open; they all show the same session.

mod files;
mod render;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::session::Entry;

const PAGE: &str = include_str!("web/index.html");
const STYLE: &str = include_str!("web/app.css");
const SCRIPT: &str = include_str!("web/app.js");

/// Tool output sent per call; the rest is cut.
const MAX_TOOL_OUTPUT: usize = 24_000;

/// What a tab asked for that the app has to handle.
pub enum WebEvent {
    /// A tab connected and needs the whole session; see [`Server::welcome`].
    Connected(u64),
    /// A prompt or slash command.
    Send(String),
    Interrupt,
    Approve(Choice, Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    Yes,
    Always,
    No,
}

/// The session's state besides its entries, as the page's status bar shows it.
#[derive(Serialize, PartialEq, Default, Clone, Debug)]
pub struct Status {
    pub project: String,
    pub cwd: String,
    pub dirs: Vec<String>,
    pub model: String,
    pub backend: String,
    pub signed_in: bool,
    pub mode: &'static str,
    pub effort: Option<&'static str>,
    pub thinking: &'static str,
    pub running: bool,
    pub compacting: bool,
    /// Unix seconds when the current run began.
    pub since: Option<u64>,
    pub context: u64,
    pub window: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
    pub cost_partial: bool,
    pub goal: Option<String>,
    pub queued: Vec<String>,
}

#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct Approval {
    pub name: String,
    pub summary: String,
    pub description: String,
    pub always: Vec<String>,
}

/// Everything a sync sends, borrowed from the app.
pub struct View<'a> {
    pub status: Status,
    pub entries: &'a [Entry],
    pub approval: Option<Approval>,
    /// The workspace: the working directory first, then added directories.
    pub roots: Vec<PathBuf>,
}

pub struct Server {
    pub url: String,
    pub events: mpsc::UnboundedReceiver<WebEvent>,
    shared: Arc<Shared>,
    mirror: Mirror,
    task: tokio::task::JoinHandle<()>,
}

enum Out {
    Text(String),
    Close(&'static str),
}

struct Client {
    tx: mpsc::UnboundedSender<Out>,
    /// Whether it has had the whole session; until then it gets no updates.
    welcomed: bool,
}

struct Shared {
    token: String,
    /// The `Host` the page must be loaded from, e.g. `127.0.0.1:52011`.
    host: String,
    events: mpsc::UnboundedSender<WebEvent>,
    clients: Mutex<HashMap<u64, Client>>,
    next_id: AtomicU64,
    /// The directories the code browser may read.
    roots: Mutex<Vec<PathBuf>>,
}

impl Shared {
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

    fn send_to(&self, id: u64, message: String) {
        if let Some(client) = self.clients.lock().unwrap().get(&id) {
            let _ = client.tx.send(Out::Text(message));
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.roots.lock().unwrap().clone()
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
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            roots: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/", get(page))
            .route("/ws", get(socket))
            .route("/app.css", get(style))
            .route("/app.js", get(script))
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url,
            events,
            shared,
            mirror: Mirror::default(),
            task,
        })
    }

    /// Sends welcomed tabs what changed since the last sync.
    pub fn sync(&mut self, view: View) {
        let roots_changed = {
            let mut roots = self.shared.roots.lock().unwrap();
            let changed = *roots != view.roots;
            if changed {
                *roots = view.roots.clone();
            }
            changed
        };
        let mut messages = self.mirror.update(&view);
        if roots_changed {
            messages.push(json!({ "t": "roots" }).to_string());
        }
        if messages.is_empty() {
            return;
        }
        for client in self.shared.clients.lock().unwrap().values() {
            if client.welcomed {
                for message in &messages {
                    let _ = client.tx.send(Out::Text(message.clone()));
                }
            }
        }
    }

    /// Sends a newly connected tab the whole session (call [`Self::sync`] first so it is
    /// current); from then on it gets updates.
    pub fn welcome(&mut self, id: u64) {
        let mut clients = self.shared.clients.lock().unwrap();
        let Some(client) = clients.get_mut(&id) else {
            return;
        };
        for message in self.mirror.full() {
            let _ = client.tx.send(Out::Text(message));
        }
        client.welcomed = true;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        for (_, client) in self.shared.clients.lock().unwrap().drain() {
            let _ = client
                .tx
                .send(Out::Close("tamale stopped serving the web UI."));
        }
    }
}

/// What the tabs were last sent, to send only what changed.
#[derive(Default)]
struct Mirror {
    /// Each entry's content hash and its JSON.
    entries: Vec<(u64, String)>,
    status: String,
    approval: String,
}

impl Mirror {
    fn update(&mut self, view: &View) -> Vec<String> {
        let mut out = Vec::new();
        let keys: Vec<u64> = view.entries.iter().map(entry_key).collect();
        let same = keys
            .iter()
            .zip(&self.entries)
            .take_while(|(key, (sent, _))| *key == sent)
            .count();
        if same < keys.len() || same < self.entries.len() {
            self.entries.truncate(same);
            for (key, entry) in keys[same..].iter().zip(&view.entries[same..]) {
                self.entries.push((*key, entry_json(entry).to_string()));
            }
            out.push(self.entries_message(same));
        }
        let status = json!({ "t": "status", "status": view.status }).to_string();
        if status != self.status {
            out.push(status.clone());
            self.status = status;
        }
        let approval = json!({ "t": "approval", "approval": view.approval }).to_string();
        if approval != self.approval {
            out.push(approval.clone());
            self.approval = approval;
        }
        out
    }

    fn full(&self) -> Vec<String> {
        [
            self.entries_message(0),
            self.status.clone(),
            self.approval.clone(),
        ]
        .into_iter()
        .filter(|m| !m.is_empty())
        .collect()
    }

    /// The entries from `from` on, replacing whatever the tab has from there.
    fn entries_message(&self, from: usize) -> String {
        let items: Vec<&str> = self.entries[from..]
            .iter()
            .map(|(_, j)| j.as_str())
            .collect();
        format!(
            r#"{{"t":"entries","from":{from},"items":[{}]}}"#,
            items.join(",")
        )
    }
}

fn entry_key(entry: &Entry) -> u64 {
    let mut h = DefaultHasher::new();
    match entry {
        Entry::User(t) => (0u8, t).hash(&mut h),
        Entry::Assistant(t) => (1u8, t).hash(&mut h),
        Entry::Tool {
            name,
            summary,
            description,
            output,
        } => (2u8, name, summary, description, output).hash(&mut h),
        Entry::Info(t) => (3u8, t).hash(&mut h),
        Entry::Note(t) => (4u8, t).hash(&mut h),
        Entry::Error(t) => (5u8, t).hash(&mut h),
    }
    h.finish()
}

fn entry_json(entry: &Entry) -> Value {
    match entry {
        Entry::User(text) => json!({ "kind": "user", "text": text }),
        Entry::Assistant(text) => json!({ "kind": "assistant", "html": render::markdown(text) }),
        Entry::Note(text) => json!({ "kind": "note", "html": render::markdown(text) }),
        Entry::Info(text) => json!({ "kind": "info", "text": text }),
        Entry::Error(text) => json!({ "kind": "error", "text": text }),
        Entry::Tool {
            name,
            summary,
            description,
            output,
        } => {
            let (output, error, cut) = match output {
                Some((text, error)) => {
                    let (text, cut) = clip(text, MAX_TOOL_OUTPUT);
                    (Some(text), *error, cut)
                }
                None => (None, false, false),
            };
            json!({
                "kind": "tool", "name": name, "summary": summary, "description": description,
                "output": output, "error": error, "cut": cut,
            })
        }
    }
}

/// At most `max` bytes of `text`, cut at a character boundary, and whether it was cut.
fn clip(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

async fn page(State(shared): State<Arc<Shared>>, uri: Uri, headers: HeaderMap) -> Response {
    if !shared.authorized(&uri, &headers) {
        return (StatusCode::FORBIDDEN, "Run /web in tamale for a link.").into_response();
    }
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        // Keep the token in the URL from leaking to other sites.
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
             style-src 'self'; script-src 'self'; frame-ancestors 'none'",
        ),
    ];
    (headers, PAGE).into_response()
}

/// The page's code holds nothing secret, but is only served to this server's page.
async fn style(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        STYLE,
    )
        .into_response()
}

async fn script(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        SCRIPT,
    )
        .into_response()
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
    upgrade
        .max_message_size(4 << 20)
        .on_upgrade(move |ws| client(ws, shared))
}

async fn client(ws: WebSocket, shared: Arc<Shared>) {
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let message = match out {
                Out::Text(text) => Message::Text(text.into()),
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
    shared.clients.lock().unwrap().insert(
        id,
        Client {
            tx,
            welcomed: false,
        },
    );
    let _ = shared.events.send(WebEvent::Connected(id));
    while let Some(Ok(message)) = stream.next().await {
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        if let Ok(request) = serde_json::from_str::<Value>(&text) {
            handle(&shared, id, request);
        }
    }
    shared.clients.lock().unwrap().remove(&id);
}

/// A read of the workspace, given its roots, done off the async runtime.
type Read = Box<dyn FnOnce(&[PathBuf]) -> Value + Send>;

/// Acts on one message from tab `id`.
fn handle(shared: &Arc<Shared>, id: u64, request: Value) {
    let str_field = |key: &str| {
        request
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    // Replies carry the request's sequence number, so the page can drop stale ones.
    let seq = request.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let reply = |work: Read| {
        let shared = shared.clone();
        tokio::task::spawn_blocking(move || {
            let mut value = work(&shared.roots());
            value["seq"] = seq.into();
            shared.send_to(id, value.to_string());
        });
    };
    match request.get("t").and_then(Value::as_str).unwrap_or("") {
        "send" => {
            let _ = shared.events.send(WebEvent::Send(str_field("text")));
        }
        "interrupt" => {
            let _ = shared.events.send(WebEvent::Interrupt);
        }
        "approve" => {
            let choice = match str_field("choice").as_str() {
                "yes" => Choice::Yes,
                "always" => Choice::Always,
                "no" => Choice::No,
                _ => return,
            };
            let reason = Some(str_field("reason")).filter(|r| !r.trim().is_empty());
            let _ = shared.events.send(WebEvent::Approve(choice, reason));
        }
        "tree" => reply(Box::new(files::tree)),
        "open" => {
            let root = request.get("root").and_then(Value::as_u64).unwrap_or(0) as usize;
            let path = str_field("path");
            reply(Box::new(move |roots| files::open(roots, root, &path)));
        }
        "grep" => {
            let query = str_field("query");
            reply(Box::new(move |roots| files::grep(roots, &query)));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(entries: &[Entry]) -> View<'_> {
        View {
            status: Status::default(),
            entries,
            approval: None,
            roots: Vec::new(),
        }
    }

    fn from(message: &str) -> (u64, usize) {
        let v: Value = serde_json::from_str(message).unwrap();
        (
            v["from"].as_u64().unwrap(),
            v["items"].as_array().unwrap().len(),
        )
    }

    #[test]
    fn mirror_sends_only_entries_that_changed() {
        let mut mirror = Mirror::default();
        let mut entries = vec![Entry::User("hi".into()), Entry::Assistant("He".into())];
        let first = mirror.update(&view(&entries));
        assert_eq!(first.len(), 3, "entries, status and approval");
        assert_eq!(from(&first[0]), (0, 2));
        assert!(mirror.update(&view(&entries)).is_empty());

        // Streaming text re-sends just the entry being written.
        entries[1] = Entry::Assistant("Hello **there**".into());
        let streamed = mirror.update(&view(&entries));
        assert_eq!(streamed.len(), 1);
        assert_eq!(from(&streamed[0]), (1, 1));
        assert!(streamed[0].contains("<strong>there</strong>"));

        // A removed entry (a retried request) truncates the tab's list.
        entries.pop();
        assert_eq!(from(&mirror.update(&view(&entries))[0]), (1, 0));

        let full = mirror.full();
        assert_eq!(from(&full[0]), (0, 1));
        assert!(full[1].contains("\"t\":\"status\""));
    }

    #[test]
    fn tool_output_is_clipped_at_a_character_boundary() {
        let (text, cut) = clip("aé", 2);
        assert_eq!((text, cut), ("a", true));
        assert_eq!(clip("abc", 10), ("abc", false));
        let tool = entry_json(&Entry::Tool {
            name: "read_file".into(),
            summary: "src/main.rs".into(),
            description: "Read src/main.rs".into(),
            output: Some(("x".repeat(MAX_TOOL_OUTPUT + 5), false)),
        });
        assert_eq!(tool["cut"], true);
        assert_eq!(tool["output"].as_str().unwrap().len(), MAX_TOOL_OUTPUT);
    }
}
