//! Below the chat: the agent's approvals and questions, the voice card, and the prompt.

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use serde_json::json;

use crate::app::{ARTIFACT_PROMPT, App, PROMPT, VIDEO_PROMPT};
use crate::host;
use crate::icons::{I, icon, send_icons};

/// Commands, in the order the menu offers them; `local` ones run in the page.
pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub desc: &'static str,
    pub local: bool,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/review",
        args: "",
        desc: "Show the code and its uncommitted changes; /review <path> opens a file there",
        local: true,
    },
    Command {
        name: "/help",
        args: "",
        desc: "Commands and shortcuts",
        local: true,
    },
    Command {
        name: "/model",
        args: "[id]",
        desc: "Pick a model from the list, or switch to one: /model <id>",
        local: false,
    },
    Command {
        name: "/effort",
        args: "<level>",
        desc: "low, medium, high, xhigh, max or default",
        local: false,
    },
    Command {
        name: "/thinking",
        args: "[on|off]",
        desc: "Toggle adaptive thinking",
        local: false,
    },
    Command {
        name: "/permissions",
        args: "[mode]",
        desc: "Show the rules, or set read-only, ask, accept-edits, auto",
        local: false,
    },
    Command {
        name: "/compact",
        args: "[focus]",
        desc: "Summarize the conversation to free up context",
        local: false,
    },
    Command {
        name: "/goal",
        args: "[<condition>|pause|resume|clear]",
        desc: "Keep working until the condition holds",
        local: false,
    },
    Command {
        name: "/undo",
        args: "",
        desc: "Restore files the last turn changed and drop it",
        local: false,
    },
    Command {
        name: "/add-dir",
        args: "[path]",
        desc: "Add a directory to the agent's scope",
        local: false,
    },
    Command {
        name: "/resume",
        args: "<n|id>",
        desc: "Continue a saved session",
        local: false,
    },
    Command {
        name: "/copy",
        args: "[all]",
        desc: "Copy the latest reply to the clipboard",
        local: false,
    },
    Command {
        name: "/artifacts",
        args: "",
        desc: "Browse artifacts from this and earlier sessions",
        local: true,
    },
    Command {
        name: "/video",
        args: "[what]",
        desc: "Animate something with Manim and play it here; alone, what you were just discussing",
        local: true,
    },
    Command {
        name: "/voice",
        args: "",
        desc: "Talk to tamale by voice through Gemini (needs a Gemini API key)",
        local: true,
    },
    Command {
        name: "/clear",
        args: "",
        desc: "Clear the conversation and start fresh",
        local: false,
    },
    Command {
        name: "/web off",
        args: "",
        desc: "Stop serving this page",
        local: false,
    },
    Command {
        name: "/quit",
        args: "",
        desc: "Close this session (it stays saved); in the chef, stop it",
        local: true,
    },
];

/// The commands matching what is typed, while the first word is a command.
pub fn slash_items(value: &str) -> Vec<&'static Command> {
    let is_word = value.starts_with('/') && !value.contains(char::is_whitespace);
    if !is_word {
        return Vec::new();
    }
    let mut items: Vec<_> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(value))
        .collect();
    if items.is_empty() {
        items = COMMANDS
            .iter()
            .filter(|c| c.name.contains(&value[1..]))
            .collect();
    }
    items
}

fn complete_slash(app: App, c: &Command) {
    if !c.args.is_empty() {
        app.set_input(format!("{} ", c.name));
    } else {
        app.set_input(c.name);
        app.submit();
    }
}

/// Opens the reason box for a denial.
pub fn deny(app: App) {
    let mut open = app.denying;
    open.set(true);
    spawn_forever(async move {
        host::next_frame().await;
        host::focus("ap-reason");
    });
}

#[component]
pub fn Dock() -> Element {
    let app = use_context::<App>();
    let jump = *app.jump.read();
    let toast = app.toast.read().clone();
    let queued = app
        .status
        .read()
        .as_ref()
        .map(|s| s.queued.clone())
        .unwrap_or_default();
    let voice_open = *app.voice.open.read();
    let status = app.status.read().clone();
    let hint = match &status {
        Some(s) => match &s.goal {
            Some(goal) => format!(
                "◎ {}: {goal}",
                s.goal_state.clone().unwrap_or_else(|| "goal".into())
            ),
            None => format!("via {}", s.backend),
        },
        None => String::new(),
    };
    rsx! {
        div { class: "dock",
            button { class: "jump", id: "jump", hidden: !jump, onclick: move |_| app.scroll_to_bottom(true), {icon(I::Jump)} "Latest" }
            div { class: "toast", id: "toast", hidden: toast.is_none(), dangerous_inner_html: crate::html::inline(toast.as_deref().unwrap_or("")) }
            div { class: "queued", id: "queued",
                for q in queued {
                    div { class: "queued-item", "{q}" }
                }
            }
            if voice_open { super::voice::VoiceCard {} }
            ApprovalCard {}
            QuestionCard {}
            Composer {}
            div { class: "hints",
                span { id: "hint-left", "{hint}" }
                span { class: "keys-only", kbd { "⏎" } " send " kbd { "⇧⏎" } " newline " kbd { "/" } " commands" }
            }
        }
    }
}

#[component]
fn ApprovalCard() -> Element {
    let app = use_context::<App>();
    let mut denying = app.denying;
    let mut reason = use_signal(String::new);
    let approval = app.approval.read().clone();
    // A new request starts without a reason.
    let shown = approval
        .as_ref()
        .map(|a| (a.name.clone(), a.summary.clone()));
    let mut last = use_signal(|| None::<(String, String)>);
    if *last.peek() != shown {
        last.set(shown);
        denying.set(false);
        reason.set(String::new());
    }
    let Some(a) = approval else {
        return rsx! { div { class: "approval", id: "approval", hidden: true } };
    };
    let always = a.always.join(", ");
    rsx! {
        div { class: "approval", id: "approval",
            div { class: "ap-title", "Allow " code { "{a.name}" } "?" }
            div { class: "ap-desc", "{a.description}" }
            div { class: if a.name == "bash" { "ap-call bash" } else { "ap-call" }, "{a.summary}" }
            div { class: "ap-actions",
                button { class: "btn primary", onclick: move |_| app.approve("yes", None), span { "Allow" } kbd { "Y" } }
                if !a.always.is_empty() {
                    button { class: "btn", title: "{always}", onclick: move |_| app.approve("always", None), span { "Always allow {always}" } kbd { "A" } }
                }
                button { class: "btn danger", onclick: move |_| deny(app), span { "Deny…" } kbd { "N" } }
            }
            div { class: "ap-reason", hidden: !denying(),
                input {
                    id: "ap-reason",
                    placeholder: "Tell the model what to do instead (optional)",
                    value: "{reason}",
                    oninput: move |ev| reason.set(ev.value()),
                    onkeydown: move |ev| {
                        if ev.key() == Key::Enter {
                            app.approve("no", Some(reason.peek().clone()));
                        } else if ev.key() == Key::Escape {
                            denying.set(false);
                            ev.stop_propagation();
                            ev.prevent_default();
                        }
                    },
                }
                button { class: "btn danger", onclick: move |_| app.approve("no", Some(reason.peek().clone())), "Deny" }
            }
        }
    }
}

#[component]
fn QuestionCard() -> Element {
    let mut app = use_context::<App>();
    let Some(set) = app.question.read().clone() else {
        return rsx! { div { class: "question", id: "question", hidden: true } };
    };
    let at = (*app.asking.read()).min(set.questions.len().saturating_sub(1));
    let answers = app.answers.read().clone();
    let Some(q) = set.questions.get(at).cloned() else {
        return rsx! { div { class: "question", id: "question", hidden: true } };
    };
    let many = set.questions.len() > 1;
    let last = at + 1 == set.questions.len();
    let draft = answers.get(at).cloned().unwrap_or_default();
    let title = if set.description.is_empty() {
        "tamale has a question".to_string()
    } else {
        set.description.clone()
    };
    let multi = q.multi_select;
    rsx! {
        div { class: "question", id: "question",
            div { class: "q-head",
                span { class: "q-title", "{title}" }
                if many { span { class: "q-count", "{at + 1} of {set.questions.len()}" } }
            }
            if many {
                div { class: "q-tabs",
                    for (j, qq) in set.questions.iter().enumerate() {
                        button {
                            class: format!("q-tab{}{}", if j == at { " current" } else { "" }, if answers.get(j).is_some_and(|a| a.answered()) { " done" } else { "" }),
                            onclick: move |_| app.go_question(j as isize),
                            span { class: "q-mark" }
                            span { if qq.header.is_empty() { "Q{j + 1}" } else { "{qq.header}" } }
                        }
                    }
                }
            }
            div { class: "q-question", "{q.question}" }
            if multi { div { class: "q-hint", "Pick any that apply." } }
            div { class: "q-opts",
                for (row, o) in q.options.iter().enumerate() {
                    {
                        let on = draft.chosen.contains(&o.label);
                        rsx! {
                            button {
                                class: format!("q-opt{}{}", if multi { " multi" } else { "" }, if on { " on" } else { "" }),
                                role: if multi { "checkbox" } else { "radio" },
                                "aria-checked": "{on}",
                                onclick: move |_| app.choose_option(row),
                                span { class: "q-box" }
                                span { class: "q-text",
                                    span { class: "q-label", "{o.label}" }
                                    if !o.description.is_empty() { span { class: "q-desc", "{o.description}" } }
                                }
                                if row < 9 { kbd { "{row + 1}" } }
                            }
                        }
                    }
                }
            }
            input {
                class: "q-other",
                placeholder: if q.options.is_empty() { "Your answer" } else { "Or type your own answer" },
                value: "{draft.other}",
                oninput: move |ev| {
                    let mut answers = app.answers.write();
                    if let Some(a) = answers.get_mut(at) {
                        a.other = ev.value();
                        if !multi && !a.other.trim().is_empty() {
                            a.chosen.clear();
                        }
                    }
                },
                onkeydown: move |ev| {
                    if ev.key() == Key::Enter {
                        ev.prevent_default();
                        app.next_question();
                    } else if ev.key() == Key::Escape {
                        ev.prevent_default();
                        ev.stop_propagation();
                        host::blur_active();
                    }
                },
            }
            div { class: "ap-actions q-actions",
                if at > 0 { button { class: "btn", onclick: move |_| app.go_question(at as isize - 1), span { "Back" } } }
                button {
                    class: "btn primary q-next",
                    disabled: !draft.answered(),
                    onclick: move |_| app.next_question(),
                    span { if last { if many { "Send answers" } else { "Send" } } else { "Next" } }
                    kbd { "⏎" }
                }
            }
        }
    }
}

#[component]
fn Composer() -> Element {
    let mut app = use_context::<App>();
    let value = app.input.read().clone();
    let running = app.status.read().as_ref().is_some_and(|s| s.running);
    let touch = app.media.read().touch;
    let chef = *app.chef.read();
    let asking = app.question.read().is_some();
    let empty = value.trim().is_empty();
    let items = if *app.slash_hidden.read() {
        Vec::new()
    } else {
        slash_items(&value)
    };
    let slash_at = (*app.slash_at.read()).min(items.len().saturating_sub(1));
    let send_class = if !empty {
        "send ready"
    } else if running {
        "send stop"
    } else {
        "send"
    };
    let send_title = if empty && running {
        "Interrupt (esc)"
    } else if running {
        "Queue for the next step (⏎)"
    } else {
        "Send (⏎)"
    };
    let who = if chef {
        "Ask the chef about your sessions…"
    } else {
        "Ask tamale anything…"
    };
    let placeholder = if asking {
        "Or reply in your own words…".to_string()
    } else if touch {
        who.to_string()
    } else {
        format!("{who}   / for commands")
    };
    let n_items = items.len();
    rsx! {
        div { class: if empty { "composer empty" } else { "composer" }, id: "composer",
            div { class: "slash", id: "slash", hidden: items.is_empty(),
                for (i, c) in items.iter().enumerate() {
                    button {
                        class: if i == slash_at { "slash-row on" } else { "slash-row" },
                        id: "slash-{i}",
                        onmousedown: move |ev| {
                            ev.prevent_default();
                            complete_slash(app, slash_items(&app.input.peek())[i]);
                        },
                        onmousemove: move |_| if *app.slash_at.peek() != i { app.slash_at.set(i) },
                        span { class: "slash-name", "{c.name}" }
                        if !c.args.is_empty() { span { class: "slash-args", "{c.args}" } }
                        span { class: "slash-desc", "{c.desc}" }
                        if c.local { span { class: "slash-tag", "page" } }
                    }
                }
            }
            textarea {
                id: PROMPT,
                rows: "1",
                spellcheck: "true",
                placeholder: "{placeholder}",
                enterkeyhint: "enter",
                autofocus: true,
                value: "{value}",
                oninput: move |ev| {
                    app.input.set(ev.value());
                    app.slash_at.set(0);
                    app.slash_hidden.set(false);
                    host::autosize(PROMPT);
                },
                onfocus: move |_| app.slash_hidden.set(false),
                onblur: move |_| {
                    spawn_forever(async move {
                        host::sleep(100).await;
                        app.slash_hidden.set(true);
                    });
                },
                onkeydown: move |ev| on_prompt_key(app, ev, n_items),
            }
            button {
                class: "make-artifact make-video",
                id: "make-video",
                hidden: !empty,
                title: "Make an animated video of this (Manim)  /video",
                "aria-label": "Make an animated video",
                onclick: move |_| if app.send(json!({ "t": "send", "text": VIDEO_PROMPT }), false) { app.scroll_to_bottom(true) },
                {icon(I::Film)}
            }
            button {
                class: "make-artifact",
                id: "make-artifact",
                hidden: !empty,
                title: "Generate an artifact",
                "aria-label": "Generate an artifact",
                onclick: move |_| if app.send(json!({ "t": "send", "text": ARTIFACT_PROMPT }), false) { app.scroll_to_bottom(true) },
                {icon(I::Sparkles)}
            }
            button {
                class: "{send_class}",
                id: "send",
                title: "{send_title}",
                onclick: move |_| {
                    if app.input.peek().trim().is_empty() && app.running() {
                        app.send(json!({ "t": "interrupt" }), false);
                    } else {
                        app.submit();
                    }
                },
                {send_icons()}
            }
        }
    }
}

fn on_prompt_key(mut app: App, ev: KeyboardEvent, n_items: usize) {
    if ev.is_composing() {
        return;
    }
    let key = ev.key();
    let mods = ev.modifiers();
    let shift = mods.shift();
    // With nothing typed, the keys answer the agent's questions.
    let as_key = host::Key {
        key: key.to_string(),
        meta: mods.meta(),
        ctrl: mods.ctrl(),
        alt: mods.alt(),
        shift,
        in_field: true,
        in_prompt: true,
        prompt_empty: app.input.peek().is_empty(),
        ..Default::default()
    };
    if super::question_key(app, &as_key) {
        ev.prevent_default();
        return;
    }
    if n_items > 0 && !*app.slash_hidden.peek() {
        match key {
            Key::ArrowDown | Key::ArrowUp => {
                ev.prevent_default();
                let by = if key == Key::ArrowDown {
                    1
                } else {
                    n_items - 1
                };
                let at = (*app.slash_at.peek() + by) % n_items;
                app.slash_at.set(at);
                host::scroll_into_view(&format!("slash-{at}"), false);
                return;
            }
            Key::Tab | Key::Enter if !(key == Key::Enter && shift) => {
                ev.prevent_default();
                let items = slash_items(&app.input.peek());
                let at = (*app.slash_at.peek()).min(items.len().saturating_sub(1));
                // Enter on a command typed in full runs it; otherwise the choice is completed.
                if key == Key::Enter
                    && items
                        .get(at)
                        .is_some_and(|c| c.name == app.input.peek().trim())
                {
                    app.submit();
                } else if let Some(c) = items.get(at) {
                    complete_slash(app, c);
                }
                return;
            }
            Key::Escape => {
                ev.prevent_default();
                app.slash_hidden.set(true);
                return;
            }
            _ => {}
        }
    }
    // On a touch keyboard Return is the only way to start a new line; the button sends.
    if key == Key::Enter && !shift && !mods.alt() && !app.media.peek().touch {
        ev.prevent_default();
        app.submit();
        return;
    }
    // Earlier prompts, from the first line up or the last line down.
    if key == Key::ArrowUp || key == Key::ArrowDown {
        let value = app.input.peek().clone();
        let (start, end) = host::selection(PROMPT);
        let units: Vec<u16> = value.encode_utf16().collect();
        let before = String::from_utf16_lossy(&units[..start.min(units.len())]);
        let after = String::from_utf16_lossy(&units[end.min(units.len())..]);
        if key == Key::ArrowUp && !before.contains('\n') {
            if app.recall(true) {
                ev.prevent_default();
            }
        } else if key == Key::ArrowDown && !after.contains('\n') && app.recall(false) {
            ev.prevent_default();
        }
    }
}
