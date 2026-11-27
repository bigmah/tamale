//! The page: the top bar, the chat and the code side by side (or one of them), and the
//! overlays.

mod dock;
mod panels;
mod thread;
mod voice;

use std::rc::Rc;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use futures_util::StreamExt;
use serde_json::json;

use crate::app::{App, Media, View, fmt_tokens};
use crate::host::{self, HostEvent, Key};
use crate::icons::{I, icon};

/// The app, as the platform launches it.
#[component]
pub fn Root() -> Element {
    let app = use_hook(|| {
        let app = dioxus::core::Runtime::current().in_scope(ScopeId::ROOT, App::new);
        provide_context(app);
        app
    });
    use_hook(move || start(app));
    rsx! { Page {} }
}

/// Everything that runs for the page's whole life: the connection, the window's events, and
/// the clocks.
fn start(mut app: App) {
    let runtime = dioxus::core::Runtime::current();
    let (tx, mut rx) = futures_channel::mpsc::unbounded::<HostEvent>();
    *app.host_events.write() = Some(tx.clone());
    let keys: host::KeyHandler = {
        let runtime = runtime.clone();
        Rc::new(move |key: &Key| {
            let mut handled = false;
            host::in_runtime(&runtime, || handled = on_key(app, key));
            handled
        })
    };
    host::install(keys, tx, crate::app::PROMPT);
    host::fit_viewport();
    // Side by side is a way of working, so it's how the page comes back; the code alone is
    // somewhere you went, and the chat is where a reload starts.
    let resume = host::store_get("view").and_then(|v| v.as_str().map(str::to_string))
        == Some("split".into());
    let view = if resume && !app.media.peek().tight {
        View::Split
    } else {
        View::Chat
    };
    app.set_view(view, false);
    app.start();
    app.watch();
    spawn_forever(async move {
        while let Some(event) = rx.next().await {
            match event {
                HostEvent::Back => app.back(),
                HostEvent::Wake => app.check_connection(),
                HostEvent::Focus => app.focused(),
                HostEvent::Media => {
                    let now = Media::now();
                    let before = *app.media.peek();
                    if now != before {
                        app.media.set(now);
                        if now.tight && *app.view.peek() == View::Split {
                            app.set_view(View::Code, false);
                        }
                        if app.is_chef() && now.narrow != before.narrow {
                            app.kitchen_drawer.set(false);
                            app.kitchen_open.set(!now.narrow);
                        }
                    }
                }
                HostEvent::Viewport => {
                    let stick = app.near_bottom();
                    if host::fit_viewport() {
                        app.stick(stick);
                    }
                }
                HostEvent::HubClose => app.end_session(),
                HostEvent::OpenPath { path, diff } => {
                    if !app.is_chef() {
                        let (p, line) = crate::html::file_path(&path).unwrap_or((path, None));
                        app.open_in_code(&p, line, diff);
                    }
                }
            }
        }
    });
    // The elapsed times on the activity line.
    spawn_forever(async move {
        loop {
            host::sleep(1000).await;
            if app.running() {
                let t = *app.tick.peek();
                app.tick.set(t + 1);
            }
        }
    });
}

/// Keys the page answers wherever they're pressed. True if it took the key.
fn on_key(app: App, key: &Key) -> bool {
    // Already handled by the field it was typed in (the slash menu, the reason box…).
    if key.default_prevented || key.composing {
        return false;
    }
    let mod_ = key.meta || key.ctrl;
    if mod_ && key.key.eq_ignore_ascii_case("b") {
        app.toggle_review();
        return true;
    }
    // Pressed in the code view: pullspace's, as if it were a page of its own.
    if key.in_code {
        return false;
    }
    let overlay = app.ending.peek().open
        || *app.help.peek()
        || app.gallery.peek().open
        || app.picker.peek().open;
    if key.key == "Escape" {
        let (mut ending, mut help) = (app.ending, app.help);
        if app.full.peek().is_some() {
            app.close_full();
        } else if app.ending.peek().open {
            ending.write().open = false;
        } else if *app.help.peek() {
            help.set(false);
        } else if app.gallery.peek().open {
            app.close_gallery();
        } else if app.picker.peek().open {
            app.close_models();
        } else if app.running() {
            app.send(json!({ "t": "interrupt" }), false);
            return true;
        }
        return false;
    }
    if approval_key(app, key) || question_key(app, key) {
        return true;
    }
    // Typing anywhere that isn't a field goes to the prompt.
    if key.in_field || mod_ || key.alt || key.key.chars().count() != 1 || overlay {
        return false;
    }
    host::focus(crate::app::PROMPT);
    false
}

/// y / a / n answer the approval, unless they are being typed into something.
pub fn approval_key(app: App, key: &Key) -> bool {
    let Some(approval) = app.approval.peek().clone() else {
        return false;
    };
    if key.meta || key.ctrl || key.alt || host::now_ms() - app.mem.read().approval_at < 300.0 {
        return false;
    }
    if (key.in_field && !key.in_prompt) || (key.in_prompt && !key.prompt_empty) {
        return false;
    }
    match key.key.to_lowercase().as_str() {
        "y" => app.approve("yes", None),
        "a" if !approval.always.is_empty() => app.approve("always", None),
        "n" => dock::deny(app),
        _ => return false,
    }
    true
}

/// 1–9 pick, ⏎ moves on, ←/→ go between questions, unless they're being typed into something.
pub fn question_key(app: App, key: &Key) -> bool {
    let Some(q) = app.question.peek().clone() else {
        return false;
    };
    if key.meta || key.ctrl || key.alt || host::now_ms() - app.mem.read().question_at < 300.0 {
        return false;
    }
    if (key.in_field && !key.in_prompt) || (key.in_prompt && !key.prompt_empty) {
        return false;
    }
    let many = q.questions.len() > 1;
    let at = *app.asking.peek() as isize;
    match key.key.as_str() {
        k if k.len() == 1 && ('1'..='9').contains(&k.chars().next().unwrap_or(' ')) => {
            app.choose_option(k.parse::<usize>().unwrap_or(1) - 1);
        }
        "Enter" if !key.shift => app.next_question(),
        "ArrowLeft" if many => app.go_question(at - 1),
        "ArrowRight" if many => app.go_question(at + 1),
        _ => return false,
    }
    true
}

#[component]
fn Page() -> Element {
    let app = use_context::<App>();
    let view = *app.view.read();
    let chef = *app.chef.read();
    let kitchen_open = *app.kitchen_open.read();
    let drawer = *app.kitchen_drawer.read();
    let main_class = match view {
        View::Chat => "main",
        View::Split => "main split",
        View::Code => "main code",
    };
    use_effect(move || {
        let share = *app.chat_share.read();
        host::set_style(
            "main",
            "--chat-w",
            share.map(|s| format!("{:.2}%", s * 100.0)).as_deref(),
        );
    });
    use_effect(move || {
        let w = *app.kitchen_width.read();
        host::set_style(
            "kitchen",
            "--k-w",
            w.map(|px| format!("{}px", px.round())).as_deref(),
        );
    });
    let review = app.review.read().clone();
    let toast = app.toast.read().clone();
    let lock = app.lock.read().clone();
    // Made by `load_review` before `loaded` is set, so it's there whenever it's drawn.
    let link = review
        .loaded
        .then(|| app.mem.read().review_link.clone())
        .flatten();
    rsx! {
        div { class: "app",
            Topbar {}
            main { class: "{main_class}", id: "main",
                section { class: "chat",
                    div {
                        class: "scroller",
                        id: "scroller",
                        onscroll: move |_| {
                            let mut jump = app.jump;
                            let near = host::near_bottom(crate::app::SCROLLER, 80.0);
                            if *jump.peek() == near {
                                jump.set(!near);
                            }
                        },
                        thread::Thread {}
                        thread::Activity {}
                    }
                    dock::Dock {}
                }
                panels::Seam { which: panels::Which::Chat, hidden: view != View::Split }
                section {
                    class: if review.shown { "review shown" } else { "review" },
                    id: "review",
                    "aria-label": "Code",
                    hidden: view == View::Chat,
                    div { class: "review-wait", id: "review-wait",
                        {icon(I::Code)}
                        span { class: "review-wait-bar" }
                        span { id: "review-wait-note", "{review.note}" }
                    }
                    if let Some(link) = link {
                        div { class: "review-pane",
                            pullspace::Embedded { link }
                        }
                    }
                }
                panels::Seam { which: panels::Which::Kitchen, hidden: false }
                aside {
                    class: if drawer { "kitchen drawer" } else { "kitchen" },
                    id: "kitchen",
                    hidden: !(chef && kitchen_open),
                    panels::KitchenPanel {}
                }
            }
            // Where the toast goes while the chat, and the dock it lives in, is out of sight.
            div { class: "toast float", id: "toast-float", hidden: toast.is_none(),
                dangerous_inner_html: crate::html::inline(toast.as_deref().unwrap_or("")),
            }
            panels::GalleryDialog {}
            panels::ModelsDialog {}
            panels::EndDialog {}
            panels::HelpDialog {}
            panels::DragMask {}
            if let Some(lock) = lock {
                div { class: "locked",
                    div { class: "locked-box",
                        h1 { "{lock.title}" }
                        p { "{lock.text}" }
                        if let Some(link) = lock.link {
                            p { a { href: "{link}", target: "_top", "Open it through the hub" } }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Topbar() -> Element {
    let app = use_context::<App>();
    let status = app.status.read().clone().unwrap_or_default();
    let chef = *app.chef.read();
    let view = *app.view.read();
    let changes = *app.changes.read();
    let conn = app.conn_view.read().clone();
    let kitchen_open = *app.kitchen_open.read();
    let voice_on = app.voice.talk.read().phase.active();
    let ending = *app.ending.read();
    let pct = if status.window > 0 {
        ((status.context * 100) / status.window).min(100)
    } else {
        0
    };
    let ctx_class = if pct >= 80 {
        "ctx full"
    } else if pct >= 60 {
        "ctx warn"
    } else {
        "ctx"
    };
    let cost = if status.cost > 0.0 {
        format!(
            "${:.*}{}",
            if status.cost < 1.0 { 3 } else { 2 },
            status.cost,
            if status.cost_partial { "+" } else { "" }
        )
    } else {
        String::new()
    };
    let model = match &status.effort {
        Some(e) => format!("{} · {e}", status.model),
        None => status.model.clone(),
    };
    let model_title = format!(
        "{} via {}{} · thinking {}\nClick to switch models",
        status.model,
        status.backend,
        if status.signed_in {
            ""
        } else {
            " (not signed in)"
        },
        status.thinking
    );
    let code_title = if changes.files > 0 {
        format!(
            "Code · {} file{} changed, +{} −{}  ⌘B",
            changes.files,
            if changes.files == 1 { "" } else { "s" },
            changes.added,
            changes.removed
        )
    } else {
        "Code  ⌘B".into()
    };
    let count = if changes.files > 99 {
        "99+".to_string()
    } else {
        changes.files.to_string()
    };
    let kitchen = app.kitchen.read().clone().unwrap_or_default();
    let needs = kitchen
        .sessions
        .iter()
        .filter(|s| s.state == "approval")
        .count();
    let kitchen_count = if needs > 0 {
        needs.to_string()
    } else if kitchen.sessions.is_empty() {
        String::new()
    } else {
        kitchen.sessions.len().to_string()
    };
    let switch = move |v: View| move |_| app.set_view(v, true);
    let desktop = host::desktop();
    const MODES: [&str; 4] = ["ask", "accept-edits", "auto", "read-only"];
    rsx! {
        header { class: "topbar",
            if desktop {
                // In the desktop app, the way back to its list of sessions.
                button { class: "brand", id: "to-sessions", title: "All sessions  ⌘O", onclick: move |_| host::desktop_home(),
                    span { class: "brand-mark", "🫔" }
                    span { class: "brand-name",
                        "tamale"
                        if chef { " " span { class: "brand-chef", "chef" } }
                    }
                }
            } else {
                div { class: "brand",
                    span { class: "brand-mark", "🫔" }
                    span { class: "brand-name",
                        "tamale"
                        if chef { " " span { class: "brand-chef", "chef" } }
                    }
                }
            }
            div { class: "wschip", id: "project", title: "{[status.cwd.clone()].into_iter().chain(status.dirs.iter().cloned()).collect::<Vec<_>>().join(\"\\n\")}",
                {icon(I::Folder)}
                span { id: "project-name", if status.project.is_empty() { "…" } else { "{status.project}" } }
                span { class: "wschip-dirs", id: "project-dirs", if !status.dirs.is_empty() { "+{status.dirs.len()}" } }
            }
            // What's on screen: the chat, the code (pullspace) or both.
            div { class: "viewsw", id: "viewsw", role: "group", "aria-label": "Layout",
                button { id: "view-chat", class: if view == View::Chat { "on" }, title: "Chat", "aria-label": "Chat", "aria-pressed": "{view == View::Chat}", onclick: switch(View::Chat), {icon(I::Chat)} }
                button { id: "view-split", class: if view == View::Split { "on" }, title: "Chat and code, side by side", "aria-label": "Chat and code, side by side", "aria-pressed": "{view == View::Split}", onclick: switch(View::Split), {icon(I::Split)} }
                button { id: "view-code", class: if view == View::Code { "on" }, title: "{code_title}", "aria-label": "Code", "aria-pressed": "{view == View::Code}", onclick: switch(View::Code),
                    {icon(I::Code)}
                    span { class: "viewsw-n", id: "changes-n", hidden: changes.files == 0, "{count}" }
                }
            }
            div { class: "spacer" }
            div { class: "meters",
                button {
                    class: "pill mode {status.mode}",
                    id: "mode",
                    title: "Permission mode (click to cycle)",
                    onclick: move |_| {
                        let mode = app.status.peek().as_ref().map(|s| s.mode.clone()).unwrap_or_default();
                        let next = MODES[(MODES.iter().position(|m| *m == mode).map_or(0, |i| i + 1)) % MODES.len()];
                        app.send(json!({ "t": "send", "text": format!("/permissions {next}") }), false);
                    },
                    "{status.mode}"
                }
                button { class: "pill model", id: "model", title: "{model_title}", onclick: move |_| app.open_models(), "{model}" }
                span {
                    class: "{ctx_class}",
                    id: "ctx",
                    title: "Context {pct}% full · ↑{fmt_tokens(status.input_tokens)} ↓{fmt_tokens(status.output_tokens)} this session",
                    span { class: "ctx-bar", span { class: "ctx-fill", id: "ctx-fill", width: "{pct}%" } }
                    span { id: "ctx-label", "{fmt_tokens(status.context)} / {fmt_tokens(status.window)}" }
                }
                span { class: "cost", id: "cost", "{cost}" }
            }
            // Talk to the session through Gemini.
            button {
                class: if voice_on { "iconbtn voicebtn on" } else { "iconbtn voicebtn" },
                id: "toggle-voice",
                title: "Talk to tamale with Gemini  /voice",
                "aria-label": "Talk to tamale with Gemini",
                onclick: move |_| {
                    if app.voice.active() { app.voice.close() } else { app.voice.start(app, None, host::Audio::new()) }
                },
                {icon(I::Wave)}
            }
            button {
                class: "iconbtn",
                id: "toggle-artifacts",
                title: "Artifacts from this and earlier sessions  /artifacts",
                onclick: move |_| if app.gallery.peek().open { app.close_gallery() } else { app.open_gallery() },
                {icon(I::Artifacts)}
            }
            button {
                class: format!("kitchen-toggle{}{}", if needs > 0 { " alert" } else { "" }, if kitchen_open { " on" } else { "" }),
                id: "toggle-kitchen",
                title: "Your sessions",
                hidden: !chef,
                onclick: move |_| app.toggle_kitchen(),
                span { class: "kt-mark", "🫔" }
                span { class: "kt-count", id: "kitchen-count", "{kitchen_count}" }
            }
            button {
                class: "iconbtn endbtn",
                id: "end-session",
                title: if chef { "Stop the chef  /quit" } else { "Close this session  /quit" },
                "aria-label": if chef { "Stop the chef" } else { "Close this session" },
                disabled: ending.sent,
                onclick: move |_| app.open_end_dialog(),
                {icon(I::Power)}
                span { class: "end-label", "Stop chef" }
            }
            span { class: "conn {conn.kind}", id: "conn", title: "{conn.title}" }
        }
    }
}
