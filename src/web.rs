//! `/web`: a chat-first browser UI for the session, with a code browser beside it.
//!
//! Unlike `/browser`, which mirrors the terminal's cells, this sends the conversation
//! itself as JSON over a WebSocket: entries (assistant markdown already rendered to
//! HTML), the status line and any pending approval. [`Mirror`] remembers what tabs were
//! sent, so each sync only carries the entries that changed. The page answers with
//! prompts, interrupts and approvals, which the event loop hands to the app, and asks
//! for files, which this server reads itself (see `files`). Any number of tabs can be
//! open; they all show the same session.
//!
//! Assistant replies may end in an interactive HTML artifact (a ```` ```html artifact ````
//! block; prompts sent from the page ask for one). Each finished artifact is stored under
//! an unguessable id and served from `/artifact/<id>` with a sandboxing CSP, so it runs in
//! an opaque origin that can't reach the session; the page embeds it in an iframe.

mod files;
mod render;

pub use render::{artifact_title, is_artifact};

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
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

/// Added to every prompt during a web session: replies are a line or two of prose and an
/// HTML artifact that carries the answer.
pub const ARTIFACT_PROMPT: &str = "\n\n<web-ui>The user is working in tamale's web UI, \
which shows every reply as a short note followed by an HTML artifact. Use tools as usual to \
do the work, then write your final reply in exactly this shape:\n\
1. At most one very brief paragraph (one to three sentences) of plain prose: the answer in a \
nutshell. No headings, lists, tables or code blocks outside the artifact.\n\
2. Then exactly one fenced code block whose info string is exactly `html artifact`, holding a \
complete, self-contained HTML document with a <title> that presents the full answer. If you \
changed code, it shows what changed and why.\n\
Make the artifact genuinely well designed: clear hierarchy, comfortable spacing, good \
typography, a dark theme (background #17181d, panels #1e2026, text #d4d8e0, muted #9aa0b0, \
accent #5c9cf5) and a layout that fits any frame width without horizontal scrolling. Use \
inline SVG diagrams where structure or flow matters, and <pre> blocks for code. Make it \
interactive (tabs, step-throughs, toggles, hover or click details, sliders, filters) when \
that helps the user understand or explore; a well laid out static page is fine otherwise. \
Inline all CSS and JS; load libraries only from https CDNs, and only if needed. It runs in \
a sandboxed iframe with no access to tamale or the files, so embed all data it shows.\
</web-ui>";

/// Script added to each artifact so the page can size its iframe to fit.
const ARTIFACT_RESIZER: &str = "<script>(()=>{const post=()=>parent.postMessage(\
{tamaleArtifactHeight:Math.ceil(document.documentElement.scrollHeight)},'*');\
addEventListener('load',post);new ResizeObserver(post).observe(document.documentElement);\
post();})();</script>";

/// Artifacts by id, and the id given to each document, so a re-rendered reply keeps its URLs.
#[derive(Default)]
struct Artifacts {
    ids: HashMap<u64, String>,
    pages: HashMap<String, Arc<str>>,
}

impl Artifacts {
    /// The URL `html` is served from, storing it if new.
    fn register(&mut self, html: &str) -> String {
        let mut h = DefaultHasher::new();
        html.hash(&mut h);
        let id = self
            .ids
            .entry(h.finish())
            .or_insert_with(|| {
                let mut bytes = [0u8; 16];
                // Without randomness, fall back to the hash: still served only by this host.
                if getrandom::fill(&mut bytes).is_err() {
                    bytes[..8].copy_from_slice(&h.finish().to_le_bytes());
                }
                bytes.iter().map(|b| format!("{b:02x}")).collect()
            })
            .clone();
        self.pages
            .entry(id.clone())
            .or_insert_with(|| format!("{html}\n{ARTIFACT_RESIZER}").into());
        format!("/artifact/{id}")
    }
}

/// What a tab asked for that the app has to handle.
pub enum WebEvent {
    /// A tab connected and needs the whole session; see [`Server::welcome`].
    Connected(u64),
    Disconnected,
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
    artifacts: Arc<Mutex<Artifacts>>,
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
        let mirror = Mirror::default();
        let shared = Arc::new(Shared {
            token,
            host,
            events: tx,
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            roots: Mutex::new(Vec::new()),
            artifacts: mirror.artifacts.clone(),
        });
        let router = Router::new()
            .route("/", get(page))
            .route("/ws", get(socket))
            .route("/app.css", get(style))
            .route("/app.js", get(script))
            .route("/artifact/{id}", get(artifact))
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url,
            events,
            shared,
            mirror,
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

impl Server {
    /// How many tabs are open.
    pub fn tabs(&self) -> usize {
        self.shared.clients.lock().unwrap().len()
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
    artifacts: Arc<Mutex<Artifacts>>,
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
                let json = entry_json(entry, &self.artifacts).to_string();
                self.entries.push((*key, json));
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

fn entry_json(entry: &Entry, artifacts: &Mutex<Artifacts>) -> Value {
    match entry {
        Entry::User(text) => json!({ "kind": "user", "text": text }),
        Entry::Assistant(text) => {
            let mut artifacts = artifacts.lock().unwrap();
            let mut register = |html: &str| artifacts.register(html);
            json!({ "kind": "assistant", "html": render::markdown_with(text, Some(&mut register)) })
        }
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

/// An artifact, sandboxed: scripts run, but in an opaque origin that can't read this
/// server's pages or open its socket, even if opened in a tab of its own.
async fn artifact(
    State(shared): State<Arc<Shared>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(page) = shared.artifacts.lock().unwrap().pages.get(&id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            "No such artifact; it may be from an earlier run.",
        )
            .into_response();
    };
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "sandbox allow-scripts allow-popups allow-modals allow-forms; \
             default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' https:; \
             style-src 'unsafe-inline' https:; img-src data: blob: https:; \
             font-src data: https:; media-src data: blob: https:; connect-src https:; \
             frame-ancestors 'self'",
        ),
    ];
    (headers, page.to_string()).into_response()
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
    let _ = shared.events.send(WebEvent::Disconnected);
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
    fn artifacts_keep_their_url_across_renders() {
        let mut mirror = Mirror::default();
        let reply = "Sure.\n\n```html artifact\n<title>T</title>\n```";
        let mut entries = vec![Entry::Assistant(reply.into())];
        let first = mirror.update(&view(&entries));
        let url = first[0]
            .split("data-src=\\\"")
            .nth(1)
            .and_then(|rest| rest.split("\\\"").next())
            .expect("an embed")
            .to_string();
        let id = url.strip_prefix("/artifact/").unwrap();
        assert_eq!(id.len(), 32);
        let page = mirror.artifacts.lock().unwrap().pages[id].clone();
        assert!(page.starts_with("<title>T</title>") && page.contains("tamaleArtifactHeight"));

        entries[0] = Entry::Assistant(format!("{reply}\n\nMore."));
        let again = mirror.update(&view(&entries));
        assert!(again[0].contains(&url), "{}", again[0]);
        assert_eq!(mirror.artifacts.lock().unwrap().pages.len(), 1);
    }

    #[tokio::test]
    async fn artifacts_are_served_sandboxed_to_this_host_only() {
        let mut server = Server::start().await.unwrap();
        let entries = vec![Entry::Assistant(
            "```html artifact\n<title>T</title><script>1</script>\n```".into(),
        )];
        server.sync(view(&entries));
        let id = server
            .shared
            .artifacts
            .lock()
            .unwrap()
            .pages
            .keys()
            .next()
            .unwrap()
            .clone();
        let base = server.url.split("/?").next().unwrap().to_string();
        let client = reqwest::Client::new();

        let response = client
            .get(format!("{base}/artifact/{id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(csp.starts_with("sandbox allow-scripts "), "{csp}");
        assert!(!csp.contains("allow-same-origin"), "{csp}");
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("<script>1</script>")
        );

        let missing = client
            .get(format!("{base}/artifact/nope"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);
        let rebound = client
            .get(format!("{base}/artifact/{id}"))
            .header(header::HOST, "evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(rebound.status(), 403);
    }

    #[test]
    fn tool_output_is_clipped_at_a_character_boundary() {
        let (text, cut) = clip("aé", 2);
        assert_eq!((text, cut), ("a", true));
        assert_eq!(clip("abc", 10), ("abc", false));
        let tool = entry_json(
            &Entry::Tool {
                name: "read_file".into(),
                summary: "src/main.rs".into(),
                description: "Read src/main.rs".into(),
                output: Some(("x".repeat(MAX_TOOL_OUTPUT + 5), false)),
            },
            &Mutex::default(),
        );
        assert_eq!(tool["cut"], true);
        assert_eq!(tool["output"].as_str().unwrap().len(), MAX_TOOL_OUTPUT);
    }
}
