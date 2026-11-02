//! `/web remote`: makes this session reachable through a tamale-hub.
//!
//! tamale connects out to `wss://<hub>/agent` with an agent token and says `hello` with
//! this process's session id (sid). The hub serves the session at
//! `https://<sid>.<hub>/`, and relays to it: page GETs (answered by [`super::asset`]) and
//! each browser tab's socket as a channel `c`, whose messages go to the same
//! [`super::handle`] a local tab's do. A relayed tab is a [`super::Client`] like any
//! other, so it gets the same `welcome` and updates. The frames are in
//! `tamale-hub-protocol`.
//!
//! The connection is retried with backoff until it's turned off, except when the hub
//! rejects it for good (a bad token, say).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tamale_hub_protocol::{self as protocol, ToAgent, ToHub};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};

use super::{Out, Shared, WebEvent};

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

/// The link that opens session `sid` through the hub whose agent socket is at `hub`
/// (`wss://tamale-hub.com` → `https://tamale-hub.com/open/<sid>`).
pub fn open_url(hub: &str, sid: &str) -> String {
    let base = hub.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        base.to_string()
    };
    format!("{base}/open/{sid}")
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

pub struct Relay {
    pub open_url: String,
    shared: Arc<Shared>,
    status: watch::Sender<HubStatus>,
    live: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    pub fn start(shared: Arc<Shared>, url: &str, token: &str, label: Label) -> Self {
        let (status, status_rx) = watch::channel(HubStatus::default());
        let live = Arc::new(AtomicBool::new(false));
        let open_url = open_url(url, sid());
        let task = tokio::spawn(run(
            shared.clone(),
            Target {
                url: format!("{}/agent", url.trim_end_matches('/')),
                token: token.to_string(),
                label,
                open_url: open_url.clone(),
            },
            status_rx,
            live.clone(),
        ));
        Self {
            open_url,
            shared,
            status,
            live,
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
) {
    let report = |result| {
        let _ = shared.events.send(WebEvent::Hub(result));
    };
    let mut delay = Duration::from_secs(1);
    // The last problem reported, so a hub that stays down is reported once.
    let mut reported: Option<String> = None;
    loop {
        let started = tokio::time::Instant::now();
        let ended = connection(&shared, &target, &mut status, &live, &mut reported).await;
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
                Ended::Rejected("the agent token was not accepted (check TAMALE_HUB_TOKEN)".into())
            } else {
                Ended::Retry(format!("the hub answered {code}"))
            };
        }
        Ok(Err(e)) => return Ended::Retry(format!("couldn't connect to {}: {e}", target.url)),
        Err(_) => return Ended::Retry(format!("timed out connecting to {}", target.url)),
    };

    let hello = ToHub::Hello {
        proto: protocol::PROTO,
        sid: sid().to_string(),
        label: target.label.clone(),
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
    let _ = shared.events.send(WebEvent::Hub(Ok(format!(
        "Reachable through the hub at {}",
        target.open_url
    ))));
    status.mark_changed();

    // Frames from relayed tabs' channels, to send to the hub.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ToHub>();
    // The hub's channel ids and this server's client ids.
    let mut channels: HashMap<u64, u64> = HashMap::new();
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
                if let Some(reply) = on_frame(shared, frame, &mut channels, &out_tx)
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
                let frame = ToHub::Status { busy: s.busy, approval: s.approval, title: s.title, model: s.model };
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
    for (_, id) in channels {
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

/// Acts on one frame from the hub; returns a frame to answer with, if any.
fn on_frame(
    shared: &Arc<Shared>,
    frame: ToAgent,
    channels: &mut HashMap<u64, u64>,
    out: &mpsc::UnboundedSender<ToHub>,
) -> Option<ToHub> {
    match frame {
        ToAgent::Open { c } => {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let id = shared.add_client(tx, true);
            if let Some(old) = channels.insert(c, id) {
                shared.remove_client(old);
            }
            let out = out.clone();
            tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    let frame = match message {
                        Out::Text(data) => ToHub::Msg { c, data },
                        Out::Close(reason) => ToHub::Close {
                            c,
                            reason: reason.into(),
                        },
                    };
                    if out.send(frame).is_err() {
                        return;
                    }
                }
            });
            None
        }
        ToAgent::Msg { c, data } => {
            let id = *channels.get(&c)?;
            if let Ok(request) = serde_json::from_str::<Value>(&data) {
                super::handle(shared, id, request);
            }
            None
        }
        ToAgent::Close { c } => {
            if let Some(id) = channels.remove(&c) {
                shared.remove_client(id);
            }
            None
        }
        ToAgent::Get { r, path } => {
            let asset = if protocol::tunnelled_path(&path) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
