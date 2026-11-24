//! Around the chat: the seams between panes, a chef's kitchen, and the dialogs (the
//! artifacts gallery, the model picker, closing the session, help).

use dioxus::prelude::*;
use serde_json::json;

use crate::app::{App, GalleryRow, gallery_rows, picker_rows};
use crate::host;
use crate::html;
use crate::icons::{I, icon, toque};
use crate::wire::KitchenSession;

pub use crate::app::Seam as Which;

impl Which {
    fn key(self) -> &'static str {
        match self {
            Which::Chat => "chatShare",
            Which::Kitchen => "kitchenWidth",
            Which::Art => "artListWidth",
        }
    }

    fn signal(self, app: App) -> Signal<Option<f64>> {
        match self {
            Which::Chat => app.chat_share,
            Which::Kitchen => app.kitchen_width,
            Which::Art => app.art_width,
        }
    }

    /// Keeps a size sensible for the room there is.
    fn clamp(self, value: f64) -> f64 {
        match self {
            Which::Chat => {
                let width = host::rect("main").map_or(1.0, |r| r.2.max(1.0));
                // Room for a conversation on one side and a file on the other.
                let least = 240.0 / width;
                let most = least.max(1.0 - 280.0 / width);
                value.clamp(least, most)
            }
            Which::Kitchen => {
                let width = host::rect("main").map_or(f64::INFINITY, |r| {
                    if r.2 > 0.0 { r.2 } else { f64::INFINITY }
                });
                value.min(width - 300.0).max(220.0)
            }
            Which::Art => {
                let width = host::rect("art-box").map_or(f64::INFINITY, |r| {
                    if r.2 > 0.0 { r.2 } else { f64::INFINITY }
                });
                value.min(width - 320.0).max(160.0)
            }
        }
    }

    /// The size the pointer at `x` makes.
    fn at(self, x: f64) -> Option<f64> {
        match self {
            Which::Chat => {
                host::rect("main").map(|(left, _, width, _)| (x - left) / width.max(1.0))
            }
            Which::Kitchen => host::rect("main").map(|(left, _, width, _)| left + width - x),
            Which::Art => host::rect("art-list").map(|(left, ..)| x - left),
        }
    }

    /// The pane's size now, for the arrow keys.
    fn now(self) -> Option<f64> {
        match self {
            Which::Chat => Some(host::rect("scroller")?.2 / host::rect("main")?.2.max(1.0)),
            Which::Kitchen => host::rect("kitchen").map(|r| r.2),
            Which::Art => host::rect("art-list").map(|r| r.2),
        }
    }

    fn step(self) -> f64 {
        if self == Which::Chat { 0.02 } else { 24.0 }
    }

    fn set(self, app: App, value: Option<f64>, keep: bool) {
        let value = value.map(|v| self.clamp(v));
        let mut signal = self.signal(app);
        signal.set(value);
        if keep {
            host::store_set(self.key(), &json!(value));
        }
    }
}

/// A line between panes, which drags to resize them, as in pullspace. A double-click goes
/// back to the stylesheet's size; the arrow keys move it.
#[component]
pub fn Seam(which: Which, hidden: bool) -> Element {
    let mut app = use_context::<App>();
    let on = *app.dragging.read() == Some(which);
    let (id, class, label) = match which {
        Which::Chat => ("divider", "divider", "Resize the chat and the code"),
        Which::Kitchen => ("k-divider", "divider k-divider", "Resize the kitchen"),
        Which::Art => ("art-divider", "divider art-divider", "Resize the list"),
    };
    rsx! {
        div {
            class: if on { "{class} on" } else { "{class}" },
            id: "{id}",
            role: "separator",
            "aria-orientation": "vertical",
            "aria-label": "{label}",
            tabindex: "0",
            title: "Drag to resize · double-click to reset",
            hidden,
            onpointerdown: move |ev| {
                if ev.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) {
                    return;
                }
                ev.prevent_default();
                app.dragging.set(Some(which));
            },
            ondoubleclick: move |_| which.set(app, None, true),
            onkeydown: move |ev| {
                let by = match ev.key() {
                    Key::ArrowLeft => -which.step(),
                    Key::ArrowRight => which.step(),
                    _ => return,
                };
                ev.prevent_default();
                let now = which.signal(app).peek().or_else(|| which.now()).unwrap_or(0.0);
                which.set(app, Some(now + by), true);
            },
        }
    }
}

/// Over everything while a seam is dragged: a frame would take the pointer as soon as it
/// crossed into it.
#[component]
pub fn DragMask() -> Element {
    let mut app = use_context::<App>();
    let Some(which) = *app.dragging.read() else {
        return rsx! {};
    };
    let finish = move |_| {
        let value = *which.signal(app).peek();
        host::store_set(which.key(), &json!(value));
        app.dragging.set(None);
    };
    rsx! {
        div {
            class: "dragmask",
            onpointermove: move |ev| {
                if let Some(v) = which.at(ev.client_coordinates().x) {
                    which.set(app, Some(v), false);
                }
            },
            onpointerup: finish,
            onpointercancel: finish,
        }
    }
}

// ---------------------------------------------------------------- the kitchen

const STATES: &[(&str, &str, u8)] = &[
    ("approval", "needs you", 0),
    ("working", "working", 1),
    ("idle", "idle", 2),
    ("offline", "offline", 3),
];

fn rank(state: &str) -> u8 {
    STATES.iter().find(|s| s.0 == state).map_or(9, |s| s.2)
}

/// A chef's sessions (see tamale's `src/chef.rs`), kept current by `kitchen` messages.
#[component]
pub fn KitchenPanel() -> Element {
    let mut app = use_context::<App>();
    let k = app.kitchen.read().clone().unwrap_or_default();
    let changed = app.kitchen_changed.read().clone();
    let mut sessions = k.sessions.clone();
    sessions.sort_by_key(|s| rank(&s.state));
    let count = |st: &str| sessions.iter().filter(|s| s.state == st).count();
    let stats: Vec<(&str, usize, &str)> = [
        ("approval", count("approval"), "need you"),
        ("working", count("working"), "working"),
        ("idle", count("idle"), "idle"),
        ("offline", count("offline"), "offline"),
    ]
    .into_iter()
    .filter(|(_, n, _)| *n > 0)
    .collect();
    let no_stats = stats.is_empty();
    let live = if k.loading {
        "k-live loading"
    } else if k.problem.is_some() {
        "k-live stale"
    } else {
        "k-live"
    };
    let live_title = k
        .problem
        .clone()
        .unwrap_or_else(|| "Live: refreshes every few seconds".into());
    let narrow = app.media.read().narrow;
    rsx! {
        div { class: "k-hdr",
            span { class: "k-mark", "🫔" }
            span { class: "k-title", "Kitchen" }
            span { class: "{live}", id: "k-live", title: "{live_title}" }
            div { class: "spacer" }
            button {
                class: "iconbtn sm k-close",
                id: "k-close",
                title: "Close",
                onclick: move |_| if narrow { app.close_kitchen() } else { app.kitchen_open.set(false) },
                {icon(I::Close)}
            }
        }
        div { class: "k-stats", id: "k-stats", hidden: no_stats,
            for (st, n, label) in stats {
                span { class: "k-stat st-{st}", b { "{n}" } " {label}" }
            }
        }
        div { class: "k-list", id: "k-list",
            if sessions.is_empty() {
                div { class: "k-empty",
                    div { class: "k-empty-mark", {toque()} }
                    p { if k.loading { "Looking around…" } else { "{k.problem.clone().unwrap_or_else(|| \"No other sessions yet.\".into())}" } }
                    if !k.loading && k.problem.is_none() {
                        p { class: "k-empty-hint", "Turn on " kbd { "/remote" } " in a session (with the same hub key) to bring it into the kitchen." }
                    }
                }
            }
            for s in sessions {
                KitchenCard { key: "{s.id}", changed: changed.contains(&s.id), s }
            }
        }
        div { class: "k-foot",
            button {
                class: "k-askall",
                id: "k-askall",
                onclick: move |_| {
                    app.set_input("Ask all sessions: ");
                    if narrow { app.close_kitchen() }
                },
                {icon(I::Chat)}
                "Ask every session…"
            }
        }
    }
}

#[component]
fn KitchenCard(s: KitchenSession, changed: bool) -> Element {
    let app = use_context::<App>();
    let place = [s.host.as_str(), s.cwd.as_str()]
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(":");
    let place = if place.is_empty() {
        s.id.clone()
    } else {
        place
    };
    let label = STATES
        .iter()
        .find(|st| st.0 == s.state)
        .map_or(s.state.as_str(), |st| st.1)
        .to_string();
    let who = format!("{} ({place})", s.id);
    let id = s.id.clone();
    let narrow = app.media.read().narrow;
    let run = std::rc::Rc::new(move |which: &'static str| {
        match which {
            "needs" => app.ask(&format!("What is session {who} waiting for me to approve, and should I?")),
            "catchup" => app.ask(&format!("Catch me up on session {who}: what was it asked, what has it done, and where is it now?")),
            "ask" => app.set_input(format!("Ask {id}: ")),
            _ => app.ask(&format!("What is session {who} working on right now?")),
        }
        if narrow {
            app.close_kitchen();
        }
    });
    let act = {
        let run = run.clone();
        move |which: &'static str| {
            let run = run.clone();
            move |ev: Event<MouseData>| {
                ev.stop_propagation();
                run(which);
            }
        }
    };
    rsx! {
        div {
            class: format!("k-card st-{}{}", s.state, if changed { " changed" } else { "" }),
            tabindex: "0",
            title: "{place}\nClick to ask what it's doing",
            onclick: act("open"),
            onkeydown: move |ev: Event<KeyboardData>| if ev.key() == Key::Enter { run("open") },
            div { class: "k-top",
                span { class: "k-dot" }
                span { class: "k-host", if s.host.is_empty() { "?" } else { "{s.host}" } }
                span { class: "k-id", "{s.id}" }
            }
            div { class: "k-cwd", "{s.cwd}" }
            div { class: "k-what", if s.title.is_empty() { i { "Nothing asked yet" } } else { "{s.title}" } }
            div { class: "k-meta",
                span { class: "k-state", "{label}" }
                if !s.model.is_empty() { span { class: "k-model", "{s.model}" } }
                if s.viewers > 0 { span { "{s.viewers} viewing" } }
            }
            div { class: "k-actions",
                if s.state == "approval" { button { class: "primary", onclick: act("needs"), "What does it need?" } }
                button { onclick: act("catchup"), "Catch me up" }
                button { onclick: act("ask"), "Ask…" }
            }
        }
    }
}

// ---------------------------------------------------------------- the gallery

#[component]
pub fn GalleryDialog() -> Element {
    let mut app = use_context::<App>();
    let g = app.gallery.read().clone();
    if !g.open {
        return rsx! {};
    }
    let rows = gallery_rows(&g);
    use_effect(move || {
        let w = *app.art_width.read();
        host::set_style(
            "art-box",
            "--art-w",
            w.map(|px| format!("{}px", px.round())).as_deref(),
        );
    });
    let shown = g.at.as_ref().and_then(|at| {
        rows.iter().find_map(|r| match r {
            GalleryRow::Item {
                url,
                title,
                file,
                current,
                ..
            } if url == at => Some((url.clone(), title.clone(), file.clone(), *current)),
            _ => None,
        })
    });
    let query = g.query.trim().to_string();
    rsx! {
        div {
            class: "pickmask",
            id: "artifacts",
            onmousedown: move |_| app.close_gallery(),
            div { class: if g.viewing { "pickbox artbox viewing" } else { "pickbox artbox" }, id: "art-box", role: "dialog", "aria-label": "Artifacts",
                onmousedown: move |ev| ev.stop_propagation(),
                div { class: "art-hdr",
                    input {
                        class: "pickinput",
                        id: "art-find",
                        placeholder: "Filter artifacts…",
                        spellcheck: "false",
                        autocomplete: "off",
                        value: "{g.query}",
                        oninput: move |ev| {
                            app.gallery.write().query = ev.value();
                            app.reselect_gallery();
                        },
                        onkeydown: move |ev| match ev.key() {
                            Key::ArrowDown => { ev.prevent_default(); app.step_gallery(1) }
                            Key::ArrowUp => { ev.prevent_default(); app.step_gallery(-1) }
                            Key::Enter => {
                                ev.prevent_default();
                                if let Some(url) = app.gallery.peek().at.clone() {
                                    app.open_artifact_tab(&url);
                                }
                            }
                            _ => {}
                        },
                    }
                    button { class: "iconbtn sm", id: "art-close", title: "Close (esc)", onclick: move |_| app.close_gallery(), {icon(I::Close)} }
                }
                div { class: "art-body",
                    div { class: "art-list", id: "art-list",
                        if g.loading {
                            div { class: "pickempty", "Loading…" }
                        } else if !rows.iter().any(|r| matches!(r, GalleryRow::Item { .. })) {
                            div { class: "pickempty", if query.is_empty() { "No saved artifacts in this project yet." } else { "No artifacts match." } }
                        }
                        for row in rows {
                            match row {
                                GalleryRow::Group { name, when } => rsx! {
                                    div { class: "art-group", title: "{name}", span { "{name}" } span { "{when}" } }
                                },
                                GalleryRow::Item { url, title, modified, .. } => {
                                    let on = g.at.as_deref() == Some(url.as_str());
                                    let u = url.clone();
                                    rsx! {
                                        button {
                                            class: if on { "art-row on" } else { "art-row" },
                                            id: "art-{html::hash(&url)}",
                                            title: "{title}",
                                            onclick: move |_| app.select_gallery(u.clone(), true),
                                            span { {icon(I::Spark)} }
                                            span { class: "t", "{title}" }
                                            span { class: "ago", "{crate::app::ago(modified)}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Seam { which: Which::Art, hidden: false }
                    div { class: "art-view", id: "art-view",
                        match shown {
                            None => rsx! { div { class: "art-empty", "Artifacts from /web replies in this project show up here, including ones from earlier sessions." } },
                            Some((url, title, file, current)) => rsx! { GalleryView { key: "{url}", url, title, file, current } },
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn GalleryView(url: String, title: String, file: String, current: bool) -> Element {
    let app = use_context::<App>();
    let key = use_hook(|| format!("g{}", html::hash(&format!("{url}{}", host::now_ms()))));
    {
        let (key, url) = (key.clone(), url.clone());
        use_hook(move || app.frame_wants(&key, &url));
    }
    let in_chat = host::exists(&format!(".artifact[data-src=\"{url}\"]"));
    let posting = app.posting();
    let base = app.place.read().base.clone();
    let frame_src = app.frame_src(&url);
    let (u1, u2, u3, t1, t2, f1) = (
        url.clone(),
        url.clone(),
        url.clone(),
        title.clone(),
        title.clone(),
        file.clone(),
    );
    rsx! {
        div { class: "artifact-bar",
            button { class: "artifact-btn art-back", title: "Back to the list", onclick: move |_| app.close_gallery_pane(), {icon(I::Back)} }
            span { class: "artifact-icon", {icon(I::Spark)} }
            span { class: "artifact-title", title: "{title}", "{title}" }
            span { class: "art-path", title: "{file}", "{file}" }
            span { class: "spacer" }
            if in_chat {
                button {
                    class: "artifact-btn",
                    title: "Show in the chat",
                    onclick: move |_| {
                        app.close_gallery();
                        host::scroll_selector_into_view(&format!(".artifact[data-src=\"{u1}\"]"));
                    },
                    {icon(I::Chat)}
                }
            }
            button {
                class: "artifact-btn",
                title: "Revise this artifact",
                onclick: move |_| {
                    app.close_gallery();
                    app.set_input(if current {
                        format!("Revise the \"{t1}\" artifact: ")
                    } else {
                        format!("Revise the \"{t1}\" artifact from an earlier session (saved at {f1}; read it first): ")
                    });
                },
                {icon(I::Edit)}
            }
            a {
                class: "artifact-btn wide-only",
                title: "Download as HTML",
                href: "{base}{url}",
                download: "{html::slug(&title)}.html",
                onclick: move |ev: Event<MouseData>| if posting { ev.prevent_default(); app.save_artifact(u2.clone(), t2.clone()); },
                {icon(I::Download)}
            }
            a {
                class: "artifact-btn",
                title: "Open in a new tab",
                href: "{base}{url}",
                target: "_blank",
                rel: "noopener noreferrer",
                onclick: move |ev: Event<MouseData>| if posting { ev.prevent_default(); app.open_artifact_tab(&u3); },
                {icon(I::Open)}
            }
        }
        iframe {
            title: "{title}",
            "data-frame": "{key}",
            "sandbox": "allow-scripts allow-popups allow-modals allow-forms",
            referrerpolicy: "no-referrer",
            src: "{frame_src}",
        }
    }
}

// ---------------------------------------------------------------- the model picker

#[component]
pub fn ModelsDialog() -> Element {
    let mut app = use_context::<App>();
    let p = app.picker.read().clone();
    if !p.open {
        return rsx! {};
    }
    let rows = picker_rows(&p);
    let words = p.query.split_whitespace().count();
    let listed = rows.iter().filter(|r| !r.typed).count();
    let total = p.models.len();
    let at = p.at.min(rows.len().saturating_sub(1));
    rsx! {
        div {
            class: "pickmask",
            id: "models",
            onmousedown: move |_| app.close_models(),
            div { class: "pickbox modelbox", role: "dialog", "aria-label": "Models",
                onmousedown: move |ev| ev.stop_propagation(),
                div { class: "art-hdr",
                    input {
                        class: "pickinput",
                        id: "model-find",
                        placeholder: "Filter models…",
                        spellcheck: "false",
                        autocomplete: "off",
                        autocapitalize: "off",
                        role: "combobox",
                        "aria-controls": "model-list",
                        "aria-expanded": "true",
                        "aria-activedescendant": "model-{at}",
                        value: "{p.query}",
                        oninput: move |ev| {
                            let mut picker = app.picker.write();
                            picker.query = ev.value();
                            picker.at = 0;
                        },
                        onkeydown: move |ev| match ev.key() {
                            Key::ArrowDown => { ev.prevent_default(); app.step_model(1) }
                            Key::ArrowUp => { ev.prevent_default(); app.step_model(-1) }
                            Key::PageDown => { ev.prevent_default(); app.step_model(8) }
                            Key::PageUp => { ev.prevent_default(); app.step_model(-8) }
                            Key::Enter => {
                                ev.prevent_default();
                                let at = app.picker.peek().at;
                                app.choose_model(at);
                            }
                            _ => {}
                        },
                    }
                    button { class: "iconbtn sm", id: "model-close", title: "Close (esc)", onclick: move |_| app.close_models(), {icon(I::Close)} }
                }
                if let Some(error) = &p.error {
                    div { class: "model-note", id: "model-note",
                        "Couldn't fetch the provider's list ({error}); these are the built-in ones. /model <id> takes any id."
                    }
                }
                div { class: "model-list", id: "model-list", role: "listbox", "aria-label": "Models",
                    if !p.loaded {
                        div { class: "pickempty", "Loading models…" }
                    } else if rows.is_empty() {
                        div { class: "pickempty", if total > 0 { "No models match." } else { "No models." } }
                    } else {
                        if total > 1 {
                            div { class: "model-count", if words > 0 { "{listed} of {total} models" } else { "{total} models" } }
                        }
                        for (i, m) in rows.into_iter().enumerate() {
                            button {
                                class: if i == at { "model-row on" } else { "model-row" },
                                role: "option",
                                id: "model-{i}",
                                "aria-selected": if i == at { "true" } else { "false" },
                                title: "{m.id}",
                                onclick: move |_| app.choose_model(i),
                                span { class: "tick", if m.id == p.current { {icon(I::Check)} } }
                                span { class: "names",
                                    span { class: "n", if m.name.is_empty() { "{m.id}" } else { "{m.name}" } }
                                    if !m.name.is_empty() && m.name != m.id && !m.typed { span { class: "id", "{m.id}" } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- closing, and help

#[component]
pub fn EndDialog() -> Element {
    let mut app = use_context::<App>();
    if !app.ending.read().open {
        return rsx! {};
    }
    let s = app.status.read().clone().unwrap_or_default();
    let chef = s.chef;
    let busy = if s.running {
        if chef {
            "It's asking around, and stops mid-turn. "
        } else {
            "It's working, and stops mid-turn. "
        }
    } else {
        ""
    };
    let go = if chef {
        "Stop chef"
    } else if busy.is_empty() {
        "Close session"
    } else {
        "Stop and close"
    };
    let mut close = move || app.ending.write().open = false;
    rsx! {
        div {
            class: "pickmask",
            id: "ending",
            onmousedown: move |_| close(),
            onkeydown: move |ev| if ev.key() == Key::Escape { ev.prevent_default(); ev.stop_propagation(); close() },
            div { class: "pickbox endbox", role: "alertdialog", "aria-modal": "true", "aria-labelledby": "end-title", "aria-describedby": "end-text",
                onmousedown: move |ev| ev.stop_propagation(),
                h2 { id: "end-title", if chef { "Stop the tamale chef?" } else { "Close this session?" } }
                p { id: "end-text",
                    "{busy}"
                    if chef {
                        "It stays saved, and your other sessions keep running. " code { "tamale chef" } " or " code { "/chef" } " starts it again."
                    } else {
                        "tamale ends it on its machine, where it stays saved: " code { "tamale --continue" } " picks it back up."
                    }
                }
                div { class: "end-actions",
                    button { class: "btn", id: "end-cancel", onclick: move |_| close(), "Cancel" }
                    button { class: "btn end-go", id: "end-go", onclick: move |_| { close(); app.end_session(); }, "{go}" }
                }
            }
        }
    }
}

const KEYS: &[(&str, &str)] = &[
    (
        "⏎ / ⇧⏎",
        "Send / new line (while the agent runs, ⏎ queues for its next step)",
    ),
    ("esc", "Interrupt the agent; close a menu"),
    ("↑ / ↓", "Earlier prompts, from the first or last line"),
    ("⌘B / Ctrl+B", "Show or hide the code"),
    (
        "⌘⏎ / Ctrl+⏎",
        "In the code: ask about the selection, the picked line or the file",
    ),
    ("y / a / n", "Answer a tool approval (when not typing)"),
    (
        "1–9 / ⏎ / ← →",
        "Pick an answer to the agent's questions, move on, switch questions (when not typing)",
    ),
    ("/", "Start a command from anywhere"),
];

#[component]
pub fn HelpDialog() -> Element {
    let mut app = use_context::<App>();
    if !*app.help.read() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "pickmask",
            id: "help",
            onmousedown: move |_| app.help.set(false),
            div { class: "pickbox helpbox", id: "help-body",
                onmousedown: move |ev| ev.stop_propagation(),
                h2 { "tamale on the web" }
                p { "The same session as the terminal, chat first. The switch in the top bar shows the code beside the chat or instead of it: pullspace, on your uncommitted changes and the whole repository. Steps that touch a file, and file paths in replies, open there." }
                h3 { "Commands" }
                for c in super::dock::COMMANDS {
                    div { class: "helprow",
                        code { if c.args.is_empty() { "{c.name}" } else { "{c.name} {c.args}" } }
                        span { "{c.desc}" }
                    }
                }
                h3 { "Keys" }
                for (k, v) in KEYS {
                    div { class: "helprow", code { "{k}" } span { "{v}" } }
                }
            }
        }
    }
}
