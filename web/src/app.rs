//! The app's state, and what it does with it: everything the page knows about the session,
//! as signals the components read, and the actions that change it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use futures_channel::oneshot;
use serde_json::{Value, json};

use crate::host::{self, FrameTarget, Place};
use crate::html;
use crate::voice::Voice;
use crate::wire::{self, Approval, Entry, Kitchen, Model, Questions, SavedSession, Status};

/// The prompt's element id.
pub const PROMPT: &str = "input";
/// The chat's scrolling box.
pub const SCROLLER: &str = "scroller";

/// One entry, shared: components compare them by identity, so an update only redraws the
/// entries it replaced.
#[derive(Clone, Debug)]
pub struct Item(pub Rc<Entry>);

impl PartialEq for Item {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// What's on screen: the chat, the code, or both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Chat,
    Split,
    Code,
}

impl View {
    pub fn name(mut self) -> &'static str {
        match self {
            View::Chat => "chat",
            View::Split => "split",
            View::Code => "code",
        }
    }
}

/// The connection dot in the top bar: its class and what it says.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct ConnView {
    pub kind: &'static str,
    pub title: String,
}

/// The page can't show this session, and why.
#[derive(Clone, PartialEq, Debug)]
pub struct Lock {
    pub title: String,
    pub text: String,
    /// The hub's page for this session, as a way on.
    pub link: Option<String>,
}

/// An answer being put together for one of the agent's questions.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Draft {
    pub chosen: Vec<String>,
    pub other: String,
}

impl Draft {
    pub fn answered(&self) -> bool {
        !self.chosen.is_empty() || !self.other.trim().is_empty()
    }
}

/// The layout's media queries.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Media {
    /// A phone or tablet.
    pub touch: bool,
    /// Too narrow for side-by-side panes.
    pub narrow: bool,
    /// Too narrow for the chat and the code at once.
    pub tight: bool,
}

impl Media {
    pub fn now() -> Self {
        Media {
            touch: host::touch(),
            narrow: host::matches("(max-width: 760px)"),
            tight: host::matches("(max-width: 980px)"),
        }
    }
}

/// How much has changed in the working tree, for the code button.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Changes {
    pub files: u64,
    pub added: u64,
    pub removed: u64,
}

/// The code view's frame, as the page shows it.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct ReviewView {
    /// Whether the frame has been made: it's loaded the first time the code is shown.
    pub loaded: bool,
    /// Whether it has something to show.
    pub shown: bool,
    pub note: String,
}

/// The artifacts gallery.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Gallery {
    pub open: bool,
    pub loading: bool,
    pub sessions: Vec<SavedSession>,
    pub query: String,
    /// The row shown, by its artifact's URL.
    pub at: Option<String>,
    /// On a phone, the artifact is shown instead of the list.
    pub viewing: bool,
}

/// One of the gallery's rows.
#[derive(Clone, PartialEq, Debug)]
pub enum GalleryRow {
    Group {
        name: String,
        when: String,
    },
    Item {
        url: String,
        title: String,
        file: String,
        modified: f64,
        current: bool,
    },
}

/// The model picker.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Picker {
    pub open: bool,
    pub loaded: bool,
    pub models: Vec<Model>,
    pub current: String,
    pub error: Option<String>,
    pub query: String,
    pub at: usize,
}

/// One row of the picker: a listed model, or an id typed that isn't listed.
#[derive(Clone, PartialEq, Debug)]
pub struct PickerRow {
    pub id: String,
    pub name: String,
    pub typed: bool,
}

/// Closing the session.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Ending {
    pub open: bool,
    /// Asked for while not connected: sent once there is a connection.
    pub due: bool,
    pub sent: bool,
}

/// Overlays the browser's Back button closes, innermost last.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    FullArtifact,
    Gallery,
    GalleryPane,
    Models,
    Kitchen,
}

/// The connection's own state, which nothing draws.
#[derive(Default)]
pub struct ConnState {
    pub socket: Option<Rc<host::Socket>>,
    /// Counts connections, so a late event from an old one is known for what it is.
    pub generation: u64,
    pub handshake: Option<crate::e2e::Handshake>,
    pub channel: Option<crate::e2e::Channel>,
    pub retry: u32,
    /// The next attempt, while one is waiting: its number, to cancel it by.
    pub retry_due: Option<u64>,
    pub timers: u64,
    /// Shows a dropped connection, if it's still down by then: a blip shouldn't flash.
    pub down_due: Option<u64>,
    pub connected_at: f64,
    pub last_heard: f64,
    pub locked: bool,
    pub seq: u64,
    pub awaited: HashMap<u64, oneshot::Sender<Result<Value, String>>>,
    pub latest: HashMap<&'static str, u64>,
}

/// What the page remembers without drawing it.
#[derive(Default)]
pub struct Mem {
    pub history: Vec<String>,
    pub history_at: usize,
    pub draft: String,
    pub approval_at: f64,
    pub question_at: f64,
    pub run_since: Option<u64>,
    /// The view the code was last shown in, for ⌘B and /review to come back to.
    pub review_last: Option<View>,
    pub review_ready: bool,
    pub review_queue: Vec<Value>,
    /// Socket seq → the frame's request id.
    pub review_pending: HashMap<u64, Value>,
    /// What the frame asked for while there was no connection to ask over.
    pub review_held: Vec<(Value, &'static str, Value)>,
    /// Files may have changed while the code was out of sight.
    pub review_stale: bool,
    pub refresh: u64,
    pub toast: u64,
    /// The document at each artifact URL, once it came.
    pub artifact_pages: HashMap<String, String>,
    /// What's waiting for each artifact id.
    pub artifacts_due: HashMap<String, Vec<oneshot::Sender<String>>>,
    /// Frames and tabs waiting for an artifact, and which.
    pub frames_due: Vec<(FrameTarget, String)>,
    /// Each video's blob URL and type, by path, once it came.
    pub videos: HashMap<String, (String, String)>,
    pub kitchen_seen: HashMap<String, String>,
    pub layers: Vec<Layer>,
    pub chef_ready: bool,
}

/// Everything the page knows about the session. It's `Copy`: every handle is a signal.
#[derive(Clone, Copy)]
pub struct App {
    pub place: CopyValue<Place>,
    pub conn: CopyValue<ConnState>,
    pub mem: CopyValue<Mem>,
    pub host_events: CopyValue<Option<futures_channel::mpsc::UnboundedSender<host::HostEvent>>>,

    pub entries: Signal<Vec<Item>>,
    pub status: Signal<Option<Status>>,
    pub approval: Signal<Option<Approval>>,
    pub question: Signal<Option<Questions>>,
    pub answers: Signal<Vec<Draft>>,
    pub asking: Signal<usize>,
    pub kitchen: Signal<Option<Kitchen>>,
    pub chef: Signal<bool>,
    pub kitchen_open: Signal<bool>,
    pub kitchen_drawer: Signal<bool>,
    pub kitchen_changed: Signal<HashSet<String>>,
    pub conn_view: Signal<ConnView>,
    pub lock: Signal<Option<Lock>>,
    pub toast: Signal<Option<String>>,
    pub input: Signal<String>,
    pub slash_at: Signal<usize>,
    pub slash_hidden: Signal<bool>,
    pub view: Signal<View>,
    pub review: Signal<ReviewView>,
    pub changes: Signal<Changes>,
    pub jump: Signal<bool>,
    /// Ticks each second while the agent works, for the elapsed times.
    pub tick: Signal<u64>,
    pub media: Signal<Media>,
    pub gallery: Signal<Gallery>,
    pub picker: Signal<Picker>,
    pub ending: Signal<Ending>,
    pub help: Signal<bool>,
    /// The artifact shown full screen, by its frame's key.
    pub full: Signal<Option<String>>,
    /// Each artifact frame's height, by its key, as the document in it asked.
    pub heights: Signal<HashMap<String, f64>>,
    /// A long run finished out of sight, and a card waits on the user out of sight.
    pub done_unseen: Signal<bool>,
    pub warn: Signal<bool>,
    pub chat_share: Signal<Option<f64>>,
    pub kitchen_width: Signal<Option<f64>>,
    pub art_width: Signal<Option<f64>>,
    pub voice: Voice,
    /// The approval's reason box is open.
    pub denying: Signal<bool>,
    /// The seam being dragged.
    pub dragging: Signal<Option<Seam>>,
}

/// The seams between panes, which drag to resize them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seam {
    /// Between the chat and the code: the chat's share of the width.
    Chat,
    /// Before a chef's kitchen: its width.
    Kitchen,
    /// In the artifacts gallery, between the list and the artifact: the list's width.
    Art,
}

impl App {
    pub fn new() -> App {
        let place = host::place();
        let mut mem = Mem::default();
        mem.history = host::store_get("history")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        mem.history_at = mem.history.len();
        let review_full = host::store_get("reviewFull")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        mem.review_last = Some(if review_full { View::Code } else { View::Split });
        let share = |key| host::store_get(key).and_then(|v| v.as_f64());
        App {
            place: CopyValue::new(place),
            conn: CopyValue::new(ConnState::default()),
            mem: CopyValue::new(mem),
            host_events: CopyValue::new(None),
            entries: Signal::new(Vec::new()),
            status: Signal::new(None),
            approval: Signal::new(None),
            question: Signal::new(None),
            answers: Signal::new(Vec::new()),
            asking: Signal::new(0),
            kitchen: Signal::new(None),
            chef: Signal::new(false),
            kitchen_open: Signal::new(false),
            kitchen_drawer: Signal::new(false),
            kitchen_changed: Signal::new(HashSet::new()),
            conn_view: Signal::new(ConnView {
                kind: "",
                title: "Connecting…".into(),
            }),
            lock: Signal::new(None),
            toast: Signal::new(None),
            input: Signal::new(String::new()),
            slash_at: Signal::new(0),
            slash_hidden: Signal::new(false),
            view: Signal::new(View::Chat),
            review: Signal::new(ReviewView {
                note: "Opening the code…".into(),
                ..Default::default()
            }),
            changes: Signal::new(Changes::default()),
            jump: Signal::new(false),
            tick: Signal::new(0),
            media: Signal::new(Media::now()),
            gallery: Signal::new(Gallery::default()),
            picker: Signal::new(Picker::default()),
            ending: Signal::new(Ending::default()),
            help: Signal::new(false),
            full: Signal::new(None),
            heights: Signal::new(HashMap::new()),
            done_unseen: Signal::new(false),
            warn: Signal::new(false),
            chat_share: Signal::new(share("chatShare")),
            kitchen_width: Signal::new(share("kitchenWidth")),
            art_width: Signal::new(share("artListWidth")),
            voice: Voice::new(),
            denying: Signal::new(false),
            dragging: Signal::new(None),
        }
    }

    pub fn e2e(&self) -> bool {
        self.place.read().e2e.is_some()
    }

    pub fn is_chef(&self) -> bool {
        *self.chef.peek()
    }

    pub fn running(&self) -> bool {
        self.status.peek().as_ref().is_some_and(|s| s.running)
    }

    // ------------------------------------------------------------ toasts and the title

    /// Says `text` for a while: above the prompt, and over the code when that has the page.
    pub fn toast(mut self, text: impl Into<String>, ms: u64) {
        let generation = {
            let mut mem = self.mem.write();
            mem.toast += 1;
            mem.toast
        };
        self.toast.set(Some(text.into()));
        spawn_forever(async move {
            host::sleep(ms).await;
            if self.mem.read().toast == generation {
                self.toast.set(None);
            }
        });
    }

    pub fn copy(mut self, text: String, what: &'static str) {
        spawn_forever(async move {
            if host::copy_text(&text).await {
                self.toast(what, 1800);
            } else {
                self.toast("The browser blocked the clipboard.", 5000);
            }
        });
    }

    /// The tab's title: the project (or chef), marked while the agent works, when a long run
    /// finished out of sight, and when a card waits out of sight.
    pub fn update_title(&self) {
        let status = self.status.peek();
        let name = match status.as_ref() {
            Some(s) if s.chef => "chef".to_string(),
            Some(s) => s.project.clone(),
            None => String::new(),
        };
        let running = status.as_ref().is_some_and(|s| s.running);
        let mark = if *self.warn.peek() {
            "⚠ "
        } else if running {
            "● "
        } else if *self.done_unseen.peek() {
            "✓ "
        } else {
            ""
        };
        host::set_title(&if name.is_empty() {
            "tamale".to_string()
        } else {
            format!("{mark}{name} · tamale")
        });
    }

    // ------------------------------------------------------------ messages

    pub fn on_message(mut self, m: Value) {
        // The welcome's first message: drawn by the time it's painted.
        host::shown();
        let seq = m.get("seq").and_then(Value::as_u64);
        if let Some(seq) = seq {
            let waiting = self.conn.write().awaited.remove(&seq);
            if let Some(done) = waiting {
                let _ = done.send(Ok(m));
                return;
            }
        }
        let latest = |kind: &str| self.conn.read().latest.get(kind).copied();
        match m.get("t").and_then(Value::as_str).unwrap_or("") {
            "entries" => {
                let from = m.get("from").and_then(Value::as_u64).unwrap_or(0) as usize;
                self.apply_entries(from, wire::entries(m.get("items")));
            }
            "status" => {
                if let Some(status) = wire::read_opt(m.get("status")) {
                    self.apply_status(status);
                }
            }
            "approval" => self.apply_approval(wire::read_opt(m.get("approval"))),
            "question" => self.apply_question(wire::read_opt(m.get("question"))),
            "roots" => {
                self.ask_changes();
                self.review_changed();
            }
            "diff_stat" => {
                if seq.is_some() && seq == latest("diff_stat") {
                    let n = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
                    self.changes.set(Changes {
                        files: n("files"),
                        added: n("added"),
                        removed: n("removed"),
                    });
                }
            }
            "ps_snapshot" | "ps_blobs" => self.review_reply(&m),
            "artifacts" => {
                if seq.is_some() && seq == latest("artifacts") {
                    let sessions: Vec<SavedSession> = wire::read(m.get("sessions"));
                    self.apply_gallery(sessions);
                }
            }
            "artifact" => self.on_artifact(&m),
            "kitchen" => {
                if let Some(kitchen) = wire::read_opt::<Kitchen>(m.get("kitchen")) {
                    self.apply_kitchen(kitchen);
                }
            }
            "models" => {
                if seq.is_some() && seq == latest("models") {
                    let mut picker = self.picker.write();
                    picker.models = wire::read(m.get("models"));
                    picker.current = m
                        .get("current")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    picker.error = m.get("error").and_then(Value::as_str).map(str::to_string);
                    picker.loaded = true;
                    let rows = picker_rows(&picker);
                    picker.at = rows
                        .iter()
                        .position(|r| r.id == picker.current)
                        .unwrap_or(0);
                }
                self.scroll_model_into_view();
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ the chat

    pub fn near_bottom(&self) -> bool {
        host::near_bottom(SCROLLER, 80.0)
    }

    /// Scrolls to the end once what's changing is drawn, if `stick`.
    pub fn stick(mut self, stick: bool) {
        if !stick {
            return;
        }
        spawn_forever(async move {
            host::next_frame().await;
            host::scroll_to_bottom(SCROLLER, false);
        });
    }

    pub fn scroll_to_bottom(mut self, smooth: bool) {
        spawn_forever(async move {
            host::next_frame().await;
            host::scroll_to_bottom(SCROLLER, smooth);
        });
    }

    fn apply_entries(mut self, from: usize, items: Vec<Entry>) {
        let stick = self.near_bottom();
        let tool_done = items.iter().any(|e| {
            matches!(
                e,
                Entry::Tool {
                    output: Some(_),
                    ..
                }
            )
        });
        {
            let mut entries = self.entries.write();
            let from = from.min(entries.len());
            // Entries that are the same as before stay the same: only what changed redraws.
            let mut kept = from;
            let fresh: Vec<Item> = items
                .into_iter()
                .enumerate()
                .map(|(k, e)| match entries.get(from + k) {
                    Some(old) if *old.0 == e && kept == from + k => {
                        kept += 1;
                        old.clone()
                    }
                    _ => Item(Rc::new(e)),
                })
                .collect();
            entries.truncate(from);
            entries.extend(fresh);
        }
        self.stick(stick);
        if tool_done {
            self.after_tool_result();
        }
    }

    fn apply_status(mut self, s: Status) {
        let was_running = self.running();
        if s.chef {
            self.set_up_chef();
        }
        let now = host::now_ms() / 1000.0;
        let run_since = self.mem.read().run_since;
        // A long run that finished while you were away says so in the tab until you look.
        let ran_long =
            was_running && !s.running && run_since.is_some_and(|since| now - since as f64 >= 60.0);
        if ran_long && !host::has_focus() {
            self.done_unseen.set(true);
        }
        if s.running {
            self.done_unseen.set(false);
        }
        self.mem.write().run_since = if s.running { s.since } else { None };
        let stick = self.near_bottom();
        let running = s.running;
        self.status.set(Some(s));
        self.update_title();
        self.stick(stick);
        // The agent may have changed files while it ran.
        if was_running && !running {
            self.after_tool_result();
            self.voice.turn_ended(self);
        }
    }

    fn apply_approval(mut self, a: Option<Approval>) {
        let had = self.approval.peek().is_some();
        let stick = self.near_bottom();
        match &a {
            None => {
                if had {
                    self.warn.set(false);
                    self.voice.answered(self, "approval");
                }
            }
            Some(a) => {
                self.mem.write().approval_at = host::now_ms();
                if !had {
                    self.show_chat();
                    self.voice.approval(self, a);
                }
                if !host::has_focus() {
                    self.warn.set(true);
                }
            }
        }
        self.approval.set(a);
        self.update_title();
        self.stick(stick);
    }

    fn apply_question(mut self, q: Option<Questions>) {
        let had = self.question.peek().clone();
        // A resync sends the same questions again; keep what's been picked.
        if q.is_some() && had.is_some() && q == had {
            return;
        }
        let stick = self.near_bottom();
        match &q {
            None => {
                if had.is_some() {
                    self.warn.set(false);
                    self.voice.answered(self, "question");
                }
            }
            Some(q) => {
                self.voice.questions(self, q);
                self.mem.write().question_at = host::now_ms();
                self.show_chat();
                self.answers.set(vec![Draft::default(); q.questions.len()]);
                self.asking.set(0);
                if !host::has_focus() {
                    self.warn.set(true);
                }
            }
        }
        self.question.set(q);
        self.update_title();
        self.stick(stick);
    }

    /// The window got the focus: what happened out of sight has been seen.
    pub fn focused(mut self) {
        if *self.done_unseen.peek() {
            self.done_unseen.set(false);
        }
        if *self.warn.peek() {
            self.warn.set(false);
        }
        self.update_title();
    }

    pub fn approve(mut self, choice: &str, reason: Option<String>) {
        let mut message = json!({ "t": "approve", "choice": choice });
        if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
            message["reason"] = json!(reason);
        }
        self.send(message, false);
    }

    pub fn choose_option(mut self, row: usize) {
        let Some(set) = self.question.peek().clone() else {
            return;
        };
        let at = *self.asking.peek();
        let Some(q) = set.questions.get(at) else {
            return;
        };
        let Some(label) = q.options.get(row).map(|o| o.label.clone()) else {
            return;
        };
        let mut answers = self.answers.write();
        let Some(a) = answers.get_mut(at) else { return };
        if q.multi_select {
            if a.chosen.contains(&label) {
                a.chosen.retain(|l| l != &label);
            } else {
                a.chosen.push(label);
            }
            // Keep the options' order.
            let order: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
            a.chosen.sort_by_key(|l| order.iter().position(|o| o == l));
        } else {
            a.chosen = vec![label];
            a.other.clear();
        }
    }

    pub fn go_question(mut self, i: isize) {
        let count = self
            .question
            .peek()
            .as_ref()
            .map_or(0, |q| q.questions.len());
        if count == 0 {
            return;
        }
        self.asking.set(i.clamp(0, count as isize - 1) as usize);
    }

    /// Moves on once the shown question is answered; from the last, sends them all.
    pub fn next_question(mut self) {
        let Some(set) = self.question.peek().clone() else {
            return;
        };
        let at = *self.asking.peek();
        let answers = self.answers.peek().clone();
        if !answers.get(at).is_some_and(Draft::answered) {
            return;
        }
        if at + 1 < set.questions.len() {
            return self.go_question(at as isize + 1);
        }
        if let Some(missing) = answers.iter().position(|a| !a.answered()) {
            return self.go_question(missing as isize);
        }
        let answers: Vec<wire::Answer> = answers
            .into_iter()
            .map(|a| wire::Answer {
                chosen: a.chosen,
                other: Some(a.other.trim().to_string()).filter(|o| !o.is_empty()),
            })
            .collect();
        self.send(json!({ "t": "answer", "answers": answers }), false);
    }

    // ------------------------------------------------------------ the composer

    /// Sets the prompt to `text`, the caret at its end, ready to type.
    pub fn set_input(mut self, text: impl Into<String>) {
        let text = text.into();
        host::set_value(PROMPT, &text);
        self.input.set(text);
        self.slash_at.set(0);
        self.slash_hidden.set(false);
        host::autosize(PROMPT);
        host::focus(PROMPT);
        host::set_caret(PROMPT, None);
    }

    /// Focuses the prompt, except on touch screens, where that would pop up the keyboard.
    pub fn focus_prompt(&self) {
        if !self.media.peek().touch {
            host::focus(PROMPT);
        }
    }

    pub fn submit(mut self) {
        let text = self.input.peek().trim().to_string();
        if text.is_empty() {
            return;
        }
        {
            let mut mem = self.mem.write();
            if mem.history.last() != Some(&text) {
                mem.history.push(text.clone());
                if mem.history.len() > 200 {
                    mem.history.remove(0);
                }
                host::store_set("history", &json!(mem.history));
            }
            mem.history_at = mem.history.len();
            mem.draft.clear();
        }
        if text.starts_with('/') && self.run_local(&text) {
            self.set_input("");
            return;
        }
        if self.send(json!({ "t": "send", "text": text }), false) {
            self.set_input("");
            // Put the keyboard away so the reply has the screen.
            if self.media.peek().touch {
                host::blur(PROMPT);
            }
            self.scroll_to_bottom(true);
        }
    }

    /// Sends `text` as a prompt straight away, as if typed and sent.
    pub fn ask(mut self, text: &str) {
        self.set_input(text);
        self.submit();
    }

    /// Adds `text` to the prompt on a paragraph of its own, ready for a question.
    pub fn append_to_input(mut self, text: &str) {
        let mut value = self.input.peek().clone();
        if !value.trim().is_empty() && !value.ends_with("\n\n") {
            value.push_str(if value.ends_with('\n') { "\n" } else { "\n\n" });
        }
        value.push_str(text);
        self.set_input(value);
    }

    /// An earlier prompt (`back`) or a later one, as ↑ and ↓ recall them.
    pub fn recall(mut self, back: bool) -> bool {
        let value = self.input.peek().clone();
        let text = {
            let mut mem = self.mem.write();
            if back {
                if mem.history_at == 0 {
                    return false;
                }
                if mem.history_at == mem.history.len() {
                    mem.draft = value;
                }
                mem.history_at -= 1;
                mem.history[mem.history_at].clone()
            } else {
                if mem.history_at >= mem.history.len() {
                    return false;
                }
                mem.history_at += 1;
                if mem.history_at == mem.history.len() {
                    mem.draft.clone()
                } else {
                    mem.history[mem.history_at].clone()
                }
            }
        };
        self.set_input(text);
        true
    }

    /// Commands that run in the page. True if `text` was one.
    pub fn run_local(mut self, text: &str) -> bool {
        let name = text.split_whitespace().next().unwrap_or("");
        let arg = text[name.len()..].trim();
        match name {
            // pullspace is the page's code browser and diff viewer, so these open it too.
            "/review" | "/code" | "/browse" | "/diff" | "/files" => {
                if arg.is_empty() {
                    self.toggle_review();
                } else {
                    let (path, line) = html::file_path(arg).unwrap_or((arg.to_string(), None));
                    self.open_in_code(&path, line, false);
                }
                true
            }
            "/help" => {
                let mut help = self.help;
                help.set(true);
                true
            }
            "/quit" | "/exit" => {
                self.open_end_dialog();
                true
            }
            "/artifacts" => {
                self.open_gallery();
                true
            }
            "/video" | "/manim" => {
                let text = if arg.is_empty() {
                    VIDEO_PROMPT.to_string()
                } else {
                    format!("Make an animated video with Manim of: {arg}")
                };
                if self.send(json!({ "t": "send", "text": text }), false) {
                    self.scroll_to_bottom(true);
                }
                true
            }
            "/voice" | "/gemini" => {
                self.voice.start(self, None, host::Audio::new());
                true
            }
            "/model" | "/models" if arg.is_empty() => {
                self.open_models();
                true
            }
            _ => {
                let always = ["/rewind", "/login", "/paste", "/browser"].contains(&name);
                match terminal_only(name) {
                    Some(note) if arg.is_empty() || always => {
                        self.toast(note, 7000);
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// After the agent ran a tool: files may have changed.
    pub fn after_tool_result(mut self) {
        let generation = {
            let mut mem = self.mem.write();
            mem.refresh += 1;
            mem.refresh
        };
        spawn_forever(async move {
            host::sleep(400).await;
            if self.mem.read().refresh == generation {
                self.ask_changes();
                self.review_changed();
            }
        });
    }

    // ------------------------------------------------------------ the chef

    fn set_up_chef(mut self) {
        if self.mem.read().chef_ready {
            return;
        }
        self.mem.write().chef_ready = true;
        self.chef.set(true);
        host::body_class("chef", true);
        // A chef has sessions to look after, not a repository to read.
        self.set_view(View::Chat, false);
        host::theme_color("#1f1a12");
        self.kitchen_open.set(!self.media.peek().narrow);
    }

    fn apply_kitchen(mut self, k: Kitchen) {
        let mut seen = HashMap::new();
        let mut changed = HashSet::new();
        {
            let mem = self.mem.read();
            for s in &k.sessions {
                seen.insert(s.id.clone(), s.state.clone());
                if !mem.kitchen_seen.is_empty() && mem.kitchen_seen.get(&s.id) != Some(&s.state) {
                    changed.insert(s.id.clone());
                }
            }
        }
        if !k.sessions.is_empty() {
            self.mem.write().kitchen_seen = seen;
        }
        self.kitchen_changed.set(changed);
        self.kitchen.set(Some(k));
    }

    pub fn toggle_kitchen(mut self) {
        if !self.media.peek().narrow {
            // Wide enough to sit beside the chat: the button shows or hides it.
            let open = *self.kitchen_open.peek();
            self.kitchen_open.set(!open);
        } else if !*self.kitchen_open.peek() {
            self.kitchen_open.set(true);
            self.kitchen_drawer.set(true);
            self.push_layer(Layer::Kitchen);
        } else {
            self.close_kitchen();
        }
    }

    pub fn close_kitchen(mut self) {
        self.kitchen_open.set(false);
        self.kitchen_drawer.set(false);
        self.drop_layer(Layer::Kitchen);
    }

    // ------------------------------------------------------------ layers

    pub fn push_layer(mut self, layer: Layer) {
        self.mem.write().layers.push(layer);
        self.sync_history();
    }

    /// Forgets `layer` when its overlay is closed some other way, rewinding its entry.
    pub fn drop_layer(mut self, layer: Layer) {
        let dropped = {
            let mut mem = self.mem.write();
            match mem.layers.iter().rposition(|l| *l == layer) {
                Some(at) => {
                    mem.layers.remove(at);
                    true
                }
                None => false,
            }
        };
        if dropped {
            self.sync_history();
        }
    }

    /// Brings the history entries in line with the open layers once the current change is
    /// done, so closing one overlay and opening another reuses the entry rather than racing
    /// Back (which is asynchronous) against a push.
    fn sync_history(mut self) {
        spawn_forever(async move {
            let n = self.mem.read().layers.len();
            host::sync_history(n);
        });
    }

    /// The browser's Back: closes the innermost overlay.
    pub fn back(mut self) {
        let layer = self.mem.write().layers.pop();
        match layer {
            Some(Layer::FullArtifact) => self.close_full(),
            Some(Layer::Gallery) => self.close_gallery(),
            Some(Layer::GalleryPane) => self.close_gallery_pane(),
            Some(Layer::Models) => self.close_models(),
            Some(Layer::Kitchen) => self.close_kitchen(),
            None => {}
        }
    }

    // ------------------------------------------------------------ artifacts

    pub fn open_full(mut self, key: String) {
        if self.full.peek().as_deref() == Some(&key) {
            return;
        }
        self.close_full();
        self.full.set(Some(key));
        host::body_class("artifact-open", true);
        self.push_layer(Layer::FullArtifact);
    }

    pub fn close_full(mut self) {
        if self.full.peek().is_none() {
            return;
        }
        self.full.set(None);
        host::body_class("artifact-open", false);
        self.drop_layer(Layer::FullArtifact);
    }

    /// What an artifact frame's `src` is: the artifact itself, or on an encrypted session
    /// (where it can't come through the hub by URL) the empty page it's posted to.
    pub fn frame_src(&self, url: &str) -> String {
        let place = self.place.read();
        if place.post_artifacts {
            format!("{}/artifact-frame", place.frames)
        } else {
            format!("{}{url}", place.base)
        }
    }

    /// Whether an artifact's links (open, download) are the app's to follow rather than the
    /// browser's.
    pub fn posting(&self) -> bool {
        self.place.read().post_artifacts
    }

    /// The document at `url` (`/artifact/<id>`), asked of tamale over the socket.
    pub async fn artifact_html(mut self, url: String) -> String {
        if let Some(page) = self.mem.read().artifact_pages.get(&url) {
            return page.clone();
        }
        let id = url.trim_start_matches("/artifact/").to_string();
        let (tx, rx) = oneshot::channel();
        let first = {
            let mut mem = self.mem.write();
            let due = mem.artifacts_due.entry(id.clone()).or_default();
            due.push(tx);
            due.len() == 1
        };
        if first {
            self.send(json!({ "t": "artifact", "id": id }), true);
        }
        rx.await.unwrap_or_default()
    }

    fn on_artifact(mut self, m: &Value) {
        let id = m
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let html = m.get("html").and_then(Value::as_str).map(str::to_string);
        let url = format!("/artifact/{id}");
        let page = html
            .clone()
            .unwrap_or_else(|| "<p>No such artifact; it may be from an earlier run.</p>".into());
        let waiting = {
            let mut mem = self.mem.write();
            if html.is_some() {
                mem.artifact_pages.insert(url, page.clone());
            }
            mem.artifacts_due.remove(&id).unwrap_or_default()
        };
        for tx in waiting {
            let _ = tx.send(page.clone());
        }
    }

    /// A frame (or tab) on `/artifact-frame` is ready: sends it its artifact.
    pub fn frame_ready(mut self, target: FrameTarget) {
        let url = {
            let mut mem = self.mem.write();
            let at = mem.frames_due.iter().position(|(t, _)| *t == target);
            at.map(|at| mem.frames_due[at].1.clone())
        };
        let Some(url) = url else { return };
        spawn_forever(async move {
            let html = self.artifact_html(url).await;
            host::post_artifact(&target, &html);
        });
    }

    /// Notes which artifact the frame with `key` shows, for when it asks.
    pub fn frame_wants(mut self, key: &str, url: &str) {
        let mut mem = self.mem.write();
        let target = FrameTarget::Frame(key.to_string());
        mem.frames_due.retain(|(t, _)| *t != target);
        mem.frames_due.push((target, url.to_string()));
    }

    pub fn open_artifact_tab(mut self, url: &str) {
        if !self.e2e() {
            let base = self.place.read().base.clone();
            host::open_tab(&format!("{base}{url}"));
            return;
        }
        // The frame page clears its opener before it writes the artifact.
        match host::open_frame_tab(&self.frame_src(url)) {
            Some(tab) => self
                .mem
                .write()
                .frames_due
                .push((FrameTarget::Tab(tab), url.to_string())),
            None => self.toast("The browser blocked opening a new tab.", 5000),
        }
    }

    pub fn save_artifact(mut self, url: String, title: String) {
        spawn_forever(async move {
            let html = self.artifact_html(url).await;
            host::download(
                &format!("{}.html", html::slug(&title)),
                html.as_bytes(),
                "text/html",
            );
        });
    }

    pub fn set_height(mut self, frame: String, height: f64) {
        let stick = self.near_bottom();
        let height = height.clamp(120.0, 8000.0);
        if self.heights.peek().get(&frame) == Some(&height) {
            return;
        }
        self.heights.write().insert(frame, height);
        self.stick(stick);
    }

    /// A video's blob URL and type, asked of tamale once.
    pub async fn video(mut self, path: String) -> Result<(String, String), String> {
        if let Some(got) = self.mem.read().videos.get(&path) {
            return Ok(got.clone());
        }
        let m = self
            .request_reply("video", json!({ "path": path }), 180_000)
            .await?;
        if let Some(error) = m.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        let mime = m
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("video/mp4")
            .to_string();
        let data = m
            .get("data")
            .and_then(Value::as_str)
            .and_then(crate::e2e::decode)
            .unwrap_or_default();
        let url = host::blob_url(&data, &mime).ok_or("Couldn't play the video here.")?;
        self.mem
            .write()
            .videos
            .insert(path, (url.clone(), mime.clone()));
        Ok((url, mime))
    }

    // ------------------------------------------------------------ the gallery

    pub fn open_gallery(mut self) {
        if self.gallery.peek().open {
            return;
        }
        self.close_full();
        {
            let mut g = self.gallery.write();
            g.open = true;
            g.loading = true;
            g.query.clear();
            g.viewing = false;
        }
        self.request("artifacts", json!({}), "artifacts");
        if !self.media.peek().touch {
            spawn_forever(async move {
                host::next_frame().await;
                host::focus("art-find");
            });
        }
        self.push_layer(Layer::Gallery);
    }

    pub fn close_gallery(mut self) {
        if !self.gallery.peek().open {
            return;
        }
        // Back from an artifact on a phone returns to the list first.
        if self.gallery.peek().viewing {
            self.drop_layer(Layer::GalleryPane);
        }
        {
            let mut g = self.gallery.write();
            g.open = false;
            g.viewing = false;
            g.at = None;
        }
        self.drop_layer(Layer::Gallery);
        self.focus_prompt();
    }

    pub fn close_gallery_pane(mut self) {
        if !self.gallery.peek().viewing {
            return;
        }
        self.gallery.write().viewing = false;
        self.drop_layer(Layer::GalleryPane);
    }

    fn apply_gallery(mut self, sessions: Vec<SavedSession>) {
        {
            let mut g = self.gallery.write();
            g.sessions = sessions;
            g.loading = false;
        }
        self.reselect_gallery();
    }

    /// After the list or its filter changed: the artifact shown, if it's still listed, else
    /// the first (except on a phone, which shows the list first).
    pub fn reselect_gallery(mut self) {
        let rows = gallery_rows(&self.gallery.peek());
        let items: Vec<&GalleryRow> = rows
            .iter()
            .filter(|r| matches!(r, GalleryRow::Item { .. }))
            .collect();
        let at = self.gallery.peek().at.clone();
        let still = at.as_ref().is_some_and(|at| {
            items
                .iter()
                .any(|r| matches!(r, GalleryRow::Item { url, .. } if url == at))
        });
        if !still {
            let first = items.first().and_then(|r| match r {
                GalleryRow::Item { url, .. } => Some(url.clone()),
                _ => None,
            });
            self.gallery.write().at = if self.media.peek().narrow {
                None
            } else {
                first
            };
        }
    }

    pub fn select_gallery(mut self, url: String, tapped: bool) {
        self.gallery.write().at = Some(url.clone());
        host::scroll_into_view(&format!("art-{}", html::hash(&url)), false);
        if tapped && self.media.peek().narrow && !self.gallery.peek().viewing {
            self.gallery.write().viewing = true;
            self.push_layer(Layer::GalleryPane);
        }
    }

    pub fn step_gallery(mut self, by: isize) {
        let rows = gallery_rows(&self.gallery.peek());
        let urls: Vec<String> = rows
            .into_iter()
            .filter_map(|r| match r {
                GalleryRow::Item { url, .. } => Some(url),
                _ => None,
            })
            .collect();
        if urls.is_empty() {
            return;
        }
        let at = self.gallery.peek().at.clone();
        let now = at
            .and_then(|at| urls.iter().position(|u| *u == at))
            .map_or(-1, |i| i as isize);
        let next = (now + by).clamp(0, urls.len() as isize - 1) as usize;
        self.select_gallery(urls[next].clone(), false);
    }

    // ------------------------------------------------------------ the model picker

    pub fn open_models(mut self) {
        if self.picker.peek().open {
            return;
        }
        self.picker.set(Picker {
            open: true,
            ..Picker::default()
        });
        self.request("models", json!({}), "models");
        // After whatever opened it is done: sending `/model` from the prompt clears and
        // focuses it.
        if !self.media.peek().touch {
            spawn_forever(async move {
                host::next_frame().await;
                if self.picker.peek().open {
                    host::focus("model-find");
                }
            });
        }
        self.push_layer(Layer::Models);
    }

    pub fn close_models(mut self) {
        if !self.picker.peek().open {
            return;
        }
        self.picker.write().open = false;
        self.drop_layer(Layer::Models);
        self.focus_prompt();
    }

    pub fn choose_model(mut self, i: usize) {
        let (rows, current) = {
            let p = self.picker.peek();
            (picker_rows(&p), p.current.clone())
        };
        let Some(model) = rows.get(i) else { return };
        self.close_models();
        if model.id != current {
            self.send(
                json!({ "t": "send", "text": format!("/model {}", model.id) }),
                false,
            );
        }
    }

    pub fn step_model(mut self, by: isize) {
        let count = picker_rows(&self.picker.peek()).len();
        if count == 0 {
            return;
        }
        let at = self.picker.peek().at as isize;
        self.picker.write().at = (at + by).clamp(0, count as isize - 1) as usize;
        self.scroll_model_into_view();
    }

    fn scroll_model_into_view(mut self) {
        spawn_forever(async move {
            host::next_frame().await;
            let at = self.picker.peek().at;
            host::scroll_into_view(&format!("model-{at}"), false);
        });
    }

    // ------------------------------------------------------------ closing the session

    pub fn open_end_dialog(mut self) {
        if self.ending.peek().sent {
            return;
        }
        self.ending.write().open = true;
        spawn_forever(async move {
            host::next_frame().await;
            host::focus("end-go");
        });
    }

    /// Sends the close, now if connected, or as soon as it is.
    pub fn end_session(mut self) {
        if self.ending.peek().sent {
            return;
        }
        if !self.is_ready() {
            self.ending.write().due = true;
            return;
        }
        {
            let mut e = self.ending.write();
            e.due = false;
            e.sent = true;
        }
        self.send(json!({ "t": "close" }), false);
        self.set_conn("down", "Closing…");
        let chef = self.is_chef();
        self.toast(
            if chef {
                "Stopping the chef…"
            } else {
                "Closing the session…"
            },
            1_000_000_000,
        );
    }
}

/// What the video button and `/video` send: the backend knows how from the artifact guide.
pub const VIDEO_PROMPT: &str = "Can you make an animated video of this with Manim?";
/// The iOS app's artifact button sends the same words.
pub const ARTIFACT_PROMPT: &str = "Can you generate an artifact for this?";

/// Commands that open a picker or screen in the terminal when given no arguments.
fn terminal_only(name: &str) -> Option<&'static str> {
    Some(match name {
        "/effort" => "Set it with `/effort <level>`: low, medium, high, xhigh, max or default.",
        "/resume" | "/continue" => {
            "Resume with `/resume <n|id>` here, or `/resume` in the terminal for the list."
        }
        "/backend" => {
            "Use `/backend api|claude-code|codex` here, or `/backend` in the terminal for the list."
        }
        "/rewind" => "`/rewind` picks a turn in the terminal; `/undo` works here.",
        "/login" => "Sign-in runs in the terminal tamale is running in.",
        "/paste" => "Paste with ⌘V here.",
        "/mouse" => "`/mouse` only affects the terminal.",
        "/browser" => "`/browser` mirrors the terminal; you are already in the web UI.",
        _ => return None,
    })
}

/// The gallery's rows for its filter: each session's matching artifacts under its name.
pub fn gallery_rows(g: &Gallery) -> Vec<GalleryRow> {
    let query = g.query.trim().to_lowercase();
    let mut rows = Vec::new();
    for session in &g.sessions {
        let items: Vec<_> = session
            .items
            .iter()
            .filter(|a| query.is_empty() || a.title.to_lowercase().contains(&query))
            .collect();
        if items.is_empty() {
            continue;
        }
        let name = if session.current {
            "This session".to_string()
        } else if session.title.is_empty() {
            "Untitled session".to_string()
        } else {
            session.title.clone()
        };
        rows.push(GalleryRow::Group {
            name,
            when: if session.current {
                String::new()
            } else {
                ago(session.updated)
            },
        });
        for item in items {
            rows.push(GalleryRow::Item {
                url: item.url.clone(),
                title: item.title.clone(),
                file: item.file.clone(),
                modified: item.modified,
                current: session.current,
            });
        }
    }
    rows
}

/// The picker's rows for its filter: each word somewhere in a model's id or name, and an id
/// that isn't listed, which `/model <id>` would still take.
pub fn picker_rows(p: &Picker) -> Vec<PickerRow> {
    let query = p.query.trim();
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut rows: Vec<PickerRow> = p
        .models
        .iter()
        .filter(|m| {
            let hay = format!("{} {}", m.id, m.name).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .map(|m| PickerRow {
            id: m.id.clone(),
            name: m.name.clone(),
            typed: false,
        })
        .collect();
    if !query.is_empty()
        && !query.contains(char::is_whitespace)
        && rows.is_empty()
        && !p.models.is_empty()
    {
        rows.push(PickerRow {
            id: query.to_string(),
            name: format!("Use “{query}”"),
            typed: true,
        });
    }
    rows
}

/// How long ago `secs` (since the epoch) was, in a word or two.
pub fn ago(secs: f64) -> String {
    let d = (host::now_ms() / 1000.0 - secs).max(0.0);
    if d < 60.0 {
        "just now".into()
    } else if d < 3600.0 {
        format!("{}m ago", (d / 60.0) as u64)
    } else if d < 86400.0 {
        format!("{}h ago", (d / 3600.0) as u64)
    } else if d < 86400.0 * 30.0 {
        format!("{}d ago", (d / 86400.0) as u64)
    } else {
        host::date(secs)
    }
}

pub fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.*}k", if n < 10_000 { 1 } else { 0 }, n as f64 / 1000.0)
    } else {
        format!("{:.1}M", n as f64 / 1e6)
    }
}

pub fn fmt_elapsed(s: u64) -> String {
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        format!("{m}m {}s", s % 60)
    } else {
        format!("{}h {}m", m / 60, m % 60)
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
