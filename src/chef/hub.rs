//! Reaching sessions through the hub.

use std::time::Instant;

use futures::future::join_all;
use tokio::sync::mpsc;

use super::*;
use crate::remote::{self, Connection, Event, HubSession, Incoming};
use crate::web::{Approval, Status};

/// An open channel to one session, with what it has said so far.
pub(super) struct Link {
    name: String,
    connection: Connection,
    events: mpsc::UnboundedReceiver<Event>,
    pub(super) entries: Vec<Entry>,
    status: Option<Status>,
    approval: Option<Approval>,
    problem: Option<String>,
}

/// What a session said back, besides its entries and status.
pub(super) enum Heard {
    Changed,
    Answer {
        id: String,
        text: String,
        error: bool,
    },
}

impl Link {
    /// Connects and waits for the session's welcome (its entries, then its status).
    pub(super) async fn open(hub: &HubSettings, session: &HubSession) -> Result<Self, String> {
        let name = format!(
            "{} ({})",
            place(&session.host, &session.cwd),
            short_id(&session.sid)
        );
        if !session.e2e {
            return Err(format!(
                "{name} isn't end-to-end encrypted ([hub] e2e = false there), so the chef \
                 won't talk to it."
            ));
        }
        let (tx, events) = mpsc::unbounded_channel();
        let connection = Connection::open(hub, &session.sid, move |event| {
            let _ = tx.send(event);
        })?;
        let mut link = Self {
            name,
            connection,
            events,
            entries: Vec::new(),
            status: None,
            approval: None,
            problem: None,
        };
        let welcome = async {
            while link.status.is_none() {
                link.next().await?;
            }
            Ok::<(), String>(())
        };
        match tokio::time::timeout(CONNECT_TIMEOUT, welcome).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                let why = link
                    .problem
                    .clone()
                    .unwrap_or_else(|| "it didn't answer".into());
                return Err(format!("Couldn't reach {}: {why}", link.name));
            }
        }
        Ok(link)
    }

    pub(super) fn send(&self, message: Value) {
        self.connection.send(message);
    }

    /// Waits for the next thing the session says, and keeps track of it.
    pub(super) async fn next(&mut self) -> Result<Heard, String> {
        loop {
            let Some(event) = self.events.recv().await else {
                return Err(format!("The connection to {} ended.", self.name));
            };
            match event {
                Event::Live => self.problem = None,
                Event::Connecting(why) => self.problem = why,
                Event::Stopped(why) => return Err(format!("{}: {why}", self.name)),
                Event::Message(Incoming::Entries { from, items }) => {
                    self.entries.truncate(from.min(self.entries.len()));
                    self.entries.extend(items);
                    return Ok(Heard::Changed);
                }
                Event::Message(Incoming::Status(status)) => {
                    self.status = Some(*status);
                    return Ok(Heard::Changed);
                }
                Event::Message(Incoming::Approval(approval)) => {
                    self.approval = approval;
                    return Ok(Heard::Changed);
                }
                Event::Message(Incoming::Answer { id, text, error }) => {
                    return Ok(Heard::Answer { id, text, error });
                }
            }
        }
    }

    pub(super) fn running(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.running || s.compacting)
    }

    /// One line on what it's doing now.
    fn doing(&self) -> String {
        let Some(status) = &self.status else {
            return "unknown".into();
        };
        if let Some(approval) = &self.approval {
            return format!(
                "waiting for approval: {} ({})",
                approval.description,
                clip(&approval.summary, 200)
            );
        }
        if !status.running {
            return "idle".into();
        }
        let elapsed = status
            .since
            .map(|since| crate::session::now().saturating_sub(since))
            .map(|secs| format!(" for {}", duration(secs)))
            .unwrap_or_default();
        let step = if status.step.is_empty() {
            String::new()
        } else {
            format!(": {}", status.step)
        };
        format!("working{elapsed}{step} ({} steps so far)", status.steps)
    }

    fn status_lines(&self) -> String {
        let mut out = format!("Status: {}\n", self.doing());
        if let Some(status) = &self.status {
            out.push_str(&format!(
                "Model: {} · mode {} · context {}/{} tokens · ${:.2} so far\n",
                status.model, status.mode, status.context, status.window, status.cost
            ));
            if !status.queued.is_empty() {
                out.push_str(&format!("Queued messages: {}\n", status.queued.join(" | ")));
            }
        }
        out
    }
}

fn tilde(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

fn state(session: &HubSession) -> &'static str {
    if !session.online {
        "offline"
    } else if session.approval {
        "waiting for approval"
    } else if session.busy {
        "working"
    } else {
        "idle"
    }
}

impl Kitchen {
    /// The key's sessions, without this chef.
    async fn sessions(&self) -> Result<Vec<HubSession>, String> {
        let sessions = remote::sessions(&self.hub).await?;
        Ok(sessions
            .into_iter()
            .filter(|s| s.sid != self.own_sid)
            .collect())
    }

    /// The session `key` names: a short id (or any prefix of the sid), else a unique
    /// match on its place or title.
    async fn find(&self, key: &str) -> Result<HubSession, String> {
        let sessions = self.sessions().await?;
        resolve(&sessions, key).cloned()
    }

    pub(super) async fn list(&self) -> Result<String, String> {
        let sessions = self.sessions().await?;
        let mut out = String::new();
        let working = sessions.iter().filter(|s| s.online && s.busy).count();
        let waiting = sessions.iter().filter(|s| s.online && s.approval).count();
        out.push_str(&format!(
            "{} session{} on the hub ({working} working, {waiting} waiting for approval):\n",
            sessions.len(),
            if sessions.len() == 1 { "" } else { "s" }
        ));
        for s in &sessions {
            let title = if s.title.is_empty() {
                "(nothing asked yet)".to_string()
            } else {
                format!("“{}”", s.title)
            };
            let mut notes = Vec::new();
            if s.chef {
                notes.push("another chef".to_string());
            }
            if !s.e2e {
                notes.push("not encrypted, can't be reached".to_string());
            }
            if s.viewers > 0 {
                notes.push(format!("{} viewing", s.viewers));
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!(" · {}", notes.join(" · "))
            };
            out.push_str(&format!(
                "- [{}] {} — {} · {title} · {}{notes}\n",
                short_id(&s.sid),
                place(&s.host, &s.cwd),
                state(s),
                s.model,
            ));
        }
        // Sessions here that aren't on the hub can't be reached, but are worth knowing of.
        let local: Vec<_> = crate::background::list()
            .into_iter()
            .filter(|l| l.sid != self.own_sid && !sessions.iter().any(|s| s.sid == l.sid))
            .collect();
        if !local.is_empty() {
            out.push_str(
                "\nOn this machine but not on the hub (turn on /remote there to reach them):\n",
            );
            for l in local {
                let title = if l.title.is_empty() {
                    String::new()
                } else {
                    format!(" · “{}”", l.title)
                };
                out.push_str(&format!(
                    "- {} — {}{title}\n",
                    tilde(&l.cwd),
                    l.state.label()
                ));
            }
        }
        Ok(out)
    }

    pub(super) async fn read(&self, args: &Value) -> Result<String, String> {
        let key = args.get("session").and_then(Value::as_str).unwrap_or("");
        let count = args
            .get("entries")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_ENTRIES, |n| n.clamp(1, 500) as usize);
        let session = self.find(key).await?;
        let link = Link::open(&self.hub, &session).await?;
        let shown = &link.entries[link.entries.len().saturating_sub(count)..];
        let title = if session.title.is_empty() {
            String::new()
        } else {
            format!(" “{}”", session.title)
        };
        Ok(format!(
            "Session {} [{}]{title}\n{}\nTranscript ({} of {} entries):\n{}",
            place(&session.host, &session.cwd),
            short_id(&session.sid),
            link.status_lines(),
            shown.len(),
            link.entries.len(),
            transcript(shown, MAX_TRANSCRIPT_CHARS)
        ))
    }

    pub(super) async fn ask(&self, args: &Value) -> Result<String, String> {
        let question = args
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if question.is_empty() {
            return Err("`question` is empty.".into());
        }
        let keys = sessions_arg(args);
        if keys.is_empty() {
            return Err("Say which `sessions` to ask, or [\"all\"].".into());
        }
        let all = self.sessions().await?;
        let mut targets: Vec<HubSession> = Vec::new();
        if keys.iter().any(|k| k.eq_ignore_ascii_case("all")) {
            targets.extend(all.iter().filter(|s| s.online && s.e2e && !s.chef).cloned());
            if targets.is_empty() {
                return Err("No sessions are online to ask.".into());
            }
        } else {
            for key in &keys {
                let session = resolve(&all, key)?;
                if !targets.iter().any(|t| t.sid == session.sid) {
                    targets.push(session.clone());
                }
            }
        }
        let asked = join_all(
            targets
                .iter()
                .map(|session| self.ask_one(session, &question)),
        );
        let answers = asked.await;
        let mut out = String::new();
        for (session, answer) in targets.iter().zip(answers) {
            out.push_str(&format!(
                "## {} [{}]\n",
                place(&session.host, &session.cwd),
                short_id(&session.sid)
            ));
            match answer {
                Ok(text) => out.push_str(text.trim()),
                Err(e) => out.push_str(&format!("(no answer: {e})")),
            }
            out.push_str("\n\n");
        }
        Ok(out.trim_end().to_string())
    }

    async fn ask_one(&self, session: &HubSession, question: &str) -> Result<String, String> {
        if !session.online {
            return Err("it's offline".into());
        }
        let mut link = Link::open(&self.hub, session).await?;
        if !link.status.as_ref().is_some_and(|s| s.asks) {
            return Err(
                "its tamale is too old to answer side questions; update it, or use \
                 message_session (which adds to its conversation)"
                    .into(),
            );
        }
        let id = ask_id();
        link.send(json!({ "t": "ask", "id": id, "question": question }));
        let answer = async {
            loop {
                if let Heard::Answer {
                    id: answered,
                    text,
                    error,
                } = link.next().await?
                    && answered == id
                {
                    return if error { Err(text) } else { Ok(text) };
                }
            }
        };
        tokio::time::timeout(ASK_TIMEOUT, answer)
            .await
            .unwrap_or_else(|_| Err("it didn't answer in time".into()))
    }

    pub(super) async fn message(&self, args: &Value) -> Result<String, String> {
        let key = args.get("session").and_then(Value::as_str).unwrap_or("");
        let message = args
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if message.is_empty() {
            return Err("`message` is empty.".into());
        }
        let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
        let wait_secs = args
            .get("wait_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_WAIT)
            .clamp(5, MAX_WAIT);
        let session = self.find(key).await?;
        if session.chef {
            return Err("That's another chef; message a session that does the work.".into());
        }
        let mut link = Link::open(&self.hub, &session).await?;
        let start = link.entries.len();
        let text = format!("{MESSAGE_PREFIX}{message}");
        link.send(json!({ "t": "send", "text": text }));
        let name = link.name.clone();
        if !wait {
            // Give it a moment to arrive before the connection closes.
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                while !link.entries[start.min(link.entries.len())..]
                    .iter()
                    .any(|e| matches!(e, Entry::User(t) if t.contains(&message)))
                    && !link
                        .status
                        .as_ref()
                        .is_some_and(|s| s.queued.iter().any(|q| q.contains(&message)))
                {
                    if link.next().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            return Ok(format!("Sent to {name}. It's {} now.", link.doing()));
        }
        let deadline = Instant::now() + Duration::from_secs(wait_secs);
        let mut delivered: Option<usize> = None;
        // The message shows up a moment before the run it starts: done means it has been
        // taken in, something has happened since, and nothing is happening now.
        let mut started = false;
        let finished = loop {
            if delivered.is_none() {
                delivered = link.entries[start.min(link.entries.len())..]
                    .iter()
                    .position(|e| matches!(e, Entry::User(t) if t.contains(&message)))
                    .map(|i| start + i);
            }
            if let Some(at) = delivered {
                started |= link.running() || link.entries.len() > at + 1;
                if started && !link.running() && link.approval.is_none() {
                    break true;
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, link.next()).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => break false,
            }
        };
        let Some(at) = delivered else {
            return Ok(format!(
                "Sent to {name}, but it hasn't taken the message in yet (it's {}). \
                 Check on it later with read_session.",
                link.doing()
            ));
        };
        let reply = transcript(&link.entries[at + 1..], MAX_TRANSCRIPT_CHARS);
        let reply = if reply.trim().is_empty() {
            "(nothing yet)".to_string()
        } else {
            reply
        };
        Ok(if finished {
            format!("Sent to {name}; it has finished. What it did and said:\n{reply}")
        } else {
            format!(
                "Sent to {name}; it's still going after {wait_secs}s ({}). So far:\n{reply}",
                link.doing()
            )
        })
    }
}

/// The session `key` names among `sessions`.
pub(super) fn resolve<'a>(sessions: &'a [HubSession], key: &str) -> Result<&'a HubSession, String> {
    let key = key
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if key.is_empty() {
        return Err("Say which session (its short id from list_sessions).".into());
    }
    let lower = key.to_lowercase();
    let by_id: Vec<_> = sessions
        .iter()
        .filter(|s| s.sid.starts_with(&lower))
        .collect();
    if let [one] = by_id.as_slice() {
        return Ok(one);
    }
    let by_name: Vec<_> = sessions
        .iter()
        .filter(|s| {
            place(&s.host, &s.cwd).to_lowercase().contains(&lower)
                || s.title.to_lowercase().contains(&lower)
        })
        .collect();
    match by_name.as_slice() {
        [one] => Ok(one),
        [] if by_id.is_empty() => Err(format!(
            "No session matches “{key}”. Call list_sessions for their ids."
        )),
        several => {
            let several = if several.is_empty() { &by_id } else { several };
            Err(format!(
                "“{key}” matches several sessions: {}. Use a short id.",
                several
                    .iter()
                    .map(|s| format!("[{}] {}", short_id(&s.sid), place(&s.host, &s.cwd)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

fn ask_id() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
