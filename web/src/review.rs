//! The code view: pullspace, rendered in this page, on the working tree against HEAD.
//!
//! pullspace asks for both sides' file lists and for blobs; [`Code`] answers by asking
//! tamale over the socket (sealed, on an encrypted session; tamale's side is
//! `src/web/pullspace.rs`). The app tells it, through the [`Link`], when files may have
//! changed and which file the chat linked to; it tells the app what the reader asked about.

use base64::Engine;
use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use futures_channel::oneshot;
use pullspace::protocol::{BlobsReply, Open, Quote, Snapshot, Want};
use pullspace::{Host, Link, Reply};
use serde_json::{Value, json};

use crate::app::{App, View};
use crate::host;

const REVIEW_TIMEOUT: u64 = 60_000;

/// What pullspace asks this page.
struct Code {
    app: App,
}

impl Host for Code {
    fn snapshot(&self) -> Reply<Snapshot> {
        let app = self.app;
        Box::pin(async move {
            let reply = app.review_ask("ps_snapshot", json!({})).await;
            // It has the workspace (or why not), so it has something to draw.
            spawn_forever(async move {
                host::sleep(60).await;
                app.review_shown();
            });
            serde_json::from_value(reply?).map_err(|e| format!("tamale's answer: {e}"))
        })
    }

    fn blobs(&self, wants: Vec<Want>) -> Reply<Vec<(String, Vec<u8>)>> {
        let app = self.app;
        Box::pin(async move {
            let wants: Vec<Want> = wants.into_iter().take(1024).collect();
            let reply = app
                .review_ask("ps_blobs", json!({ "wants": wants }))
                .await?;
            let reply: BlobsReply =
                serde_json::from_value(reply).map_err(|e| format!("tamale's answer: {e}"))?;
            let engine = base64::engine::general_purpose::STANDARD;
            Ok(reply
                .blobs
                .into_iter()
                .filter_map(|(sha, b64)| Some((sha, engine.decode(b64?).ok()?)))
                .collect())
        })
    }

    fn quote(&self, quote: Quote) {
        self.app.review_quote(quote);
    }

    fn missing(&self, open: Open) {
        let path: String = open.path.chars().take(200).collect();
        self.app
            .toast(format!("`{path}` isn't a file in this repository."), 5000);
    }
}

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
                self.review_link().changed();
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
                host::focus_code();
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

    /// The link to pullspace, made the first time it's asked for. Events sent on it before
    /// pullspace is drawn wait for it.
    pub fn review_link(mut self) -> Link {
        if let Some(link) = self.mem.read().review_link.clone() {
            return link;
        }
        let link = Link::new(Code { app: self });
        self.mem.write().review_link = Some(link.clone());
        link
    }

    fn load_review(mut self) {
        self.mem.write().review_stale = false;
        self.review_link();
        self.review.write().loaded = true;
        // Shown once it has something to show; a module that never answers gets to say so.
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

    fn review_shown(mut self) {
        if !self.review.peek().shown {
            self.review.write().shown = true;
        }
    }

    /// After the agent changed files: the code view reads them again (when it's looking).
    pub fn review_changed(mut self) {
        if !self.review.peek().loaded {
            return;
        }
        if *self.view.peek() != View::Chat {
            self.review_link().changed();
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
        self.review_link().open(Open {
            path: path.to_string(),
            line: line.map(|l| l as usize),
            diff,
        });
    }

    /// One of pullspace's questions, asked of tamale. Asked before the connection is up (a
    /// reload that comes back side by side), or while it's down, it goes once there is one,
    /// rather than failing a read that only has to wait.
    async fn review_ask(mut self, t: &'static str, body: Value) -> Result<Value, String> {
        if !self.is_ready() {
            let (tx, rx) = oneshot::channel();
            self.mem.write().review_held.push(tx);
            let _ = rx.await;
        }
        let m = self
            .request_reply(t, body, REVIEW_TIMEOUT)
            .await
            .map_err(|e| match e.as_str() {
                "tamale didn't answer in time." => "tamale took too long to answer.".to_string(),
                _ => e,
            })?;
        match m.get("error") {
            Some(error) if !error.is_null() => Err(error
                .as_str()
                .map_or_else(|| error.to_string(), str::to_string)),
            _ => Ok(m.get("body").cloned().unwrap_or(Value::Null)),
        }
    }

    /// What the reader asked about in the code (a line, a selection, or the file), quoted
    /// into the prompt.
    fn review_quote(mut self, quote: Quote) {
        let path = quote.path;
        if path.is_empty() {
            return;
        }
        let line = quote.line;
        let end = quote.end.filter(|e| line.is_some_and(|l| *e > l));
        let text: String = quote
            .text
            .unwrap_or_default()
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
