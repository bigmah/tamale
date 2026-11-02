//! `tamale-hub serve`: the relay.
//!
//! Requests are told apart by `Host`: the public host (`tamale-hub.com`) is the hub's
//! own pages and the agent socket; `<sid>.tamale-hub.com` is one session, its own
//! origin, whose page, artifacts and socket are relayed to that session's tamale.
//! [`rewrite`] maps each request onto an internal path (`/d/…` or `/s/<sid>/…`) before
//! routing.
//!
//! Credentials: the dashboard keeps the browser's view tokens in an `HttpOnly` cookie
//! that only the public host ever sees. Opening a session trades it for a one-time
//! ticket, which the session host trades for a cookie of its own, signed and scoped to
//! that one session (see [`SessionCookie`]). A session's page, even a hostile one, so
//! never holds anything that reaches another session.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, ServiceExt};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tamale_hub_protocol::{self as protocol, Label, ToAgent, ToHub, close};
use tokio::sync::{mpsc, oneshot};
use tower::Layer;

use crate::state::{self, State as Tokens, Store};

const PAGE: &str = include_str!("../static/index.html");
const SCRIPT: &str = include_str!("../static/hub.js");
const STYLE: &str = include_str!("../static/hub.css");

/// How long a disconnected session stays listed (and its page keeps retrying), and how
/// long after a restart unknown sessions are assumed to be on their way back.
const GRACE: Duration = Duration::from_secs(600);
const TICKET_LIFE: Duration = Duration::from_secs(60);
const SESSION_COOKIE_LIFE: u64 = 30 * 24 * 3600;
const VIEW_COOKIE_LIFE: u64 = 34_560_000;
const MAX_CLIENTS: usize = 16;
const MAX_SESSIONS_PER_PAIR: usize = 64;
const GET_TIMEOUT: Duration = Duration::from_secs(30);
/// Auth failures an IP may have per window before it gets 429s.
const MAX_FAILURES: u32 = 30;
const FAILURE_WINDOW: Duration = Duration::from_secs(600);

pub struct Options {
    pub listen: SocketAddr,
    pub public_url: String,
    pub insecure: bool,
}

/// Where the hub is reached, from `--public-url`.
#[derive(Clone, Debug)]
pub struct Public {
    /// `https` or `http`.
    pub scheme: String,
    /// With the port, if any: `tamale-hub.com`, `hub.localhost:8787`.
    pub host: String,
    pub secure: bool,
}

impl Public {
    pub fn parse(url: &str, insecure: bool) -> Result<Self> {
        let url = url.trim_end_matches('/');
        let (scheme, host) = url
            .split_once("://")
            .context("--public-url must look like https://tamale-hub.com")?;
        if host.is_empty() || host.contains('/') {
            bail!("--public-url must be just a scheme and host, like https://tamale-hub.com");
        }
        match scheme {
            "https" => {}
            "http" if insecure => {}
            "http" => {
                bail!("--public-url is http://; pass --insecure to allow that (testing only)")
            }
            _ => bail!("--public-url must start with https://"),
        }
        Ok(Self {
            scheme: scheme.into(),
            host: host.to_ascii_lowercase(),
            secure: scheme == "https",
        })
    }

    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.host)
    }

    pub fn session_origin(&self, sid: &str) -> String {
        format!("{}://{sid}.{}", self.scheme, self.host)
    }

    fn cookie_name(&self, name: &str) -> String {
        // `__Host-` cookies can't be set for a parent domain, so a session's page can't
        // overwrite the dashboard's or another session's. They require `Secure`.
        if self.secure {
            format!("__Host-{name}")
        } else {
            name.into()
        }
    }

    fn set_cookie(&self, name: &str, value: &str, max_age: u64) -> String {
        let secure = if self.secure { "; Secure" } else { "" };
        format!(
            "{}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{secure}",
            self.cookie_name(name)
        )
    }
}

/// A browser tab on a session.
struct Client {
    tx: mpsc::UnboundedSender<Out>,
    /// The view token hash its cookie was made from; a rotated view half drops it.
    view_hash: String,
}

enum Out {
    Text(String),
    Close(u16, String),
}

struct Session {
    pair: String,
    agent_hash: String,
    label: Label,
    busy: bool,
    approval: bool,
    title: String,
    /// Unix seconds.
    last_activity: u64,
    /// The connected agent, `None` while offline.
    agent: Option<mpsc::UnboundedSender<AgentOut>>,
    /// Which agent connection is current, so a replaced one doesn't clean up after the
    /// new one.
    generation: u64,
    offline_since: Option<Instant>,
    clients: HashMap<u64, Client>,
    pending: HashMap<u64, oneshot::Sender<Got>>,
}

enum AgentOut {
    Frame(ToAgent),
    Close(u16, String),
}

struct Got {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

struct Ticket {
    sid: String,
    view_hash: String,
    expires: Instant,
}

pub struct Hub {
    public: Public,
    store: Store,
    tokens: RwLock<Tokens>,
    sessions: Mutex<HashMap<String, Session>>,
    /// Keyed by the ticket's hash.
    tickets: Mutex<HashMap<String, Ticket>>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    started: Instant,
    next: AtomicU64,
}

impl Hub {
    pub fn new(public: Public, store: Store) -> Result<Arc<Self>> {
        // Makes the cookie secret if there isn't one yet.
        let tokens = store.edit(|s| Ok(s.clone()))?;
        Ok(Arc::new(Self {
            public,
            store,
            tokens: RwLock::new(tokens),
            sessions: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            started: Instant::now(),
            next: AtomicU64::new(1),
        }))
    }

    fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn secret(&self) -> Vec<u8> {
        self.tokens.read().unwrap().secret.as_bytes().to_vec()
    }

    /// Re-reads the token pairs and drops whatever a revoke or rotate took away.
    pub fn reload(&self) -> Result<()> {
        let tokens = self.store.load()?;
        let pairs: HashMap<&str, &state::Pair> =
            tokens.pairs.iter().map(|p| (p.name.as_str(), p)).collect();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|sid, session| {
            let Some(pair) = pairs.get(session.pair.as_str()) else {
                eprintln!(
                    "session {} dropped: pair {} revoked",
                    short(sid),
                    session.pair
                );
                end_session(session, "This session's token pair was revoked.");
                return false;
            };
            if pair.agent_hash != session.agent_hash {
                eprintln!(
                    "session {} dropped: agent token of {} rotated",
                    short(sid),
                    session.pair
                );
                end_session(session, "This session's agent token was rotated.");
                return false;
            }
            session.clients.retain(|_, client| {
                let keep = client.view_hash == pair.view_hash;
                if !keep {
                    let _ = client.tx.send(Out::Close(
                        4000,
                        "Your view token was revoked or rotated.".into(),
                    ));
                }
                keep
            });
            true
        });
        drop(sessions);
        *self.tokens.write().unwrap() = tokens;
        Ok(())
    }

    /// Notes an auth failure from `ip`.
    fn fail(&self, ip: IpAddr) {
        let mut failures = self.failures.lock().unwrap();
        let entry = failures.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() > FAILURE_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    fn limited(&self, ip: IpAddr) -> bool {
        self.failures
            .lock()
            .unwrap()
            .get(&ip)
            .is_some_and(|(n, since)| *n >= MAX_FAILURES && since.elapsed() <= FAILURE_WINDOW)
    }

    /// The view token hashes of the pairs the browser's dashboard cookie holds tokens for,
    /// and the tokens still valid.
    fn views(&self, headers: &HeaderMap) -> (Vec<String>, Vec<state::Pair>) {
        let tokens = self.tokens.read().unwrap();
        let mut valid = Vec::new();
        let mut pairs = Vec::new();
        for token in cookie(headers, &self.public.cookie_name("th"))
            .unwrap_or_default()
            .split(',')
            .filter(|t| !t.is_empty())
        {
            if let Some(pair) = tokens.by_view(token)
                && !pairs.iter().any(|p: &state::Pair| p.name == pair.name)
            {
                valid.push(token.to_string());
                pairs.push(pair.clone());
            }
        }
        (valid, pairs)
    }

    /// Records that a pair was used, at most once an hour (it rewrites state.json).
    fn touch(&self, name: &str) {
        let stale = self.tokens.read().unwrap().by_name(name).is_some_and(|p| {
            p.last_used
                .is_none_or(|t| state::now().saturating_sub(t) > 3600)
        });
        if stale {
            let name = name.to_string();
            let _ = self.store.edit(|s| {
                if let Some(p) = s.pairs.iter_mut().find(|p| p.name == name) {
                    p.last_used = Some(state::now());
                }
                Ok(())
            });
            let _ = self.reload();
        }
    }

    /// Drops sessions offline past the grace period, expired tickets and old failures.
    fn sweep(&self) {
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, s| s.offline_since.is_none_or(|t| t.elapsed() < GRACE));
        self.tickets
            .lock()
            .unwrap()
            .retain(|_, t| t.expires > Instant::now());
        self.failures
            .lock()
            .unwrap()
            .retain(|_, (_, since)| since.elapsed() <= FAILURE_WINDOW);
    }
}

fn end_session(session: &mut Session, reason: &str) {
    if let Some(agent) = session.agent.take() {
        let _ = agent.send(AgentOut::Close(close::BAD_TOKEN, reason.into()));
    }
    for (_, client) in session.clients.drain() {
        let _ = client.tx.send(Out::Close(4000, reason.into()));
    }
}

fn short(sid: &str) -> &str {
    &sid[..sid.len().min(8)]
}

/// A session host's cookie: `<sid>.<expiry>.<view hash prefix>.<mac>`, signed with the
/// hub's secret, so it needs no server-side state and survives restarts. A rotated or
/// revoked view half invalidates it.
struct SessionCookie;

impl SessionCookie {
    fn make(secret: &[u8], sid: &str, view_hash: &str) -> String {
        let expires = state::now() + SESSION_COOKIE_LIFE;
        let body = format!("{sid}.{expires}.{}", &view_hash[..16]);
        let mac = state::hmac(secret, format!("ts|{body}").as_bytes());
        format!("{body}.{mac}")
    }

    /// The view hash prefix a valid cookie for `sid` was made from.
    fn check<'a>(secret: &[u8], sid: &str, value: &'a str) -> Option<&'a str> {
        let (body, mac) = value.rsplit_once('.')?;
        let mut parts = body.split('.');
        let (cookie_sid, expires, view) = (parts.next()?, parts.next()?, parts.next()?);
        let good = state::ct_eq(mac, &state::hmac(secret, format!("ts|{body}").as_bytes()));
        (good && cookie_sid == sid && expires.parse::<u64>().ok()? > state::now()).then_some(view)
    }
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// The client's address: the TCP peer, or behind a local reverse proxy, the address it
/// says it forwarded for.
fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if peer.ip().is_loopback()
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse().ok())
    {
        return ip;
    }
    peer.ip()
}

fn origin_is(headers: &HeaderMap, expected: &str) -> bool {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(expected)
}

/// Which part of the hub a request is for, from its `Host`.
fn rewrite(public: &Public, mut request: Request) -> Request {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| request.uri().authority().map(|a| a.as_str()))
        .unwrap_or("")
        .to_ascii_lowercase();
    let path = request.uri().path().to_string();
    let target = if host == public.host {
        format!("/d{path}")
    } else if let Some(sub) = host.strip_suffix(&format!(".{}", public.host)) {
        if protocol::valid_sid(sub) {
            format!("/s/{sub}{path}")
        } else if sub == "www" {
            "/x/www".into()
        } else {
            "/x/none".into()
        }
    } else {
        "/x/none".into()
    };
    let target = match request.uri().query() {
        Some(q) => format!("{target}?{q}"),
        None => target,
    };
    if let Ok(uri) = target.parse::<Uri>() {
        *request.uri_mut() = uri;
    }
    request
}

pub async fn serve(options: Options, store: Store) -> Result<()> {
    let public = Public::parse(&options.public_url, options.insecure)?;
    let hub = Hub::new(public.clone(), store)?;
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .with_context(|| format!("couldn't listen on {}", options.listen))?;
    eprintln!(
        "tamale-hub listening on {}, public at {}",
        listener.local_addr()?,
        public.origin()
    );
    run(hub, listener).await
}

/// Serves on `listener` until the process ends; also reloads on change or SIGHUP.
pub async fn run(hub: Arc<Hub>, listener: tokio::net::TcpListener) -> Result<()> {
    let watcher = hub.clone();
    tokio::spawn(async move {
        let mut seen = watcher.store.mtime();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let mut sweep = 0u32;
        loop {
            tick.tick().await;
            let mtime = watcher.store.mtime();
            if mtime != seen {
                seen = mtime;
                if let Err(e) = watcher.reload() {
                    eprintln!("reloading state.json: {e:#}");
                }
            }
            sweep += 1;
            if sweep.is_multiple_of(15) {
                watcher.sweep();
            }
        }
    });
    #[cfg(unix)]
    {
        let reloader = hub.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let Ok(mut hup) = signal(SignalKind::hangup()) else {
                return;
            };
            while hup.recv().await.is_some() {
                eprintln!("SIGHUP: reloading state.json");
                if let Err(e) = reloader.reload() {
                    eprintln!("reloading state.json: {e:#}");
                }
            }
        });
    }
    let router = Router::new()
        .route("/d/", get(dashboard))
        .route("/d/open/{sid}", get(dashboard))
        .route(
            "/d/hub.js",
            get(|| async { asset("text/javascript; charset=utf-8", SCRIPT) }),
        )
        .route(
            "/d/hub.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE) }),
        )
        .route("/d/api/tokens", post(api_tokens))
        .route("/d/api/sessions", get(api_sessions))
        .route("/d/api/ticket", post(api_ticket))
        .route("/d/agent", get(agent))
        .route("/s/{sid}/__hub/auth", get(auth))
        .route("/s/{sid}/ws", get(session_socket))
        .route("/s/{sid}/", get(session_root))
        .route("/s/{sid}/{*path}", get(session_get))
        .route("/x/www", get(www))
        .fallback(|| async { (StatusCode::NOT_FOUND, "Not found.") })
        .with_state(hub.clone());
    let public = hub.public.clone();
    let app = tower::util::MapRequestLayer::new(move |r| rewrite(&public, r)).layer(router);
    axum::serve(
        listener,
        ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(app),
    )
    .await?;
    Ok(())
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

/// Headers every dashboard page and API response carries.
fn guard(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn dashboard() -> Response {
    guard(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE).into_response())
}

async fn www(State(hub): State<Arc<Hub>>) -> Response {
    (
        StatusCode::PERMANENT_REDIRECT,
        [(header::LOCATION, format!("{}/", hub.public.origin()))],
    )
        .into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    guard((status, Json(json!({ "error": message }))).into_response())
}

#[derive(Deserialize)]
struct TokensRequest {
    add: Option<String>,
    #[serde(default)]
    forget: bool,
}

/// Adds a view token to the browser's cookie (or forgets them all).
async fn api_tokens(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<TokensRequest>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if !origin_is(&headers, &hub.public.origin()) {
        return error(StatusCode::FORBIDDEN, "Wrong origin.");
    }
    if hub.limited(ip) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts; try again later.",
        );
    }
    if request.forget {
        let mut response = guard(Json(json!({ "tokens": 0 })).into_response());
        if let Ok(v) = hub.public.set_cookie("th", "", 0).parse() {
            response.headers_mut().insert(header::SET_COOKIE, v);
        }
        return response;
    }
    let (mut tokens, _) = hub.views(&headers);
    if let Some(add) = request.add.map(|t| t.trim().to_string()) {
        let pair = hub
            .tokens
            .read()
            .unwrap()
            .by_view(&add)
            .map(|p| p.name.clone());
        match pair {
            Some(name) => {
                eprintln!("view token of {name} added from {ip}");
                if !tokens.contains(&add) {
                    tokens.push(add);
                }
            }
            None => {
                hub.fail(ip);
                let hint = if add.starts_with("tha_") {
                    "That's an agent token (tha_…); the browser needs the view token (thv_…)."
                } else {
                    "That isn't a current view token."
                };
                return error(StatusCode::UNAUTHORIZED, hint);
            }
        }
    }
    let mut response = guard(Json(json!({ "tokens": tokens.len() })).into_response());
    if let Ok(v) = hub
        .public
        .set_cookie("th", &tokens.join(","), VIEW_COOKIE_LIFE)
        .parse()
    {
        response.headers_mut().insert(header::SET_COOKIE, v);
    }
    response
}

/// The sessions under the browser's view tokens.
async fn api_sessions(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    let (_, pairs) = hub.views(&headers);
    let names: HashSet<&str> = pairs.iter().map(|p| p.name.as_str()).collect();
    let now = state::now();
    let mut list: Vec<Value> = hub
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, s)| names.contains(s.pair.as_str()))
        .map(|(sid, s)| {
            json!({
                "sid": sid,
                "open": format!("/open/{sid}"),
                "pair": s.pair,
                "host": s.label.host,
                "cwd": s.label.cwd,
                "model": s.label.model,
                "title": s.title,
                "busy": s.busy,
                "approval": s.approval,
                "online": s.agent.is_some(),
                "viewers": s.clients.len(),
                "idle": now.saturating_sub(s.last_activity),
            })
        })
        .collect();
    // Waiting for approval first, then busy, then most recently active.
    list.sort_by_key(|s| {
        (
            !s["approval"].as_bool().unwrap_or(false),
            !s["busy"].as_bool().unwrap_or(false),
            !s["online"].as_bool().unwrap_or(false),
            s["idle"].as_u64().unwrap_or(0),
        )
    });
    guard(
        Json(json!({
            "tokens": pairs.len(),
            "pairs": pairs.iter().map(|p| &p.name).collect::<Vec<_>>(),
            "sessions": list,
        }))
        .into_response(),
    )
}

#[derive(Deserialize)]
struct TicketRequest {
    sid: String,
}

/// A one-time ticket that lets the browser into one session's host.
async fn api_ticket(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<TicketRequest>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if !origin_is(&headers, &hub.public.origin()) {
        return error(StatusCode::FORBIDDEN, "Wrong origin.");
    }
    if hub.limited(ip) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts; try again later.",
        );
    }
    let (_, pairs) = hub.views(&headers);
    if pairs.is_empty() {
        return error(
            StatusCode::UNAUTHORIZED,
            "This browser has no view token. Open your view link (or paste the token) first.",
        );
    }
    let pair = hub
        .sessions
        .lock()
        .unwrap()
        .get(&request.sid)
        .and_then(|s| pairs.iter().find(|p| p.name == s.pair).cloned());
    let Some(pair) = pair else {
        return error(
            StatusCode::NOT_FOUND,
            "No such session under your tokens. It may have ended; the list has the live ones.",
        );
    };
    let Ok(bytes) = state::random::<32>() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "No randomness.");
    };
    let ticket = state::hex(&bytes);
    hub.tickets.lock().unwrap().insert(
        state::hash(&ticket),
        Ticket {
            sid: request.sid.clone(),
            view_hash: pair.view_hash.clone(),
            expires: Instant::now() + TICKET_LIFE,
        },
    );
    hub.touch(&pair.name);
    guard(
        Json(json!({
            "url": format!("{}/__hub/auth?ticket={ticket}", hub.public.session_origin(&request.sid)),
        }))
        .into_response(),
    )
}

#[derive(Deserialize)]
struct AuthQuery {
    ticket: String,
}

/// Trades a ticket for this session host's cookie.
async fn auth(
    State(hub): State<Arc<Hub>>,
    Path(sid): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if hub.limited(ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts; try again later.",
        )
            .into_response();
    }
    let ticket = hub
        .tickets
        .lock()
        .unwrap()
        .remove(&state::hash(&query.ticket));
    let Some(ticket) = ticket.filter(|t| t.sid == sid && t.expires > Instant::now()) else {
        hub.fail(ip);
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            format!(
                "<!doctype html><meta name=viewport content='width=device-width'><p>That link has \
expired. <a href='{}/open/{sid}'>Open the session again</a>.",
                hub.public.origin()
            ),
        )
            .into_response();
    };
    let value = SessionCookie::make(&hub.secret(), &sid, &ticket.view_hash);
    let mut response = (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, "/".to_string()),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
    )
        .into_response();
    if let Ok(v) = hub
        .public
        .set_cookie("ts", &value, SESSION_COOKIE_LIFE)
        .parse()
    {
        response.headers_mut().insert(header::SET_COOKIE, v);
    }
    response
}

/// The view hash of the current pair a session host's cookie is good for, if it is.
fn session_view(hub: &Hub, sid: &str, headers: &HeaderMap) -> Option<String> {
    let value = cookie(headers, &hub.public.cookie_name("ts"))?;
    let prefix = SessionCookie::check(&hub.secret(), sid, value)?;
    let tokens = hub.tokens.read().unwrap();
    let pair = tokens
        .pairs
        .iter()
        .find(|p| p.view_hash.starts_with(prefix))?;
    // A session the hub knows must belong to that pair.
    let sessions = hub.sessions.lock().unwrap();
    if sessions.get(sid).is_some_and(|s| s.pair != pair.name) {
        return None;
    }
    Some(pair.view_hash.clone())
}

async fn session_root(
    State(hub): State<Arc<Hub>>,
    Path(sid): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    tunnel(hub, sid, "/".into(), client_ip(peer, &headers), headers).await
}

async fn session_get(
    State(hub): State<Arc<Hub>>,
    Path((sid, path)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    tunnel(
        hub,
        sid,
        format!("/{path}"),
        client_ip(peer, &headers),
        headers,
    )
    .await
}

/// Relays a page GET to the session's tamale.
async fn tunnel(
    hub: Arc<Hub>,
    sid: String,
    path: String,
    ip: IpAddr,
    headers: HeaderMap,
) -> Response {
    if !protocol::tunnelled_path(&path) {
        return (StatusCode::NOT_FOUND, "Not found.").into_response();
    }
    if session_view(&hub, &sid, &headers).is_none() {
        if path == "/" {
            // A bookmark, a link from another app or a hub restart: get a ticket again.
            return (
                StatusCode::SEE_OTHER,
                [(
                    header::LOCATION,
                    format!("{}/open/{sid}", hub.public.origin()),
                )],
            )
                .into_response();
        }
        hub.fail(ip);
        return (StatusCode::NOT_FOUND, "Not found.").into_response();
    }
    let (tx, rx) = oneshot::channel();
    let r = hub.next();
    {
        let mut sessions = hub.sessions.lock().unwrap();
        let Some(agent) = sessions.get_mut(&sid).and_then(|s| {
            let agent = s.agent.clone()?;
            s.pending.insert(r, tx);
            Some(agent)
        }) else {
            drop(sessions);
            return offline_page();
        };
        let _ = agent.send(AgentOut::Frame(ToAgent::Get { r, path }));
    }
    let got = match tokio::time::timeout(GET_TIMEOUT, rx).await {
        Ok(Ok(got)) => got,
        _ => {
            if let Some(s) = hub.sessions.lock().unwrap().get_mut(&sid) {
                s.pending.remove(&r);
            }
            return offline_page();
        }
    };
    let mut response = (
        StatusCode::from_u16(got.status).unwrap_or(StatusCode::BAD_GATEWAY),
        got.body,
    )
        .into_response();
    let out = response.headers_mut();
    for (name, value) in got.headers {
        let name = name.to_ascii_lowercase();
        // Never `set-cookie`: a session mustn't touch cookies.
        if matches!(
            name.as_str(),
            "content-type"
                | "cache-control"
                | "content-security-policy"
                | "referrer-policy"
                | "x-frame-options"
        ) && let (Ok(n), Ok(v)) = (name.parse::<header::HeaderName>(), value.parse())
        {
            out.insert(n, v);
        }
    }
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn offline_page() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFRESH, "5"),
        ],
        "<!doctype html><meta name=viewport content='width=device-width'>\
<body style='font:16px system-ui;background:#1e2026;color:#ccc;padding:2em'>\
<p>This session's tamale is offline. Retrying every few seconds…",
    )
        .into_response()
}

/// A browser tab's socket for one session.
async fn session_socket(
    State(hub): State<Arc<Hub>>,
    Path(sid): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let ip = client_ip(peer, &headers);
    // Sibling session hosts are the same site, so the cookie alone isn't enough.
    if !origin_is(&headers, &hub.public.session_origin(&sid)) {
        hub.fail(ip);
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(view_hash) = session_view(&hub, &sid, &headers) else {
        hub.fail(ip);
        return StatusCode::NOT_FOUND.into_response();
    };
    let ended = {
        let sessions = hub.sessions.lock().unwrap();
        match sessions.get(&sid) {
            Some(s) if s.agent.is_some() => {
                if s.clients.len() >= MAX_CLIENTS {
                    return StatusCode::TOO_MANY_REQUESTS.into_response();
                }
                false
            }
            // Offline: the page keeps retrying until its tamale is back.
            Some(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            // Unknown just after a restart: its tamale is probably reconnecting.
            None if hub.started.elapsed() < GRACE => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            None => true,
        }
    };
    let upgrade = upgrade.max_message_size(protocol::MAX_BROWSER_MESSAGE);
    if ended {
        return upgrade.on_upgrade(|mut ws| async move {
            let _ = ws
                .send(Message::Close(Some(CloseFrame {
                    code: 4000,
                    reason: "This session ended. Open the hub for the list.".into(),
                })))
                .await;
        });
    }
    upgrade.on_upgrade(move |ws| browser(hub, sid, view_hash, ws))
}

async fn browser(hub: Arc<Hub>, sid: String, view_hash: String, ws: WebSocket) {
    let c = hub.next();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let agent = {
        let mut sessions = hub.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(&sid) else {
            return;
        };
        let Some(agent) = session.agent.clone() else {
            return;
        };
        session.clients.insert(c, Client { tx, view_hash });
        session.last_activity = state::now();
        agent
    };
    let _ = agent.send(AgentOut::Frame(ToAgent::Open { c }));
    let (mut sink, mut stream) = ws.split();
    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let message = match out {
                Out::Text(text) => Message::Text(text.into()),
                Out::Close(code, reason) => {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code,
                            reason: reason.into(),
                        })))
                        .await;
                    return;
                }
            };
            if sink.send(message).await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });
    while let Some(Ok(message)) = stream.next().await {
        match message {
            Message::Text(text) => {
                let _ = agent.send(AgentOut::Frame(ToAgent::Msg {
                    c,
                    data: text.to_string(),
                }));
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let removed = hub
        .sessions
        .lock()
        .unwrap()
        .get_mut(&sid)
        .and_then(|s| s.clients.remove(&c))
        .is_some();
    if removed {
        let _ = agent.send(AgentOut::Frame(ToAgent::Close { c }));
    }
    writer.abort();
}

/// tamale's socket.
async fn agent(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let ip = client_ip(peer, &headers);
    if hub.limited(ip) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let Some(pair) = hub.tokens.read().unwrap().by_agent(token).cloned() else {
        hub.fail(ip);
        eprintln!("agent rejected from {ip}: unknown token");
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .max_message_size(protocol::MAX_AGENT_FRAME)
        .max_frame_size(protocol::MAX_AGENT_FRAME)
        .on_upgrade(move |ws| agent_socket(hub, pair, ip, ws))
}

async fn agent_socket(hub: Arc<Hub>, pair: state::Pair, ip: IpAddr, ws: WebSocket) {
    let (mut sink, mut stream) = ws.split();
    let reject = |code, reason: &str| {
        Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_string().into(),
        }))
    };
    let hello = match tokio::time::timeout(Duration::from_secs(15), stream.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<ToHub>(&text).ok(),
        _ => None,
    };
    let Some(ToHub::Hello { proto, sid, label }) = hello else {
        let _ = sink
            .send(reject(close::BAD_HELLO, "expected a hello frame"))
            .await;
        return;
    };
    if proto != protocol::PROTO {
        let reason = format!(
            "this hub speaks protocol {}, not {proto}; upgrade the older side",
            protocol::PROTO
        );
        let _ = sink.send(reject(close::BAD_HELLO, &reason)).await;
        return;
    }
    if !protocol::valid_sid(&sid) {
        let _ = sink
            .send(reject(close::BAD_HELLO, "malformed session id"))
            .await;
        return;
    }
    let generation = hub.next();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let refused = {
        let mut sessions = hub.sessions.lock().unwrap();
        let taken = sessions.get(&sid).is_some_and(|s| s.pair != pair.name);
        let count = sessions
            .iter()
            .filter(|(s, session)| **s != sid && session.pair == pair.name)
            .count();
        if taken {
            Some((close::SID_TAKEN, "that session id is in use"))
        } else if count >= MAX_SESSIONS_PER_PAIR {
            Some((close::TOO_MANY, "this token pair has too many sessions"))
        } else {
            let session = sessions.entry(sid.clone()).or_insert_with(|| Session {
                pair: pair.name.clone(),
                agent_hash: pair.agent_hash.clone(),
                label: label.clone(),
                busy: false,
                approval: false,
                title: String::new(),
                last_activity: state::now(),
                agent: None,
                generation,
                offline_since: None,
                clients: HashMap::new(),
                pending: HashMap::new(),
            });
            // A reconnect after a network drop: the new connection takes over.
            if let Some(old) = session.agent.take() {
                let _ = old.send(AgentOut::Close(
                    close::REPLACED,
                    "replaced by a new connection".into(),
                ));
            }
            for (_, client) in session.clients.drain() {
                let _ = client
                    .tx
                    .send(Out::Close(1012, "tamale reconnected".into()));
            }
            session.pending.clear();
            session.agent = Some(tx);
            session.generation = generation;
            session.offline_since = None;
            session.label = label;
            session.agent_hash = pair.agent_hash.clone();
            session.last_activity = state::now();
            None
        }
    };
    if let Some((code, reason)) = refused {
        eprintln!(
            "agent rejected from {ip}: session {}: {reason}",
            short(&sid)
        );
        let _ = sink.send(reject(code, reason)).await;
        return;
    }
    eprintln!(
        "agent connected: pair {} session {} from {ip}",
        pair.name,
        short(&sid)
    );
    hub.touch(&pair.name);
    let welcome = serde_json::to_string(&ToAgent::Welcome {
        proto: protocol::PROTO,
    })
    .unwrap_or_default();
    if sink.send(Message::Text(welcome.into())).await.is_err() {
        return;
    }
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(30));
        ping.tick().await;
        loop {
            let message = tokio::select! {
                out = rx.recv() => match out {
                    Some(AgentOut::Frame(frame)) => Message::Text(serde_json::to_string(&frame).unwrap_or_default().into()),
                    Some(AgentOut::Close(code, reason)) => {
                        let _ = sink.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
                        return;
                    }
                    None => return,
                },
                _ = ping.tick() => Message::Ping(Vec::new().into()),
            };
            if sink.send(message).await.is_err() {
                return;
            }
        }
    });
    // Anything at all (pongs included) within 75 s, or the agent counts as gone.
    while let Ok(Some(Ok(message))) =
        tokio::time::timeout(Duration::from_secs(75), stream.next()).await
    {
        let Message::Text(text) = message else {
            if matches!(message, Message::Close(_)) {
                break;
            }
            continue;
        };
        let Ok(frame) = serde_json::from_str::<ToHub>(&text) else {
            continue;
        };
        let mut sessions = hub.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&sid)
            .filter(|s| s.generation == generation)
        else {
            break;
        };
        match frame {
            ToHub::Msg { c, data } => {
                if let Some(client) = session.clients.get(&c) {
                    let _ = client.tx.send(Out::Text(data));
                }
            }
            ToHub::Close { c, reason } => {
                if let Some(client) = session.clients.remove(&c) {
                    let _ = client.tx.send(Out::Close(4000, reason));
                }
            }
            ToHub::Got {
                r,
                status,
                headers,
                body,
            } => {
                if let Some(waiting) = session.pending.remove(&r) {
                    let _ = waiting.send(Got {
                        status,
                        headers,
                        body,
                    });
                }
            }
            ToHub::Status {
                busy,
                approval,
                title,
                model,
            } => {
                session.busy = busy;
                session.approval = approval;
                session.title = title;
                if !model.is_empty() {
                    session.label.model = model;
                }
                session.last_activity = state::now();
            }
            ToHub::Hello { .. } => {}
        }
    }
    writer.abort();
    let mut sessions = hub.sessions.lock().unwrap();
    if let Some(session) = sessions
        .get_mut(&sid)
        .filter(|s| s.generation == generation)
    {
        session.agent = None;
        session.offline_since = Some(Instant::now());
        session.pending.clear();
        // A normal close: the pages keep retrying and pick up when tamale is back.
        for (_, client) in session.clients.drain() {
            let _ = client
                .tx
                .send(Out::Close(1012, "tamale disconnected".into()));
        }
        eprintln!(
            "agent disconnected: pair {} session {}",
            pair.name,
            short(&sid)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_urls_need_https_unless_insecure() {
        let p = Public::parse("https://Tamale-Hub.com/", false).unwrap();
        assert_eq!(
            (p.origin().as_str(), p.secure),
            ("https://tamale-hub.com", true)
        );
        assert_eq!(p.session_origin("ab"), "https://ab.tamale-hub.com");
        assert!(Public::parse("http://hub.localhost:8787", false).is_err());
        assert!(
            !Public::parse("http://hub.localhost:8787", true)
                .unwrap()
                .secure
        );
        assert!(Public::parse("https://x.com/path", false).is_err());
        assert_eq!(p.cookie_name("th"), "__Host-th");
        assert!(
            p.set_cookie("th", "v", 5)
                .contains("HttpOnly; SameSite=Strict; Path=/; Max-Age=5; Secure")
        );
    }

    #[test]
    fn session_cookies_are_bound_to_their_sid_and_secret() {
        let sid = "a".repeat(32);
        let view = "f".repeat(64);
        let value = SessionCookie::make(b"secret", &sid, &view);
        assert_eq!(
            SessionCookie::check(b"secret", &sid, &value),
            Some(&view[..16])
        );
        assert_eq!(SessionCookie::check(b"other", &sid, &value), None);
        assert_eq!(
            SessionCookie::check(b"secret", &"b".repeat(32), &value),
            None
        );
        let forged = value.replace(&view[..16], &"0".repeat(16));
        assert_eq!(SessionCookie::check(b"secret", &sid, &forged), None);
    }

    #[test]
    fn forwarded_for_counts_only_behind_a_local_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "9.9.9.9, 1.2.3.4".parse().unwrap());
        let local: SocketAddr = "127.0.0.1:5".parse().unwrap();
        let remote: SocketAddr = "5.6.7.8:5".parse().unwrap();
        assert_eq!(
            client_ip(local, &headers),
            "1.2.3.4".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(remote, &headers),
            "5.6.7.8".parse::<IpAddr>().unwrap()
        );
    }

    mod end_to_end {
        use super::super::*;
        use crate::state::{Pair, Store, hash, new_token};
        use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

        struct Setup {
            port: u16,
            agent: String,
            view: String,
            other_view: String,
            other_agent: String,
            store: Store,
        }

        async fn start() -> Setup {
            let dir = std::env::temp_dir().join(format!(
                "tamale-hub-e2e-{}",
                state::hex(&state::random::<8>().unwrap())
            ));
            let store = Store::open(Some(dir)).unwrap();
            let (agent, view) = (new_token("tha_").unwrap(), new_token("thv_").unwrap());
            let (other_agent, other_view) =
                (new_token("tha_").unwrap(), new_token("thv_").unwrap());
            store
                .edit(|s| {
                    for (name, a, v) in [
                        ("mine", &agent, &view),
                        ("other", &other_agent, &other_view),
                    ] {
                        s.pairs.push(Pair {
                            name: name.into(),
                            agent_hash: hash(a),
                            view_hash: hash(v),
                            created: 0,
                            last_used: None,
                        });
                    }
                    Ok(())
                })
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
            let hub = Hub::new(
                public,
                Store {
                    dir: store.dir.clone(),
                },
            )
            .unwrap();
            tokio::spawn(run(hub, listener));
            Setup {
                port,
                agent,
                view,
                other_view,
                other_agent,
                store,
            }
        }

        type Ws = tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >;

        async fn ws(
            port: u16,
            path: &str,
            headers: &[(&str, String)],
        ) -> Result<Ws, tungstenite::Error> {
            let mut request = format!("ws://127.0.0.1:{port}{path}")
                .into_client_request()
                .unwrap();
            for (k, v) in headers {
                request.headers_mut().insert(
                    tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                );
            }
            tokio_tungstenite::connect_async(request)
                .await
                .map(|(ws, _)| ws)
        }

        async fn next_text(ws: &mut Ws) -> String {
            loop {
                match tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                {
                    tungstenite::Message::Text(t) => return t.to_string(),
                    tungstenite::Message::Close(f) => panic!("closed: {f:?}"),
                    _ => {}
                }
            }
        }

        async fn close_code(ws: &mut Ws) -> u16 {
            loop {
                match tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await
                    .unwrap()
                {
                    Some(Ok(tungstenite::Message::Close(Some(f)))) => return f.code.into(),
                    Some(Ok(_)) => {}
                    other => panic!("no close frame: {other:?}"),
                }
            }
        }

        async fn agent_hello(port: u16, token: &str, sid: &str) -> Ws {
            let mut agent = ws(
                port,
                "/agent",
                &[
                    ("host", format!("hub.localhost:{port}")),
                    ("authorization", format!("Bearer {token}")),
                ],
            )
            .await
            .unwrap();
            let hello = ToHub::Hello {
                proto: protocol::PROTO,
                sid: sid.into(),
                label: Label {
                    host: "vps-b".into(),
                    cwd: "~/api".into(),
                    model: "m".into(),
                },
            };
            agent
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&hello).unwrap().into(),
                ))
                .await
                .unwrap();
            agent
        }

        fn set_cookie(response: &reqwest::Response) -> String {
            response
                .headers()
                .get("set-cookie")
                .unwrap()
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        }

        #[tokio::test]
        async fn a_browser_reaches_a_session_through_ticket_cookie_tunnel_and_socket() {
            let setup = start().await;
            let port = setup.port;
            let dash = format!("hub.localhost:{port}");
            let origin = format!("http://{dash}");
            let sid = "0123456789abcdef0123456789abcdef".to_string();
            let session_host = format!("{sid}.hub.localhost:{port}");
            let base = format!("http://127.0.0.1:{port}");
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();

            // An agent token can't be used as the view token, and vice versa.
            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", &origin)
                .json(&json!({"add": setup.agent}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401);
            assert!(
                ws(
                    port,
                    "/agent",
                    &[
                        ("host", dash.clone()),
                        ("authorization", format!("Bearer {}", setup.view))
                    ]
                )
                .await
                .is_err()
            );

            let mut agent = agent_hello(port, &setup.agent, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));

            // The same sid under another pair is refused.
            let mut thief = agent_hello(port, &setup.other_agent, &sid).await;
            assert_eq!(close_code(&mut thief).await, close::SID_TAKEN);

            // Wrong origin on the token endpoint.
            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", "http://evil.example")
                .json(&json!({"add": setup.view}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 403);

            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", &origin)
                .json(&json!({"add": setup.view}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            let th = set_cookie(&r);
            assert!(
                r.headers()["set-cookie"]
                    .to_str()
                    .unwrap()
                    .contains("HttpOnly; SameSite=Strict")
            );

            let list: Value = http
                .get(format!("{base}/api/sessions"))
                .header("host", &dash)
                .header("cookie", &th)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(list["sessions"][0]["sid"], sid.as_str());
            assert_eq!(list["sessions"][0]["host"], "vps-b");

            // The other pair's view token sees nothing and can't get a ticket.
            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", &origin)
                .json(&json!({"add": setup.other_view}))
                .send()
                .await
                .unwrap();
            let other_th = set_cookie(&r);
            let r = http
                .post(format!("{base}/api/ticket"))
                .header("host", &dash)
                .header("origin", &origin)
                .header("cookie", &other_th)
                .json(&json!({"sid": sid}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404);

            let ticket: Value = http
                .post(format!("{base}/api/ticket"))
                .header("host", &dash)
                .header("origin", &origin)
                .header("cookie", &th)
                .json(&json!({"sid": sid}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let url = ticket["url"].as_str().unwrap();
            let path = url.split(&format!(":{port}")).nth(1).unwrap();

            // Without its cookie, the session host sends the browser for a ticket.
            let r = http
                .get(format!("{base}/"))
                .header("host", &session_host)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 303);
            assert!(
                r.headers()["location"]
                    .to_str()
                    .unwrap()
                    .ends_with(&format!("/open/{sid}"))
            );

            let r = http
                .get(format!("{base}{path}"))
                .header("host", &session_host)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 303);
            let ts = set_cookie(&r);
            // A ticket works once.
            let again = http
                .get(format!("{base}{path}"))
                .header("host", &session_host)
                .send()
                .await
                .unwrap();
            assert_eq!(again.status(), 400);
            // And a session cookie works only on its own host.
            let elsewhere = format!("{}.hub.localhost:{port}", "f".repeat(32));
            let r = http
                .get(format!("{base}/app.js"))
                .header("host", &elsewhere)
                .header("cookie", &ts)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404);

            // A tunnelled GET: tamale's headers pass, except set-cookie.
            let page = tokio::spawn({
                let http = http.clone();
                let (base, host, ts) = (base.clone(), session_host.clone(), ts.clone());
                async move {
                    http.get(format!("{base}/"))
                        .header("host", host)
                        .header("cookie", ts)
                        .send()
                        .await
                        .unwrap()
                }
            });
            let get: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
            let ToAgent::Get { r, path } = get else {
                panic!("{get:?}")
            };
            assert_eq!(path, "/");
            let got = ToHub::Got {
                r,
                status: 200,
                body: "<p>page".into(),
                headers: vec![
                    ("content-type".into(), "text/html; charset=utf-8".into()),
                    (
                        "content-security-policy".into(),
                        "default-src 'self'".into(),
                    ),
                    ("set-cookie".into(), "th=x; Domain=hub.localhost".into()),
                ],
            };
            agent
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&got).unwrap().into(),
                ))
                .await
                .unwrap();
            let page = page.await.unwrap();
            assert_eq!(page.status(), 200);
            assert!(page.headers().get("set-cookie").is_none());
            assert_eq!(page.headers()["x-content-type-options"], "nosniff");
            assert_eq!(
                page.headers()["content-security-policy"],
                "default-src 'self'"
            );
            assert_eq!(page.text().await.unwrap(), "<p>page");

            // Paths outside the tunnel never reach tamale.
            let r = http
                .get(format!("{base}/etc/passwd"))
                .header("host", &session_host)
                .header("cookie", &ts)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404);

            // The socket needs this session's own origin, even with the cookie.
            let sibling = format!("http://{}.hub.localhost:{port}", "f".repeat(32));
            assert!(
                ws(
                    port,
                    "/ws",
                    &[
                        ("host", session_host.clone()),
                        ("cookie", ts.clone()),
                        ("origin", sibling)
                    ]
                )
                .await
                .is_err()
            );
            let mut tab = ws(
                port,
                "/ws",
                &[
                    ("host", session_host.clone()),
                    ("cookie", ts.clone()),
                    ("origin", format!("http://{session_host}")),
                ],
            )
            .await
            .unwrap();
            let open: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
            let ToAgent::Open { c } = open else {
                panic!("{open:?}")
            };
            agent
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&ToHub::Msg {
                        c,
                        data: "{\"t\":\"status\"}".into(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            assert_eq!(next_text(&mut tab).await, "{\"t\":\"status\"}");
            tab.send(tungstenite::Message::Text(
                "{\"t\":\"send\",\"text\":\"hi\"}".into(),
            ))
            .await
            .unwrap();
            assert_eq!(
                serde_json::from_str::<ToAgent>(&next_text(&mut agent).await).unwrap(),
                ToAgent::Msg {
                    c,
                    data: "{\"t\":\"send\",\"text\":\"hi\"}".into()
                }
            );

            // A reconnect from the same pair takes over; the tab is closed normally to retry.
            let mut agent2 = agent_hello(port, &setup.agent, &sid).await;
            assert!(next_text(&mut agent2).await.contains("\"welcome\""));
            assert_eq!(close_code(&mut agent).await, close::REPLACED);
            assert_eq!(close_code(&mut tab).await, 1012);

            // While the agent is gone, the socket answers 503 so the page keeps retrying.
            drop(agent2);
            tokio::time::sleep(Duration::from_millis(300)).await;
            let offline = ws(
                port,
                "/ws",
                &[
                    ("host", session_host.clone()),
                    ("cookie", ts.clone()),
                    ("origin", format!("http://{session_host}")),
                ],
            )
            .await;
            match offline {
                Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 503),
                other => panic!("{:?}", other.map(|_| ())),
            }

            // Revoking the pair (as the CLI would) drops the session within seconds.
            let mut agent3 = agent_hello(port, &setup.agent, &sid).await;
            assert!(next_text(&mut agent3).await.contains("\"welcome\""));
            tokio::time::sleep(Duration::from_millis(1100)).await;
            setup
                .store
                .edit(|s| {
                    s.pairs.retain(|p| p.name != "mine");
                    Ok(())
                })
                .unwrap();
            let code = tokio::time::timeout(Duration::from_secs(6), close_code(&mut agent3))
                .await
                .unwrap();
            assert_eq!(code, close::BAD_TOKEN);
            let r = http
                .get(format!("{base}/app.js"))
                .header("host", &session_host)
                .header("cookie", &ts)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404, "the session cookie dies with its pair");
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn unknown_hosts_and_www_are_handled() {
            let setup = start().await;
            let base = format!("http://127.0.0.1:{}", setup.port);
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let r = http
                .get(&base)
                .header("host", "evil.example")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404);
            let r = http
                .get(&base)
                .header("host", format!("www.hub.localhost:{}", setup.port))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 308);
            let r = http
                .get(&base)
                .header("host", format!("hub.localhost:{}", setup.port))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            assert_eq!(r.headers()["x-frame-options"], "DENY");
            assert!(r.text().await.unwrap().contains("tamale-hub"));
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }
    }
}
