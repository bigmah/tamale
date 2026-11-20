//! The agent socket: a tamale's own connection to the hub, which its session's page,
//! artifacts and browsers are relayed over.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::{SinkExt, StreamExt};
use tamale_hub_protocol::{self as protocol, ToAgent, ToHub, close};
use tokio::sync::mpsc;

use super::{
    AgentLink, AgentOut, Backlog, CHEF_REPLACED, Got, Hub, MAX_SESSIONS_PER_KEY, Out, PING_EVERY,
    SESSION_ENDED, SILENT_FOR, Session, bearer_value, client_ip, short,
};
use crate::state;

/// tamale's socket.
pub(super) async fn agent(
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
        chef,
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
        || sealed
            .as_ref()
            .is_some_and(|s| s.len() > protocol::MAX_SEALED)
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
        let mut count = sessions
            .iter()
            .filter(|(s, session)| **s != sid && session.key == key.name)
            .count();
        // Offline sessions are kept a long while, so at the limit the one offline longest
        // makes way, rather than a new session being refused.
        if count >= MAX_SESSIONS_PER_KEY && !taken {
            let oldest = sessions
                .iter()
                .filter(|(s, other)| **s != sid && other.key == key.name)
                .filter_map(|(s, other)| Some((other.offline_since?, s.clone())))
                .min()
                .map(|(_, s)| s);
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
                count -= 1;
            }
        }
        // One chef per key: one that's online keeps the place, and a new one takes it from
        // one that's offline (which is refused in turn if it comes back).
        let chef_online = chef
            && sessions.iter().any(|(s, other)| {
                *s != sid && other.key == key.name && other.chef && other.agent.is_some()
            });
        if taken {
            Some((close::SID_TAKEN, "that session id is in use"))
        } else if chef_online {
            Some((
                close::CHEF_TAKEN,
                "this key already has a tamale chef running; open that one instead",
            ))
        } else if count >= MAX_SESSIONS_PER_KEY {
            Some((close::TOO_MANY, "this key has too many sessions"))
        } else {
            if chef {
                // An offline chef this one replaces leaves the list, and its tabs with it.
                sessions.retain(|s, other| {
                    let replaced = *s != sid && other.key == key.name && other.chef;
                    if replaced {
                        for (_, client) in other.clients.drain() {
                            let _ = client.tx.send(Out::Close(4000, CHEF_REPLACED.into()));
                        }
                    }
                    !replaced
                });
            }
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
                chef: false,
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
            session.chef = chef;
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
        let mut ping = tokio::time::interval(PING_EVERY);
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
            next = tokio::time::timeout(SILENT_FOR, stream.next()) => next,
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
        if frame == ToHub::End {
            // Over for good: nothing to keep listing or for pages to wait for.
            if sessions
                .get(&sid)
                .is_some_and(|s| s.generation == generation)
                && let Some(mut session) = sessions.remove(&sid)
            {
                session.agent = None;
                for (_, client) in session.clients.drain() {
                    let _ = client.tx.send(Out::Close(4000, SESSION_ENDED.into()));
                }
                eprintln!("agent ended: key {} session {}", key.name, short(&sid));
            }
            break;
        }
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
                base64,
            } => {
                if let Some(waiting) = session.pending.remove(&r) {
                    let body = if base64 {
                        // Bytes tamale sent as text. Ones that don't decode are a broken page,
                        // not a reason to drop the session.
                        BASE64.decode(body).unwrap_or_default()
                    } else {
                        body.into_bytes()
                    };
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
            ToHub::Hello { .. } | ToHub::End => {}
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
