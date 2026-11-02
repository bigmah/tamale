//! `/web remote`: makes this session reachable through a tamale-hub.
//!
//! tamale connects out to `wss://<hub>/agent` with a key's auth value and says `hello` with
//! this process's session id (sid). The hub serves the session at
//! `https://<sid>.<hub>/`, and relays to it: page GETs (answered by [`super::asset`]) and
//! each browser tab's socket as a channel `c`, whose messages go to the same
//! [`super::handle`] a local tab's do. A relayed tab is a [`super::Client`] like any
//! other, so it gets the same `welcome` and updates. The frames are in
//! `tamale-hub-protocol`.
//!
//! With the key's handshake secret ([`e2e::Keys`]), a channel carries nothing but a Noise handshake and
//! then sealed messages, so the hub only moves opaque bytes: a tab exists only once its
//! handshake succeeds, and anything unsealed closes the channel. See `e2e.md`.
//!
//! The connection is retried with backoff until it's turned off, except when the hub
//! rejects it for good (a bad token, say).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tamale_hub_protocol::{self as protocol, ToAgent, ToHub};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};

use super::{Out, Shared, WebEvent, e2e};

pub use protocol::Label;

/// What the hub's session list shows of this session.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct HubStatus {
    pub busy: bool,
    pub approval: bool,
    pub title: String,
    pub model: String,
}

/// This process's session id: random, and the same for every connection it makes, so
/// the session's URL survives reconnects (and `/clear`, `/resume`).
pub fn sid() -> &'static str {
    static SID: OnceLock<String> = OnceLock::new();
    SID.get_or_init(|| {
        let mut bytes = [0u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            bytes = (nanos ^ u128::from(std::process::id())).to_le_bytes();
        }
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// The site of the hub whose agent socket is at `hub`
/// (`wss://tamale-hub.com` → `https://tamale-hub.com`).
fn site_url(hub: &str) -> String {
    let base = hub.trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        base.to_string()
    }
}

/// The link that opens session `sid` through the hub whose agent socket is at `hub`.
pub fn open_url(hub: &str, sid: &str) -> String {
    format!("{}/open/{sid}", site_url(hub))
}

/// This machine's name, for the session list.
pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates or
        // truncates within it.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).into_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "tamale".into())
}

/// Why tamale closed a channel, which the page acts on (see `src/web/app.js`).
const WRONG_KEY: &str = "e2e: wrong key";
const BROKEN: &str = "e2e: the encrypted connection broke; reload to reconnect";
const THROTTLED: &str = "e2e: too many failed handshakes; try again later";

/// Failed handshakes in a row before tamale starts turning new ones away.
const FREE_FAILURES: u32 = 5;

/// Backs off from a run of failed handshakes. 256-bit keys make guessing hopeless anyway,
/// but this stops the work and makes the abuse visible.
#[derive(Default)]
struct Throttle {
    failures: u32,
    until: Option<tokio::time::Instant>,
}

impl Throttle {
    fn blocked(&self) -> bool {
        self.until
            .is_some_and(|until| tokio::time::Instant::now() < until)
    }

    /// Counts a failure; true the first time it starts backing off.
    fn failed(&mut self) -> bool {
        self.failures += 1;
        if self.failures < FREE_FAILURES {
            return false;
        }
        let secs = 1u64 << (self.failures - FREE_FAILURES).min(8);
        self.until = Some(tokio::time::Instant::now() + Duration::from_secs(secs));
        self.failures == FREE_FAILURES
    }

    fn succeeded(&mut self) {
        *self = Self::default();
    }
}

/// A browser's channel.
enum Channel {
    /// In plaintext (`[hub] e2e = false`): a tab from the start.
    Plain(u64),
    /// Encrypted, and waiting for a handshake that answers this nonce.
    Waiting([u8; 16]),
    /// Encrypted, and a tab.
    Sealed(u64, Arc<Mutex<e2e::Channel>>),
}

impl Channel {
    fn client(&self) -> Option<u64> {
        match self {
            Self::Plain(id) | Self::Sealed(id, _) => Some(*id),
            Self::Waiting(_) => None,
        }
    }
}

pub struct Relay {
    /// The link that opens this session, for a browser that holds the key. Nothing secret.
    pub open_url: String,
    shared: Arc<Shared>,
    status: watch::Sender<HubStatus>,
    live: Arc<AtomicBool>,
    /// What's wrong with the connection right now, for the terminal's screen.
    problem: Arc<Mutex<Option<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    pub fn start(
        shared: Arc<Shared>,
        url: &str,
        token: &str,
        label: Label,
        keys: Option<e2e::Keys>,
    ) -> Self {
        let (status, status_rx) = watch::channel(HubStatus::default());
        let live = Arc::new(AtomicBool::new(false));
        let problem = Arc::new(Mutex::new(None));
        let open_url = open_url(url, sid());
        let task = tokio::spawn(run(
            shared.clone(),
            Target {
                url: format!("{}/agent", url.trim_end_matches('/')),
                token: token.to_string(),
                label,
                open_url: open_url.clone(),
                site: site_url(url),
                keys,
            },
            status_rx,
            live.clone(),
            problem.clone(),
        ));
        Self {
            open_url,
            shared,
            status,
            live,
            problem,
            task,
        }
    }

    /// Sends the hub `status` if it changed.
    pub fn status(&self, status: &HubStatus) {
        self.status.send_if_modified(|current| {
            let changed = current != status;
            if changed {
                *current = status.clone();
            }
            changed
        });
    }

    pub fn live(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }

    pub fn problem(&self) -> Option<String> {
        self.problem.lock().unwrap().clone()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        drop_remote_clients(&self.shared);
    }
}

fn drop_remote_clients(shared: &Shared) {
    let ids: Vec<u64> = shared
        .clients
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, c)| c.remote)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        shared.remove_client(id);
    }
}

struct Target {
    url: String,
    token: String,
    label: Label,
    open_url: String,
    /// The hub's site, for when a key needs replacing.
    site: String,
    keys: Option<e2e::Keys>,
}

/// How a connection ended.
enum Ended {
    /// Worth retrying; the message says what happened.
    Retry(String),
    /// The hub refused this session; retrying wouldn't help.
    Rejected(String),
}

async fn run(
    shared: Arc<Shared>,
    target: Target,
    mut status: watch::Receiver<HubStatus>,
    live: Arc<AtomicBool>,
    problem: Arc<Mutex<Option<String>>>,
) {
    let report = |result: Result<String, String>| {
        *problem.lock().unwrap() = result.as_ref().err().cloned();
        let _ = shared.events.send(WebEvent::Hub(result));
    };
    let mut delay = Duration::from_secs(1);
    // The last problem reported, so a hub that stays down is reported once.
    let mut reported: Option<String> = None;
    let mut throttle = Throttle::default();
    loop {
        let started = tokio::time::Instant::now();
        let ended = connection(
            &shared,
            &target,
            &mut status,
            &live,
            &mut reported,
            &mut throttle,
            &problem,
        )
        .await;
        live.store(false, Ordering::Relaxed);
        drop_remote_clients(&shared);
        match ended {
            Ended::Rejected(why) => {
                report(Err(format!("The hub refused this session: {why}")));
                return;
            }
            Ended::Retry(why) => {
                if reported.as_deref() != Some(&why) {
                    report(Err(format!("Hub connection: {why}; retrying.")));
                    reported = Some(why);
                }
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            delay = Duration::from_secs(1);
        }
        tokio::time::sleep(jitter(delay)).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

/// `delay`, give or take a quarter, so many tamales don't reconnect in step.
fn jitter(delay: Duration) -> Duration {
    let mut byte = [0u8; 1];
    let _ = getrandom::fill(&mut byte);
    let spread = delay.as_millis() as u64 / 2;
    let offset = spread * u64::from(byte[0]) / 255;
    delay - Duration::from_millis(spread / 2) + Duration::from_millis(offset)
}

async fn connection(
    shared: &Arc<Shared>,
    target: &Target,
    status: &mut watch::Receiver<HubStatus>,
    live: &AtomicBool,
    reported: &mut Option<String>,
    throttle: &mut Throttle,
    problem: &Mutex<Option<String>>,
) -> Ended {
    let mut request = match target.url.as_str().into_client_request() {
        Ok(request) => request,
        Err(e) => return Ended::Rejected(format!("bad hub URL {}: {e}", target.url)),
    };
    let Ok(auth) = format!("Bearer {}", target.token).parse() else {
        return Ended::Rejected("the token has characters a header can't carry".into());
    };
    request.headers_mut().insert("authorization", auth);
    let config = WebSocketConfig::default()
        .max_message_size(Some(protocol::MAX_AGENT_FRAME))
        .max_frame_size(Some(protocol::MAX_AGENT_FRAME));
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let mut ws = match tokio::time::timeout(Duration::from_secs(20), connect).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(tungstenite::Error::Http(response))) => {
            let code = response.status();
            return if code == 401 || code == 403 {
                Ended::Rejected(format!(
                    "it doesn't know this key: it may have been replaced or deleted. Check \
TAMALE_HUB_KEY, or make a new key at {}",
                    target.site
                ))
            } else {
                Ended::Retry(format!("the hub answered {code}"))
            };
        }
        Ok(Err(e)) => return Ended::Retry(format!("couldn't connect to {}: {e}", target.url)),
        Err(_) => return Ended::Retry(format!("timed out connecting to {}", target.url)),
    };

    let hello = match hello(target) {
        Ok(hello) => hello,
        Err(e) => return Ended::Rejected(format!("couldn't seal the session's label: {e:#}")),
    };
    if ws.send(text(&hello)).await.is_err() {
        return Ended::Retry("the hub closed the connection".into());
    }
    match tokio::time::timeout(Duration::from_secs(20), ws.next()).await {
        Ok(Some(Ok(Message::Text(t))))
            if matches!(serde_json::from_str(&t), Ok(ToAgent::Welcome { .. })) => {}
        Ok(Some(Ok(Message::Close(Some(frame))))) => {
            return closed(frame.code.into(), &frame.reason);
        }
        _ => return Ended::Retry("the hub didn't welcome this session".into()),
    }
    live.store(true, Ordering::Relaxed);
    *reported = None;
    *problem.lock().unwrap() = None;
    let _ = shared.events.send(WebEvent::Hub(Ok(format!(
        "Reachable through the hub at {}",
        target.open_url
    ))));
    status.mark_changed();

    // Frames from relayed tabs' channels, to send to the hub.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ToHub>();
    // The hub's channel ids and this server's client ids.
    let mut channels: HashMap<u64, Channel> = HashMap::new();
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    ping.tick().await;
    let mut heard = tokio::time::Instant::now();
    let ended = loop {
        tokio::select! {
            message = ws.next() => {
                heard = tokio::time::Instant::now();
                let message = match message {
                    Some(Ok(message)) => message,
                    Some(Err(e)) => break Ended::Retry(format!("connection lost: {e}")),
                    None => break Ended::Retry("connection lost".into()),
                };
                let frame = match message {
                    Message::Text(t) => t,
                    Message::Close(Some(frame)) => break closed(frame.code.into(), &frame.reason),
                    Message::Close(None) => break Ended::Retry("the hub closed the connection".into()),
                    _ => continue,
                };
                let Ok(frame) = serde_json::from_str::<ToAgent>(&frame) else { continue };
                if let Some(reply) = on_frame(shared, target.keys.as_ref(), throttle, frame, &mut channels, &out_tx)
                    && ws.send(text(&reply)).await.is_err() {
                        break Ended::Retry("connection lost".into());
                    }
            }
            Some(frame) = out_rx.recv() => {
                if ws.send(text(&frame)).await.is_err() {
                    break Ended::Retry("connection lost".into());
                }
            }
            Ok(()) = status.changed() => {
                let s = status.borrow_and_update().clone();
                let Some(frame) = status_frame(target.keys.as_ref(), s) else { continue };
                if ws.send(text(&frame)).await.is_err() {
                    break Ended::Retry("connection lost".into());
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > Duration::from_secs(75) {
                    break Ended::Retry("the hub stopped answering".into());
                }
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break Ended::Retry("connection lost".into());
                }
            }
        }
    };
    for id in channels.values().filter_map(Channel::client) {
        shared.remove_client(id);
    }
    ended
}

fn closed(code: u16, reason: &str) -> Ended {
    let why = if reason.is_empty() {
        format!("closed with code {code}")
    } else {
        reason.to_string()
    };
    if protocol::is_permanent(code) {
        Ended::Rejected(why)
    } else {
        Ended::Retry(why)
    }
}

fn text(frame: &impl serde::Serialize) -> Message {
    Message::Text(serde_json::to_string(frame).unwrap_or_default().into())
}

/// The first frame: with a handshake secret, the label goes sealed.
fn hello(target: &Target) -> anyhow::Result<ToHub> {
    let (label, mid, sealed) = match &target.keys {
        None => (target.label.clone(), None, None),
        Some(keys) => {
            let sealed = keys.seal(sid(), "label", &serde_json::to_string(&target.label)?)?;
            (Label::default(), Some(keys.mid.clone()), Some(sealed))
        }
    };
    Ok(ToHub::Hello {
        proto: protocol::PROTO,
        sid: sid().to_string(),
        label,
        e2e: mid.is_some(),
        mid,
        sealed,
    })
}

/// A status frame, with the title and model sealed if there's a handshake secret. `None` if
/// they can't be, rather than send them in the clear.
fn status_frame(keys: Option<&e2e::Keys>, s: HubStatus) -> Option<ToHub> {
    let Some(keys) = keys else {
        return Some(ToHub::Status {
            busy: s.busy,
            approval: s.approval,
            title: s.title,
            model: s.model,
            sealed: None,
        });
    };
    let plain = serde_json::json!({ "title": s.title, "model": s.model }).to_string();
    Some(ToHub::Status {
        busy: s.busy,
        approval: s.approval,
        title: String::new(),
        model: String::new(),
        sealed: Some(keys.seal(sid(), "status", &plain).ok()?),
    })
}

/// Makes channel `c` a tab: what the app sends it goes to the hub, sealed if `cipher`.
fn open_tab(
    shared: &Arc<Shared>,
    c: u64,
    cipher: Option<Arc<Mutex<e2e::Channel>>>,
    out: &mpsc::UnboundedSender<ToHub>,
) -> u64 {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let id = shared.add_client(tx, true);
    let out = out.clone();
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let frames = match (message, &cipher) {
                (Out::Text(data), None) => vec![ToHub::Msg { c, data }],
                (Out::Text(data), Some(cipher)) => {
                    let sealed = cipher.lock().unwrap().seal(&data);
                    match sealed {
                        Ok(boxes) => boxes.into_iter().map(|m| box_frame(c, m)).collect(),
                        Err(_) => vec![ToHub::Close {
                            c,
                            reason: BROKEN.into(),
                        }],
                    }
                }
                (Out::Close(reason), _) => vec![ToHub::Close {
                    c,
                    reason: reason.into(),
                }],
            };
            for frame in frames {
                if out.send(frame).is_err() {
                    return;
                }
            }
        }
    });
    id
}

fn box_frame(c: u64, sealed: String) -> ToHub {
    ToHub::Msg {
        c,
        data: serde_json::json!({ "t": "box", "m": sealed }).to_string(),
    }
}

/// Ends channel `c`: tells the hub why, and drops its tab if it had one.
fn refuse(
    shared: &Shared,
    channels: &mut HashMap<u64, Channel>,
    c: u64,
    reason: &str,
) -> Option<ToHub> {
    if let Some(id) = channels.remove(&c).and_then(|channel| channel.client()) {
        shared.remove_client(id);
    }
    Some(ToHub::Close {
        c,
        reason: reason.into(),
    })
}

/// Acts on one frame from the hub; returns a frame to answer with, if any.
fn on_frame(
    shared: &Arc<Shared>,
    keys: Option<&e2e::Keys>,
    throttle: &mut Throttle,
    frame: ToAgent,
    channels: &mut HashMap<u64, Channel>,
    out: &mpsc::UnboundedSender<ToHub>,
) -> Option<ToHub> {
    match frame {
        ToAgent::Open { c } => {
            if keys.is_none() {
                let id = open_tab(shared, c, None, out);
                if let Some(id) = channels
                    .insert(c, Channel::Plain(id))
                    .and_then(|old| old.client())
                {
                    shared.remove_client(id);
                }
                return None;
            }
            let mut nonce = [0u8; 16];
            if getrandom::fill(&mut nonce).is_err() {
                return Some(ToHub::Close {
                    c,
                    reason: BROKEN.into(),
                });
            }
            if let Some(id) = channels
                .insert(c, Channel::Waiting(nonce))
                .and_then(|old| old.client())
            {
                shared.remove_client(id);
            }
            use base64::Engine;
            let nonce = base64::engine::general_purpose::STANDARD.encode(nonce);
            Some(ToHub::Msg {
                c,
                data: serde_json::json!({ "t": "hs0", "n": nonce }).to_string(),
            })
        }
        ToAgent::Msg { c, data } => match channels.get(&c)? {
            Channel::Plain(id) => {
                if let Ok(request) = serde_json::from_str::<Value>(&data) {
                    super::handle(shared, *id, request);
                }
                None
            }
            Channel::Waiting(nonce) => {
                let nonce = *nonce;
                let keys = keys?;
                let Some(hs1) = sealed_field(&data, "hs1") else {
                    return refuse(shared, channels, c, BROKEN);
                };
                if throttle.blocked() {
                    return refuse(shared, channels, c, THROTTLED);
                }
                match keys.respond(sid(), &nonce, &hs1) {
                    Ok((cipher, hs2)) => {
                        throttle.succeeded();
                        let cipher = Arc::new(Mutex::new(cipher));
                        let id = open_tab(shared, c, Some(cipher.clone()), out);
                        channels.insert(c, Channel::Sealed(id, cipher));
                        // Answered before anything the new tab is sent: the caller sends
                        // this now, and the tab's messages wait in `out`.
                        Some(ToHub::Msg {
                            c,
                            data: serde_json::json!({ "t": "hs2", "m": hs2 }).to_string(),
                        })
                    }
                    Err(_) => {
                        if throttle.failed() {
                            let _ = shared.events.send(WebEvent::Hub(Err(
                                "Several browsers failed the encrypted handshake with this session. \
It may be a browser with an old key, or someone guessing; tamale is slowing them down."
                                    .into(),
                            )));
                        }
                        refuse(shared, channels, c, WRONG_KEY)
                    }
                }
            }
            Channel::Sealed(id, cipher) => {
                let (id, cipher) = (*id, cipher.clone());
                let Some(sealed) = sealed_field(&data, "box") else {
                    return refuse(shared, channels, c, BROKEN);
                };
                let opened = cipher
                    .lock()
                    .unwrap()
                    .open(&sealed, protocol::MAX_BROWSER_MESSAGE);
                match opened {
                    Ok(None) => None,
                    Ok(Some(message)) => {
                        if let Ok(request) = serde_json::from_str::<Value>(&message) {
                            super::handle(shared, id, request);
                        }
                        None
                    }
                    Err(_) => refuse(shared, channels, c, BROKEN),
                }
            }
        },
        ToAgent::Close { c } => {
            if let Some(id) = channels.remove(&c).and_then(|channel| channel.client()) {
                shared.remove_client(id);
            }
            None
        }
        ToAgent::Get { r, path } => {
            // An encrypted session's artifacts travel over the sealed socket only.
            let refused = keys.is_some() && path.starts_with("/artifact/");
            let asset = if protocol::tunnelled_path(&path) && !refused {
                super::asset(shared, &path)
            } else {
                super::Asset {
                    status: axum::http::StatusCode::NOT_FOUND,
                    headers: Vec::new(),
                    body: String::new(),
                }
            };
            Some(ToHub::Got {
                r,
                status: asset.status.as_u16(),
                headers: asset
                    .headers
                    .into_iter()
                    .map(|(name, value)| (name.as_str().to_string(), value.to_string()))
                    .collect(),
                body: asset.body,
            })
        }
        ToAgent::Welcome { .. } => None,
    }
}

/// The `m` of a `{"t": kind, "m": ...}` frame, and nothing else will do.
fn sealed_field(data: &str, kind: &str) -> Option<String> {
    let frame: Value = serde_json::from_str(data).ok()?;
    if frame.get("t")?.as_str()? != kind {
        return None;
    }
    Some(frame.get("m")?.as_str()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_handshake_failures_back_off_until_one_succeeds() {
        let mut throttle = Throttle::default();
        for _ in 1..FREE_FAILURES {
            assert!(!throttle.failed());
            assert!(!throttle.blocked());
        }
        assert!(throttle.failed(), "reported the first time");
        assert!(throttle.blocked());
        assert!(!throttle.failed(), "and only then");
        throttle.succeeded();
        assert!(!throttle.blocked());
    }

    #[test]
    fn open_urls_follow_the_hub_scheme() {
        assert_eq!(
            open_url("wss://tamale-hub.com/", "ab"),
            "https://tamale-hub.com/open/ab"
        );
        assert_eq!(
            open_url("ws://hub.localhost:8787", "ab"),
            "http://hub.localhost:8787/open/ab"
        );
        assert!(protocol::valid_sid(sid()));
        assert_eq!(sid(), sid());
    }

    #[test]
    fn jitter_stays_within_a_quarter() {
        for _ in 0..50 {
            let d = jitter(Duration::from_secs(8));
            assert!(
                d >= Duration::from_secs(6) && d <= Duration::from_secs(10),
                "{d:?}"
            );
        }
    }
}
