//! `/web`: a chat-first browser UI for the session, with pullspace beside it for the code
//! (drawn by the same WebAssembly module as the rest of the page: see `web/src/review.rs`).
//!
//! Unlike `/browser`, which mirrors the terminal's cells, this sends the conversation
//! itself as JSON over a WebSocket: entries (assistant markdown already rendered to
//! HTML), the status line and any pending approval. [`Mirror`] remembers what tabs were
//! sent, so each sync only carries the entries that changed. The page answers with
//! prompts, interrupts and approvals, which the event loop hands to the app, and asks
//! for the workspace's files and changes, which this server reads itself (see `pullspace`
//! and `diff`). Any number of tabs can be open; they all show the same session.
//!
//! Assistant replies may end in an artifact: an OpenUI Lang document in a ```` ```openui ````
//! block (prompts sent from the page ask for one), which the page draws with its own Dioxus
//! components (`openui/dioxus`), so an artifact is data and never runs script. Each
//! finished artifact is kept under an unguessable id: `/artifact/<id>` is a page that draws
//! it alone, and the `artifact` request returns its document.

mod artifacts;
mod diff;
pub mod e2e;
#[cfg(test)]
mod fixtures;
mod gemini;
mod mirror;
mod pullspace;
mod relay;
mod render;
#[cfg(test)]
mod tests;
pub mod wire;

pub use relay::{Label, hostname, sid};

/// How a connection to a tamale-hub is going.
#[derive(Clone, Debug, PartialEq)]
pub enum HubState<'a> {
    /// Connected; the link opens this session.
    Live(&'a str),
    /// Not yet, and what's gone wrong so far.
    Connecting(Option<String>),
}
pub use artifacts::{
    ARTIFACT_PROMPT, EXTENSION as ARTIFACT_EXTENSION, artifact_dir, artifact_guide,
};
pub use gemini::child_env as gemini_env;
pub use render::{artifact_marker, artifact_title, is_artifact};
pub use wire::{Approval, Status};

use std::collections::HashMap;
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
use artifacts::{Artifacts, artifact_reply, saved_artifacts};
use mirror::Mirror;

const PAGE: &str = include_str!("web/index.html");
/// The page's own rules, then those of the components artifacts are drawn with.
const STYLE: &str = concat!(
    include_str!("web/app.css"),
    "\n",
    include_str!("../openui/dioxus/openui.css")
);
/// The page's stylesheet as served: every rule kept out of pullspace's box, since pullspace
/// is drawn in this page with a stylesheet of its own (see `pullspace_embed::css`).
static SERVED_STYLE: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| pullspace_embed::css::outside(STYLE));
/// The page's app: tamale's web UI in Dioxus (`web/`), built to WebAssembly by `build.rs`.
const SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/app.js"));
const MODULE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app_bg.wasm.gz"));
/// The artifact viewer (`openui/dioxus`), built to WebAssembly by `build.rs`: its script,
/// started at `/openui_bg.wasm`, and its module, gzipped.
const VIEWER_SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/openui.js"));
const VIEWER_MODULE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/openui_bg.wasm.gz"));

/// What a tab asked for that the app has to handle.
pub enum WebEvent {
    /// A tab connected and needs the session; see [`Server::welcome`].
    Connected(u64),
    /// A tab that loads history as it scrolls wants the entries before `before`; see
    /// [`Server::older`].
    Older {
        id: u64,
        before: usize,
    },
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
    /// Answers to the agent's questions, one per question.
    Answer(Vec<crate::tools::ask::Answer>),
    /// End the session, as `tamale kill` does: it stays saved.
    Close,
    /// The hub refused this tamale chef because its key has another one online, with the
    /// hub's reason.
    ChefTaken(String),
    /// Tab `tab` opened the model picker and wants the list, for its request `seq`.
    Models {
        tab: u64,
        seq: u64,
    },
    /// A side question from a tamale chef, answered from this session's context without
    /// adding to its conversation. The answer goes back to tab `tab` with [`Replier`].
    Ask {
        tab: u64,
        id: String,
        question: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    Yes,
    Always,
    No,
}

/// The agent's `ask_user` questions, waiting for answers.
#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct Questions {
    pub description: String,
    pub questions: Vec<crate::tools::ask::Question>,
}

/// Everything a sync sends, borrowed from the app.
pub struct View<'a> {
    pub status: Status,
    pub entries: &'a [Entry],
    pub approval: Option<Approval>,
    pub question: Option<Questions>,
    /// The workspace: the working directory first, then added directories.
    pub roots: Vec<PathBuf>,
    /// Where finished artifacts are saved, if anywhere.
    pub artifact_dir: Option<PathBuf>,
    /// A chef's view of the other sessions, for the page's kitchen panel.
    pub kitchen: Option<Kitchen>,
}

/// The sessions a chef looks after, as its page lists them.
#[derive(Serialize, PartialEq, Clone, Debug, Default)]
pub struct Kitchen {
    pub sessions: Vec<KitchenSession>,
    /// Why the list couldn't be had (no key, the hub's down), if it couldn't.
    pub problem: Option<String>,
    pub loading: bool,
}

#[derive(Serialize, PartialEq, Clone, Debug)]
pub struct KitchenSession {
    /// The short id the chef and the user call it by.
    pub id: String,
    pub host: String,
    pub cwd: String,
    pub title: String,
    pub model: String,
    /// `working`, `approval`, `idle` or `offline`.
    pub state: &'static str,
    pub viewers: usize,
}

/// Sends replies to one tab outside the sync, from a task that finishes later.
#[derive(Clone)]
pub struct Replier(Arc<Shared>);

impl Replier {
    pub fn send(&self, tab: u64, message: String) {
        self.0.send_to(tab, message);
    }
}

pub struct Server {
    pub url: String,
    /// The same page at a loopback address, for a browser on this machine: `url` itself
    /// when serving only to this machine. Browsers treat loopback as a secure context and
    /// a LAN address over plain http as not, and pullspace runs far slower outside one
    /// (Safari takes seconds to open a file), so this machine's browser opens this one.
    pub local_url: String,
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
    /// How many of the latest entries it wants when it connects, and it asks for the rest as
    /// it needs them. `None` for clients that want the whole session up front.
    tail: Option<usize>,
}

struct Shared {
    token: String,
    /// The `Host`s the page may be loaded from, e.g. `127.0.0.1:52011`: the address it
    /// listens on, and on the local network its loopback address too.
    hosts: Vec<String>,
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
        self.host(headers).is_some()
    }

    /// Which of this server's hosts a request was addressed to, if any.
    fn host(&self, headers: &HeaderMap) -> Option<&str> {
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok())?;
        self.hosts.iter().map(String::as_str).find(|h| *h == host)
    }

    /// Whether a request is from this server's page: addressed to it and with the token.
    fn authorized(&self, uri: &Uri, headers: &HeaderMap) -> bool {
        let token = format!("t={}", self.token);
        self.own_host(headers)
            && uri.query().is_some_and(|q| {
                q.split('&')
                    .any(|pair| crate::secure::same_secret(pair, &token))
            })
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
    fn add_client(&self, tx: mpsc::UnboundedSender<Out>, remote: bool, tail: Option<usize>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.clients.lock().unwrap().insert(
            id,
            Client {
                tx,
                welcomed: false,
                remote,
                terminal: false,
                tail,
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
    /// Bytes to send instead of `body`, for files that aren't text.
    bytes: Option<&'static [u8]>,
}

impl IntoResponse for Asset {
    fn into_response(self) -> Response {
        let mut response = match self.bytes {
            Some(bytes) => (self.status, bytes).into_response(),
            None => (self.status, self.body).into_response(),
        };
        for (name, value) in self.headers {
            response
                .headers_mut()
                .insert(name, axum::http::HeaderValue::from_static(value));
        }
        response
    }
}

/// Gemini's Live API is the one site the page talks to besides tamale (see `gemini`), and
/// videos play from the `blob:` URLs the page makes of what it's sent. The page is a
/// WebAssembly module (tamale's `web/`), which `wasm-unsafe-eval` lets the browser compile.
///
/// pullspace, the code view, is drawn in the page by the same module, and needs two things
/// tamale's own code doesn't: `unsafe-eval`, since it runs small scripts through Dioxus'
/// `document::eval` (a `Function` made of text), and inline styles, for its stylesheet and
/// the sizes of its panes. Inline script stays shut, so a summary's link bridge in one of its
/// `srcdoc` frames (which inherit this policy) doesn't run. Before, pullspace had a frame of
/// this origin to itself, under its own looser policy (`pullspace_embed::CSP`).
const PAGE_CSP: &str = "default-src 'self'; connect-src 'self' \
     https://generativelanguage.googleapis.com wss://generativelanguage.googleapis.com; \
     img-src 'self' data: blob:; media-src blob:; font-src 'self' data:; \
     style-src 'self' 'unsafe-inline'; \
     script-src 'self' 'unsafe-eval' 'wasm-unsafe-eval'; object-src 'none'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

/// An artifact in a tab of its own: the viewer's own script (`unsafe-eval` is for Dioxus's
/// `eval`, as on the page) and inline style, drawing a document that's only data. Its
/// images may come from https sites (see `openui` `Image`).
const ARTIFACT_CSP: &str = "default-src 'self'; img-src 'self' data: https:; \
     style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-eval' 'wasm-unsafe-eval'; \
     connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; \
     frame-ancestors 'self'";

/// The page at `path` (`/`, `/app.js`, `/app_bg.wasm`, `/app.css` or `/artifact/<id>`), without checking
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
            bytes: None,
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
        "/app.css" => ok("text/css; charset=utf-8", &SERVED_STYLE, &[]),
        "/app.js" => ok("text/javascript; charset=utf-8", SCRIPT, &[]),
        "/openui.js" => ok("text/javascript; charset=utf-8", VIEWER_SCRIPT, &[]),
        "/openui_bg.wasm" => Asset {
            status: StatusCode::OK,
            headers: vec![
                (header::CONTENT_TYPE, "application/wasm"),
                (header::CONTENT_ENCODING, "gzip"),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            body: String::new(),
            bytes: Some(VIEWER_MODULE),
        },
        // The app's module, gzipped. Its URL carries a version (see `build.rs`), so it
        // can be cached for good.
        "/app_bg.wasm" => Asset {
            status: StatusCode::OK,
            headers: vec![
                (header::CONTENT_TYPE, "application/wasm"),
                (header::CONTENT_ENCODING, "gzip"),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            body: String::new(),
            bytes: Some(MODULE),
        },
        _ => {
            let source = path
                .strip_prefix("/artifact/")
                .and_then(|id| shared.artifacts.lock().unwrap().sources.get(id).cloned());
            match source {
                Some(source) => ok(
                    "text/html; charset=utf-8",
                    &artifacts::viewer_page(&source),
                    &[
                        (header::REFERRER_POLICY, "no-referrer"),
                        (header::CONTENT_SECURITY_POLICY, ARTIFACT_CSP),
                    ],
                ),
                None => Asset {
                    status: StatusCode::NOT_FOUND,
                    headers: vec![(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    body: "No such artifact; it may be from an earlier run.".into(),
                    bytes: None,
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
        let port = listener.local_addr()?.port();
        let host = format!("{ip}:{port}");
        // On the network, this machine's browser gets a loopback address as well (see
        // `local_url`): the same port if it's free, so the two differ only in the IP.
        let loopback = if lan {
            let ip = IpAddr::from([127, 0, 0, 1]);
            match tokio::net::TcpListener::bind((ip, port)).await {
                Ok(listener) => Some(listener),
                Err(_) => tokio::net::TcpListener::bind((ip, 0)).await.ok(),
            }
        } else {
            None
        };
        let loopback_host = loopback
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
            .map(|addr| format!("127.0.0.1:{}", addr.port()));
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).context("couldn't generate an access token")?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let url = format!("http://{host}/?t={token}");
        let local_url = match &loopback_host {
            Some(local) => format!("http://{local}/?t={token}"),
            None => url.clone(),
        };
        let hosts = std::iter::once(host).chain(loopback_host).collect();
        let (tx, events) = mpsc::unbounded_channel();
        let mirror = Mirror::default();
        let shared = Arc::new(Shared {
            token,
            hosts,
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
            .route("/app_bg.wasm", get(module))
            .route("/openui.js", get(viewer_script))
            .route("/openui_bg.wasm", get(viewer_module))
            .route("/artifact/{id}", get(artifact))
            .with_state(shared.clone());
        let task = tokio::spawn(async move {
            match loopback {
                Some(loopback) => {
                    let _ = tokio::join!(
                        axum::serve(listener, router.clone()),
                        axum::serve(loopback, router),
                    );
                }
                None => {
                    let _ = axum::serve(listener, router).await;
                }
            }
        });
        Ok(Self {
            url,
            local_url,
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

    /// The session is over: tells the hub, if connected to one, so it stops listing it.
    pub async fn end(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.end().await;
        }
    }

    /// The link that opens this session through the hub, while connected to one.
    pub fn hub_url(&self) -> Option<&str> {
        self.relay.as_ref().map(|r| r.open_url.as_str())
    }

    /// The link for `c` on the terminal's screen: the hub's while connected to one,
    /// since it works from any device and the local one doesn't.
    pub fn share_url(&self) -> &str {
        self.hub_url().unwrap_or(&self.url)
    }

    /// The link for `o`, which opens a browser on this machine: as [`Self::share_url`],
    /// but at the loopback address rather than the network's (see `local_url`).
    pub fn open_url(&self) -> &str {
        self.hub_url().unwrap_or(&self.local_url)
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

    /// Sends a newly connected tab the session, or as much of its end as it asked for (call
    /// [`Self::sync`] first so it is current); from then on it gets updates.
    pub fn welcome(&mut self, id: u64) {
        let mut clients = self.shared.clients.lock().unwrap();
        let Some(client) = clients.get_mut(&id) else {
            return;
        };
        for message in self.mirror.full(client.tail) {
            let _ = client.tx.send(Out::Text(message));
        }
        client.welcomed = true;
    }

    /// What answers a tab later, after the event that asked has been handled.
    pub fn replier(&self) -> Replier {
        Replier(self.shared.clone())
    }

    /// Sends a tab the page of entries before `before`. This runs between syncs, so it can't
    /// overtake an update that changes those entries.
    pub fn older(&self, id: u64, before: usize) {
        self.shared.send_to(id, self.mirror.older(before));
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

/// The artifact viewer's script (see [`artifacts::viewer_page`]).
async fn viewer_script(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/openui.js").into_response()
}

async fn viewer_module(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/openui_bg.wasm").into_response()
}

async fn script(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/app.js").into_response()
}

async fn module(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !shared.own_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    asset(&shared, "/app_bg.wasm").into_response()
}

/// An artifact in a page of its own (see [`artifacts::viewer_page`]).
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
    let own_origin = shared.host(&headers).map(|host| format!("http://{host}"));
    if !shared.authorized(&uri, &headers) || origin.is_none() || origin != own_origin.as_deref() {
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
        // Pinged, so a tab on a phone that's gone to sleep is noticed (see below).
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
        ping.tick().await;
        loop {
            let message = tokio::select! {
                out = rx.recv() => match out {
                    Some(Out::Text(text)) => Message::Text(text.into()),
                    Some(Out::Close(reason)) => Message::Close(Some(CloseFrame {
                        code: 4000,
                        reason: reason.into(),
                    })),
                    None => break,
                },
                _ = ping.tick() => Message::Ping(Vec::new().into()),
            };
            if sink.send(message).await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });
    let id = shared.add_client(tx, false, None);
    // Browsers answer pings, so a tab silent this long is gone, and stops being sent updates.
    let silent_for = std::time::Duration::from_secs(75);
    while let Ok(Some(Ok(message))) = tokio::time::timeout(silent_for, stream.next()).await {
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
        // The page checking its connection is alive, as after a phone wakes: a browser
        // can't send a WebSocket ping itself. Through a hub this comes back sealed, so
        // it shows the whole way to tamale works, not just the hop to the hub.
        "ping" => shared.send_to(id, json!({ "t": "pong" }).to_string()),
        "terminal" => {
            if let Some(client) = shared.clients.lock().unwrap().get_mut(&id) {
                client.terminal = true;
            }
        }
        "send" => {
            let terminal = shared
                .clients
                .lock()
                .unwrap()
                .get(&id)
                .is_some_and(|c| c.terminal);
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
        "close" => {
            let _ = shared.events.send(WebEvent::Close);
        }
        "models" => {
            let _ = shared.events.send(WebEvent::Models { tab: id, seq });
        }
        // A chef's side question. Only clients that are another tamale ask (a page has no
        // use for it), and the answer goes back to just the one that asked.
        "ask" => {
            let question = str_field("question");
            let ask_id = str_field("id");
            if !question.trim().is_empty() && !ask_id.is_empty() {
                let _ = shared.events.send(WebEvent::Ask {
                    tab: id,
                    id: ask_id,
                    question,
                });
            }
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
        "answer" => {
            let answers = request.get("answers").cloned().unwrap_or_default();
            if let Ok(answers) = serde_json::from_value(answers) {
                let _ = shared.events.send(WebEvent::Answer(answers));
            }
        }
        "diff" => reply(Box::new(diff::diff)),
        "diff_stat" => reply(Box::new(diff::diff_stat)),
        // The review panel's two questions (see `pullspace`).
        "ps_snapshot" => reply(Box::new(pullspace::snapshot)),
        "ps_blobs" => {
            let wants: Vec<pullspace_embed::protocol::Want> = request
                .get("wants")
                .cloned()
                .and_then(|wants| serde_json::from_value(wants).ok())
                .unwrap_or_default();
            reply(Box::new(move |roots| pullspace::blobs(roots, &wants)));
        }
        "older" => {
            let before = request.get("before").and_then(Value::as_u64).unwrap_or(0);
            let _ = shared.events.send(WebEvent::Older {
                id,
                before: before as usize,
            });
        }
        // An artifact's document, for the gallery, saving and talking about it.
        "artifact" => {
            let artifact = str_field("id");
            let (source, hash) = {
                let artifacts = shared.artifacts.lock().unwrap();
                (
                    artifacts.sources.get(&artifact).cloned(),
                    artifacts.hashes.get(&artifact).cloned(),
                )
            };
            // The web UI asks for the document alone; the iOS app also wants a page for it.
            let page = request.get("page").and_then(Value::as_bool).unwrap_or(true);
            let reply = artifact_reply(&artifact, source.as_deref(), hash.as_deref(), page);
            shared.send_to(id, reply.to_string());
        }
        // The voice assistant's key and choices (see `gemini`).
        "gemini" => reply(Box::new(|_| {
            let settings = gemini::settings_path()
                .map(|path| gemini::load(&path))
                .unwrap_or_default();
            gemini::reply(&settings, gemini::env_key())
        })),
        "gemini_save" => {
            let request = request.clone();
            reply(Box::new(move |_| {
                let Some(path) = gemini::settings_path() else {
                    return json!({ "t": "gemini", "error": "There's no home directory to save in." });
                };
                let mut settings = gemini::load(&path);
                gemini::update(&mut settings, &request);
                match gemini::save(&path, &settings) {
                    Ok(()) => gemini::reply(&settings, gemini::env_key()),
                    Err(e) => {
                        json!({ "t": "gemini", "error": format!("Couldn't save {}: {e}", path.display()) })
                    }
                }
            }));
        }
        // A video a reply shows (a ```video block): from the workspace, or the temporary
        // directory a scratch render went to.
        "video" => {
            let path = str_field("path");
            reply(Box::new(move |roots| {
                let mut allowed = roots.to_vec();
                // `mktemp -d` uses $TMPDIR (on macOS, not /tmp); agents also use /tmp itself.
                allowed.push(std::env::temp_dir());
                #[cfg(unix)]
                allowed.push(PathBuf::from("/tmp"));
                gemini::video(&allowed, &path)
            }));
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
