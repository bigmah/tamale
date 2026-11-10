//! Driving a session on another machine from this terminal, through a tamale-hub.
//!
//! This is what the iPhone app does (`apple/ios.md`): list the key's sessions with
//! `Authorization: Bearer`, then open `<sid>.<hub>/ws`, where tamale starts a Noise handshake
//! and every `/web` message after it travels sealed. Anything unsealed drops the connection,
//! so a hub can't forge a session's output or talk this end into plaintext.

use std::time::Duration;

use anyhow::{Context, anyhow};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::config::HubSettings;
use crate::session::Entry;
use crate::web::e2e::{self, Keys};

const TIMEOUT: Duration = Duration::from_secs(20);
const PING: Duration = Duration::from_secs(25);
const MAX_RETRY: Duration = Duration::from_secs(5);
/// Largest `/web` message taken from a session.
const MAX_MESSAGE: usize = 64 << 20;

/// A session in the hub's list, with its sealed labels opened.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HubSession {
    pub sid: String,
    pub host: String,
    pub cwd: String,
    pub model: String,
    pub title: String,
    pub e2e: bool,
    pub busy: bool,
    pub approval: bool,
    pub online: bool,
    pub viewers: usize,
    /// A tamale chef, looking after the others rather than working in a project.
    pub chef: bool,
}

/// The sessions this key can reach, in the hub's order: waiting for approval first.
pub async fn sessions(hub: &HubSettings) -> Result<Vec<HubSession>, String> {
    let key = hub.key_bytes()?;
    let url = format!("{}/api/sessions", hub.site());
    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(tamale_hub_protocol::auth_value(&key))
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach the hub: {}", e.without_url()))?;
    match response.status().as_u16() {
        200 => {}
        401 => return Err("The hub doesn't know TAMALE_HUB_KEY; it may have been rotated.".into()),
        429 => return Err("Too many failed attempts at the hub; try again later.".into()),
        code => return Err(format!("The hub answered {code}.")),
    }
    let list: Value = response
        .json()
        .await
        .map_err(|_| "The hub sent something unexpected.".to_string())?;
    // A hub from before bearer keys looks for a cookie, finds none, and lists no keys.
    if list["keys"].as_array().is_none_or(Vec::is_empty) {
        return Err("This hub needs upgrading to list sessions here.".into());
    }
    Ok(parse_sessions(&list, &Keys::from_key(&key)))
}

fn parse_sessions(list: &Value, keys: &Keys) -> Vec<HubSession> {
    let text = |value: &Value, name: &str| value[name].as_str().unwrap_or("").to_string();
    let empty = Vec::new();
    list["sessions"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|s| {
            let sid = text(s, "sid");
            // It goes into a hostname that gets the key's auth value, so it must be one.
            if !tamale_hub_protocol::valid_sid(&sid) {
                return None;
            }
            let mut session = HubSession {
                host: text(s, "host"),
                cwd: text(s, "cwd"),
                model: text(s, "model"),
                title: text(s, "title"),
                e2e: s["e2e"].as_bool().unwrap_or(false),
                busy: s["busy"].as_bool().unwrap_or(false),
                approval: s["approval"].as_bool().unwrap_or(false),
                online: s["online"].as_bool().unwrap_or(false),
                viewers: s["viewers"].as_u64().unwrap_or(0) as usize,
                chef: s["chef"].as_bool().unwrap_or(false),
                sid,
            };
            let open = |name: &str, kind: &str| -> Option<Value> {
                let sealed = s[name].as_str()?;
                serde_json::from_str(&keys.open(&session.sid, kind, sealed).ok()?).ok()
            };
            let label = open("sealed_label", "label");
            let status = open("sealed_status", "status");
            if let Some(label) = label {
                session.host = text(&label, "host");
                session.cwd = text(&label, "cwd");
                session.model = text(&label, "model");
                session.chef = label["chef"].as_bool().unwrap_or(false);
            }
            if let Some(status) = status {
                session.title = text(&status, "title");
                let model = text(&status, "model");
                if !model.is_empty() {
                    session.model = model;
                }
            }
            Some(session)
        })
        .collect()
}

/// What a session's `status` message says, as far as the terminal shows it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Status {
    pub project: String,
    pub cwd: String,
    pub model: String,
    pub mode: String,
    pub running: bool,
    pub compacting: bool,
    /// Unix seconds when the current run began.
    pub since: Option<u64>,
    /// What the run is doing now, and since when (see `app::activity::step`).
    pub step: String,
    pub step_kind: String,
    pub step_since: Option<u64>,
    pub steps: usize,
    pub sub_steps: usize,
    pub context: u64,
    pub window: u64,
    pub cost: f64,
    pub queued: Vec<String>,
    /// Whether it's a tamale chef.
    pub chef: bool,
    /// Whether it answers side questions (`ask`), as tamales since the chef do.
    pub asks: bool,
}

/// A tool call waiting for an answer.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Approval {
    pub name: String,
    pub summary: String,
    pub description: String,
    pub always: Vec<String>,
}

/// A `/web` message this end acts on.
pub enum Incoming {
    /// The entries from `from` on, replacing what's there.
    Entries { from: usize, items: Vec<Entry> },
    Status(Box<Status>),
    Approval(Option<Approval>),
    /// The answer to a side question asked with `{"t":"ask"}`, by its id.
    Answer { id: String, text: String, error: bool },
}

fn parse_incoming(text: &str) -> Option<Incoming> {
    let mut message: Value = serde_json::from_str(text).ok()?;
    match message["t"].as_str()? {
        "entries" => Some(Incoming::Entries {
            from: message["from"].as_u64()? as usize,
            items: message["items"]
                .as_array()?
                .iter()
                .filter_map(parse_entry)
                .collect(),
        }),
        "status" => Some(Incoming::Status(Box::new(
            serde_json::from_value(message["status"].take()).ok()?,
        ))),
        "approval" => Some(Incoming::Approval(
            serde_json::from_value(message["approval"].take()).ok()?,
        )),
        "answer" => Some(Incoming::Answer {
            id: message["id"].as_str()?.to_string(),
            text: message["text"].as_str().unwrap_or("").to_string(),
            error: message["error"].as_bool().unwrap_or(false),
        }),
        _ => None,
    }
}

fn parse_entry(item: &Value) -> Option<Entry> {
    let text = |name: &str| item[name].as_str().unwrap_or("").to_string();
    Some(match item["kind"].as_str()? {
        "user" => Entry::User(text("text")),
        "assistant" => Entry::Assistant(with_artifacts(&text("md"), &item["artifacts"])),
        "note" => Entry::Note(text("md")),
        "info" => Entry::Info(text("text")),
        "error" => Entry::Error(text("text")),
        "tool" => Entry::Tool {
            name: text("name"),
            summary: text("summary"),
            description: text("description"),
            output: item["output"]
                .as_str()
                .map(|output| (output.to_string(), item["error"].as_bool().unwrap_or(false))),
        },
        _ => return None,
    })
}

/// Puts each artifact back where its marker is, as the kind of block the transcript shows
/// as one line with its title. Its document stays on the other machine.
fn with_artifacts(md: &str, artifacts: &Value) -> String {
    let mut text = md.to_string();
    for (n, artifact) in artifacts.as_array().into_iter().flatten().enumerate() {
        let title = artifact["title"].as_str().unwrap_or("Artifact");
        let block = format!("```html artifact\n<title>{title}</title>\n```");
        text = text.replacen(&crate::web::artifact_marker(n), &block, 1);
    }
    text
}

/// How a connection is going.
pub enum Event {
    /// Not connected, and why not yet.
    Connecting(Option<String>),
    Live,
    /// Won't retry: the session ended, or the key doesn't fit.
    Stopped(String),
    Message(Incoming),
}

/// A connection to one session, retried until dropped.
pub struct Connection {
    out: mpsc::UnboundedSender<String>,
    task: tokio::task::JoinHandle<()>,
}

impl Connection {
    /// Connects to session `sid` on the hub in `hub`.
    pub fn open(
        hub: &HubSettings,
        sid: &str,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let key = hub.key_bytes()?;
        Ok(Self::to(
            socket_url(&hub.url, sid),
            format!("Bearer {}", tamale_hub_protocol::auth_value(&key)),
            Keys::from_key(&key),
            sid.to_string(),
            events,
        ))
    }

    fn to(
        url: String,
        auth: String,
        keys: Keys,
        sid: String,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        let (out, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(url, auth, keys, sid, rx, events));
        Self { out, task }
    }

    pub fn send(&self, message: Value) {
        let _ = self.out.send(message.to_string());
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Ends session `sid` on whichever machine it runs, as `tamale kill` would there: it stays
/// saved. Waits until it has gone.
pub async fn close(hub: &HubSettings, sid: &str) -> Result<(), String> {
    let (tx, events) = mpsc::unbounded_channel();
    let connection = Connection::open(hub, sid, move |event| {
        let _ = tx.send(event);
    })?;
    close_over(connection, events).await
}

async fn close_over(
    connection: Connection,
    mut events: mpsc::UnboundedReceiver<Event>,
) -> Result<(), String> {
    // Goes as soon as the channel is up.
    connection.send(json!({ "t": "close" }));
    let gone = async {
        let mut live = false;
        while let Some(event) = events.recv().await {
            match event {
                Event::Live => live = true,
                // Once it has had the message, the connection ending is the session ending.
                Event::Connecting(_) | Event::Stopped(_) if live => return Ok(()),
                Event::Stopped(why) => return Err(why),
                Event::Connecting(_) | Event::Message(_) => {}
            }
        }
        Err("The connection to it ended.".to_string())
    };
    tokio::time::timeout(TIMEOUT, gone)
        .await
        .unwrap_or_else(|_| Err("It didn't answer in time.".into()))
}

/// `wss://<sid>.<hub>/ws`, from the hub's agent socket base.
fn socket_url(hub: &str, sid: &str) -> String {
    let base = hub.trim_end_matches('/');
    match base.split_once("://") {
        Some((scheme, host)) => format!("{scheme}://{sid}.{host}/ws"),
        None => format!("wss://{sid}.{base}/ws"),
    }
}

enum Ended {
    Retry(String),
    Stop(String),
}

async fn run(
    url: String,
    auth: String,
    keys: Keys,
    sid: String,
    mut rx: mpsc::UnboundedReceiver<String>,
    events: impl Fn(Event) + Send + Sync,
) {
    let mut failures = 0;
    events(Event::Connecting(None));
    loop {
        let mut went_live = false;
        let ended = connection(&url, &auth, &keys, &sid, &mut rx, &events, &mut went_live).await;
        if went_live {
            failures = 0;
        }
        let why = match ended {
            Ended::Stop(why) => {
                events(Event::Stopped(why));
                return;
            }
            Ended::Retry(why) => why,
        };
        events(Event::Connecting(Some(why)));
        let delay = Duration::from_millis(250 * (1u64 << failures.min(8))).min(MAX_RETRY);
        failures += 1;
        tokio::time::sleep(delay).await;
    }
}

async fn connection(
    url: &str,
    auth: &str,
    keys: &Keys,
    sid: &str,
    rx: &mut mpsc::UnboundedReceiver<String>,
    events: &(impl Fn(Event) + Sync),
    went_live: &mut bool,
) -> Ended {
    let request = url.into_client_request().map_err(|e| anyhow!(e)).and_then(|mut request| {
        request
            .headers_mut()
            .insert("authorization", auth.parse().context("a bad key")?);
        Ok(request)
    });
    let request = match request {
        Ok(request) => request,
        Err(e) => return Ended::Stop(format!("Can't connect to {url}: {e:#}")),
    };
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE));
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let mut socket = match tokio::time::timeout(TIMEOUT, connect).await {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(tungstenite::Error::Http(response))) => {
            return match response.status().as_u16() {
                401 => Ended::Stop("The hub doesn't know this key.".into()),
                404 => Ended::Stop("This key doesn't have that session, or it has ended.".into()),
                503 => Ended::Retry("tamale is offline there.".into()),
                429 => Ended::Retry("Too many connections.".into()),
                code => Ended::Retry(format!("The hub answered {code}.")),
            };
        }
        Ok(Err(e)) => return Ended::Retry(format!("Couldn't connect: {e}")),
        Err(_) => return Ended::Retry("Timed out connecting.".into()),
    };
    let mut ping = tokio::time::interval(PING);
    ping.tick().await;
    let mut handshake: Option<e2e::Handshake> = None;
    let mut channel: Option<e2e::Channel> = None;
    const BROKEN: &str = "The encrypted connection broke.";
    loop {
        tokio::select! {
            frame = socket.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(close))) => {
                        let (code, reason) = close
                            .map(|c| (u16::from(c.code), c.reason.to_string()))
                            .unwrap_or((0, String::new()));
                        return if code == 4000 {
                            Ended::Stop(explain(&reason))
                        } else {
                            Ended::Retry("The connection dropped.".into())
                        };
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Ended::Retry(format!("The connection dropped: {e}")),
                    None => return Ended::Retry("The connection dropped.".into()),
                };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                    return Ended::Retry(BROKEN.into());
                };
                let field = |name: &str| frame[name].as_str().unwrap_or("");
                match field("t") {
                    "hs0" if handshake.is_none() && channel.is_none() => {
                        let Ok((hs, hs1)) = keys.initiate(sid, field("n")) else {
                            return Ended::Retry(BROKEN.into());
                        };
                        handshake = Some(hs);
                        let hello = json!({ "t": "hs1", "m": hs1 }).to_string();
                        if socket.send(Message::text(hello)).await.is_err() {
                            return Ended::Retry("The connection dropped.".into());
                        }
                    }
                    "hs2" if handshake.is_some() => {
                        let Some(Ok(mut opened)) = handshake.take().map(|hs| hs.finish(field("m"))) else {
                            return Ended::Retry(BROKEN.into());
                        };
                        // Says replies shouldn't be written for the web UI (see `web::handle`).
                        let hello = json!({ "t": "terminal" }).to_string();
                        if send_sealed(&mut socket, &mut opened, &hello).await.is_err() {
                            return Ended::Retry("The connection dropped.".into());
                        }
                        channel = Some(opened);
                        *went_live = true;
                        events(Event::Live);
                    }
                    "box" if channel.is_some() => {
                        let opened = channel.as_mut().map(|c| c.open(field("m"), MAX_MESSAGE));
                        match opened {
                            Some(Ok(Some(message))) => {
                                if let Some(incoming) = parse_incoming(&message) {
                                    events(Event::Message(incoming));
                                }
                            }
                            Some(Ok(None)) => {}
                            _ => return Ended::Retry(BROKEN.into()),
                        }
                    }
                    // Plaintext, or a step out of order: nothing a session of ours sends.
                    _ => return Ended::Retry(BROKEN.into()),
                }
            }
            Some(message) = rx.recv(), if channel.is_some() => {
                let Some(opened) = &mut channel else { continue };
                if send_sealed(&mut socket, opened, &message).await.is_err() {
                    return Ended::Retry("The connection dropped.".into());
                }
            }
            _ = ping.tick() => {
                if socket.send(Message::Ping(Default::default())).await.is_err() {
                    return Ended::Retry("The connection dropped.".into());
                }
            }
        }
    }
}

async fn send_sealed<S>(socket: &mut S, channel: &mut e2e::Channel, message: &str) -> Result<(), ()>
where
    S: futures::Sink<Message> + Unpin,
{
    for sealed in channel.seal(message).map_err(|_| ())? {
        let frame = json!({ "t": "box", "m": sealed }).to_string();
        socket.send(Message::text(frame)).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Why a session closed the connection, in words for this screen.
fn explain(reason: &str) -> String {
    match reason {
        "" => "That session stopped serving.".into(),
        "e2e: wrong key" => "This key doesn't fit that session.".into(),
        _ => reason.strip_prefix("e2e: ").unwrap_or(reason).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::accept_async;

    fn keys() -> Keys {
        Keys::from_psk([7; 32])
    }

    #[test]
    fn the_list_opens_sealed_labels_and_refuses_a_hostile_sid() {
        let keys = keys();
        let sid = "0123456789abcdef0123456789abcdef";
        let list = json!({ "sessions": [
            {
                "sid": sid, "e2e": true, "online": true, "busy": true, "viewers": 2,
                "host": "", "title": "",
                "sealed_label": keys.seal(sid, "label",
                    r#"{"host":"box","cwd":"~/x","model":"m1"}"#).unwrap(),
                "sealed_status": keys.seal(sid, "status",
                    r#"{"title":"fix it","model":"m2"}"#).unwrap(),
            },
            { "sid": "evil.example/#", "online": true },
        ]});
        let sessions = parse_sessions(&list, &keys);
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        let session = &sessions[0];
        assert_eq!(
            (&*session.host, &*session.cwd, &*session.title, &*session.model),
            ("box", "~/x", "fix it", "m2")
        );
        assert!(session.busy && session.online && session.e2e);
        assert_eq!(session.viewers, 2);
    }

    #[test]
    fn socket_urls_go_to_the_sessions_own_host() {
        assert_eq!(socket_url("wss://hub.example/", "ab"), "wss://ab.hub.example/ws");
        assert_eq!(socket_url("ws://hub.localhost:8787", "ab"), "ws://ab.hub.localhost:8787/ws");
    }

    #[test]
    fn entries_read_as_the_transcript_expects_with_artifacts_back_in_place() {
        let message = json!({ "t": "entries", "from": 3, "items": [
            { "kind": "user", "text": "hi" },
            { "kind": "assistant", "html": "<p>ignored</p>", "md": "Done.\n\n<!-- artifact 0 -->\n\n",
              "artifacts": [{ "title": "Sync flow", "done": true }] },
            { "kind": "tool", "name": "bash", "summary": "ls", "description": "List",
              "output": "a", "error": true },
            { "kind": "future" },
        ]});
        let Some(Incoming::Entries { from, items }) = parse_incoming(&message.to_string()) else {
            panic!("not entries");
        };
        assert_eq!((from, items.len()), (3, 3));
        let Entry::Assistant(text) = &items[1] else { panic!() };
        assert!(text.contains("```html artifact\n<title>Sync flow</title>\n```"), "{text}");
        assert!(matches!(&items[2], Entry::Tool { output: Some((o, true)), .. } if o == "a"));
        assert!(matches!(
            parse_incoming(r#"{"t":"approval","approval":null}"#),
            Some(Incoming::Approval(None))
        ));
    }

    /// A stand-in for the hub and the session behind it, doing the session's side.
    async fn listen() -> (String, tokio::net::TcpListener) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        (format!("ws://{}/ws", listener.local_addr().unwrap()), listener)
    }

    fn collect() -> (impl Fn(Event) + Send + Sync + 'static, mpsc::UnboundedReceiver<Event>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (move |event| { let _ = tx.send(event); }, rx)
    }

    #[tokio::test]
    async fn drives_a_session_over_the_encrypted_channel() {
        use base64::Engine;
        let (url, listener) = listen().await;
        let session = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let nonce = [9u8; 16];
            let n = base64::engine::general_purpose::STANDARD.encode(nonce);
            ws.send(Message::text(json!({ "t": "hs0", "n": n }).to_string())).await.unwrap();
            let Some(Ok(Message::Text(hs1))) = ws.next().await else { panic!() };
            let hs1: Value = serde_json::from_str(&hs1).unwrap();
            let (mut channel, hs2) = keys().respond("ab", &nonce, hs1["m"].as_str().unwrap()).unwrap();
            ws.send(Message::text(json!({ "t": "hs2", "m": hs2 }).to_string())).await.unwrap();
            let mut received = Vec::new();
            for _ in 0..2 {
                let Some(Ok(Message::Text(frame))) = ws.next().await else { panic!() };
                let frame: Value = serde_json::from_str(&frame).unwrap();
                received.push(channel.open(frame["m"].as_str().unwrap(), 1 << 20).unwrap().unwrap());
            }
            let status = json!({ "t": "status", "status": { "model": "m", "running": true } });
            for m in channel.seal(&status.to_string()).unwrap() {
                ws.send(Message::text(json!({ "t": "box", "m": m }).to_string())).await.unwrap();
            }
            ws.close(Some(tungstenite::protocol::CloseFrame {
                code: 4000.into(),
                reason: "e2e: wrong key".into(),
            })).await.unwrap();
            received
        });
        let (events, mut rx) = collect();
        let connection = Connection::to(url, "Bearer x".into(), keys(), "ab".into(), events);
        connection.send(json!({ "t": "send", "text": "hello" }));

        assert!(matches!(rx.recv().await, Some(Event::Connecting(None))));
        assert!(matches!(rx.recv().await, Some(Event::Live)));
        match rx.recv().await {
            Some(Event::Message(Incoming::Status(status))) => {
                assert!(status.running);
                assert_eq!(status.model, "m");
            }
            _ => panic!("no status"),
        }
        assert!(matches!(rx.recv().await, Some(Event::Stopped(why)) if why.contains("doesn't fit")));
        let received = session.await.unwrap();
        assert!(received[0].contains("terminal"), "says what it is first: {received:?}");
        assert!(received[1].contains("hello"), "{received:?}");
    }

    #[tokio::test]
    async fn closing_asks_the_session_to_end_and_waits_for_it_to_go() {
        use base64::Engine;
        let (url, listener) = listen().await;
        let session = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let nonce = [3u8; 16];
            let n = base64::engine::general_purpose::STANDARD.encode(nonce);
            ws.send(Message::text(json!({ "t": "hs0", "n": n }).to_string())).await.unwrap();
            let Some(Ok(Message::Text(hs1))) = ws.next().await else { panic!() };
            let hs1: Value = serde_json::from_str(&hs1).unwrap();
            let (mut channel, hs2) = keys().respond("ab", &nonce, hs1["m"].as_str().unwrap()).unwrap();
            ws.send(Message::text(json!({ "t": "hs2", "m": hs2 }).to_string())).await.unwrap();
            let mut received = Vec::new();
            for _ in 0..2 {
                let Some(Ok(Message::Text(frame))) = ws.next().await else { panic!() };
                let frame: Value = serde_json::from_str(&frame).unwrap();
                received.push(channel.open(frame["m"].as_str().unwrap(), 1 << 20).unwrap().unwrap());
            }
            // What the hub does when tamale says it has ended.
            ws.close(Some(tungstenite::protocol::CloseFrame {
                code: 4000.into(),
                reason: "This session ended.".into(),
            })).await.unwrap();
            received
        });
        let (tx, events) = mpsc::unbounded_channel();
        let connection = Connection::to(url, "Bearer x".into(), keys(), "ab".into(), move |event| {
            let _ = tx.send(event);
        });
        assert_eq!(close_over(connection, events).await, Ok(()));
        let received = session.await.unwrap();
        assert_eq!(received[1], r#"{"t":"close"}"#);
    }

    #[tokio::test]
    async fn closing_a_session_that_isnt_there_says_why() {
        let (tx, events) = mpsc::unbounded_channel();
        tx.send(Event::Connecting(None)).unwrap();
        tx.send(Event::Stopped("This key doesn't have that session, or it has ended.".into())).unwrap();
        let (url, _listener) = listen().await;
        let connection = Connection::to(url, "Bearer x".into(), keys(), "ab".into(), |_| {});
        let result = close_over(connection, events).await;
        assert!(result.is_err_and(|why| why.contains("ended")));
    }

    #[tokio::test]
    async fn plaintext_from_the_hub_never_reaches_the_screen() {
        let (url, listener) = listen().await;
        let accepted = Arc::new(Mutex::new(0));
        let count = accepted.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = accept_async(stream).await.unwrap();
                *count.lock().unwrap() += 1;
                let forged = json!({ "t": "status", "status": { "model": "forged" } });
                let _ = ws.send(Message::text(forged.to_string())).await;
                let _ = ws.next().await;
            }
        });
        let (events, mut rx) = collect();
        let _connection = Connection::to(url, "Bearer x".into(), keys(), "ab".into(), events);
        assert!(matches!(rx.recv().await, Some(Event::Connecting(None))));
        match rx.recv().await {
            Some(Event::Connecting(Some(why))) => assert!(why.contains("encrypted"), "{why}"),
            _ => panic!("expected to drop the connection"),
        }
        assert_eq!(*accepted.lock().unwrap(), 1);
    }
}
