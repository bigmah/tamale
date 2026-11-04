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

pub mod e2e;
mod files;
#[cfg(test)]
mod fixtures;
mod relay;
mod render;

pub use relay::{Label, hostname, sid};

/// How a connection to a tamale-hub is going.
#[derive(Clone, Debug, PartialEq)]
pub enum HubState<'a> {
    /// Connected; the link opens this session.
    Live(&'a str),
    /// Not yet, and what's gone wrong so far.
    Connecting(Option<String>),
}
pub use render::{artifact_marker, artifact_title, is_artifact};

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri, header};
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
const NOISE_SCRIPT: &str = include_str!("web/noise.js");
const ARTIFACT_FRAME: &str = include_str!("web/artifact-frame.html");

/// Tool output sent per call; the rest is cut.
const MAX_TOOL_OUTPUT: usize = 24_000;

/// Added to every prompt during a web session: a reminder of the reply's shape. The design
/// guide itself is in the system prompt (see [`artifact_guide`]).
pub const ARTIFACT_PROMPT: &str = "\n\n<web-ui>Reply for tamale's web UI: do the work with \
tools as usual, then write at most three sentences of plain prose followed by exactly one \
complete, self-contained HTML document in a fenced block whose info string is exactly \
`html artifact`, following the artifact design guide in your instructions. To revise an \
earlier artifact, send the whole updated document.</web-ui>";

/// The built-in artifact design guide; `~/.config/tamale/artifacts.md` replaces it.
const ARTIFACT_GUIDE: &str = include_str!("web/artifact.md");

/// The artifact design guide for the system prompt: the user's
/// `~/.config/tamale/artifacts.md` if it has any text, else the built-in one.
pub fn artifact_guide() -> String {
    crate::config::user_config_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("artifacts.md")).ok())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| ARTIFACT_GUIDE.to_string())
}

/// Where a session's artifacts are saved: `<sessions>/<id>/artifacts/`.
pub fn artifact_dir(session_id: &str) -> Option<PathBuf> {
    Some(crate::session::dir()?.join(session_id).join("artifacts"))
}

/// The file an artifact is saved as: its title as a slug, then a short content hash so
/// revisions sit side by side and re-rendering the same document finds the same file.
fn artifact_file_name(html: &str) -> String {
    let mut h = DefaultHasher::new();
    html.hash(&mut h);
    let mut slug = String::new();
    for c in render::artifact_title(html).unwrap_or_default().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "artifact" } else { slug };
    format!("{slug}-{:08x}.html", h.finish() as u32)
}

/// Saves `html` into `dir` unless it's already there.
fn save_artifact(dir: &std::path::Path, html: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(artifact_file_name(html));
    if !path.exists() {
        std::fs::create_dir_all(dir)?;
        crate::secure::write_private(&path, html)?;
    }
    Ok(path)
}

/// Script added to each artifact so the page can size its iframe to fit.
const ARTIFACT_RESIZER: &str = "<script>(()=>{const post=()=>parent.postMessage(\
{tamaleArtifactHeight:Math.ceil(document.documentElement.scrollHeight)},'*');\
addEventListener('load',post);new ResizeObserver(post).observe(document.documentElement);\
post();})();</script>";

/// Script added to each artifact so nothing needs sideways scrolling on a narrow screen.
const ARTIFACT_FIT: &str = concat!("<script>", include_str!("web/artifact-fit.js"), "</script>");

/// SHA-256 of `text` in lowercase hex: an artifact's stable name, the same in every
/// process and Rust version, for clients that keep a library of them (see `apple/ios.md`).
pub fn content_hash(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Artifacts by id, and the id given to each document, so a re-rendered reply keeps its URLs.
#[derive(Default)]
struct Artifacts {
    ids: HashMap<u64, String>,
    pages: HashMap<String, Arc<str>>,
    /// Each id's [`content_hash`] of the document as the model wrote it.
    hashes: HashMap<String, String>,
}

impl Artifacts {
    /// The URL `html` is served from, storing it if new and saving it into `dir`.
    fn register(&mut self, html: &str, dir: Option<&std::path::Path>) -> String {
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
        self.hashes
            .entry(id.clone())
            .or_insert_with(|| content_hash(html));
        self.pages.entry(id.clone()).or_insert_with(|| {
            if let Some(dir) = dir {
                let _ = save_artifact(dir, html);
            }
            format!("{html}\n{ARTIFACT_FIT}\n{ARTIFACT_RESIZER}").into()
        });
        format!("/artifact/{id}")
    }
}

/// What a tab asked for that the app has to handle.
pub enum WebEvent {
    /// A tab connected and needs the whole session; see [`Server::welcome`].
    Connected(u64),
    Disconnected,
    /// How the connection to a tamale-hub went: `Ok` with news worth showing, `Err` with
    /// a problem. Either way the footer's hub indicator may have changed.
    Hub(Result<String, String>),
    /// A prompt or slash command.
    Send(String),
    /// The same from another terminal, where replies shouldn't be written for the web UI.
    TerminalSend(String),
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
    /// Where finished artifacts are saved, if anywhere.
    pub artifact_dir: Option<PathBuf>,
}

pub struct Server {
    pub url: String,
    /// Whether it listens on the local network instead of only this machine.
    pub lan: bool,
    pub events: mpsc::UnboundedReceiver<WebEvent>,
    shared: Arc<Shared>,
    mirror: Mirror,
    task: tokio::task::JoinHandle<()>,
    /// The connection to a tamale-hub, while `/remote` is on.
    relay: Option<relay::Relay>,
}

enum Out {
    Text(String),
    Close(&'static str),
}

struct Client {
    tx: mpsc::UnboundedSender<Out>,
    /// Whether it has had the whole session; until then it gets no updates.
    welcomed: bool,
    /// Whether it connects through a tamale-hub rather than to this server directly.
    remote: bool,
    /// Whether it's another terminal driving this session (see [`crate::remote`]), which
    /// shows an artifact as one line.
    terminal: bool,
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
    /// Where the current session saves its artifacts.
    artifact_dir: Mutex<Option<PathBuf>>,
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
                .is_some_and(|q| q.split('&').any(|pair| crate::secure::same_secret(pair, &token)))
    }

    fn send_to(&self, id: u64, message: String) {
        if let Some(client) = self.clients.lock().unwrap().get(&id) {
            let _ = client.tx.send(Out::Text(message));
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.roots.lock().unwrap().clone()
    }

    /// Registers a tab, local or relayed, and asks the app to welcome it.
    fn add_client(&self, tx: mpsc::UnboundedSender<Out>, remote: bool) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.clients.lock().unwrap().insert(
            id,
            Client {
                tx,
                welcomed: false,
                remote,
                terminal: false,
            },
        );
        let _ = self.events.send(WebEvent::Connected(id));
        id
    }

    fn remove_client(&self, id: u64) {
        if self.clients.lock().unwrap().remove(&id).is_some() {
            let _ = self.events.send(WebEvent::Disconnected);
        }
    }
}

/// A response to one of the page's GETs, whether served here or relayed through a hub.
struct Asset {
    status: StatusCode,
    headers: Vec<(HeaderName, &'static str)>,
    body: String,
}

impl IntoResponse for Asset {
    fn into_response(self) -> Response {
        let mut response = (self.status, self.body).into_response();
        for (name, value) in self.headers {
            response
                .headers_mut()
                .insert(name, axum::http::HeaderValue::from_static(value));
        }
        response
    }
}

const PAGE_CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
     style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

/// Scripts run, but in an opaque origin that can't read this server's pages or open its
/// socket, even if opened in a tab of its own.
const ARTIFACT_CSP: &str = "sandbox allow-scripts allow-popups allow-modals allow-forms; \
     default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval' https:; \
     style-src 'unsafe-inline' https:; img-src data: blob: https:; \
     font-src data: https:; media-src data: blob: https:; connect-src https:; \
     frame-ancestors 'self'";

/// The page at `path` (`/`, `/app.js`, `/app.css` or `/artifact/<id>`), without checking
/// who asks: the HTTP routes check first, and a hub has checked its browser.
fn asset(shared: &Shared, path: &str) -> Asset {
    let ok = |content_type, body: &str, extra: &[(HeaderName, &'static str)]| {
        let mut headers = vec![
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ];
        headers.extend_from_slice(extra);
        Asset {
            status: StatusCode::OK,
            headers,
            body: body.to_string(),
        }
    };
    match path {
        "/" => ok(
            "text/html; charset=utf-8",
            PAGE,
            &[
                // Keep the token in the URL from leaking to other sites.
                (header::REFERRER_POLICY, "no-referrer"),
                (header::X_FRAME_OPTIONS, "DENY"),
                (header::CONTENT_SECURITY_POLICY, PAGE_CSP),
            ],
        ),
        "/app.css" => ok("text/css; charset=utf-8", STYLE, &[]),
        "/app.js" => ok("text/javascript; charset=utf-8", SCRIPT, &[]),
        "/noise.js" => ok("text/javascript; charset=utf-8", NOISE_SCRIPT, &[]),
        // Holds nothing itself: on an encrypted session the page posts an artifact to it.
        "/artifact-frame" => ok(
            "text/html; charset=utf-8",
            ARTIFACT_FRAME,
            &[
                (header::REFERRER_POLICY, "no-referrer"),
                (header::CONTENT_SECURITY_POLICY, ARTIFACT_CSP),
            ],
        ),
        _ => {
            let page = path
                .strip_prefix("/artifact/")
                .and_then(|id| shared.artifacts.lock().unwrap().pages.get(id).cloned());
            match page {
                Some(page) => ok(
                    "text/html; charset=utf-8",
                    &page,
                    &[
                        (header::REFERRER_POLICY, "no-referrer"),
                        (header::CONTENT_SECURITY_POLICY, ARTIFACT_CSP),
                    ],
                ),
                None => Asset {
                    status: StatusCode::NOT_FOUND,
                    headers: vec![(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    body: "No such artifact; it may be from an earlier run.".into(),
                },
            }
        }
    }
}

impl Server {
    /// Starts serving on a free port of 127.0.0.1, or with `lan` of this machine's
    /// local-network address, so other devices on the network can open it.
    pub async fn start(lan: bool) -> Result<Self> {
        let ip = if lan {
            lan_ip().context("couldn't find this machine's local-network address")?
        } else {
            IpAddr::from([127, 0, 0, 1])
        };
        let listener = tokio::net::TcpListener::bind((ip, 0))
            .await
            .with_context(|| format!("couldn't listen on {ip}"))?;
        let host = format!("{ip}:{}", listener.local_addr()?.port());
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
            artifact_dir: Mutex::new(None),
        });
        let router = Router::new()
            .route("/", get(page))
            .route("/ws", get(socket))
            .route("/app.css", get(style))
            .route("/app.js", get(script))
            .route("/noise.js", get(noise_script))
            .route("/artifact/{id}", get(artifact))
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            url,
            lan,
            events,
            shared,
            mirror,
            task,
            relay: None,
        })
    }

    /// Makes the session reachable through the tamale-hub at `url` (`wss://…`), with
    /// a key's auth value `token`, end-to-end encrypted if there are `keys`. Progress arrives as
    /// [`WebEvent::Hub`].
    pub fn connect_hub(
        &mut self,
        url: &str,
        token: &str,
        label: relay::Label,
        keys: Option<e2e::Keys>,
    ) {
        self.relay = Some(relay::Relay::start(
            self.shared.clone(),
            url,
            token,
            label,
            keys,
        ));
        if let Some(relay) = &self.relay {
            relay.status(&self.mirror.hub_status);
        }
    }

    pub fn disconnect_hub(&mut self) -> bool {
        self.relay.take().is_some()
    }

    /// The link that opens this session through the hub, while connected to one.
    pub fn hub_url(&self) -> Option<&str> {
        self.relay.as_ref().map(|r| r.open_url.as_str())
    }

    /// The link for `o` and `c` on the terminal's screen: the hub's while connected to one,
    /// since it works from any device and the local one doesn't.
    pub fn share_url(&self) -> &str {
        self.hub_url().unwrap_or(&self.url)
    }

    /// How the connection to the hub is going, for the terminal's screen.
    pub fn hub_state(&self) -> Option<HubState<'_>> {
        let relay = self.relay.as_ref()?;
        Some(if relay.live() {
            HubState::Live(&relay.open_url)
        } else {
            HubState::Connecting(relay.problem())
        })
    }

    /// Sends welcomed tabs what changed since the last sync.
    pub fn sync(&mut self, view: View) {
        *self.shared.artifact_dir.lock().unwrap() = view.artifact_dir.clone();
        let roots_changed = {
            let mut roots = self.shared.roots.lock().unwrap();
            let changed = *roots != view.roots;
            if changed {
                *roots = view.roots.clone();
            }
            changed
        };
        let mut messages = self.mirror.update(&view);
        if let Some(relay) = &self.relay {
            relay.status(&self.mirror.hub_status);
        }
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

    /// How many of them are open through a tamale-hub.
    pub fn remote_tabs(&self) -> usize {
        self.shared
            .clients
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.remote)
            .count()
    }

    /// How many of those show artifacts, unlike another terminal.
    pub fn web_remote_tabs(&self) -> usize {
        self.shared
            .clients
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.remote && !c.terminal)
            .count()
    }
}

/// The address this machine reaches the local network from: connecting a UDP socket
/// sends nothing, but picks the interface a packet to the internet would leave by.
fn lan_ip() -> Result<IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
    socket.connect(("8.8.8.8", 80))?;
    let ip = socket.local_addr()?.ip();
    anyhow::ensure!(
        !ip.is_loopback() && !ip.is_unspecified(),
        "not on a network"
    );
    Ok(ip)
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
    /// What a hub's session list shows, as of the last update.
    hub_status: relay::HubStatus,
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
                let json =
                    entry_json(entry, &self.artifacts, view.artifact_dir.as_deref()).to_string();
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
        self.hub_status = relay::HubStatus {
            busy: view.status.running || view.status.compacting,
            approval: view.approval.is_some(),
            title: view
                .entries
                .iter()
                .find_map(|e| match e {
                    Entry::User(text) => Some(text.lines().next().unwrap_or("").chars().take(80).collect()),
                    _ => None,
                })
                .unwrap_or_default(),
            model: view.status.model.clone(),
        };
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

fn entry_json(entry: &Entry, artifacts: &Mutex<Artifacts>, dir: Option<&std::path::Path>) -> Value {
    match entry {
        Entry::User(text) => json!({ "kind": "user", "text": text }),
        Entry::Assistant(text) => {
            let mut artifacts = artifacts.lock().unwrap();
            let mut register = |html: &str| artifacts.register(html, dir);
            let html = render::markdown_with(text, Some(&mut register));
            // For native clients, which render markdown themselves: the text without its
            // artifacts, and the artifacts to put where their markers are.
            let (md, blocks) = render::split_artifacts(text);
            let blocks: Vec<Value> = blocks
                .iter()
                .map(|(body, done)| {
                    let id = done.then(|| {
                        let url = register(body);
                        url.trim_start_matches("/artifact/").to_string()
                    });
                    json!({
                        "id": id,
                        "title": render::artifact_title(body).unwrap_or_else(|| "Artifact".into()),
                        "done": done,
                        "hash": done.then(|| content_hash(body)),
                    })
                })
                .collect();
            json!({ "kind": "assistant", "html": html, "md": md, "artifacts": blocks })
        }
        Entry::Note(text) => json!({ "kind": "note", "html": render::markdown(text), "md": text }),
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
    asset(&shared, "/").into_response()
}

/// The page's code holds nothing secret, but is only served to this server's page.
async fn style(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/app.css").into_response()
}

async fn script(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/app.js").into_response()
}

async fn noise_script(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/noise.js").into_response()
}

/// An artifact, sandboxed (see [`ARTIFACT_CSP`]).
async fn artifact(
    State(shared): State<Arc<Shared>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, &format!("/artifact/{id}")).into_response()
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
    let id = shared.add_client(tx, false);
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
    shared.remove_client(id);
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
        "terminal" => {
            if let Some(client) = shared.clients.lock().unwrap().get_mut(&id) {
                client.terminal = true;
            }
        }
        "send" => {
            let terminal = shared.clients.lock().unwrap().get(&id).is_some_and(|c| c.terminal);
            let text = str_field("text");
            let _ = shared.events.send(if terminal {
                WebEvent::TerminalSend(text)
            } else {
                WebEvent::Send(text)
            });
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
        // On an encrypted session the page can't fetch `/artifact/<id>`; it asks here.
        "artifact" => {
            let artifact = str_field("id");
            let (page, hash) = {
                let artifacts = shared.artifacts.lock().unwrap();
                (
                    artifacts.pages.get(&artifact).cloned(),
                    artifacts.hashes.get(&artifact).cloned(),
                )
            };
            let reply = artifact_reply(&artifact, page.as_deref(), hash.as_deref());
            shared.send_to(id, reply.to_string());
        }
        "artifacts" => {
            let shared = shared.clone();
            let current = shared.artifact_dir.lock().unwrap().clone();
            reply(Box::new(move |roots| {
                let sessions = roots
                    .first()
                    .map(|cwd| crate::session::list(Some(cwd)))
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|meta| Some((artifact_dir(&meta.id)?, Some(meta))));
                saved_artifacts(current, sessions, &shared.artifacts)
            }));
        }
        _ => {}
    }
}

/// The answer to an `artifact` request: the page as served (with tamale's scripts), and
/// the [`content_hash`] of the document as written.
fn artifact_reply(id: &str, html: Option<&str>, hash: Option<&str>) -> Value {
    json!({ "t": "artifact", "id": id, "html": html, "hash": hash })
}

/// The artifacts saved by the current session (its directory is `current`) and by
/// `sessions`, newest first, each with a URL to view it at. Sessions without any are left
/// out.
fn saved_artifacts(
    current: Option<PathBuf>,
    sessions: impl Iterator<Item = (PathBuf, Option<crate::session::Meta>)>,
    artifacts: &Mutex<Artifacts>,
) -> Value {
    let mut dirs: Vec<(PathBuf, Option<crate::session::Meta>)> =
        current.clone().map(|dir| (dir, None)).into_iter().collect();
    for (dir, meta) in sessions {
        match dirs.iter_mut().find(|(d, _)| *d == dir) {
            Some(slot) => slot.1 = meta,
            None => dirs.push((dir, meta)),
        }
    }
    let mut out = Vec::new();
    for (dir, meta) in dirs {
        let mut items: Vec<(u64, Value)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "html"))
            .filter_map(|e| {
                let html = std::fs::read_to_string(e.path()).ok()?;
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                let item = json!({
                    "title": render::artifact_title(&html).unwrap_or_else(|| "Artifact".into()),
                    "file": e.path().display().to_string(),
                    "modified": modified,
                    "hash": content_hash(&html),
                    "url": artifacts.lock().unwrap().register(&html, None),
                });
                Some((modified, item))
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        items.sort_by(|a, b| b.0.cmp(&a.0));
        let newest = items[0].0;
        out.push(json!({
            "current": current.as_ref() == Some(&dir),
            "title": meta.as_ref().map(|m| m.title.clone()).unwrap_or_default(),
            "updated": meta.map_or(newest, |m| m.updated.max(newest)),
            "items": items.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        }));
    }
    // The current session first, then the rest by their latest activity.
    out.sort_by_key(|s| {
        (
            !s["current"].as_bool().unwrap_or(false),
            std::cmp::Reverse(s["updated"].as_u64().unwrap_or(0)),
        )
    });
    json!({ "t": "artifacts", "sessions": out })
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
            artifact_dir: None,
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

    #[test]
    fn native_clients_get_markdown_artifact_blocks_and_stable_hashes() {
        let mut mirror = Mirror::default();
        let doc = "<title>T</title><p>x</p>\n";
        let entries = vec![
            Entry::Assistant(format!("Sure.\n\n```html artifact\n{doc}```\n\nMore.")),
            Entry::Note("*noted*".into()),
            Entry::Assistant("```html artifact\n<title>Half</title>".into()),
        ];
        let sent = mirror.update(&view(&entries));
        let message: Value = serde_json::from_str(&sent[0]).unwrap();
        let items = message["items"].as_array().unwrap();

        let reply = &items[0];
        assert_eq!(reply["md"], "Sure.\n\n<!-- artifact 0 -->\n\nMore.");
        assert!(reply["html"].as_str().unwrap().contains("data-src"), "the page's html stays");
        let block = &reply["artifacts"][0];
        assert_eq!(block["title"], "T");
        assert_eq!(block["done"], true);
        assert_eq!(block["hash"], content_hash(doc));
        let id = block["id"].as_str().unwrap();
        assert!(reply["html"].as_str().unwrap().contains(&format!("/artifact/{id}")));

        assert_eq!(items[1]["md"], "*noted*");
        assert!(items[1].get("artifacts").is_none());

        let building = &items[2]["artifacts"][0];
        assert_eq!(building["done"], false);
        assert!(building["id"].is_null() && building["hash"].is_null());
        assert_eq!(building["title"], "Half");

        // The artifact reply names the document by the same hash, though the page served has
        // tamale's scripts added.
        let artifacts = mirror.artifacts.lock().unwrap();
        let reply = artifact_reply(id, artifacts.pages.get(id).map(|p| &**p), artifacts.hashes.get(id).map(|h| h.as_str()));
        assert_eq!(reply["hash"], content_hash(doc));
        assert!(reply["html"].as_str().unwrap().contains("tamaleArtifactHeight"));
        assert_eq!(
            content_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn artifacts_are_saved_once_under_their_title() {
        let dir = std::env::temp_dir().join(format!(
            "tamale-artifacts-test-{}",
            crate::session::new_id()
        ));
        let reply = "Done.\n\n```html artifact\n<title>Auth flow: v2!</title><p>hi</p>\n```";
        let entries = vec![Entry::Assistant(reply.into())];
        let mut mirror = Mirror::default();
        let mut with_dir = view(&entries);
        with_dir.artifact_dir = Some(dir.clone());
        mirror.update(&with_dir);
        mirror.update(&with_dir);
        let files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].starts_with("auth-flow-v2-") && files[0].ends_with(".html"));
        let saved = std::fs::read_to_string(dir.join(&files[0])).unwrap();
        assert_eq!(saved, "<title>Auth flow: v2!</title><p>hi</p>\n");
        assert_eq!(
            artifact_file_name("<p>x</p>").split('-').next(),
            Some("artifact")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saved_artifacts_list_the_current_session_first_and_share_chat_urls() {
        let root =
            std::env::temp_dir().join(format!("tamale-saved-test-{}", crate::session::new_id()));
        let (now, old, empty) = (root.join("now"), root.join("old"), root.join("empty"));
        let store = Mutex::new(Artifacts::default());
        save_artifact(&old, "<title>Old</title>").unwrap();
        save_artifact(&now, "<title>New</title>").unwrap();
        let chat_url = store.lock().unwrap().register("<title>New</title>", None);
        let listed = saved_artifacts(
            Some(now.clone()),
            [(old, None), (now, None), (empty, None)].into_iter(),
            &store,
        );
        let sessions = listed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0]["current"], true);
        assert_eq!(sessions[0]["items"][0]["title"], "New");
        assert_eq!(sessions[0]["items"][0]["url"], chat_url.as_str());
        assert_eq!(sessions[0]["items"][0]["hash"], content_hash("<title>New</title>"));
        assert_eq!(sessions[1]["items"][0]["title"], "Old");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_terminal_says_what_it_is_and_gets_ordinary_replies() {
        let mut server = Server::start(false).await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let id = server.shared.add_client(tx, true);
        assert!(matches!(server.events.recv().await, Some(WebEvent::Connected(_))));
        assert_eq!((server.remote_tabs(), server.web_remote_tabs()), (1, 1));
        handle(&server.shared, id, json!({ "t": "send", "text": "a" }));
        assert!(matches!(server.events.recv().await, Some(WebEvent::Send(t)) if t == "a"));

        handle(&server.shared, id, json!({ "t": "terminal" }));
        assert_eq!((server.remote_tabs(), server.web_remote_tabs()), (1, 0));
        handle(&server.shared, id, json!({ "t": "send", "text": "b" }));
        assert!(matches!(server.events.recv().await, Some(WebEvent::TerminalSend(t)) if t == "b"));
    }

    #[tokio::test]
    async fn the_share_link_is_the_hubs_while_connected_to_one() {
        let mut server = Server::start(false).await.unwrap();
        let local = server.url.clone();
        assert_eq!(server.share_url(), local);
        let label = relay::Label {
            host: "h".into(),
            cwd: "c".into(),
            model: "m".into(),
        };
        // Nothing listens there: the link doesn't wait for the connection.
        server.connect_hub("ws://127.0.0.1:1", "token", label, None);
        assert_eq!(server.share_url(), relay::open_url("ws://127.0.0.1:1", relay::sid()));
        assert!(server.disconnect_hub());
        assert_eq!(server.share_url(), local);
    }

    #[tokio::test]
    async fn artifacts_are_served_sandboxed_to_this_host_only() {
        let mut server = Server::start(false).await.unwrap();
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

    #[tokio::test]
    async fn relayed_tabs_get_the_same_session_and_pages_as_local_ones() {
        use tamale_hub_protocol::{ToAgent, ToHub};
        // A stand-in hub: welcomes the agent, opens one channel, asks for the page.
        let (frames_tx, mut frames) = mpsc::unbounded_channel::<ToHub>();
        let (auth_tx, mut auth) = mpsc::unbounded_channel::<String>();
        let app = Router::new().route(
            "/agent",
            get(move |headers: HeaderMap, upgrade: WebSocketUpgrade| {
                let (frames_tx, auth_tx) = (frames_tx.clone(), auth_tx.clone());
                async move {
                    let _ = auth_tx.send(
                        headers[header::AUTHORIZATION].to_str().unwrap().to_string(),
                    );
                    upgrade.on_upgrade(move |mut ws| async move {
                        let send = |f: &ToAgent| Message::Text(serde_json::to_string(f).unwrap().into());
                        let Some(Ok(Message::Text(hello))) = ws.next().await else { return };
                        let _ = frames_tx.send(serde_json::from_str(&hello).unwrap());
                        ws.send(send(&ToAgent::Welcome { proto: 1 })).await.unwrap();
                        ws.send(send(&ToAgent::Open { c: 7 })).await.unwrap();
                        ws.send(send(&ToAgent::Get { r: 1, path: "/".into() })).await.unwrap();
                        ws.send(send(&ToAgent::Get { r: 2, path: "/ws".into() })).await.unwrap();
                        while let Some(Ok(Message::Text(t))) = ws.next().await {
                            let _ = frames_tx.send(serde_json::from_str(&t).unwrap());
                        }
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let mut server = Server::start(false).await.unwrap();
        let entries = vec![Entry::User("Fix the flaky test".into())];
        server.sync(view(&entries));
        server.connect_hub(&hub, "f00d", Label::default(), None);
        assert_eq!(auth.recv().await.unwrap(), "Bearer f00d");
        let next = async |frames: &mut mpsc::UnboundedReceiver<ToHub>| {
            tokio::time::timeout(std::time::Duration::from_secs(5), frames.recv()).await.unwrap().unwrap()
        };
        let ToHub::Hello { sid, .. } = next(&mut frames).await else { panic!("no hello") };
        assert_eq!(sid, relay::sid());

        // The channel registers as a remote tab, which the app welcomes as it would a local one.
        let id = loop {
            if let Some(WebEvent::Connected(id)) = tokio::time::timeout(std::time::Duration::from_secs(5), server.events.recv()).await.unwrap() { break id }
        };
        assert_eq!((server.tabs(), server.remote_tabs()), (1, 1));
        server.welcome(id);

        let mut got_page = false;
        let mut got_404 = false;
        let mut got_entries = false;
        let mut got_status = false;
        while !(got_page && got_404 && got_entries && got_status) {
            match next(&mut frames).await {
                ToHub::Got { r: 1, status, headers, body } => {
                    assert_eq!(status, 200);
                    assert!(body.contains("<script type=\"module\" src=\"/app.js\">"));
                    assert!(headers.iter().any(|(k, v)| k == "content-security-policy" && v == PAGE_CSP));
                    got_page = true;
                }
                ToHub::Got { r: 2, status, .. } => {
                    assert_eq!(status, 404, "only page paths are served");
                    got_404 = true;
                }
                ToHub::Msg { c: 7, data } if data.contains("\"t\":\"entries\"") => {
                    assert!(data.contains("Fix the flaky test"));
                    got_entries = true;
                }
                ToHub::Status { title, .. } => {
                    assert_eq!(title, "Fix the flaky test");
                    got_status = true;
                }
                _ => {}
            }
        }
        assert!(matches!(server.hub_state(), Some(HubState::Live(_))));
        assert!(server.hub_url().unwrap().ends_with(&format!("/open/{}", relay::sid())));
        assert!(server.disconnect_hub());
        assert_eq!(server.remote_tabs(), 0);
    }

    /// A stand-in hub for tests of the relay: frames the test sends go to the agent, and
    /// what the agent sends comes back.
    async fn stand_in_hub() -> (String, mpsc::UnboundedSender<tamale_hub_protocol::ToAgent>, mpsc::UnboundedReceiver<String>) {
        use tamale_hub_protocol::ToAgent;
        let (to_agent, from_test) = mpsc::unbounded_channel::<ToAgent>();
        let (to_test, from_agent) = mpsc::unbounded_channel::<String>();
        let from_test = Arc::new(tokio::sync::Mutex::new(from_test));
        let app = Router::new().route(
            "/agent",
            get(move |upgrade: WebSocketUpgrade| {
                let (from_test, to_test) = (from_test.clone(), to_test.clone());
                async move {
                    upgrade.max_message_size(64 << 20).on_upgrade(move |mut ws| async move {
                        let Some(Ok(Message::Text(hello))) = ws.next().await else { return };
                        let _ = to_test.send(hello.to_string());
                        let welcome = serde_json::to_string(&ToAgent::Welcome { proto: 2 }).unwrap();
                        ws.send(Message::Text(welcome.into())).await.unwrap();
                        let mut from_test = from_test.lock().await;
                        loop {
                            tokio::select! {
                                Some(frame) = from_test.recv() => {
                                    let text = serde_json::to_string(&frame).unwrap();
                                    ws.send(Message::Text(text.into())).await.unwrap();
                                }
                                message = ws.next() => match message {
                                    Some(Ok(Message::Text(t))) => { let _ = to_test.send(t.to_string()); }
                                    Some(Ok(_)) => {}
                                    _ => return,
                                },
                            }
                        }
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (hub, to_agent, from_agent)
    }

    #[tokio::test]
    async fn an_encrypted_relay_shows_the_hub_nothing_and_takes_nothing_from_it() {
        use super::e2e::testing::Initiator;
        use std::time::Duration;
        use tamale_hub_protocol::{ToAgent, ToHub};
        const SECRET: &str = "Fix-the-flaky-test-SECRET";
        const LABEL: &str = "SECRET-CWD";
        let psk = [9u8; 32];
        let (hub, to_agent, mut frames) = stand_in_hub().await;
        let mut server = Server::start(false).await.unwrap();
        let entries = vec![
            Entry::User(SECRET.into()),
            Entry::Assistant(format!("```html artifact\n<title>T</title><p>{SECRET}</p>\n```")),
        ];
        server.sync(view(&entries));
        let keys = e2e::Keys::from_psk(psk);
        server.connect_hub(
            &hub,
            "f00d",
            Label { host: "laptop".into(), cwd: LABEL.into(), model: "m".into() },
            Some(keys.clone()),
        );
        // The link shown on the terminal has nothing secret in it.
        assert!(server.hub_url().unwrap().ends_with(&format!("/open/{}", relay::sid())));

        // Everything the hub is sent, checked as it passes.
        let next = async |frames: &mut mpsc::UnboundedReceiver<String>| -> ToHub {
            let text = tokio::time::timeout(Duration::from_secs(5), frames.recv()).await.unwrap().unwrap();
            assert!(!text.contains(SECRET) && !text.contains(LABEL) && !text.contains("laptop"), "{text}");
            serde_json::from_str(&text).unwrap()
        };
        let ToHub::Hello { mid, e2e: on, sealed, label, .. } = next(&mut frames).await else { panic!("no hello") };
        assert_eq!((mid.as_deref(), on), (Some(keys.mid.as_str()), true));
        assert!(sealed.is_some());
        assert_eq!(label, Label::default());
        let ToHub::Status { title, sealed, .. } = next(&mut frames).await else { panic!("no status") };
        assert!(title.is_empty() && sealed.is_some());

        // Frames until channel `c` says something, or closes.
        async fn answer(frames: &mut mpsc::UnboundedReceiver<String>, c: u64) -> Result<Value, String> {
            loop {
                let text = tokio::time::timeout(Duration::from_secs(5), frames.recv()).await.unwrap().unwrap();
                assert!(!text.contains(SECRET), "{text}");
                match serde_json::from_str::<ToHub>(&text).unwrap() {
                    ToHub::Msg { c: from, data } if from == c => return Ok(serde_json::from_str(&data).unwrap()),
                    ToHub::Close { c: from, reason } if from == c => return Err(reason),
                    _ => {}
                }
            }
        }
        let nonce = |v: &Value| {
            use base64::Engine;
            assert_eq!(v["t"], "hs0");
            base64::engine::general_purpose::STANDARD.decode(v["n"].as_str().unwrap()).unwrap()
        };
        let msg = |c, data: String| ToAgent::Msg { c, data };

        // A hub without the key, typing a prompt or approving a tool, is refused.
        for (c, forged) in [
            (1, json!({ "t": "send", "text": "rm -rf /" })),
            (2, json!({ "t": "approve", "choice": "yes" })),
            (3, json!({ "t": "box", "m": "AAAA" })),
        ] {
            to_agent.send(ToAgent::Open { c }).unwrap();
            nonce(&answer(&mut frames, c).await.unwrap());
            to_agent.send(msg(c, forged.to_string())).unwrap();
            assert!(answer(&mut frames, c).await.is_err());
        }

        // Another machine's key fails the handshake.
        to_agent.send(ToAgent::Open { c: 4 }).unwrap();
        let n = nonce(&answer(&mut frames, 4).await.unwrap());
        let hs1 = Initiator::new(&[1u8; 32], relay::sid(), &n, None).hs1(b"{}");
        to_agent.send(msg(4, json!({ "t": "hs1", "m": hs1 }).to_string())).unwrap();
        assert_eq!(answer(&mut frames, 4).await.unwrap_err(), "e2e: wrong key");
        assert_eq!(server.remote_tabs(), 0, "no tab has been made");

        // The right key, on channel 8.
        to_agent.send(ToAgent::Open { c: 8 }).unwrap();
        let n = nonce(&answer(&mut frames, 8).await.unwrap());
        let mut browser = Initiator::new(&psk, relay::sid(), &n, None);
        let hs1 = browser.hs1(b"{}");
        to_agent.send(msg(8, json!({ "t": "hs1", "m": hs1 }).to_string())).unwrap();
        let hs2 = answer(&mut frames, 8).await.unwrap();
        assert_eq!(hs2["t"], "hs2");
        let mut browser = browser.finish(hs2["m"].as_str().unwrap()).unwrap();

        // Only now is it a tab, and what it's sent is sealed.
        let id = loop {
            let event = tokio::time::timeout(Duration::from_secs(5), server.events.recv()).await.unwrap();
            match event {
                Some(WebEvent::Connected(id)) => break id,
                Some(WebEvent::Send(_) | WebEvent::Approve(..)) => panic!("a forged message got through"),
                _ => {}
            }
        };
        server.welcome(id);
        let mut opened = |v: Value| browser.open(v["m"].as_str().unwrap(), 64 << 20).unwrap();
        let first = answer(&mut frames, 8).await.unwrap();
        assert_eq!(first["t"], "box");
        let welcome = opened(first).unwrap();
        assert!(welcome.contains(SECRET) && welcome.contains("\"t\":\"entries\""));
        let artifact_id = {
            let artifacts = server.shared.artifacts.lock().unwrap();
            artifacts.pages.keys().next().unwrap().clone()
        };
        let _ = opened(answer(&mut frames, 8).await.unwrap()); // status
        let _ = opened(answer(&mut frames, 8).await.unwrap()); // approval

        // The hub can't tunnel an artifact; the frame page holds nothing.
        to_agent.send(ToAgent::Get { r: 1, path: format!("/artifact/{artifact_id}") }).unwrap();
        to_agent.send(ToAgent::Get { r: 2, path: "/artifact-frame".into() }).unwrap();
        let mut got = 0;
        while got < 2 {
            match next(&mut frames).await {
                ToHub::Got { r: 1, status, body, .. } => {
                    assert_eq!(status, 404);
                    assert!(body.is_empty());
                    got += 1;
                }
                ToHub::Got { r: 2, status, headers, body } => {
                    assert_eq!(status, 200);
                    assert!(body.contains("tamaleArtifactHtml"));
                    let csp = &headers.iter().find(|(k, _)| k == "content-security-policy").unwrap().1;
                    assert!(csp.starts_with("sandbox allow-scripts ") && !csp.contains("allow-same-origin"), "{csp}");
                    got += 1;
                }
                _ => {}
            }
        }

        // Asked over the sealed socket, a long artifact arrives whole.
        let send_sealed = |browser: &mut e2e::Channel, c: u64, text: String| -> Vec<String> {
            let boxes = browser.seal(&text).unwrap();
            for m in &boxes {
                to_agent.send(msg(c, json!({ "t": "box", "m": m }).to_string())).unwrap();
            }
            boxes
        };
        let up = send_sealed(&mut browser, 8, json!({ "t": "artifact", "id": artifact_id }).to_string());
        let reply: Value = {
            let frame = answer(&mut frames, 8).await.unwrap();
            serde_json::from_str(&browser.open(frame["m"].as_str().unwrap(), 64 << 20).unwrap().unwrap()).unwrap()
        };
        assert_eq!(reply["t"], "artifact");
        assert!(reply["html"].as_str().unwrap().contains(SECRET));

        // A genuine prompt gets through, in several pieces if need be.
        let long = "p".repeat(200_000);
        send_sealed(&mut browser, 8, json!({ "t": "send", "text": long }).to_string());
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), server.events.recv()).await.unwrap();
            if let Some(WebEvent::Send(text)) = event {
                assert_eq!(text, long);
                break;
            }
        }

        // A replay closes the channel, and the tab with it.
        to_agent.send(msg(8, json!({ "t": "box", "m": up[0] }).to_string())).unwrap();
        assert!(answer(&mut frames, 8).await.is_err());
        assert_eq!(server.remote_tabs(), 0);

        // A handshake recorded on channel 8, replayed on channel 9, fails.
        to_agent.send(ToAgent::Open { c: 9 }).unwrap();
        nonce(&answer(&mut frames, 9).await.unwrap());
        to_agent.send(msg(9, json!({ "t": "hs1", "m": hs1 }).to_string())).unwrap();
        assert_eq!(answer(&mut frames, 9).await.unwrap_err(), "e2e: wrong key");
    }

    /// Against a real hub: `TAMALE_HUB_URL` and `TAMALE_HUB_KEY` set, then
    /// `cargo test real_hub -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn real_hub_relays_a_browser_end_to_end() {
        use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
        let env = |k| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
        let hub = env("TAMALE_HUB_URL");
        let key = tamale_hub_protocol::decode_key(&env("TAMALE_HUB_KEY")).expect("a key");
        let token = tamale_hub_protocol::auth_value(&key);
        let origin = hub.replacen("wss://", "https://", 1);
        let mut server = Server::start(false).await.unwrap();
        let entries = vec![Entry::User("Remote smoke test".into())];
        server.sync(view(&entries));
        server.connect_hub(&hub, &token, Label { host: "laptop".into(), cwd: "~/dev/tamale".into(), model: "test".into() }, None);
        let deadline = std::time::Duration::from_secs(15);
        loop {
            match tokio::time::timeout(deadline, server.events.recv()).await.unwrap().unwrap() {
                WebEvent::Hub(Ok(_)) => break,
                WebEvent::Hub(Err(e)) => panic!("{e}"),
                _ => {}
            }
        }
        let sid = relay::sid().to_string();
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        let cookie_of = |r: &reqwest::Response| {
            r.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string()
        };
        let r = http.post(format!("{origin}/api/tokens")).header("origin", &origin)
            .json(&json!({ "add": token })).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let th = cookie_of(&r);
        assert!(th.starts_with("__Host-th="));
        let list: Value = http.get(format!("{origin}/api/sessions")).header("cookie", &th)
            .send().await.unwrap().json().await.unwrap();
        assert!(list["sessions"].as_array().unwrap().iter().any(|s| s["sid"] == sid.as_str()), "{list}");
        let ticket: Value = http.post(format!("{origin}/api/ticket")).header("origin", &origin)
            .header("cookie", &th).json(&json!({ "sid": sid })).send().await.unwrap().json().await.unwrap();
        let r = http.get(ticket["url"].as_str().unwrap()).send().await.unwrap();
        assert_eq!(r.status(), 303);
        let ts = cookie_of(&r);
        let session = origin.replacen("https://", &format!("https://{sid}."), 1);
        let page = http.get(format!("{session}/")).header("cookie", &ts).send().await.unwrap();
        assert_eq!(page.status(), 200);
        assert_eq!(page.headers()[header::CONTENT_SECURITY_POLICY], PAGE_CSP);
        assert!(page.text().await.unwrap().contains("/app.js"));

        let mut request = format!("{}/ws", session.replacen("https://", "wss://", 1)).into_client_request().unwrap();
        request.headers_mut().insert("origin", session.parse().unwrap());
        request.headers_mut().insert("cookie", ts.parse().unwrap());
        let (mut tab, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let id = loop {
            match tokio::time::timeout(deadline, server.events.recv()).await.unwrap().unwrap() {
                WebEvent::Connected(id) => break id,
                _ => {}
            }
        };
        server.welcome(id);
        loop {
            let Some(Ok(tungstenite::Message::Text(t))) = tokio::time::timeout(deadline, tab.next()).await.unwrap() else { panic!("tab closed") };
            if t.contains("Remote smoke test") { break }
        }
        tab.send(tungstenite::Message::Text(r#"{"t":"send","text":"hello through the hub"}"#.into())).await.unwrap();
        loop {
            match tokio::time::timeout(deadline, server.events.recv()).await.unwrap().unwrap() {
                WebEvent::Send(text) => { assert_eq!(text, "hello through the hub"); break }
                _ => {}
            }
        }
        assert_eq!(server.remote_tabs(), 1);
    }

    /// What the stand-in session answers "long" with: taller than the screen, with a background
    /// of its own and a sticky header, as a real artifact has.
    const LONG_ARTIFACT: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Launch checklist</title>
<style>
body { margin: 0; background: #f6efe2; color: #2b2620; font: 16px/1.5 -apple-system, sans-serif; }
header { position: sticky; top: 0; background: #b8532c; color: #fff; padding: 12px 16px; font-weight: 600; }
section { padding: 8px 16px; }
table { border-collapse: collapse; width: 100%; }
td, th { border-bottom: 1px solid #d9cdb8; padding: 6px; text-align: left; }
pre { background: #2b2620; color: #f6efe2; padding: 10px; border-radius: 6px; }
footer { padding: 16px; background: #b8532c; color: #fff; }
</style></head><body><header>Launch checklist</header><main id="m"></main><footer>The end of the page</footer>
<script>
let html = '';
for (let i = 1; i <= 14; i++) {
  html += `<section><h2>Step ${i}</h2><p>Everything that has to be true before step ${i} is done, in enough words to wrap across a few lines on a phone.</p>
<table><tr><th>Owner</th><th>Due</th></tr><tr><td>Sam</td><td>Mar ${i}</td></tr></table><pre>make step-${i}</pre></section>`;
}
document.getElementById('m').innerHTML = html;
</script></body></html>"#;

    /// A stand-in session for the iOS app's integration test (`apple/scripts/integration.sh`):
    /// connects to the hub at `TAMALE_HUB_URL` with `TAMALE_HUB_KEY`, end-to-end encrypted,
    /// prints `SID=<sid>`, and answers each prompt with a reply and an artifact until
    /// `TAMALE_FAKE_SECONDS` (default 120) pass.
    #[tokio::test]
    #[ignore]
    async fn fake_session_for_the_apple_app() {
        let env = |k| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
        let hub = env("TAMALE_HUB_URL");
        let key = tamale_hub_protocol::decode_key(&env("TAMALE_HUB_KEY")).expect("a key");
        let seconds: u64 = std::env::var("TAMALE_FAKE_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        let mut server = Server::start(false).await.unwrap();
        let mut entries = vec![
            Entry::Info("Welcome! A stand-in session for the iOS app's tests.".into()),
            Entry::User("Show me the plan".into()),
            Entry::Assistant(
                "Here it is.\n\n```html artifact\n<!doctype html><title>The plan</title><h1>Plan</h1>\n```".into(),
            ),
        ];
        let mut approval = Some(Approval {
            name: "bash".into(),
            summary: "rm -rf target".into(),
            description: "Clean the build".into(),
            always: vec!["bash: rm *".into()],
        });
        let sync = |server: &mut Server, entries: &[Entry], approval: &Option<Approval>| {
            let mut v = view(entries);
            v.approval = approval.clone();
            server.sync(v);
        };
        sync(&mut server, &entries, &approval);
        server.connect_hub(
            &hub,
            &tamale_hub_protocol::auth_value(&key),
            Label { host: "fake-host".into(), cwd: "~/dev/fake".into(), model: "fake-model".into() },
            Some(e2e::Keys::from_key(&key)),
        );
        println!("SID={}", relay::sid());
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, server.events.recv()).await {
            match event {
                WebEvent::Connected(id) => server.welcome(id),
                WebEvent::Send(text) => {
                    approval.get_or_insert_with(|| Approval {
                        name: "bash".into(),
                        summary: "rm -rf target".into(),
                        description: "Clean the build".into(),
                        always: Vec::new(),
                    });
                    entries.push(Entry::User(text.clone()));
                    if text == "long" {
                        entries.push(Entry::Assistant(format!(
                            "A long one.\n\n```html artifact\n{LONG_ARTIFACT}\n```"
                        )));
                        sync(&mut server, &entries, &approval);
                        continue;
                    }
                    entries.push(Entry::Assistant(format!(
                        "You said **{text}**.\n\n```html artifact\n<!doctype html><title>Echo</title><p>{}</p>\n```",
                        render::escape(&text)
                    )));
                }
                WebEvent::Approve(choice, reason) => {
                    approval = None;
                    entries.push(Entry::Info(format!("approved: {choice:?} {}", reason.unwrap_or_default())));
                }
                WebEvent::Interrupt => entries.push(Entry::Info("interrupted".into())),
                WebEvent::Hub(Err(e)) => eprintln!("hub: {e}"),
                _ => {}
            }
            sync(&mut server, &entries, &approval);
        }
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
            None,
        );
        assert_eq!(tool["cut"], true);
        assert_eq!(tool["output"].as_str().unwrap().len(), MAX_TOOL_OUTPUT);
    }
}
