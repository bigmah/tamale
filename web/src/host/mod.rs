//! What the app needs of wherever it runs: the address it talks to, sockets, storage, the
//! clock, and the few things only the document can do (focus, scrolling, frames, the
//! clipboard), from the browser through web-sys (`web`). The rest of the app sees only this
//! module.

use std::rc::Rc;

use dioxus::prelude::*;
use serde_json::Value;

mod web;

pub use web::*;

/// Where the session is and how to reach it.
#[derive(Clone, Debug, Default)]
pub struct Place {
    /// What links to the session's server start with: empty, as the page is served from
    /// there.
    pub base: String,
    /// Where the code view (`/pullspace/`) and the empty artifact frame (`/artifact-frame`) are
    /// served from: the page's own origin, which both need to talk to it.
    pub frames: String,
    /// Artifacts are posted into `/artifact-frame` rather than loaded by URL: on an encrypted
    /// session, where they can't come through a hub by URL.
    pub post_artifacts: bool,
    /// The session's socket.
    pub ws_url: String,
    /// The session's id, the first label of its host name, for the encryption's prologue.
    pub sid: String,
    /// The hub's own page, `https://hub`, when the session is reached through one.
    pub hub: Option<String>,
    /// Whether it's end-to-end encrypted, and the key if this page has it.
    pub e2e: Option<Option<Vec<u8>>>,
}

/// What a socket says.
#[derive(Debug)]
pub enum SocketEvent {
    Open,
    Text(String),
    Closed { code: u16, reason: String },
}

/// A key pressed anywhere in the page (or in the code view's frame, for ⌘B).
#[derive(Clone, Debug, Default)]
pub struct Key {
    pub key: String,
    /// Which key it is on the keyboard (`Digit1`), whatever it types.
    #[allow(dead_code)]
    pub code: String,
    pub meta: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub composing: bool,
    /// Whether it went to an `input` or a `textarea`.
    pub in_field: bool,
    /// Whether it went to the prompt, and the prompt has something typed.
    pub in_prompt: bool,
    pub prompt_empty: bool,
    pub default_prevented: bool,
}

/// What the document tells the app, besides its own elements' events.
#[derive(Debug)]
pub enum HostEvent {
    /// The browser's Back went over one of the app's history entries.
    Back,
    /// The page came back into view, or online: check the connection.
    Wake,
    /// The window got the focus.
    Focus,
    /// A media query the layout depends on changed.
    Media,
    /// The visible height changed (the on-screen keyboard came or went).
    Viewport,
    /// An artifact's frame says how tall its document is.
    ArtifactHeight { frame: String, height: f64 },
    /// A frame or tab opened on `/artifact-frame` is ready for its document.
    FrameReady { target: FrameTarget },
    /// A message from the code view's frame.
    Review(Value),
    /// ⌘B in the code view's frame.
    ToggleCode,
    /// The hub's page asks to close this session (it asked the user first).
    HubClose,
    /// A file path in a reply was clicked: open it in the code view.
    OpenPath { path: String, diff: bool },
}

/// Where an artifact's document goes once its frame is ready.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameTarget {
    /// An `iframe` with this `data-frame`.
    Frame(String),
    /// A tab opened with [`open_frame_tab`].
    Tab(u32),
}

/// The handler for keys the app answers anywhere. It runs while the key is being
/// dispatched, so its `true` (handled) can still keep the browser from acting on it.
pub type KeyHandler = Rc<dyn Fn(&Key) -> bool>;

/// Formats a number of seconds since the epoch as a local date.
pub fn date(secs: f64) -> String {
    local_date(secs)
}

/// Runs `f` with Dioxus' runtime current, for callbacks that come from outside it.
pub fn in_runtime(runtime: &Rc<dioxus::core::Runtime>, f: impl FnOnce()) {
    let _guard = dioxus::core::RuntimeGuard::new(runtime.clone());
    runtime.in_scope(ScopeId::ROOT, f);
}
