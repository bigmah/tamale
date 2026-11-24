//! The code view: pullspace (tamale's `src/web/pullspace.rs`) in a frame, on the working tree
//! against HEAD. The frame asks for snapshots and blobs over `postMessage`; the app forwards
//! them over the socket (sealed, on an encrypted session) and posts the replies back, tells it
//! when files may have changed, and asks it to show the files the chat links to.

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::app::{App, View};
use crate::host;

const REVIEW_TIMEOUT: u64 = 60_000;

impl App {
    pub fn set_view(mut self, view: View, focus: bool) {
        let mut view = view;
        if view == View::Split && self.media.peek().tight {
            view = View::Code;
        }
        if view != View::Chat && self.is_chef() {
            view = View::Chat;
        }
        let stick = self.near_bottom();
        host::store_set("view", &json!(view.name()));
        if view != View::Chat {
            self.mem.write().review_last = Some(view);
            host::store_set("reviewFull", &json!(view == View::Code));
            if !self.review.peek().loaded {
                self.load_review();
            } else if self.mem.read().review_stale {
                self.mem.write().review_stale = false;
                self.review_post(json!({ "event": "changed" }));
            }
        }
        self.view.set(view);
        // The chat is a different width now; stay at the end of it.
        if view != View::Code {
            self.stick(stick);
        }
        if !focus {
            return;
        }
        spawn_forever(async move {
            host::next_frame().await;
            if view == View::Code {
                host::focus_frame("review-frame");
            } else {
                self.focus_prompt();
            }
        });
    }

    /// Brings the chat back beside the code (or in place of it, with no room for both): the
    /// agent is waiting on an answer there.
    pub fn show_chat(mut self) {
        if *self.view.peek() == View::Code {
            let view = if self.media.peek().tight {
                View::Chat
            } else {
                View::Split
            };
            self.set_view(view, false);
        }
    }

    /// The code, away and back: ⌘B, /review.
    pub fn toggle_review(mut self) {
        let view = *self.view.peek();
        if view == View::Chat {
            let last = self.mem.read().review_last.unwrap_or(View::Code);
            self.set_view(last, true);
        } else {
            self.set_view(View::Chat, true);
        }
    }

    fn load_review(mut self) {
        self.mem.write().review_stale = false;
        self.review.write().loaded = true;
        // Shown once it has something to show; a module that never arrives gets to say so.
        spawn_forever(async move {
            host::sleep(10_000).await;
            if self.review.peek().shown {
                return;
            }
            self.review.write().note = "Still loading pullspace…".into();
            host::sleep(20_000).await;
            self.review_shown();
        });
    }

    pub fn review_loaded(mut self) {
        if let Some(events) = self.host_events.read().clone() {
            host::hook_review_keys(events);
        }
    }

    fn review_shown(mut self) {
        if !self.review.peek().shown {
            self.review.write().shown = true;
        }
    }

    fn review_post(mut self, mut message: Value) {
        message["ps"] = json!(1);
        host::post_review(&message);
    }

    /// Posts an event to the frame, once it's listening.
    fn review_send(mut self, message: Value) {
        if self.mem.read().review_ready {
            self.review_post(message);
        } else {
            self.mem.write().review_queue.push(message);
        }
    }

    fn review_ready(mut self) {
        if self.mem.read().review_ready {
            return;
        }
        let queued = {
            let mut mem = self.mem.write();
            mem.review_ready = true;
            std::mem::take(&mut mem.review_queue)
        };
        for message in queued {
            self.review_post(message);
        }
    }

    /// After the agent changed files: the code view reads them again (when it's looking).
    pub fn review_changed(mut self) {
        if !self.review.peek().loaded {
            return;
        }
        if *self.view.peek() != View::Chat {
            self.review_send(json!({ "event": "changed" }));
        } else {
            self.mem.write().review_stale = true;
        }
    }

    /// Shows a file in the code view, opening it if need be: its changes, or a line of it.
    pub fn open_in_code(mut self, path: &str, line: Option<u64>, diff: bool) {
        if self.is_chef() {
            return;
        }
        if *self.view.peek() == View::Chat {
            let last = self.mem.read().review_last.unwrap_or(View::Code);
            self.set_view(last, false);
        }
        let mut body = json!({ "path": path, "diff": diff });
        if let Some(line) = line {
            body["line"] = json!(line);
        }
        self.review_send(json!({ "event": "open", "body": body }));
    }

    pub fn review_ask(mut self, id: Value, t: &'static str, body: Value) {
        // Asked before the connection is up (a reload that comes back side by side), or while
        // it's down: it goes once there is one, rather than failing a read that only has to wait.
        if !self.is_ready() {
            self.mem.write().review_held.push((id, t, body));
            return;
        }
        let seq = {
            let mut c = self.conn.write();
            c.seq += 1;
            c.seq
        };
        self.mem.write().review_pending.insert(seq, id.clone());
        let mut message = json!({ "t": t, "seq": seq });
        if let (Some(m), Value::Object(body)) = (message.as_object_mut(), body) {
            m.extend(body);
        }
        if !self.send(message, true) {
            self.mem.write().review_pending.remove(&seq);
            self.review_shown();
            return self.review_post(json!({ "id": id, "error": "Not connected to tamale." }));
        }
        spawn_forever(async move {
            host::sleep(REVIEW_TIMEOUT).await;
            if self.mem.write().review_pending.remove(&seq).is_some() {
                self.review_shown();
                self.review_post(json!({ "id": id, "error": "tamale took too long to answer." }));
            }
        });
    }

    pub fn review_reply(mut self, m: &Value) {
        let Some(seq) = m.get("seq").and_then(Value::as_u64) else {
            return;
        };
        let Some(id) = self.mem.write().review_pending.remove(&seq) else {
            return;
        };
        match m.get("error") {
            Some(error) if !error.is_null() => {
                self.review_post(json!({ "id": id, "error": error }))
            }
            _ => self.review_post(
                json!({ "id": id, "body": m.get("body").cloned().unwrap_or(Value::Null) }),
            ),
        }
        if m.get("t").and_then(Value::as_str) != Some("ps_snapshot") {
            return;
        }
        // It has the workspace (or why not), so it has something to draw. A bundle from before
        // `ready` is listening by now too.
        spawn_forever(async move {
            host::sleep(60).await;
            self.review_shown();
        });
        self.review_ready();
    }

    /// A message from the frame.
    pub fn from_review(mut self, m: Value) {
        if m.get("ps").and_then(Value::as_i64) != Some(1) {
            return;
        }
        let t = m.get("t").and_then(Value::as_str).unwrap_or("");
        match t {
            "ready" => return self.review_ready(),
            "quote" => return self.review_quote(m.get("body").cloned().unwrap_or(Value::Null)),
            "missing" => {
                let path: String = m["body"]["path"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();
                return self.toast(format!("`{path}` isn't a file in this repository."), 5000);
            }
            _ => {}
        }
        let Some(id) = m.get("id").filter(|id| id.is_number()).cloned() else {
            return;
        };
        match t {
            "snapshot" => self.review_ask(id, "ps_snapshot", json!({})),
            "blobs" => {
                let wants: Vec<Value> = m["body"]["wants"]
                    .as_array()
                    .map(|w| {
                        w.iter()
                            .take(1024)
                            .filter(|w| w["sha"].is_string() && w["path"].is_string())
                            .map(|w| json!({ "sha": w["sha"], "path": w["path"] }))
                            .collect()
                    })
                    .unwrap_or_default();
                self.review_ask(id, "ps_blobs", json!({ "wants": wants }));
            }
            _ => self.review_post(
                json!({ "id": id, "error": format!("tamale doesn't answer \"{t}\"") }),
            ),
        }
    }

    /// What the reader asked about in the code (a line, a selection, or the file), quoted
    /// into the prompt.
    fn review_quote(mut self, body: Value) {
        let path = body["path"].as_str().unwrap_or("").to_string();
        if path.is_empty() {
            return;
        }
        let line = body["line"].as_f64().map(|l| l as u64);
        if !body["line"].is_null() && line.is_none() {
            return;
        }
        let end = body["end"]
            .as_f64()
            .map(|e| e as u64)
            .filter(|e| line.is_some_and(|l| *e > l));
        let text: String = body["text"]
            .as_str()
            .unwrap_or("")
            .replace('\r', "")
            .trim_end_matches('\n')
            .chars()
            .take(20_000)
            .collect();
        let place = match (line, end) {
            (None, _) => String::new(),
            (Some(l), Some(e)) => format!(" (lines {l}–{e})"),
            (Some(l), None) => format!(" (line {l})"),
        };
        // A fence longer than any run of backticks in what it quotes.
        let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat((longest + 1).max(3));
        let quote = if text.trim().is_empty() {
            format!("In `{path}`{place}:\n\n")
        } else {
            format!("In `{path}`{place}:\n{fence}\n{text}\n{fence}\n\n")
        };
        // The prompt has to be on screen to be typed into.
        self.show_chat();
        self.append_to_input(&quote);
    }
}
