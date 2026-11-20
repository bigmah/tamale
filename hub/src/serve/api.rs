//! The public host's API, which the hub's page and native clients call: the keys a
//! browser holds, making, rotating and deleting a key, a key's sessions, and the ticket
//! that opens one.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tamale_hub_protocol as protocol;

use super::{
    Hub, MAX_TICKETS_PER_KEY, SIGNUP_WINDOW, SIGNUPS_PER_IP, TICKET_LIFE, Ticket, VIEW_COOKIE_LIFE,
    client_ip, end_session, error, guard, limit_bucket, origin_is, short,
};
use crate::state;

#[derive(Deserialize)]
pub(super) struct TokensRequest {
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
pub(super) async fn api_tokens(
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
pub(super) async fn api_sessions(
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
                "chef": s.label.chef || s.chef,
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

#[derive(Deserialize)]
pub(super) struct ForgetRequest {
    sids: Vec<String>,
}

/// Takes offline sessions under the caller's keys off the list. Online ones stay: those
/// are closed through their own page. A forgotten session's tamale coming back lists it
/// again.
pub(super) async fn api_sessions_forget(
    State(hub): State<Arc<Hub>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ForgetRequest>,
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
    let names: HashSet<&str> = keys.iter().map(|k| k.name.as_str()).collect();
    let wanted: HashSet<&str> = request.sids.iter().map(String::as_str).collect();
    let mut forgotten = Vec::new();
    hub.sessions.lock().unwrap().retain(|sid, session| {
        let drop = wanted.contains(sid.as_str())
            && session.agent.is_none()
            && names.contains(session.key.as_str());
        if drop {
            end_session(session, "This session was removed from the hub's list.");
            forgotten.push(sid.clone());
        }
        !drop
    });
    for sid in &forgotten {
        eprintln!("session {} forgotten from the page by {ip}", short(sid));
    }
    guard(Json(json!({ "forgotten": forgotten })).into_response())
}

/// Sets the dashboard cookie to `tokens` on `response`.
fn with_view_cookie(hub: &Hub, mut response: Response, tokens: &[String]) -> Response {
    let value = if tokens.is_empty() {
        hub.public.set_cookie("th", "", 0)
    } else {
        hub.public
            .set_cookie("th", &tokens.join(","), VIEW_COOKIE_LIFE)
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
pub(super) struct NewKey {
    auth: String,
    #[serde(default)]
    label: Option<String>,
}

/// Registers a key the page made in the browser. The hub gets only its auth value.
pub(super) async fn api_key_new(
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
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Couldn't save the new key.",
            );
        }
    };
    let _ = hub.reload();
    eprintln!("key {name} made from the page by {ip}");
    let (mut tokens, _) = hub.views(&headers);
    tokens.push(request.auth);
    with_view_cookie(
        &hub,
        guard(Json(json!({ "name": name })).into_response()),
        &tokens,
    )
}

#[derive(Deserialize)]
pub(super) struct KeyAction {
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
pub(super) async fn api_key_rotate(
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
pub(super) async fn api_key_delete(
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
pub(super) struct TicketRequest {
    sid: String,
}

const NO_KEY: &str = "This browser doesn't have this session's key, or the session has ended.";

/// A one-time ticket that lets the browser into one session's host.
pub(super) async fn api_ticket(
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
