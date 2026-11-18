//! One session's own host, `<sid>.<hub>`: trading a ticket for the host's cookie, its
//! page and artifacts, which are fetched from its tamale, and its socket, which is
//! relayed to it.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tamale_hub_protocol::{self as protocol, ToAgent};
use tokio::sync::{mpsc, oneshot};

use super::{
    AgentOut, Backlog, Client, GET_TIMEOUT, GRACE, Hub, MAX_CLIENTS, Out, PING_EVERY,
    SESSION_COOKIE_LIFE, SILENT_FOR, SessionCookie, client_ip, cookie, framed_by_hub, origin_is,
};
use crate::state;

#[derive(Deserialize)]
pub(super) struct AuthQuery {
    ticket: String,
}

/// Trades a ticket for this session host's cookie.
pub(super) async fn auth(
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

pub(super) async fn session_root(
    State(hub): State<Arc<Hub>>,
    Path(sid): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    tunnel(hub, sid, "/".into(), client_ip(peer, &headers), headers).await
}

pub(super) async fn session_get(
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
                | "content-encoding"
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
pub(super) async fn session_socket(
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
        // Browsers answer pings on their own, so a tab that's gone quiet (a phone asleep)
        // is noticed, and its place and its share of tamale's updates given up.
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        loop {
            let out = tokio::select! {
                out = rx.recv() => out,
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return;
                    }
                    continue;
                }
            };
            let Some(out) = out else { break };
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
            message = tokio::time::timeout(SILENT_FOR, stream.next()) => match message {
                Ok(message) => message,
                Err(_) => break,
            },
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
