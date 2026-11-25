//! The one socket to tamale that carries everything (see tamale's `src/web.rs`), sealed
//! end to end on a session reached through a hub. It reconnects by itself, quietly: tamale's
//! machine may be asleep for hours, or the hub restarting.

use std::rc::Rc;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use futures_channel::oneshot;
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::app::{App, ConnView, Lock};
use crate::e2e;
use crate::host::{self, SocketEvent};

/// A connection with nothing on it this long gets a ping, to see it still works.
const QUIET: f64 = 20_000.0;
/// One with nothing on it this long is taken for dead. Generous, since a big message (a long
/// session's history on a slow phone) arrives all at once.
const GONE: f64 = 90_000.0;

const NEEDS_KEY: &str = "Scan this session's key on the hub's page, or paste it there.";

impl App {
    pub fn set_conn(mut self, kind: &'static str, title: &str) {
        self.conn_view.set(ConnView {
            kind,
            title: title.to_string(),
        });
    }

    /// Replaces the page with why it can't show this session, and a way on.
    pub fn lock(mut self, title: &str, text: &str) {
        host::shown();
        self.conn.write().locked = true;
        self.set_conn("stopped", title);
        let link = {
            let place = self.place.read();
            place
                .hub
                .as_ref()
                .map(|hub| format!("{hub}/open/{}", place.sid))
        };
        self.lock.set(Some(Lock {
            title: title.into(),
            text: text.into(),
            link,
        }));
    }

    /// Starts the first connection, or says why there can't be one.
    pub fn start(mut self) {
        match self.place.read().e2e.clone() {
            Some(None) => self.lock("This session is encrypted", NEEDS_KEY),
            _ => self.connect(),
        }
    }

    pub fn connect(mut self) {
        let url = self.place.read().ws_url.clone();
        let (socket, mut events) = match host::Socket::connect(&url) {
            Ok(pair) => pair,
            Err(_) => return self.reconnect_later(0.0),
        };
        let generation = {
            let mut c = self.conn.write();
            c.retry_due = None;
            c.generation += 1;
            c.socket = Some(Rc::new(socket));
            c.channel = None;
            c.handshake = None;
            let now = host::now_ms();
            c.connected_at = now;
            c.last_heard = now;
            c.generation
        };
        let e2e = self.e2e();
        spawn_forever(async move {
            while let Some(event) = events.next().await {
                if self.conn.read().generation != generation {
                    return;
                }
                match event {
                    SocketEvent::Open => {
                        if !e2e {
                            self.ready();
                        } else if self.conn.read().down_due.is_none() {
                            self.set_conn("", "Encrypting the connection…");
                        }
                    }
                    SocketEvent::Text(text) => {
                        self.conn.write().last_heard = host::now_ms();
                        if e2e {
                            // One at a time and in order: the handshake and the counters
                            // depend on it. Anything that fails here drops the connection,
                            // and with it whatever was forged.
                            if let Err(e) = self.sealed_frame(&text) {
                                web_log(&e);
                                self.drop_socket();
                                self.reconnect_later(0.0);
                                return;
                            }
                        } else {
                            let Ok(message) = serde_json::from_str::<Value>(&text) else {
                                continue;
                            };
                            if message.get("t").and_then(Value::as_str) == Some("hs0") {
                                self.drop_socket();
                                self.lock("This session is encrypted", NEEDS_KEY);
                                return;
                            }
                            self.on_message(message);
                        }
                    }
                    SocketEvent::Closed { code, reason } => {
                        self.closed(code, &reason);
                        return;
                    }
                }
            }
        });
        let _ = &mut self;
    }

    fn drop_socket(mut self) {
        let socket = {
            let mut c = self.conn.write();
            c.channel = None;
            c.handshake = None;
            c.generation += 1;
            c.socket.take()
        };
        if let Some(socket) = socket {
            socket.close();
        }
    }

    fn closed(mut self, code: u16, reason: &str) {
        {
            let mut c = self.conn.write();
            c.channel = None;
            c.handshake = None;
            c.socket = None;
            if c.locked {
                return;
            }
        }
        if self.e2e() && code == 4000 && reason == "e2e: wrong key" {
            host::forget_key();
            return self.lock("This browser's key doesn't fit this session", NEEDS_KEY);
        }
        // A broken or throttled encrypted channel: a new connection starts a new handshake.
        if code == 4000 && reason.starts_with("e2e: ") {
            return self.reconnect_later(if reason.contains("too many") {
                30_000.0
            } else {
                0.0
            });
        }
        if code == 4000 {
            // Over for good (the session ended, say): nothing to reconnect to.
            {
                let mut c = self.conn.write();
                c.locked = true;
                c.down_due = None;
            }
            self.set_conn("stopped", reason);
            let text = if reason.is_empty() {
                "tamale stopped serving this page."
            } else {
                reason
            };
            self.toast(text, 1_000_000_000);
            return;
        }
        self.reconnect_later(0.0);
    }

    /// Handles one frame of an encrypted connection; fails if it isn't what's due.
    fn sealed_frame(mut self, data: &str) -> Result<(), String> {
        let frame: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
        let t = frame.get("t").and_then(Value::as_str).unwrap_or("");
        let field = |k: &str| {
            frame
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let (has_handshake, has_channel) = {
            let c = self.conn.read();
            (c.handshake.is_some(), c.channel.is_some())
        };
        match t {
            "hs0" if !has_handshake && !has_channel => {
                let (key, sid) = {
                    let place = self.place.read();
                    (
                        place.e2e.clone().flatten().unwrap_or_default(),
                        place.sid.clone(),
                    )
                };
                let nonce =
                    e2e::decode(&field("n")).ok_or("The handshake's nonce isn't base64.")?;
                let (hs, first) = e2e::Handshake::start(&key, &sid, &nonce, b"{\"v\":1}")?;
                let socket = {
                    let mut c = self.conn.write();
                    c.handshake = Some(hs);
                    c.socket.clone()
                };
                if let Some(socket) = socket {
                    socket.send(&json!({ "t": "hs1", "m": first }).to_string());
                }
            }
            "hs2" if has_handshake => {
                let hs = self.conn.write().handshake.take().ok_or("No handshake.")?;
                let channel = hs.finish(&field("m"))?;
                self.conn.write().channel = Some(channel);
                self.ready();
            }
            "box" if has_channel => {
                let message = {
                    let mut c = self.conn.write();
                    c.channel.as_mut().ok_or("No channel.")?.open(&field("m"))?
                };
                if let Some(message) = message
                    && let Ok(value) = serde_json::from_str(&message)
                {
                    self.on_message(value);
                }
            }
            _ => return Err(format!("Refused an unexpected {t} frame.")),
        }
        Ok(())
    }

    /// The connection is up and, if it's to be, encrypted.
    fn ready(mut self) {
        {
            let mut c = self.conn.write();
            c.retry = 0;
            c.down_due = None;
        }
        if self.ending.peek().due {
            return self.end_session();
        }
        let title = if self.e2e() {
            "Connected to tamale, end-to-end encrypted"
        } else {
            "Connected to tamale"
        };
        self.set_conn("live", title);
        self.ask_changes();
        let held = std::mem::take(&mut self.mem.write().review_held);
        for waiting in held {
            let _ = waiting.send(());
        }
        let due: Vec<String> = self.mem.read().artifacts_due.keys().cloned().collect();
        for id in due {
            self.send(json!({ "t": "artifact", "id": id }), true);
        }
    }

    /// Tries again after a while. A page out of sight tries less often; coming back into view
    /// tries at once.
    pub fn reconnect_later(mut self, at: f64) {
        let (timer, delay) = {
            let mut c = self.conn.write();
            c.timers += 1;
            let cap: f64 = if host::page_hidden() {
                30_000.0
            } else {
                5_000.0
            };
            let delay = at.max(cap.min(250.0 * 2f64.powi(c.retry.min(20) as i32)));
            c.retry += 1;
            c.retry_due = Some(c.timers);
            (c.timers, delay)
        };
        spawn_forever(async move {
            host::sleep(delay as u64).await;
            if self.conn.read().retry_due == Some(timer) {
                self.connect();
            }
        });
        self.show_down();
    }

    fn show_down(mut self) {
        let timer = {
            let mut c = self.conn.write();
            if c.down_due.is_some() {
                return;
            }
            c.timers += 1;
            c.down_due = Some(c.timers);
            c.timers
        };
        self.set_conn("", "Reconnecting…");
        spawn_forever(async move {
            host::sleep(8000).await;
            if self.conn.read().down_due == Some(timer) {
                self.set_conn("down", "Reconnecting…");
            }
        });
    }

    /// Drops this connection, which may have died without a word, and starts another.
    pub fn reconnect_now(mut self) {
        if self.conn.read().locked || self.conn.read().socket.is_none() {
            return;
        }
        self.drop_socket();
        self.conn.write().retry = 0;
        self.show_down();
        self.connect();
    }

    /// Whether the connection carries messages yet (on an encrypted session, once sealed).
    pub fn is_ready(&self) -> bool {
        let c = self.conn.read();
        c.socket.as_ref().is_some_and(|s| s.is_open()) && (!self.e2e() || c.channel.is_some())
    }

    /// A connection can die without closing: a phone that slept, a laptop that changed
    /// networks. Nothing arrives on one that's quiet, so while the page is in view it's
    /// checked now and then, with a ping that goes all the way to tamale and back.
    pub fn watch(mut self) {
        spawn_forever(async move {
            loop {
                host::sleep(10_000).await;
                let (skip, connected_at, last_heard) = {
                    let c = self.conn.read();
                    (
                        host::page_hidden()
                            || c.locked
                            || c.socket.is_none()
                            || c.retry_due.is_some(),
                        c.connected_at,
                        c.last_heard,
                    )
                };
                if skip {
                    continue;
                }
                let now = host::now_ms();
                if !self.is_ready() {
                    if now - connected_at > GONE {
                        self.reconnect_now();
                    }
                } else if now - last_heard > GONE {
                    self.reconnect_now();
                } else if now - last_heard > QUIET {
                    self.send(json!({ "t": "ping" }), true);
                }
            }
        });
    }

    /// Back in view or online: makes sure the connection still works, and reconnects if not.
    pub fn check_connection(mut self) {
        let (skip, waiting, last_heard) = {
            let c = self.conn.read();
            (
                host::page_hidden() || c.locked || c.socket.is_none() && c.retry_due.is_none(),
                c.retry_due.is_some(),
                c.last_heard,
            )
        };
        if skip {
            return;
        }
        if waiting {
            {
                let mut c = self.conn.write();
                c.retry = 0;
                c.retry_due = None;
            }
            return self.connect();
        }
        // Something arrived just now, so it works.
        if !self.is_ready() || host::now_ms() - last_heard < 10_000.0 {
            return;
        }
        let asked = host::now_ms();
        self.send(json!({ "t": "ping" }), true);
        spawn_forever(async move {
            host::sleep(8000).await;
            if self.is_ready() && self.conn.read().last_heard < asked {
                self.reconnect_now();
            }
        });
    }

    /// Sends `message`; false if not connected. Unless `quiet`, says so when it isn't.
    pub fn send(mut self, message: Value, quiet: bool) -> bool {
        let text = message.to_string();
        let e2e = self.e2e();
        let sent = {
            let mut c = self.conn.write();
            let socket = c.socket.clone().filter(|s| s.is_open());
            match (socket, e2e) {
                (Some(socket), true) => match c.channel.as_mut().map(|ch| ch.seal(&text)) {
                    Some(Ok(boxes)) => {
                        for m in boxes {
                            socket.send(&json!({ "t": "box", "m": m }).to_string());
                        }
                        Some(true)
                    }
                    Some(Err(_)) => Some(false),
                    None => None,
                },
                (Some(socket), false) => Some(socket.send(&text)),
                (None, _) => None,
            }
        };
        match sent {
            Some(true) => true,
            Some(false) => {
                self.drop_socket();
                self.reconnect_later(0.0);
                false
            }
            None => {
                if !quiet {
                    self.toast("Reconnecting to tamale. Try again in a moment.", 5000);
                }
                false
            }
        }
    }

    fn next_seq(mut self) -> u64 {
        let mut c = self.conn.write();
        c.seq += 1;
        c.seq
    }

    /// Sends a request whose reply is only used if no newer one of its kind was made.
    pub fn request(mut self, t: &str, body: Value, kind: &'static str) -> u64 {
        let seq = self.next_seq();
        self.conn.write().latest.insert(kind, seq);
        self.send(with(json!({ "t": t, "seq": seq }), body), true);
        seq
    }

    /// Sends a request and waits for its reply; fails if not connected or not answered in time.
    pub async fn request_reply(mut self, t: &str, body: Value, ms: u64) -> Result<Value, String> {
        let seq = self.next_seq();
        let (tx, rx) = oneshot::channel();
        self.conn.write().awaited.insert(seq, tx);
        if !self.send(with(json!({ "t": t, "seq": seq }), body), true) {
            self.conn.write().awaited.remove(&seq);
            return Err("The session isn't connected right now.".into());
        }
        spawn_forever(async move {
            host::sleep(ms).await;
            let late = self.conn.write().awaited.remove(&seq);
            if let Some(tx) = late {
                let _ = tx.send(Err("tamale didn't answer in time.".into()));
            }
        });
        rx.await.unwrap_or_else(|_| Err("Stopped.".into()))
    }

    /// How much has changed, for the code button.
    pub fn ask_changes(mut self) {
        if !self.is_chef() {
            self.request("diff_stat", json!({}), "diff_stat");
        }
    }
}

/// `base` with `body`'s fields added.
fn with(mut base: Value, body: Value) -> Value {
    if let (Some(base), Value::Object(body)) = (base.as_object_mut(), body) {
        base.extend(body);
    }
    base
}

fn web_log(message: &str) {
    dioxus::logger::tracing::warn!("{message}");
}
