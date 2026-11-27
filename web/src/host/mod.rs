//! What the app needs of wherever it runs: the address it talks to, sockets, storage, the
//! clock, and the few things only the document can do (focus, scrolling, the clipboard), from the browser through web-sys (`web`). The rest of the app sees only this
//! module.

use std::rc::Rc;

use dioxus::prelude::*;

mod web;

pub use web::*;

/// Where the session is and how to reach it.
#[derive(Clone, Debug, Default)]
pub struct Place {
    /// What links to the session's server start with: empty, as the page is served from
    /// there.
    pub base: String,
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

/// A key pressed anywhere in the page.
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
    /// Whether it was pressed in the code view (pullspace): focus in it, or on nothing after
    /// the pointer last went down there. Those keys are pullspace's, ⌘B aside.
    pub in_code: bool,
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
    /// The hub's page asks to close this session (it asked the user first).
    HubClose,
    /// A file path in a reply was clicked: open it in the code view.
    OpenPath { path: String, diff: bool },
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
