//! `tamale-hub serve`: the relay.
//!
//! Requests are told apart by `Host`: the public host (`tamale-hub.com`) is the hub's
//! own pages and the agent socket; `<sid>.tamale-hub.com` is one session, its own
//! origin, whose page, artifacts and socket are relayed to that session's tamale.
//! [`rewrite`] maps each request onto an internal path (`/d/…` or `/s/<sid>/…`) before
//! routing.
//!
//! Credentials: the hub never sees a key, only the auth value derived from it (see
//! `single_key.md`). The dashboard keeps the browser's auth values in an `HttpOnly` cookie
//! that only the public host ever sees. Opening a session trades it for a one-time
//! ticket, which the session host trades for a cookie of its own, signed and scoped to
//! that one session (see [`SessionCookie`]). A session's page, even a hostile one, so
//! never holds anything that reaches another session.
//!
//! Native clients (the iOS app, `apple/ios.md`) send the auth value as
//! `Authorization: Bearer` on `/api/sessions`, `/api/ticket` and the session socket instead.
//! Browsers never attach that header on their own, so those requests skip the `Origin`
//! checks that guard the cookies, and the socket needs no ticket round trip.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
use tokio::sync::{Notify, mpsc, oneshot};
use tower::Layer;

use crate::state::{self, State as Tokens, Store};

const PAGE: &str = include_str!("../static/index.html");
const SCRIPT: &str = include_str!("../static/hub.js");
const SEAL: &str = include_str!("../static/seal.js");
const QR: &str = include_str!("../static/qrcode.js");
const STYLE: &str = include_str!("../static/hub.css");

/// How long a disconnected session stays listed (and its page keeps retrying), and how
/// long after a restart unknown sessions are assumed to be on their way back.
const GRACE: Duration = Duration::from_secs(600);
const TICKET_LIFE: Duration = Duration::from_secs(60);
const SESSION_COOKIE_LIFE: u64 = 30 * 24 * 3600;
const VIEW_COOKIE_LIFE: u64 = 34_560_000;
const MAX_CLIENTS: usize = 16;
const MAX_SESSIONS_PER_KEY: usize = 64;
/// Tickets a key may have waiting to be used; each session's page needs just one.
const MAX_TICKETS_PER_KEY: usize = 64;
/// Bytes that may wait to be written to one socket. A reader that stops reading is cut off,
/// rather than growing the hub until the machine, which it may share with other sites, runs
/// out of memory. Anyone can make a key, and with it both ends of a socket.
const MAX_QUEUED: usize = 128 << 20;
/// How many sockets' worth may wait in all.
const QUEUED_SOCKETS: usize = 4;
const _: () = assert!(MAX_QUEUED >= protocol::MAX_AGENT_FRAME);
const GET_TIMEOUT: Duration = Duration::from_secs(30);
/// Auth failures an IP may have per window before it gets 429s.
const MAX_FAILURES: u32 = 30;
const FAILURE_WINDOW: Duration = Duration::from_secs(600);

pub struct Options {
    pub listen: SocketAddr,
    pub public_url: String,
    pub insecure: bool,
    pub signup: Signup,
    /// `<team id>.<bundle id>` of the iOS app that may open `/open/<sid>` links, if any.
    pub apple_app_id: Option<String>,
}

/// Whether anyone may make a key from the hub's page.
#[derive(Clone, Copy, Debug)]
pub struct Signup {
    pub open: bool,
    /// Keys the hub holds at most; signup stops there.
    pub max_keys: usize,
}

/// Keys one IP may make from the page per day.
const SIGNUPS_PER_IP: usize = 5;
const SIGNUP_WINDOW: Duration = Duration::from_secs(24 * 3600);
/// A page-made key that never connects is deleted after this; one unused for
/// `IDLE_EXPIRY` too. CLI-made keys never expire.
const UNUSED_EXPIRY: u64 = 7 * 24 * 3600;
const IDLE_EXPIRY: u64 = 90 * 24 * 3600;

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

/// The bytes waiting to be written to one socket, which also count towards the whole hub's.
struct Backlog {
    own: AtomicUsize,
    total: Arc<AtomicUsize>,
    limit: usize,
    /// Told when the socket is too far behind, and should end.
    over: Notify,
}

impl Backlog {
    fn new(total: Arc<AtomicUsize>, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            own: AtomicUsize::new(0),
            total,
            limit,
            over: Notify::new(),
        })
    }

    /// Makes room for `len` more bytes; false, and they shouldn't be queued, if there isn't any.
    fn add(&self, len: usize) -> bool {
        let own = self.own.fetch_add(len, Ordering::Relaxed) + len;
        let total = self.total.fetch_add(len, Ordering::Relaxed) + len;
        if own > self.limit || total > self.limit * QUEUED_SOCKETS {
            self.sent(len);
            self.over.notify_one();
            return false;
        }
        true
    }

    fn sent(&self, len: usize) {
        self.own.fetch_sub(len, Ordering::Relaxed);
        self.total.fetch_sub(len, Ordering::Relaxed);
    }
}

impl Drop for Backlog {
    /// What was still waiting when the socket went.
    fn drop(&mut self) {
        self.total.fetch_sub(*self.own.get_mut(), Ordering::Relaxed);
    }
}

/// A browser tab on a session.
struct Client {
    tx: mpsc::UnboundedSender<Out>,
    backlog: Arc<Backlog>,
}

enum Out {
    Text(String),
    Close(u16, String),
}

struct Session {
    /// The name of the key it connected with, and that key's hash then: replacing the key
    /// ends the session.
    key: String,
    auth_hash: String,
    label: Label,
    busy: bool,
    approval: bool,
    title: String,
    /// The key's public id, and whether its channels are end-to-end encrypted. Then the label
    /// and title are empty, and these opaque blobs go to browsers instead.
    mid: Option<String>,
    e2e: bool,
    sealed_label: Option<String>,
    sealed_status: Option<String>,
    /// Unix seconds.
    last_activity: u64,
    /// The connected agent, `None` while offline.
    agent: Option<AgentLink>,
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

impl AgentOut {
    /// What it counts for in a [`Backlog`]. The rest of a frame is small, but not nothing.
    fn size(&self) -> usize {
        match self {
            Self::Frame(ToAgent::Msg { data, .. }) => data.len() + 64,
            Self::Frame(ToAgent::Get { path, .. }) => path.len() + 64,
            Self::Frame(_) => 64,
            Self::Close(..) => 0,
        }
    }
}

/// The way to a connected agent.
#[derive(Clone)]
struct AgentLink {
    tx: mpsc::UnboundedSender<AgentOut>,
    backlog: Arc<Backlog>,
}

impl AgentLink {
    /// An error if the agent has gone, or is too far behind and is about to.
    fn send(&self, out: AgentOut) -> Result<(), ()> {
        let size = out.size();
        if !self.backlog.add(size) {
            return Err(());
        }
        self.tx.send(out).map_err(|_| ())
    }
}

struct Got {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

struct Ticket {
    sid: String,
    auth_hash: String,
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
    /// When each IP made keys from the page, within the last day.
    signups: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    signup: Signup,
    /// The iOS app that universal links open (see [`apple_app_site_association`]).
    apple_app_id: std::sync::OnceLock<String>,
    started: Instant,
    next: AtomicU64,
    /// Bytes waiting to be written to any socket, and how many one may have ([`MAX_QUEUED`]).
    queued: Arc<AtomicUsize>,
    max_queued: usize,
}

impl Hub {
    pub fn new(public: Public, store: Store, signup: Signup) -> Result<Arc<Self>> {
        // Makes the cookie secret if there isn't one yet.
        let tokens = store.edit(|s| Ok(s.clone()))?;
        Ok(Arc::new(Self {
            public,
            store,
            tokens: RwLock::new(tokens),
            sessions: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            signups: Mutex::new(HashMap::new()),
            signup,
            apple_app_id: std::sync::OnceLock::new(),
            started: Instant::now(),
            next: AtomicU64::new(1),
            queued: Arc::new(AtomicUsize::new(0)),
            max_queued: MAX_QUEUED,
        }))
    }

    fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn secret(&self) -> Vec<u8> {
        self.tokens.read().unwrap().secret.as_bytes().to_vec()
    }

    /// Re-reads the keys and drops whatever a revoke or rotate took away.
    pub fn reload(&self) -> Result<()> {
        let tokens = self.store.load()?;
        let keys: HashMap<&str, &state::Key> =
            tokens.keys.iter().map(|k| (k.name.as_str(), k)).collect();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|sid, session| {
            let Some(key) = keys.get(session.key.as_str()) else {
                eprintln!(
                    "session {} dropped: key {} revoked",
                    short(sid),
                    session.key
                );
                end_session(session, "This session's key was deleted.");
                return false;
            };
            if key.auth_hash != session.auth_hash {
                eprintln!(
                    "session {} dropped: key {} replaced",
                    short(sid),
                    session.key
                );
                end_session(session, "This session's key was replaced.");
                return false;
            }
            true
        });
        drop(sessions);
        *self.tokens.write().unwrap() = tokens;
        Ok(())
    }

    /// Notes an auth failure from `ip`.
    fn fail(&self, ip: IpAddr) {
        let mut failures = self.failures.lock().unwrap();
        let entry = failures.entry(limit_bucket(ip)).or_insert((0, Instant::now()));
        if entry.1.elapsed() > FAILURE_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    fn limited(&self, ip: IpAddr) -> bool {
        self.failures
            .lock()
            .unwrap()
            .get(&limit_bucket(ip))
            .is_some_and(|(n, since)| *n >= MAX_FAILURES && since.elapsed() <= FAILURE_WINDOW)
    }

    /// A native client's key, from `Authorization: Bearer <auth value>`. `None` without the
    /// header; an error response (counted as a failure) for a key the hub doesn't know.
    fn bearer(&self, ip: IpAddr, headers: &HeaderMap) -> Option<Result<state::Key, Response>> {
        let auth = bearer_value(headers)?;
        if self.limited(ip) {
            return Some(Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many failed attempts; try again later.",
            )));
        }
        let key = self.tokens.read().unwrap().by_auth(auth).cloned();
        Some(key.ok_or_else(|| {
            self.fail(ip);
            error(StatusCode::UNAUTHORIZED, "This hub doesn't know that key.")
        }))
    }

    /// The auth values in the browser's dashboard cookie that are still good, and their keys.
    fn views(&self, headers: &HeaderMap) -> (Vec<String>, Vec<state::Key>) {
        let tokens = self.tokens.read().unwrap();
        let mut valid = Vec::new();
        let mut keys = Vec::new();
        for auth in cookie(headers, &self.public.cookie_name("th"))
            .unwrap_or_default()
            .split(',')
            .filter(|t| !t.is_empty())
        {
            if let Some(key) = tokens.by_auth(auth)
                && !keys.iter().any(|k: &state::Key| k.name == key.name)
            {
                valid.push(auth.to_string());
                keys.push(key.clone());
            }
        }
        (valid, keys)
    }

    /// Records that a key was used, at most once an hour (it rewrites state.json).
    fn touch(&self, name: &str) {
        let stale = self.tokens.read().unwrap().by_name(name).is_some_and(|k| {
            k.last_used
                .is_none_or(|t| state::now().saturating_sub(t) > 3600)
        });
        if stale {
            let name = name.to_string();
            let _ = self.store.edit(|s| {
                if let Some(k) = s.keys.iter_mut().find(|k| k.name == name) {
                    k.last_used = Some(state::now());
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
        self.signups.lock().unwrap().retain(|_, times| {
            times.retain(|t| t.elapsed() < SIGNUP_WINDOW);
            !times.is_empty()
        });
        if let Err(e) = self.expire() {
            eprintln!("expiring unused keys: {e:#}");
        }
    }

    /// Deletes page-made keys nobody uses, unless one of their sessions is connected.
    fn expire(&self) -> Result<()> {
        let online: HashSet<String> = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.agent.is_some())
            .map(|s| s.key.clone())
            .collect();
        let now = state::now();
        let doomed = |k: &state::Key| {
            k.web
                && !online.contains(&k.name)
                && match k.last_used {
                    None => now.saturating_sub(k.created) > UNUSED_EXPIRY,
                    Some(t) => now.saturating_sub(t) > IDLE_EXPIRY,
                }
        };
        if !self.tokens.read().unwrap().keys.iter().any(doomed) {
            return Ok(());
        }
        self.store.edit(|s| {
            for k in s.keys.iter().filter(|k| doomed(k)) {
                eprintln!("key {} expired unused", k.name);
            }
            s.keys.retain(|k| !doomed(k));
            Ok(())
        })?;
        self.reload()
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

/// A session host's cookie: `<sid>.<expiry>.<auth hash prefix>.<mac>`, signed with the
/// hub's secret, so it needs no server-side state and survives restarts. A replaced or
/// revoked key invalidates it.
struct SessionCookie;

impl SessionCookie {
    fn make(secret: &[u8], sid: &str, auth_hash: &str) -> String {
        let expires = state::now() + SESSION_COOKIE_LIFE;
        let body = format!("{sid}.{expires}.{}", &auth_hash[..16]);
        let mac = state::hmac(secret, format!("ts|{body}").as_bytes());
        format!("{body}.{mac}")
    }

    /// The auth hash prefix a valid cookie for `sid` was made from.
    fn check<'a>(secret: &[u8], sid: &str, value: &'a str) -> Option<&'a str> {
        let (body, mac) = value.rsplit_once('.')?;
        let mut parts = body.split('.');
        let (cookie_sid, expires, prefix) = (parts.next()?, parts.next()?, parts.next()?);
        let good = state::ct_eq(mac, &state::hmac(secret, format!("ts|{body}").as_bytes()));
        (good && cookie_sid == sid && expires.parse::<u64>().ok()? > state::now()).then_some(prefix)
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

/// Who a rate limit counts `ip` as. One IPv6 customer usually has a whole /64 to choose
/// addresses from, so the limits go by that; an IPv4 address in IPv6 clothing is itself.
fn limit_bucket(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(ip) => {
            let network = u128::from(ip) & (u128::MAX << 64);
            IpAddr::V6(Ipv6Addr::from(network))
        }
        ip => ip,
    }
}

/// The value of an `Authorization: Bearer` header, if there is one.
fn bearer_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
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
    // A request can be just short enough to parse with its own path and too long with the
    // prefix. It mustn't then go on to be routed by the path it came with.
    *request.uri_mut() = target
        .parse::<Uri>()
        .unwrap_or_else(|_| Uri::from_static("/x/none"));
    request
}

pub async fn serve(options: Options, store: Store) -> Result<()> {
    let public = Public::parse(&options.public_url, options.insecure)?;
    let hub = Hub::new(public.clone(), store, options.signup)?;
    if let Some(id) = options.apple_app_id {
        let _ = hub.apple_app_id.set(id);
    }
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
            "/d/seal.js",
            get(|| async { asset("text/javascript; charset=utf-8", SEAL) }),
        )
        .route(
            "/d/qrcode.js",
            get(|| async { asset("text/javascript; charset=utf-8", QR) }),
        )
        .route(
            "/d/hub.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE) }),
        )
        .route("/d/api/tokens", post(api_tokens))
        .route("/d/api/sessions", get(api_sessions))
        .route("/d/api/ticket", post(api_ticket))
        .route("/d/api/keys", post(api_key_new))
        .route("/d/api/keys/rotate", post(api_key_rotate))
        .route("/d/api/keys/delete", post(api_key_delete))
        .route(
            "/d/.well-known/apple-app-site-association",
            get(apple_app_site_association),
        )
        .route("/d/agent", get(agent))
        .route("/s/{sid}/__hub/auth", get(auth))
        .route("/s/{sid}/ws", get(session_socket))
        .route("/s/{sid}/", get(session_root))
        .route("/s/{sid}/{*path}", get(session_get))
        .route("/x/www", get(www))
        .fallback(|| async { (StatusCode::NOT_FOUND, "Not found.") })
        .with_state(hub.clone());
    let public = hub.public.clone();
    let secure = hub.public.secure;
    let app = tower::util::MapRequestLayer::new(move |r| rewrite(&public, r)).layer(router);
    let app = tower::util::MapResponseLayer::new(move |r| https_only(secure, r)).layer(app);
    axum::serve(
        listener,
        ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(app),
    )
    .await?;
    Ok(())
}

/// Tells browsers that have seen the hub over https not to try it any other way. Each host says
/// so for itself, so this says nothing about other sites under a self-hosted hub's domain.
fn https_only(secure: bool, mut response: Response) -> Response {
    if secure {
        response.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response
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
        // A session's page is on the same site, so these keep it to its own origin too.
        (
            header::HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

/// The hub's page, which shows sessions in frames. It's only ever framed by itself, when a
/// session whose cookie has run out redirects its frame here to get another.
async fn dashboard(State(hub): State<Arc<Hub>>) -> Response {
    let mut response =
        guard(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE).into_response());
    let csp = format!(
        "default-src 'self'; img-src 'self' data:; frame-src 'self' {}; frame-ancestors 'self'; base-uri 'none'; form-action 'self'",
        hub.public.session_origin("*"),
    );
    let headers = response.headers_mut();
    if let Ok(csp) = csp.parse() {
        headers.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    response
}

/// `csp` from a session's tamale, also letting the hub's page frame it. tamale serves the
/// same page locally, where nothing may frame it; it doesn't know the hub's page will.
fn framed_by_hub(csp: &str, hub_origin: &str) -> String {
    csp.split(';')
        .map(|directive| {
            let trimmed = directive.trim();
            match trimmed.strip_prefix("frame-ancestors") {
                Some(sources) if sources.trim() == "'none'" => {
                    format!(" frame-ancestors {hub_origin}")
                }
                // Every ancestor has to match, so frames inside the page need it too.
                Some(_) => format!(" {trimmed} {hub_origin}"),
                None => directive.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
        .trim_start()
        .to_string()
}

/// Lets the iOS app open the terminal's `/open/<sid>` links (universal links), when the hub
/// was started with `--apple-app-id`.
async fn apple_app_site_association(State(hub): State<Arc<Hub>>) -> Response {
    let Some(id) = hub.apple_app_id.get() else {
        return (StatusCode::NOT_FOUND, "Not found.").into_response();
    };
    let body = json!({
        "applinks": {
            "details": [{
                "appIDs": [id],
                "components": [{ "/": "/open/*", "comment": "a session, from the terminal's QR code" }],
            }],
        },
    });
    guard(Json(body).into_response())
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

/// A key's public id: the start of its hash, which the browser can work out from the key,
/// so it can tell which of its keys the hub's list means.
fn key_id(key: &state::Key) -> &str {
    &key.auth_hash[..16]
}

/// Adds an auth value to the browser's cookie (or forgets them all).
async fn api_tokens(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<TokensRequest>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if let Some(refused) = checked(&hub, ip, &headers) {
        return refused;
    }
    if request.forget {
        return with_view_cookie(&hub, guard(Json(json!({ "keys": 0 })).into_response()), &[]);
    }
    let (mut tokens, _) = hub.views(&headers);
    let mut added = None;
    if let Some(add) = request.add.map(|t| t.trim().to_string()) {
        // A key itself never belongs here: the page derives the auth value first.
        if add.starts_with(protocol::KEY_PREFIX) {
            return error(StatusCode::BAD_REQUEST, "Send the auth value, not the key.");
        }
        let key = hub.tokens.read().unwrap().by_auth(&add).cloned();
        let Some(key) = key else {
            hub.fail(ip);
            return error(StatusCode::UNAUTHORIZED, "This hub doesn't know that key.");
        };
        eprintln!("key {} added from {ip}", key.name);
        if !tokens.contains(&add) {
            tokens.push(add);
        }
        added = Some(key);
    }
    let body = json!({
        "keys": tokens.len(),
        "name": added.as_ref().map(|k| k.name.as_str()),
    });
    with_view_cookie(&hub, guard(Json(body).into_response()), &tokens)
}

/// The sessions under the browser's keys, or a native client's one key.
async fn api_sessions(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let keys = match hub.bearer(client_ip(peer, &headers), &headers) {
        Some(Ok(key)) => vec![key],
        Some(Err(refused)) => return refused,
        None => hub.views(&headers).1,
    };
    let names: HashSet<&str> = keys.iter().map(|k| k.name.as_str()).collect();
    let now = state::now();
    let mut list: Vec<Value> = hub
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, s)| names.contains(s.key.as_str()))
        .map(|(sid, s)| {
            json!({
                "sid": sid,
                "open": format!("/open/{sid}"),
                "key": s.key,
                "host": s.label.host,
                "cwd": s.label.cwd,
                "model": s.label.model,
                "title": s.title,
                "mid": s.mid,
                "e2e": s.e2e,
                "sealed_label": s.sealed_label,
                "sealed_status": s.sealed_status,
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
            "signup": hub.signup.open,
            "keys": keys.iter().map(|k| json!({
                "name": k.name,
                "id": key_id(k),
                "created": k.created,
                "web": k.web,
            })).collect::<Vec<_>>(),
            "sessions": list,
        }))
        .into_response(),
    )
}

/// Sets the dashboard cookie to `tokens` on `response`.
fn with_view_cookie(hub: &Hub, mut response: Response, tokens: &[String]) -> Response {
    let value = if tokens.is_empty() {
        hub.public.set_cookie("th", "", 0)
    } else {
        hub.public.set_cookie("th", &tokens.join(","), VIEW_COOKIE_LIFE)
    };
    if let Ok(v) = value.parse() {
        response.headers_mut().insert(header::SET_COOKIE, v);
    }
    response
}

/// Whether the dashboard request may change things: from the hub's own page.
fn checked(hub: &Hub, ip: IpAddr, headers: &HeaderMap) -> Option<Response> {
    if !origin_is(headers, &hub.public.origin()) {
        return Some(error(StatusCode::FORBIDDEN, "Wrong origin."));
    }
    if hub.limited(ip) {
        return Some(error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts; try again later.",
        ));
    }
    None
}

#[derive(Deserialize)]
struct NewKey {
    auth: String,
    #[serde(default)]
    label: Option<String>,
}

/// Registers a key the page made in the browser. The hub gets only its auth value.
async fn api_key_new(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<NewKey>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if let Some(refused) = checked(&hub, ip, &headers) {
        return refused;
    }
    if !hub.signup.open {
        return error(
            StatusCode::FORBIDDEN,
            "This hub doesn't make keys from its page; ask whoever runs it.",
        );
    }
    if !protocol::valid_auth(&request.auth) {
        return error(StatusCode::BAD_REQUEST, "That isn't an auth value.");
    }
    {
        let mut signups = hub.signups.lock().unwrap();
        let times = signups.entry(limit_bucket(ip)).or_default();
        times.retain(|t| t.elapsed() < SIGNUP_WINDOW);
        if times.len() >= SIGNUPS_PER_IP {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "That's enough new keys from here for today.",
            );
        }
        times.push(Instant::now());
    }
    let max = hub.signup.max_keys;
    let made = hub.store.edit(|s| {
        if s.by_auth(&request.auth).is_some() {
            bail!("taken");
        }
        if s.keys.len() >= max {
            bail!("full");
        }
        let name = state::key_name(request.label.as_deref(), &s.keys)?;
        s.keys.push(state::Key::new(&name, &request.auth, true));
        Ok(name)
    });
    let name = match made {
        Ok(name) => name,
        Err(e) if e.to_string() == "taken" => {
            return error(StatusCode::CONFLICT, "That key is already registered.");
        }
        Err(e) if e.to_string() == "full" => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "This hub is full right now. You can run your own: see the README.",
            );
        }
        Err(e) => {
            eprintln!("making a key: {e:#}");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "Couldn't save the new key.");
        }
    };
    let _ = hub.reload();
    eprintln!("key {name} made from the page by {ip}");
    let (mut tokens, _) = hub.views(&headers);
    tokens.push(request.auth);
    with_view_cookie(&hub, guard(Json(json!({ "name": name })).into_response()), &tokens)
}

#[derive(Deserialize)]
struct KeyAction {
    name: String,
    /// The new key's auth value, when replacing one.
    #[serde(default)]
    auth: Option<String>,
}

/// The browser's auth values, and whether it holds `name`, which is what lets it manage
/// that key.
fn holds(hub: &Hub, headers: &HeaderMap, name: &str) -> (Vec<String>, bool) {
    let (tokens, keys) = hub.views(headers);
    let held = keys.iter().any(|k| k.name == name);
    (tokens, held)
}

/// Replaces a key the browser holds with one it made, ending the old one's sessions.
async fn api_key_rotate(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<KeyAction>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if let Some(refused) = checked(&hub, ip, &headers) {
        return refused;
    }
    let (tokens, held) = holds(&hub, &headers, &request.name);
    if !held {
        hub.fail(ip);
        return error(StatusCode::NOT_FOUND, "This browser doesn't hold that key.");
    }
    let Some(auth) = request.auth.filter(|a| protocol::valid_auth(a)) else {
        return error(StatusCode::BAD_REQUEST, "That isn't an auth value.");
    };
    let old = hub
        .tokens
        .read()
        .unwrap()
        .by_name(&request.name)
        .map(|k| k.auth_hash.clone());
    let saved = hub.store.edit(|s| {
        if s.by_auth(&auth).is_some() {
            bail!("taken");
        }
        let key = s
            .keys
            .iter_mut()
            .find(|k| k.name == request.name)
            .context("gone")?;
        key.auth_hash = state::hash(&auth);
        Ok(())
    });
    if let Err(e) = saved {
        return if e.to_string() == "taken" {
            error(StatusCode::CONFLICT, "That key is already registered.")
        } else {
            error(StatusCode::NOT_FOUND, "This browser doesn't hold that key.")
        };
    }
    let _ = hub.reload();
    eprintln!("key {} replaced from the page by {ip}", request.name);
    let mut tokens = tokens;
    tokens.retain(|t| Some(state::hash(t)) != old);
    tokens.push(auth);
    with_view_cookie(
        &hub,
        guard(Json(json!({ "name": request.name })).into_response()),
        &tokens,
    )
}

/// Deletes a key the browser holds, dropping its sessions.
async fn api_key_delete(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<KeyAction>,
) -> Response {
    let ip = client_ip(peer, &headers);
    if let Some(refused) = checked(&hub, ip, &headers) {
        return refused;
    }
    let (tokens, held) = holds(&hub, &headers, &request.name);
    if !held {
        hub.fail(ip);
        return error(StatusCode::NOT_FOUND, "This browser doesn't hold that key.");
    }
    let old = hub
        .tokens
        .read()
        .unwrap()
        .by_name(&request.name)
        .map(|k| k.auth_hash.clone());
    let _ = hub.store.edit(|s| {
        s.keys.retain(|k| k.name != request.name);
        Ok(())
    });
    let _ = hub.reload();
    eprintln!("key {} deleted from the page by {ip}", request.name);
    let tokens: Vec<String> = tokens
        .into_iter()
        .filter(|t| Some(state::hash(t)) != old)
        .collect();
    with_view_cookie(
        &hub,
        guard(Json(json!({ "deleted": request.name })).into_response()),
        &tokens,
    )
}

#[derive(Deserialize)]
struct TicketRequest {
    sid: String,
}

const NO_KEY: &str = "This browser doesn't have this session's key, or the session has ended.";

/// A one-time ticket that lets the browser into one session's host.
async fn api_ticket(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<TicketRequest>,
) -> Response {
    let ip = client_ip(peer, &headers);
    let keys = match hub.bearer(ip, &headers) {
        Some(Ok(key)) => vec![key],
        Some(Err(refused)) => return refused,
        None => {
            if let Some(refused) = checked(&hub, ip, &headers) {
                return refused;
            }
            hub.views(&headers).1
        }
    };
    if keys.is_empty() {
        return error(StatusCode::UNAUTHORIZED, NO_KEY);
    }
    let found = hub
        .sessions
        .lock()
        .unwrap()
        .get(&request.sid)
        .and_then(|s| {
            let key = keys.iter().find(|k| k.name == s.key).cloned()?;
            Some((key, s.mid.clone(), s.e2e))
        });
    let Some((key, mid, e2e)) = found else {
        return error(StatusCode::NOT_FOUND, NO_KEY);
    };
    let Ok(bytes) = state::random::<32>() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "No randomness.");
    };
    let ticket = state::hex(&bytes);
    {
        let mut tickets = hub.tickets.lock().unwrap();
        let now = Instant::now();
        tickets.retain(|_, t| t.expires > now);
        let waiting = tickets
            .values()
            .filter(|t| t.auth_hash == key.auth_hash)
            .count();
        if waiting >= MAX_TICKETS_PER_KEY {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many sessions opening at once; try again in a minute.",
            );
        }
        tickets.insert(
            state::hash(&ticket),
            Ticket {
                sid: request.sid.clone(),
                auth_hash: key.auth_hash.clone(),
                expires: now + TICKET_LIFE,
            },
        );
    }
    hub.touch(&key.name);
    guard(
        Json(json!({
            "url": format!("{}/__hub/auth?ticket={ticket}", hub.public.session_origin(&request.sid)),
            "mid": mid,
            "e2e": e2e,
            // Which key the session is under. Its `mid` is only what it says of itself, and
            // the page mustn't hand one key's secret to a session under another.
            "id": key_id(&key),
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
    let value = SessionCookie::make(&hub.secret(), &sid, &ticket.auth_hash);
    // Tells the page to insist on the handshake. A hub that left this out would gain
    // nothing: tamale refuses unsealed messages, so the page just couldn't connect.
    let e2e = hub.sessions.lock().unwrap().get(&sid).is_some_and(|s| s.e2e);
    let mut response = (
        StatusCode::SEE_OTHER,
        [
            (header::LOCATION, if e2e { "/?e2e=1" } else { "/" }.to_string()),
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

/// Whether a session host's cookie is good: signed for this session, from a current key,
/// and, if the hub knows the session, that session's key.
fn session_allowed(hub: &Hub, sid: &str, headers: &HeaderMap) -> bool {
    let Some(value) = cookie(headers, &hub.public.cookie_name("ts")) else {
        return false;
    };
    let Some(prefix) = SessionCookie::check(&hub.secret(), sid, value) else {
        return false;
    };
    let tokens = hub.tokens.read().unwrap();
    let Some(key) = tokens.keys.iter().find(|k| k.auth_hash.starts_with(prefix)) else {
        return false;
    };
    let sessions = hub.sessions.lock().unwrap();
    !sessions.get(sid).is_some_and(|s| s.key != key.name)
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
    if !session_allowed(&hub, &sid, &headers) {
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
    // An encrypted session's artifacts travel over its socket; tamale refuses too.
    if path.starts_with("/artifact/")
        && hub.sessions.lock().unwrap().get(&sid).is_some_and(|s| s.e2e)
    {
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
    let status = StatusCode::from_u16(got.status)
        .ok()
        .filter(|status| !status.is_informational())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = (status, got.body).into_response();
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
        ) && let (Ok(n), Ok(v)) = (name.parse::<header::HeaderName>(), value.parse())
        {
            out.insert(n, v);
        }
    }
    let csp = out
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .map(|csp| framed_by_hub(csp, &hub.public.origin()));
    if let Some(Ok(csp)) = csp.map(|csp| csp.parse()) {
        out.insert(header::CONTENT_SECURITY_POLICY, csp);
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
    match hub.bearer(ip, &headers) {
        // A native client: no cookie, so no Origin to check, but the key must be the
        // session's (or the session unknown, as just after a restart).
        Some(Ok(key)) => {
            let owner = hub.sessions.lock().unwrap().get(&sid).map(|s| s.key.clone());
            if owner.is_some_and(|name| name != key.name) {
                hub.fail(ip);
                return StatusCode::NOT_FOUND.into_response();
            }
        }
        Some(Err(refused)) => return refused,
        None => {
            // Sibling session hosts are the same site, so the cookie alone isn't enough.
            if !origin_is(&headers, &hub.public.session_origin(&sid)) {
                hub.fail(ip);
                return StatusCode::FORBIDDEN.into_response();
            }
            if !session_allowed(&hub, &sid, &headers) {
                hub.fail(ip);
                return StatusCode::NOT_FOUND.into_response();
            }
        }
    }
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
    upgrade.on_upgrade(move |ws| browser(hub, sid, ws))
}

async fn browser(hub: Arc<Hub>, sid: String, ws: WebSocket) {
    let c = hub.next();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let backlog = Backlog::new(hub.queued.clone(), hub.max_queued);
    let agent = {
        let mut sessions = hub.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(&sid) else {
            return;
        };
        let Some(agent) = session.agent.clone() else {
            return;
        };
        session.clients.insert(
            c,
            Client {
                tx,
                backlog: backlog.clone(),
            },
        );
        session.last_activity = state::now();
        agent
    };
    let _ = agent.send(AgentOut::Frame(ToAgent::Open { c }));
    let (mut sink, mut stream) = ws.split();
    let writing = backlog.clone();
    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let (message, size) = match out {
                Out::Text(text) => {
                    let size = text.len();
                    (Message::Text(text.into()), size)
                }
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
            writing.sent(size);
        }
        let _ = sink.close().await;
    });
    loop {
        let message = tokio::select! {
            message = stream.next() => message,
            () = backlog.over.notified() => break,
        };
        match message {
            Some(Ok(Message::Text(text))) => {
                let frame = AgentOut::Frame(ToAgent::Msg {
                    c,
                    data: text.to_string(),
                });
                if agent.send(frame).is_err() {
                    break;
                }
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            Some(Ok(_)) => {}
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
    let auth = bearer_value(&headers).unwrap_or("");
    let Some(key) = hub.tokens.read().unwrap().by_auth(auth).cloned() else {
        hub.fail(ip);
        eprintln!("agent rejected from {ip}: unknown key");
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .max_message_size(protocol::MAX_AGENT_FRAME)
        .max_frame_size(protocol::MAX_AGENT_FRAME)
        .on_upgrade(move |ws| agent_socket(hub, key, ip, ws))
}

async fn agent_socket(hub: Arc<Hub>, key: state::Key, ip: IpAddr, ws: WebSocket) {
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
    let Some(ToHub::Hello {
        proto,
        sid,
        label,
        mid,
        e2e,
        sealed,
    }) = hello
    else {
        let _ = sink
            .send(reject(close::BAD_HELLO, "expected a hello frame"))
            .await;
        return;
    };
    if !(protocol::MIN_PROTO..=protocol::PROTO).contains(&proto) {
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
    if mid.as_deref().is_some_and(|m| !protocol::valid_mid(m))
        || (e2e && mid.is_none())
        || sealed.as_ref().is_some_and(|s| s.len() > protocol::MAX_SEALED)
    {
        let _ = sink
            .send(reject(close::BAD_HELLO, "malformed encryption fields"))
            .await;
        return;
    }
    // The hub keeps these for as long as the session lasts.
    if [&label.host, &label.cwd, &label.model]
        .iter()
        .any(|text| text.len() > protocol::MAX_SEALED)
    {
        let _ = sink.send(reject(close::BAD_HELLO, "label too long")).await;
        return;
    }
    let generation = hub.next();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let backlog = Backlog::new(hub.queued.clone(), hub.max_queued);
    let refused = {
        let mut sessions = hub.sessions.lock().unwrap();
        let taken = sessions.get(&sid).is_some_and(|s| s.key != key.name);
        let count = sessions
            .iter()
            .filter(|(s, session)| **s != sid && session.key == key.name)
            .count();
        if taken {
            Some((close::SID_TAKEN, "that session id is in use"))
        } else if count >= MAX_SESSIONS_PER_KEY {
            Some((close::TOO_MANY, "this key has too many sessions"))
        } else {
            let session = sessions.entry(sid.clone()).or_insert_with(|| Session {
                key: key.name.clone(),
                auth_hash: key.auth_hash.clone(),
                label: label.clone(),
                busy: false,
                approval: false,
                title: String::new(),
                mid: None,
                e2e: false,
                sealed_label: None,
                sealed_status: None,
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
            session.agent = Some(AgentLink {
                tx,
                backlog: backlog.clone(),
            });
            session.generation = generation;
            session.offline_since = None;
            session.label = label;
            session.mid = mid;
            session.e2e = e2e;
            session.sealed_label = sealed;
            session.auth_hash = key.auth_hash.clone();
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
        "agent connected: key {} session {} from {ip}",
        key.name,
        short(&sid)
    );
    hub.touch(&key.name);
    let welcome = serde_json::to_string(&ToAgent::Welcome {
        proto: protocol::PROTO,
    })
    .unwrap_or_default();
    if sink.send(Message::Text(welcome.into())).await.is_err() {
        return;
    }
    let writing = backlog.clone();
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(30));
        ping.tick().await;
        loop {
            let (message, size) = tokio::select! {
                out = rx.recv() => {
                    let size = out.as_ref().map_or(0, AgentOut::size);
                    match out {
                        Some(AgentOut::Frame(frame)) => (Message::Text(serde_json::to_string(&frame).unwrap_or_default().into()), size),
                        Some(AgentOut::Close(code, reason)) => {
                            let _ = sink.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
                            return;
                        }
                        None => return,
                    }
                }
                _ = ping.tick() => (Message::Ping(Vec::new().into()), 0),
            };
            if sink.send(message).await.is_err() {
                return;
            }
            writing.sent(size);
        }
    });
    loop {
        let next = tokio::select! {
            // Anything at all (pongs included) within 75 s, or the agent counts as gone.
            next = tokio::time::timeout(Duration::from_secs(75), stream.next()) => next,
            () = backlog.over.notified() => break,
        };
        let Ok(Some(Ok(message))) = next else {
            break;
        };
        let Message::Text(text) = message else {
            if matches!(message, Message::Close(_)) {
                break;
            }
            continue;
        };
        let Ok(frame) = serde_json::from_str::<ToHub>(&text) else {
            continue;
        };
        if matches!(frame, ToHub::Status { .. }) {
            // A session in use keeps its key from expiring.
            hub.touch(&key.name);
        }
        let mut sessions = hub.sessions.lock().unwrap();
        let Some(session) = sessions
            .get_mut(&sid)
            .filter(|s| s.generation == generation)
        else {
            break;
        };
        match frame {
            ToHub::Msg { c, data } => {
                if let Some(client) = session.clients.get(&c)
                    && client.backlog.add(data.len())
                {
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
                sealed,
            } => {
                session.busy = busy;
                session.approval = approval;
                if title.len() <= protocol::MAX_SEALED {
                    session.title = title;
                }
                session.sealed_status = sealed.filter(|s| s.len() <= protocol::MAX_SEALED);
                if !model.is_empty() && model.len() <= protocol::MAX_SEALED {
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
            "agent disconnected: key {} session {}",
            key.name,
            short(&sid)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_hubs_page_may_frame_a_session() {
        let hub = "https://hub.example";
        assert_eq!(
            framed_by_hub("default-src 'self'; frame-ancestors 'none'", hub),
            "default-src 'self'; frame-ancestors https://hub.example"
        );
        assert_eq!(
            framed_by_hub("sandbox allow-scripts; frame-ancestors 'self'", hub),
            "sandbox allow-scripts; frame-ancestors 'self' https://hub.example"
        );
        assert_eq!(framed_by_hub("default-src 'self'", hub), "default-src 'self'");
    }

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

    #[test]
    fn rate_limits_go_by_the_network_not_the_address() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        // Anywhere in a /64 is one customer; the next /64 is another.
        assert_eq!(limit_bucket(ip("2001:db8:1:2:aaaa::1")), ip("2001:db8:1:2::"));
        assert_eq!(limit_bucket(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")), ip("2001:db8:1:2::"));
        assert_ne!(limit_bucket(ip("2001:db8:1:3::1")), ip("2001:db8:1:2::"));
        // IPv4 is itself, however it arrives.
        assert_eq!(limit_bucket(ip("192.0.2.7")), ip("192.0.2.7"));
        assert_eq!(limit_bucket(ip("::ffff:192.0.2.7")), ip("192.0.2.7"));
    }

    #[test]
    fn a_request_too_long_to_rewrite_isnt_routed_by_its_own_path() {
        let public = Public::parse("https://hub.example", false).unwrap();
        // The longest URI there is, whose rewritten form won't parse.
        let query = "a".repeat(65534 - "/s/x/ws?".len());
        let request = Request::builder()
            .uri(format!("/s/x/ws?{query}"))
            .header(header::HOST, "hub.example")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(rewrite(&public, request).uri().path(), "/x/none");

        let request = Request::builder()
            .uri("/s/x/ws?a=1")
            .header(header::HOST, "hub.example")
            .body(axum::body::Body::empty())
            .unwrap();
        let rewritten = rewrite(&public, request);
        assert_eq!(rewritten.uri().path(), "/d/s/x/ws");
        assert_eq!(rewritten.uri().query(), Some("a=1"));
    }

    #[test]
    fn only_https_hubs_ask_browsers_to_stay_on_https() {
        let header = |secure| {
            https_only(secure, StatusCode::OK.into_response())
                .headers()
                .get(header::STRICT_TRANSPORT_SECURITY)
                .cloned()
        };
        assert_eq!(header(false), None);
        assert_eq!(header(true).unwrap(), "max-age=31536000");
    }

    #[test]
    fn a_backlog_refuses_what_wont_fit_and_gives_it_all_back() {
        let total = Arc::new(AtomicUsize::new(0));
        let one = Backlog::new(total.clone(), 100);
        let two = Backlog::new(total.clone(), 100);
        assert!(one.add(100));
        assert!(!one.add(1), "over this socket's limit");
        assert_eq!(total.load(Ordering::Relaxed), 100, "what didn't fit isn't counted");
        one.sent(50);
        assert!(one.add(50));
        assert!(two.add(100));

        // The hub's limit applies however many sockets it's spread over.
        let others: Vec<_> = (0..2).map(|_| Backlog::new(total.clone(), 100)).collect();
        assert!(others[0].add(100) && others[1].add(100));
        assert_eq!(total.load(Ordering::Relaxed), 100 * QUEUED_SOCKETS);
        let last = Backlog::new(total.clone(), 100);
        assert!(!last.add(1), "over the hub's limit");

        drop((one, two, others, last));
        assert_eq!(total.load(Ordering::Relaxed), 0, "what was waiting goes with its socket");
    }

    mod end_to_end {
        use super::super::*;
        use crate::state::{Key, Store, new_key};
        use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

        fn new_auth() -> String {
            let key = new_key().unwrap();
            protocol::auth_value(&protocol::decode_key(&key).unwrap())
        }

        struct Setup {
            port: u16,
            /// The auth value of the key `mine`, and of `other`.
            auth: String,
            other_auth: String,
            store: Store,
            hub: Arc<Hub>,
        }

        async fn start() -> Setup {
            start_with(MAX_QUEUED).await
        }

        /// With `max_queued` bytes allowed to wait for each socket.
        async fn start_with(max_queued: usize) -> Setup {
            let dir = std::env::temp_dir().join(format!(
                "tamale-hub-e2e-{}",
                state::hex(&state::random::<8>().unwrap())
            ));
            let store = Store::open(Some(dir)).unwrap();
            let (auth, other_auth) = (new_auth(), new_auth());
            store
                .edit(|s| {
                    s.keys.push(Key::new("mine", &auth, false));
                    s.keys.push(Key::new("other", &other_auth, false));
                    Ok(())
                })
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
            let mut hub = Hub::new(
                public,
                Store {
                    dir: store.dir.clone(),
                },
                Signup {
                    open: true,
                    max_keys: 4,
                },
            )
            .unwrap();
            Arc::get_mut(&mut hub).unwrap().max_queued = max_queued;
            tokio::spawn(run(hub.clone(), listener));
            Setup {
                port,
                auth,
                other_auth,
                store,
                hub,
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
                mid: None,
                e2e: false,
                sealed: None,
            };
            agent
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&hello).unwrap().into(),
                ))
                .await
                .unwrap();
            agent
        }

        /// Compares `value` with the iOS app's checked-in sample (see `apple/ios.md`), or
        /// rewrites it with `UPDATE_FIXTURES` set.
        fn check_fixture(name: &str, value: &Value) {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../apple/Fixtures")
                .join(name);
            let text = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
            if std::env::var_os("UPDATE_FIXTURES").is_some() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &text).unwrap();
                return;
            }
            let old = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                old == text,
                "apple/Fixtures/{name} is stale; run `UPDATE_FIXTURES=1 cargo test -p tamale-hub` and check it in"
            );
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

            // A key itself is refused, so a bug can't send one unnoticed; so is a stranger.
            let key = new_key().unwrap();
            for (add, status) in [(key.as_str(), 400), ("f00d", 401)] {
                let r = http
                    .post(format!("{base}/api/tokens"))
                    .header("host", &dash)
                    .header("origin", &origin)
                    .json(&json!({"add": add}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(r.status(), status, "{add}");
            }
            assert!(
                ws(
                    port,
                    "/agent",
                    &[
                        ("host", dash.clone()),
                        ("authorization", format!("Bearer {}", new_auth()))
                    ]
                )
                .await
                .is_err(),
                "an unknown key doesn't register by connecting"
            );

            let mut agent = agent_hello(port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));

            // The same sid under another key is refused.
            let mut thief = agent_hello(port, &setup.other_auth, &sid).await;
            assert_eq!(close_code(&mut thief).await, close::SID_TAKEN);

            // Wrong origin on the token endpoint.
            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", "http://evil.example")
                .json(&json!({"add": setup.auth}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 403);

            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", &origin)
                .json(&json!({"add": setup.auth}))
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

            // The other key sees nothing and can't get a ticket.
            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", &origin)
                .json(&json!({"add": setup.other_auth}))
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

            // A reconnect with the same key takes over; the tab is closed normally to retry.
            let mut agent2 = agent_hello(port, &setup.auth, &sid).await;
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

            // Revoking the key (as the CLI would) drops the session within seconds.
            let mut agent3 = agent_hello(port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent3).await.contains("\"welcome\""));
            tokio::time::sleep(Duration::from_millis(1100)).await;
            setup
                .store
                .edit(|s| {
                    s.keys.retain(|k| k.name != "mine");
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
            assert_eq!(r.status(), 404, "the session cookie dies with its key");
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn universal_links_are_served_only_when_configured() {
            let setup = start().await;
            let base = format!("http://127.0.0.1:{}", setup.port);
            let dash = format!("hub.localhost:{}", setup.port);
            let http = reqwest::Client::new();
            let get = || {
                http.get(format!("{base}/.well-known/apple-app-site-association"))
                    .header("host", &dash)
                    .send()
            };
            assert_eq!(get().await.unwrap().status(), 404);
            setup.hub.apple_app_id.set("TEAM.com.example.tamale".into()).unwrap();
            let r = get().await.unwrap();
            assert_eq!(r.status(), 200);
            assert_eq!(r.headers()["content-type"], "application/json");
            let body: Value = r.json().await.unwrap();
            assert_eq!(body["applinks"]["details"][0]["appIDs"][0], "TEAM.com.example.tamale");
            assert_eq!(body["applinks"]["details"][0]["components"][0]["/"], "/open/*");
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn a_native_client_uses_a_bearer_key_without_cookies_tickets_or_origin() {
            let setup = start().await;
            let port = setup.port;
            let dash = format!("hub.localhost:{port}");
            let sid = "0123456789abcdef0123456789abcdef".to_string();
            let other_sid = "fedcba9876543210fedcba9876543210".to_string();
            let session_host = format!("{sid}.hub.localhost:{port}");
            let base = format!("http://127.0.0.1:{port}");
            let http = reqwest::Client::new();
            let failures = |hub: &Hub| hub.failures.lock().unwrap().values().map(|f| f.0).sum::<u32>();

            let mut agent = agent_hello(port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            let mut other_agent = agent_hello(port, &setup.other_auth, &other_sid).await;
            assert!(next_text(&mut other_agent).await.contains("\"welcome\""));
            let bearer = format!("Bearer {}", setup.auth);

            // The list: only this key's sessions, with no cookie and no Origin.
            let r = http
                .get(format!("{base}/api/sessions"))
                .header("host", &dash)
                .header("authorization", &bearer)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            assert!(r.headers().get("set-cookie").is_none());
            let list: Value = r.json().await.unwrap();
            assert_eq!(list["keys"].as_array().unwrap().len(), 1);
            assert_eq!(list["keys"][0]["name"], "mine");
            let sids: Vec<&str> = list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["sid"].as_str().unwrap())
                .collect();
            assert_eq!(sids, [sid.as_str()]);

            // An encrypted session too, for the app's sample of this list.
            let e2e_sid = "feedfacefeedfacefeedfacefeedface";
            let mut e2e_agent = ws(
                port,
                "/agent",
                &[("host", dash.clone()), ("authorization", bearer.clone())],
            )
            .await
            .unwrap();
            let say = |frame: &ToHub| {
                tungstenite::Message::Text(serde_json::to_string(frame).unwrap().into())
            };
            e2e_agent
                .send(say(&ToHub::Hello {
                    proto: protocol::PROTO,
                    sid: e2e_sid.into(),
                    label: Label::default(),
                    mid: Some("a".repeat(32)),
                    e2e: true,
                    sealed: Some("SEALED-LABEL".into()),
                }))
                .await
                .unwrap();
            assert!(next_text(&mut e2e_agent).await.contains("\"welcome\""));
            e2e_agent
                .send(say(&ToHub::Status {
                    busy: true,
                    approval: true,
                    title: String::new(),
                    model: String::new(),
                    sealed: Some("SEALED-STATUS".into()),
                }))
                .await
                .unwrap();
            let mut list: Value = loop {
                let list: Value = http
                    .get(format!("{base}/api/sessions"))
                    .header("host", &dash)
                    .header("authorization", &bearer)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if list["sessions"][0]["sealed_status"].is_string() {
                    break list;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            list["keys"][0]["id"] = "0123456789abcdef".into();
            list["keys"][0]["created"] = 1_700_000_000.into();
            for row in list["sessions"].as_array_mut().unwrap() {
                row["idle"] = 0.into();
            }
            check_fixture("hub/sessions.json", &list);
            drop(e2e_agent);

            // A wrong key is a 401 and counts as a failure, everywhere.
            let before = failures(&setup.hub);
            let wrong = format!("Bearer {}", new_auth());
            let r = http
                .get(format!("{base}/api/sessions"))
                .header("host", &dash)
                .header("authorization", &wrong)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401);
            let r = http
                .post(format!("{base}/api/ticket"))
                .header("host", &dash)
                .header("authorization", &wrong)
                .json(&json!({"sid": sid}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 401);
            match ws(
                port,
                "/ws",
                &[("host", session_host.clone()), ("authorization", wrong.clone())],
            )
            .await
            {
                Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
                other => panic!("{:?}", other.map(|_| ())),
            }
            assert_eq!(failures(&setup.hub), before + 3);

            // A ticket needs no Origin with a bearer; another key's session is a 404.
            let r = http
                .post(format!("{base}/api/ticket"))
                .header("host", &dash)
                .header("authorization", &bearer)
                .json(&json!({"sid": sid}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            let r = http
                .post(format!("{base}/api/ticket"))
                .header("host", &dash)
                .header("authorization", &bearer)
                .json(&json!({"sid": other_sid}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 404);
            let other_host = format!("{other_sid}.hub.localhost:{port}");
            match ws(
                port,
                "/ws",
                &[("host", other_host), ("authorization", bearer.clone())],
            )
            .await
            {
                Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 404),
                other => panic!("{:?}", other.map(|_| ())),
            }

            // The socket: no cookie, no Origin, no ticket.
            let mut tab = ws(
                port,
                "/ws",
                &[("host", session_host.clone()), ("authorization", bearer.clone())],
            )
            .await
            .unwrap();
            let open: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
            let ToAgent::Open { c } = open else {
                panic!("{open:?}")
            };
            tab.send(tungstenite::Message::Text("{\"t\":\"hs1\"}".into()))
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_str::<ToAgent>(&next_text(&mut agent).await).unwrap(),
                ToAgent::Msg {
                    c,
                    data: "{\"t\":\"hs1\"}".into()
                }
            );

            // The cookie path is unchanged: without a bearer, Origin is still required.
            assert!(
                ws(port, "/ws", &[("host", session_host.clone())])
                    .await
                    .is_err()
            );
            let r = http
                .post(format!("{base}/api/ticket"))
                .header("host", &dash)
                .json(&json!({"sid": sid}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 403);
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn an_encrypted_session_shows_the_hub_only_opaque_labels_and_keeps_its_artifacts() {
            let setup = start().await;
            let port = setup.port;
            let dash = format!("hub.localhost:{port}");
            let origin = format!("http://{dash}");
            let sid = "feedfacefeedfacefeedfacefeedface".to_string();
            let session_host = format!("{sid}.hub.localhost:{port}");
            let base = format!("http://127.0.0.1:{port}");
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let auth = [
                ("host", dash.clone()),
                ("authorization", format!("Bearer {}", setup.auth)),
            ];
            let say = |frame: &ToHub| {
                tungstenite::Message::Text(serde_json::to_string(frame).unwrap().into())
            };
            let hello = |sid: &str, mid: Option<&str>, e2e, sealed: Option<String>| ToHub::Hello {
                proto: protocol::PROTO,
                sid: sid.into(),
                label: Label::default(),
                mid: mid.map(String::from),
                e2e,
                sealed,
            };

            // Malformed encryption fields are refused.
            let other = "1".repeat(32);
            for bad in [
                hello(&other, None, true, None),
                hello(&other, Some("NOT-HEX"), true, None),
                hello(&other, Some(&"a".repeat(32)), true, Some("x".repeat(protocol::MAX_SEALED + 1))),
            ] {
                let mut agent = ws(port, "/agent", &auth).await.unwrap();
                agent.send(say(&bad)).await.unwrap();
                assert_eq!(close_code(&mut agent).await, close::BAD_HELLO);
            }
            // So is a protocol from the future; an older tamale still connects, in plaintext.
            let mut agent = ws(port, "/agent", &auth).await.unwrap();
            let future = r#"{"t":"hello","proto":99,"sid":"11111111111111111111111111111111","label":{"host":"","cwd":"","model":""}}"#;
            agent.send(tungstenite::Message::Text(future.into())).await.unwrap();
            assert_eq!(close_code(&mut agent).await, close::BAD_HELLO);
            let mut old = ws(port, "/agent", &auth).await.unwrap();
            let v1 = r#"{"t":"hello","proto":1,"sid":"22222222222222222222222222222222","label":{"host":"old","cwd":"~","model":"m"}}"#;
            old.send(tungstenite::Message::Text(v1.into())).await.unwrap();
            assert!(next_text(&mut old).await.contains("\"welcome\""));

            let mid = "a".repeat(32);
            let mut agent = ws(port, "/agent", &auth).await.unwrap();
            agent
                .send(say(&hello(&sid, Some(&mid), true, Some("SEALED-LABEL".into()))))
                .await
                .unwrap();
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            agent
                .send(say(&ToHub::Status {
                    busy: true,
                    approval: false,
                    title: String::new(),
                    model: String::new(),
                    sealed: Some("SEALED-STATUS".into()),
                }))
                .await
                .unwrap();

            let r = http.post(format!("{base}/api/tokens")).header("host", &dash)
                .header("origin", &origin).json(&json!({"add": setup.auth})).send().await.unwrap();
            let th = set_cookie(&r);

            let session = loop {
                let list: Value = http.get(format!("{base}/api/sessions")).header("host", &dash)
                    .header("cookie", &th).send().await.unwrap().json().await.unwrap();
                let row = list["sessions"].as_array().unwrap().iter()
                    .find(|s| s["sid"] == sid.as_str()).cloned().unwrap();
                if row["sealed_status"] == "SEALED-STATUS" {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            assert_eq!(session["mid"], mid.as_str());
            assert_eq!(session["e2e"], true);
            assert_eq!(session["sealed_label"], "SEALED-LABEL");
            assert_eq!(session["busy"], true, "the list can still badge it");
            for field in ["host", "cwd", "model", "title"] {
                assert_eq!(session[field], "", "{field}");
            }

            let ticket: Value = http.post(format!("{base}/api/ticket")).header("host", &dash)
                .header("origin", &origin).header("cookie", &th).json(&json!({"sid": sid}))
                .send().await.unwrap().json().await.unwrap();
            assert_eq!(ticket["mid"], mid.as_str());
            assert_eq!(ticket["e2e"], true);
            let path = ticket["url"].as_str().unwrap().split_once(&session_host).unwrap().1.to_string();
            let r = http.get(format!("{base}{path}")).header("host", &session_host).send().await.unwrap();
            assert_eq!(r.status(), 303);
            assert_eq!(r.headers()["location"], "/?e2e=1", "the page must insist on the handshake");
            let ts = set_cookie(&r);

            // Artifacts aren't asked of tamale at all; the frame page and script are.
            let r = http.get(format!("{base}/artifact/abc")).header("host", &session_host)
                .header("cookie", &ts).send().await.unwrap();
            assert_eq!(r.status(), 404);
            let tunnelled = http.get(format!("{base}/artifact-frame")).header("host", &session_host)
                .header("cookie", &ts).send();
            let (r, get) = tokio::join!(tunnelled, next_text(&mut agent));
            assert!(get.contains("\"/artifact-frame\""), "{get}");
            assert!(!get.contains("/artifact/abc"), "{get}");
            let _ = r;

            // The hub's page has what it needs for the keys.
            let r = http.get(format!("{base}/seal.js")).header("host", &dash).send().await.unwrap();
            assert_eq!(r.status(), 200);
            assert!(r.text().await.unwrap().contains("machineId"));
            let r = http.get(format!("{base}/qrcode.js")).header("host", &dash).send().await.unwrap();
            assert_eq!(r.status(), 200);
        }

        #[tokio::test]
        async fn the_page_registers_rotates_and_deletes_keys() {
            let setup = start().await;
            let port = setup.port;
            let dash = format!("hub.localhost:{port}");
            let origin = format!("http://{dash}");
            let base = format!("http://127.0.0.1:{port}");
            let http = reqwest::Client::new();
            let post = |path: &str, cookie: &str, body: Value| {
                http.post(format!("{base}{path}"))
                    .header("host", &dash)
                    .header("origin", &origin)
                    .header("cookie", cookie)
                    .json(&body)
                    .send()
            };
            let auth = new_auth();

            // Another site can't make keys in a visitor's name.
            let r = http.post(format!("{base}/api/keys")).header("host", &dash)
                .header("origin", "http://evil.example").json(&json!({ "auth": auth })).send().await.unwrap();
            assert_eq!(r.status(), 403);

            // Only well-formed auth values, and never a key.
            for bad in ["short", &"A".repeat(64), &new_key().unwrap()] {
                let r = post("/api/keys", "", json!({ "auth": bad })).await.unwrap();
                assert_eq!(r.status(), 400, "{bad}");
            }

            let r = post("/api/keys", "", json!({ "auth": auth, "label": "My Laptop" })).await.unwrap();
            assert_eq!(r.status(), 200);
            let th = set_cookie(&r);
            let made: Value = r.json().await.unwrap();
            assert_eq!(made["name"], "my-laptop");
            let stored = setup.store.load().unwrap();
            assert!(stored.by_name("my-laptop").unwrap().web && stored.by_auth(&auth).is_some());

            // Nothing about the key comes back, and it can't be registered twice.
            assert_eq!(made.as_object().unwrap().len(), 1);
            let r = post("/api/keys", "", json!({ "auth": auth })).await.unwrap();
            assert_eq!(r.status(), 409);

            // The browser that made it holds it, and machines can connect with it.
            let list: Value = http.get(format!("{base}/api/sessions")).header("host", &dash)
                .header("cookie", &th).send().await.unwrap().json().await.unwrap();
            assert_eq!(list["keys"][0]["name"], "my-laptop");
            assert_eq!(list["keys"][0]["id"], state::hash(&auth)[..16]);
            assert_eq!(list["signup"], true);
            let sid = "abcdef0123456789abcdef0123456789";
            let mut a = agent_hello(port, &auth, sid).await;
            assert!(next_text(&mut a).await.contains("\"welcome\""));

            // Two CLI keys plus this one and another, and the test hub holds at most four.
            let r = post("/api/keys", "", json!({ "auth": new_auth() })).await.unwrap();
            assert_eq!(r.status(), 200);
            let r = post("/api/keys", "", json!({ "auth": new_auth() })).await.unwrap();
            assert_eq!(r.status(), 503);

            // Only a holder can replace a key, and only with a well-formed auth value.
            let new = new_auth();
            let r = post("/api/keys/rotate", "", json!({ "name": "my-laptop", "auth": new })).await.unwrap();
            assert_eq!(r.status(), 404);
            let r = post("/api/keys/rotate", &th, json!({ "name": "my-laptop", "auth": "short" })).await.unwrap();
            assert_eq!(r.status(), 400);
            let r = post("/api/keys/rotate", &th, json!({ "name": "my-laptop", "auth": setup.auth })).await.unwrap();
            assert_eq!(r.status(), 409, "not to another key's");
            let r = post("/api/keys/rotate", &th, json!({ "name": "my-laptop", "auth": new })).await.unwrap();
            assert_eq!(r.status(), 200);
            let new_th = set_cookie(&r);
            assert_ne!(new_th, th);
            // Its sessions end, the old key no longer lists anything or connects.
            let code = tokio::time::timeout(Duration::from_secs(6), close_code(&mut a)).await.unwrap();
            assert_eq!(code, close::BAD_TOKEN);
            let old: Value = http.get(format!("{base}/api/sessions")).header("host", &dash)
                .header("cookie", &th).send().await.unwrap().json().await.unwrap();
            assert_eq!(old["keys"].as_array().unwrap().len(), 0);
            assert!(
                ws(port, "/agent", &[("host", dash.clone()), ("authorization", format!("Bearer {auth}"))])
                    .await
                    .is_err()
            );
            let mut b = agent_hello(port, &new, sid).await;
            assert!(next_text(&mut b).await.contains("\"welcome\""));

            let r = post("/api/keys/delete", &new_th, json!({ "name": "my-laptop" })).await.unwrap();
            assert_eq!(r.status(), 200);
            assert!(r.headers()["set-cookie"].to_str().unwrap().contains("Max-Age=0"));
            assert!(setup.store.load().unwrap().by_name("my-laptop").is_none());
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn closed_signup_refuses_new_keys_and_the_daily_limit_holds() {
            let setup = start().await;
            let dir = setup.store.dir.clone();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
            let closed = Hub::new(public, Store { dir: dir.clone() }, Signup { open: false, max_keys: 500 }).unwrap();
            tokio::spawn(run(closed, listener));
            let dash = format!("hub.localhost:{port}");
            let http = reqwest::Client::new();
            let r = http.post(format!("http://127.0.0.1:{port}/api/keys")).header("host", &dash)
                .header("origin", format!("http://{dash}")).json(&json!({ "auth": new_auth() }))
                .send().await.unwrap();
            assert_eq!(r.status(), 403);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
            let open = Hub::new(public, Store { dir: dir.clone() }, Signup { open: true, max_keys: 500 }).unwrap();
            tokio::spawn(run(open, listener));
            let dash = format!("hub.localhost:{port}");
            for n in 0..=SIGNUPS_PER_IP {
                let r = http.post(format!("http://127.0.0.1:{port}/api/keys")).header("host", &dash)
                    .header("origin", format!("http://{dash}")).json(&json!({ "auth": new_auth() }))
                    .send().await.unwrap();
                assert_eq!(r.status(), if n < SIGNUPS_PER_IP { 200 } else { 429 }, "key {n}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn unused_page_keys_expire_but_cli_keys_stay() {
            let dir = std::env::temp_dir().join(format!("tamale-hub-expire-{}", state::hex(&state::random::<8>().unwrap())));
            let store = Store::open(Some(dir.clone())).unwrap();
            store.edit(|s| {
                let mut old_web = Key::new("old-web", "a1", true);
                old_web.created = 0;
                let mut idle_web = Key::new("idle-web", "a2", true);
                idle_web.last_used = Some(state::now() - IDLE_EXPIRY - 10);
                let mut old_cli = Key::new("old-cli", "a3", false);
                old_cli.created = 0;
                let fresh_web = Key::new("fresh-web", "a4", true);
                s.keys.extend([old_web, idle_web, old_cli, fresh_web]);
                Ok(())
            }).unwrap();
            let public = Public::parse("http://hub.localhost:1", true).unwrap();
            let hub = Hub::new(public, Store { dir: dir.clone() }, Signup { open: true, max_keys: 10 }).unwrap();
            hub.sweep();
            let names: Vec<String> = store.load().unwrap().keys.into_iter().map(|k| k.name).collect();
            assert_eq!(names, ["old-cli", "fresh-web"]);
            let _ = std::fs::remove_dir_all(&dir);
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
            assert_eq!(r.headers()["x-frame-options"], "SAMEORIGIN");
            let csp = r.headers()["content-security-policy"].to_str().unwrap();
            assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
            assert!(csp.contains(&format!("frame-src 'self' http://*.hub.localhost:{}", setup.port)), "{csp}");
            assert!(r.text().await.unwrap().contains("tamale-hub"));
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        /// Asks `check` every so often for a few seconds, until it's true.
        async fn until(what: &str, check: impl Fn() -> bool) {
            for _ in 0..200 {
                if check() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("never happened: {what}");
        }

        /// A browser's socket, by the native route, with a key.
        async fn tab(port: u16, sid: &str, auth: &str) -> Ws {
            ws(
                port,
                "/ws",
                &[
                    ("host", format!("{sid}.hub.localhost:{port}")),
                    ("authorization", format!("Bearer {auth}")),
                ],
            )
            .await
            .unwrap()
        }

        #[tokio::test]
        async fn a_tab_that_stops_reading_is_cut_off_rather_than_filling_the_hub() {
            let setup = start_with(1 << 20).await;
            let sid = "1".repeat(32);
            let mut agent = agent_hello(setup.port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            // Never read from again.
            let _stuck = tab(setup.port, &sid, &setup.auth).await;
            let opened = next_text(&mut agent).await;
            let Ok(ToAgent::Open { c }) = serde_json::from_str(&opened) else {
                panic!("{opened}");
            };

            // Far more than the limit, and than the socket's own buffers take.
            let piece = "x".repeat(1 << 20);
            let frame = serde_json::to_string(&ToHub::Msg { c, data: piece }).unwrap();
            let pump = async {
                for _ in 0..512 {
                    if agent
                        .send(tungstenite::Message::Text(frame.clone().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(60), pump)
                .await
                .expect("the agent kept being read");
            let hub = setup.hub.clone();
            until("the tab being dropped", || {
                hub.sessions
                    .lock()
                    .unwrap()
                    .get(&sid)
                    .is_some_and(|s| s.clients.is_empty())
            })
            .await;
            let closed = serde_json::to_string(&ToAgent::Close { c }).unwrap();
            assert_eq!(next_text(&mut agent).await, closed, "tamale is told it's gone");
            until("what was queued being let go", || {
                hub.queued.load(Ordering::Relaxed) == 0
            })
            .await;
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn an_agent_that_stops_reading_is_cut_off_rather_than_filling_the_hub() {
            let setup = start_with(1 << 20).await;
            let sid = "2".repeat(32);
            let mut agent = agent_hello(setup.port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            let mut browser = tab(setup.port, &sid, &setup.auth).await;
            // The agent reads nothing from here on, but a tab keeps sending.
            let piece = "x".repeat(1 << 20);
            let pump = async {
                for _ in 0..512 {
                    if browser
                        .send(tungstenite::Message::Text(piece.clone().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(60), pump)
                .await
                .expect("the tab kept being read");
            let hub = setup.hub.clone();
            until("the agent being dropped", || {
                hub.sessions
                    .lock()
                    .unwrap()
                    .get(&sid)
                    .is_some_and(|s| s.agent.is_none())
            })
            .await;
            until("what was queued being let go", || {
                hub.queued.load(Ordering::Relaxed) == 0
            })
            .await;
            drop(agent);
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn labels_and_titles_the_hub_keeps_are_bounded() {
            let setup = start().await;
            let port = setup.port;
            let auth = [
                ("host", format!("hub.localhost:{port}")),
                ("authorization", format!("Bearer {}", setup.auth)),
            ];
            let say = |frame: &ToHub| {
                tungstenite::Message::Text(serde_json::to_string(frame).unwrap().into())
            };
            let long = "x".repeat(protocol::MAX_SEALED + 1);
            let sid = "3".repeat(32);
            let hello = |label| ToHub::Hello {
                proto: protocol::PROTO,
                sid: sid.clone(),
                label,
                mid: None,
                e2e: false,
                sealed: None,
            };
            for label in [
                Label { host: long.clone(), ..Label::default() },
                Label { cwd: long.clone(), ..Label::default() },
                Label { model: long.clone(), ..Label::default() },
            ] {
                let mut agent = ws(port, "/agent", &auth).await.unwrap();
                agent.send(say(&hello(label))).await.unwrap();
                assert_eq!(close_code(&mut agent).await, close::BAD_HELLO);
            }

            let mut agent = ws(port, "/agent", &auth).await.unwrap();
            agent.send(say(&hello(Label::default()))).await.unwrap();
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            let status = |title: &str, model: &str| ToHub::Status {
                busy: false,
                approval: false,
                title: title.into(),
                model: model.into(),
                sealed: None,
            };
            agent.send(say(&status("short", "m"))).await.unwrap();
            agent.send(say(&status(&long, &long))).await.unwrap();
            agent.send(say(&status("t", "last"))).await.unwrap();
            let hub = setup.hub.clone();
            let shown = || {
                let sessions = hub.sessions.lock().unwrap();
                let session = sessions.get(&sid).unwrap();
                (session.title.clone(), session.label.model.clone())
            };
            until("the last status", || shown().1 == "last").await;
            assert_eq!(shown().0, "t");
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn a_ticket_says_which_key_its_session_is_under_and_a_key_cant_hoard_them() {
            let setup = start().await;
            let port = setup.port;
            let sid = "4".repeat(32);
            let mut agent = agent_hello(port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            let base = format!("http://127.0.0.1:{port}");
            let http = reqwest::Client::new();
            let ask = |auth: String| {
                http.post(format!("{base}/api/ticket"))
                    .header("host", format!("hub.localhost:{port}"))
                    .bearer_auth(auth)
                    .json(&json!({ "sid": sid }))
                    .send()
            };
            let first = ask(setup.auth.clone()).await.unwrap();
            assert_eq!(first.status(), 200);
            let first: Value = first.json().await.unwrap();
            assert_eq!(first["id"], state::hash(&setup.auth)[..16]);
            for _ in 1..MAX_TICKETS_PER_KEY {
                assert_eq!(ask(setup.auth.clone()).await.unwrap().status(), 200);
            }
            assert_eq!(ask(setup.auth.clone()).await.unwrap().status(), 429);
            // Another key's tickets are its own business: this one just doesn't have the session.
            assert_eq!(ask(setup.other_auth.clone()).await.unwrap().status(), 404);
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }

        #[tokio::test]
        async fn the_dashboard_keeps_to_its_own_origin() {
            let setup = start().await;
            let r = reqwest::Client::new()
                .get(format!("http://127.0.0.1:{}", setup.port))
                .header("host", format!("hub.localhost:{}", setup.port))
                .send()
                .await
                .unwrap();
            assert_eq!(r.headers()["cross-origin-opener-policy"], "same-origin");
            assert_eq!(r.headers()["cross-origin-resource-policy"], "same-origin");
            assert!(r.headers().get("strict-transport-security").is_none(), "this one's http");
            let _ = std::fs::remove_dir_all(&setup.store.dir);
        }
    }
}
