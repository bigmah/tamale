//! `tamale-hub serve`: the relay.
//!
//! Requests are told apart by `Host`: the public host (`tamale-hub.com`) is the hub's
//! own pages and the agent socket; `<sid>.tamale-hub.com` is one session, its own
//! origin, whose page, artifacts and socket are relayed to that session's tamale.
//! [`rewrite`] maps each request onto an internal path (`/d/…` or `/s/<sid>/…`) before
//! routing.
//!
//! The relay's state and what every request shares are here. The handlers are in [`api`]
//! (the public host's API), [`session`] (one session's host) and [`agent`] (tamale's
//! socket).
//!
//! Credentials: the hub never sees a key, only the auth value derived from it (see
//! `tamale_hub_protocol::auth_value`). The dashboard keeps the browser's auth values in an `HttpOnly` cookie
//! that only the public host ever sees. Opening a session trades it for a one-time
//! ticket, which the session host trades for a cookie of its own, signed and scoped to
//! that one session (see [`SessionCookie`]). A session's page, even a hostile one, so
//! never holds anything that reaches another session.
//!
//! Native clients (the iOS app, `apple/ios.md`) send the auth value as
//! `Authorization: Bearer` on `/api/sessions`, `/api/ticket` and the session socket instead.
//! Browsers never attach that header on their own, so those requests skip the `Origin`
//! checks that guard the cookies, and the socket needs no ticket round trip.

mod agent;
mod api;
mod session;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, ServiceExt};
use serde_json::json;
use tamale_hub_protocol::{self as protocol, Label, ToAgent, close};
use tokio::sync::{Notify, mpsc, oneshot};
use tower::Layer;

use crate::state::{self, State as Tokens, Store};
use agent::agent;
use api::{
    api_key_delete, api_key_new, api_key_rotate, api_sessions, api_sessions_forget, api_ticket,
    api_tokens,
};
use session::{auth, session_get, session_root, session_socket};

const PAGE: &str = include_str!("../static/index.html");
const SCRIPT: &str = include_str!("../static/hub.js");
const SEAL: &str = include_str!("../static/seal.js");
const QR: &str = include_str!("../static/qrcode.js");
const STYLE: &str = include_str!("../static/hub.css");

/// How long after a restart unknown sessions are assumed to be on their way back.
const GRACE: Duration = Duration::from_secs(600);
/// How long a disconnected session stays listed, and its pages keep waiting for it. Long
/// enough for a laptop that sleeps over a weekend: a session that ends on purpose says so
/// (see [`ToHub::End`]) and goes at once.
const OFFLINE_LIFE: Duration = Duration::from_secs(7 * 24 * 3600);
/// How often an idle socket is pinged, and how long it may stay silent before it counts as
/// gone (a phone that slept, a network that changed): pongs count.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENT_FOR: Duration = Duration::from_secs(75);
/// What a session's pages are told when it ends on purpose (see [`ToHub::End`]).
const SESSION_ENDED: &str = "This session ended.";
const CHEF_REPLACED: &str = "A new tamale chef took this one's place.";
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
    /// Whether it's a tamale chef: a key may have one online (see [`close::CHEF_TAKEN`]).
    chef: bool,
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
    body: Vec<u8>,
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
            .retain(|_, s| s.offline_since.is_none_or(|t| t.elapsed() < OFFLINE_LIFE));
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
        .route("/d/api/sessions/forget", post(api_sessions_forget))
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
    // A session in one of its frames opens an artifact in a tab of its own and sends it the
    // document (see artifact-frame.html). Under `same-origin` the browser cuts that tab
    // off from its opener, since the frame isn't this page's origin; this still keeps
    // whatever opens this page away from it.
    headers.insert(
        header::HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin-allow-popups"),
    );
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
