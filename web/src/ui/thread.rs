//! The conversation: the entries, tool calls folded into runs of steps, replies with their
//! artifacts and videos, and what the agent is doing now.

use dioxus::prelude::*;

use crate::app::{App, Item, fmt_elapsed, plural};
use crate::host;
use crate::html::{self, Block};
use crate::icons::{I, icon};
use crate::voice::Topic;
use crate::wire::Entry;
use serde_json::json;

use openui_dioxus::{Exchange, OpenUi};

/// Steps whose summary is the path of the file they read or changed.
const FILE_TOOLS: &[&str] = &["read_file", "write_file", "edit_file"];
const CHEF_TOOLS: &[&str] = &[
    "list_sessions",
    "read_session",
    "ask_sessions",
    "message_session",
];
/// A run of steps longer than this folds its earlier ones away.
const SHOWN_STEPS: usize = 4;

/// Consecutive tool calls go together.
enum Segment {
    One(usize, Item),
    Tools(usize, Vec<(usize, Item)>),
}

#[component]
pub fn Thread() -> Element {
    let app = use_context::<App>();
    let entries = app.entries.read().clone();
    let hero = !entries.iter().any(|e| !matches!(*e.0, Entry::Info { .. }));
    let mut segments: Vec<Segment> = Vec::new();
    for (i, item) in entries.into_iter().enumerate() {
        if item.0.is_tool() {
            if let Some(Segment::Tools(_, run)) = segments.last_mut() {
                run.push((i, item));
                continue;
            }
            segments.push(Segment::Tools(i, vec![(i, item)]));
        } else {
            segments.push(Segment::One(i, item));
        }
    }
    rsx! {
        div { class: "thread", id: "thread",
            if hero { Hero {} }
            for segment in segments {
                match segment {
                    Segment::One(i, item) => rsx! { EntryView { key: "{i}-{item.0.kind()}", index: i, item } },
                    Segment::Tools(start, run) => rsx! { ToolRun { key: "tools-{start}", run } },
                }
            }
        }
    }
}

#[component]
fn Hero() -> Element {
    let app = use_context::<App>();
    let chef = *app.chef.read();
    let cwd = app
        .status
        .read()
        .as_ref()
        .map(|s| s.cwd.clone())
        .unwrap_or_default();
    if chef {
        let kitchen = app.kitchen.read().clone().unwrap_or_default();
        let count = |st: &str| kitchen.sessions.iter().filter(|s| s.state == st).count();
        let stats: Vec<(&str, usize, &str)> = [
            ("approval", count("approval"), "need you"),
            ("working", count("working"), "working"),
            ("idle", count("idle"), "idle"),
            ("offline", count("offline"), "offline"),
        ]
        .into_iter()
        .filter(|(_, n, _)| *n > 0)
        .collect();
        let problem = kitchen
            .problem
            .clone()
            .unwrap_or_else(|| "No other sessions on the hub yet.".into());
        let n = kitchen.sessions.len();
        let chips = [
            (
                "What is everyone working on?",
                "What is everyone working on?",
            ),
            (
                "Which sessions need me right now, and for what?",
                "Which sessions need me?",
            ),
            (
                "Ask all sessions whether their tests pass, and summarize.",
                "Do the tests pass everywhere?",
            ),
            (
                "Are any two sessions working on the same files or likely to conflict?",
                "Any conflicts brewing?",
            ),
        ];
        return rsx! {
            div { class: "hero chef-hero", id: "hero",
                div { class: "hero-mark chef-mark", {crate::icons::toque()} span { class: "chef-tamale", "🫔" } }
                h1 { "What's cooking?" }
                p { class: "hero-lede", "I keep an eye on every tamale session on your machines. Ask me what they're doing, ask them questions, or pass them instructions." }
                div { class: "hero-kitchen", id: "hero-kitchen",
                    if kitchen.loading {
                        span { class: "k-stat", "Looking around the kitchen…" }
                    } else if n > 0 {
                        span { class: "k-stat total", b { "{n}" } if n == 1 { " session" } else { " sessions" } }
                        for (st, n, label) in stats {
                            span { class: "k-stat st-{st}", b { "{n}" } " {label}" }
                        }
                    } else {
                        span { class: "k-stat", "{problem}" }
                    }
                }
                div { class: "hero-chips",
                    for (send, label) in chips {
                        button { class: "chip", onclick: move |_| app.ask(send), "{label}" }
                    }
                }
            }
        };
    }
    rsx! {
        div { class: "hero", id: "hero",
            div { class: "hero-mark", "🫔" }
            h1 { "What should we work on?" }
            p { class: "hero-sub", id: "hero-sub", "{cwd}" }
            div { class: "hero-chips",
                button { class: "chip", onclick: move |_| app.set_input("Give me a tour of this codebase: what it does, how it's laid out, and where to start reading."), "Give me a tour of this codebase" }
                button { class: "chip", onclick: move |_| app.set_input("Look through the code for likely bugs and tell me the most serious ones."), "Look for likely bugs" }
                button { class: "chip", onclick: move |_| { app.run_local("/review"); }, kbd { class: "keys-only", "⌘B" } " Open the code" }
            }
        }
    }
}

#[component]
fn EntryView(index: usize, item: Item) -> Element {
    match &*item.0 {
        Entry::User { text } => match openui::lesson::parse(text) {
            // Sent from a lesson: which part, then what the reader wrote.
            Some(m) => {
                let about = format!("{} · Part {}: {}", m.title, m.part, m.part_title);
                let (question, body) = match openui::lesson::says::answered(m.body) {
                    Some((q, a)) => (Some(q.to_string()), a.to_string()),
                    None => (None, m.body.to_string()),
                };
                rsx! {
                    div { class: "msg user lesson-msg",
                        div { class: "lesson-about", {icon(I::Learn)} span { "{about}" } }
                        if let Some(q) = question { div { class: "lesson-q", "Answering: {q}" } }
                        div { dangerous_inner_html: html::user_html(&body) }
                    }
                }
            }
            None => rsx! { div { class: "msg user", dangerous_inner_html: html::user_html(text) } },
        },
        Entry::Assistant { html, .. } => {
            rsx! { Prose { class: "msg assistant prose", html: html.clone() } }
        }
        Entry::Note { html, .. } => rsx! { Prose { class: "msg note prose", html: html.clone() } },
        // The startup greeting, which the empty page says better.
        Entry::Info { text } => {
            let greeting = index == 0 && text.starts_with("Welcome!");
            rsx! { div { class: "msg info", hidden: greeting, dangerous_inner_html: html::inline(text) } }
        }
        Entry::Error { text } => {
            rsx! { div { class: "msg error", dangerous_inner_html: html::inline(text) } }
        }
        Entry::Tool { .. } | Entry::Unknown => rsx! {},
    }
}

/// A reply, block by block: streaming text redraws only the block it's adding to, so the
/// artifacts above it don't reload or lose what the reader did in them.
#[component]
fn Prose(class: String, html: String) -> Element {
    let blocks = html::blocks(&html);
    let mut seen = std::collections::HashMap::<String, usize>::new();
    rsx! {
        div { class: "{class}",
            for block in blocks {
                {
                    let base = match &block {
                        // By position, so one that finishes streaming keeps its state.
                        Block::Artifact { .. } => "artifact".to_string(),
                        Block::Video { path, .. } => format!("v{path}"),
                        Block::Code { inner } => html::hash(inner),
                        Block::Html(h) => html::hash(h),
                    };
                    let n = seen.entry(base.clone()).or_default();
                    *n += 1;
                    let key = format!("{base}-{n}");
                    rsx! { BlockView { key: "{key}", block, id: key.clone() } }
                }
            }
        }
    }
}

#[component]
fn BlockView(block: Block, id: String) -> Element {
    match block {
        Block::Artifact {
            src,
            title,
            source,
            done,
        } => rsx! { ArtifactBox { src, title, source, done } },
        Block::Video { path, title } => rsx! { VideoBox { path, title } },
        Block::Code { inner } => rsx! { CodeBlock { inner } },
        Block::Html(h) => rsx! { div { class: "blk", dangerous_inner_html: h } },
    }
}

#[component]
fn CodeBlock(inner: String) -> Element {
    let app = use_context::<App>();
    let mut copied = use_signal(|| false);
    let text = html::code_text(&inner);
    rsx! {
        div { class: "codeblock",
            div { class: "blk", dangerous_inner_html: inner }
            button {
                class: if copied() { "copybtn done" } else { "copybtn" },
                onclick: move |_| {
                    app.copy(text.clone(), "Copied code.");
                    copied.set(true);
                    spawn(async move {
                        host::sleep(1400).await;
                        copied.set(false);
                    });
                },
                if copied() { "Copied" } else { "Copy" }
            }
        }
    }
}

/// An artifact, drawn in place (see `openui`), with a bar to talk about, revise, save,
/// open or enlarge it. While it streams in, it's drawn as far as it has come.
#[component]
pub fn ArtifactBox(src: String, title: String, source: String, done: bool) -> Element {
    let app = use_context::<App>();
    let key = use_hook(|| format!("f{}", html::hash(&format!("{}", host::now_ms()))));
    let full = app.full.read().as_deref() == Some(key.as_str());
    let toggle = {
        let key = key.clone();
        move |_| {
            if app.full.peek().as_deref() == Some(key.as_str()) {
                app.close_full()
            } else {
                app.open_full(key.clone())
            }
        }
    };
    let urls = app.artifact_urls();
    let base = app.place.read().base.clone();
    let kept = done && !src.is_empty();
    let lines = source.lines().count();
    // A lesson keeps the reader in it: what they send there, and the replies, show in the
    // part they came from.
    let lesson = use_memo(use_reactive!(|source| openui::lesson::is_lesson(&source)));
    let lesson = lesson();
    let talk = if lesson && done {
        let running = app.status.read().as_ref().is_some_and(|s| s.running);
        lesson_talk(&app.entries.read(), &title, running)
    } else {
        Vec::new()
    };
    let send = Callback::new(move |text: String| {
        let from_lesson = openui::lesson::parse(&text).is_some();
        if !from_lesson {
            app.close_full();
        }
        if app.send(json!({ "t": "send", "text": text }), false) && !from_lesson {
            app.scroll_to_bottom(true)
        }
    });
    let (t1, t2, s1, s2) = (title.clone(), title.clone(), src.clone(), src.clone());
    let class = match (full, done) {
        (true, _) => "artifact full",
        (false, true) => "artifact",
        (false, false) => "artifact building",
    };
    let class = if lesson {
        format!("{class} lesson")
    } else {
        class.to_string()
    };
    rsx! {
        div { class, id: "art-box-{key}", "data-src": "{src}",
            div { class: "artifact-bar",
                if done && lesson {
                    span { class: "artifact-icon lesson-icon", title: "A lesson", {icon(I::Learn)} }
                } else if done {
                    span { class: "artifact-icon", {icon(I::Spark)} }
                } else {
                    span { class: "artifact-spark" }
                }
                // On a phone the title is the biggest target there is: tap it for full screen.
                button { class: "artifact-title", title: "{title}", onclick: toggle.clone(),
                    if done { "{title}" } else { "Building {title}…" }
                }
                span { class: "spacer" }
                if !done {
                    span { class: "artifact-meta", "{lines} line{plural_s(lines)}" }
                }
                if kept {
                    button {
                        class: "artifact-btn",
                        title: "Talk about this artifact with Gemini",
                        onclick: move |_| {
                            app.close_full();
                            app.voice.start(app, Some(Topic { title: t1.clone(), url: s1.clone() }), host::Audio::new());
                        },
                        {icon(I::Wave)}
                    }
                    button {
                        class: "artifact-btn",
                        title: "Revise this artifact",
                        onclick: move |_| {
                            app.close_full();
                            app.set_input(format!("Revise the \"{t2}\" artifact: "));
                        },
                        {icon(I::Edit)}
                    }
                    button {
                        class: "artifact-btn wide-only",
                        title: "Download its OpenUI Lang document (also saved with the session; /artifacts lists them)",
                        onclick: {
                            let (src, title) = (src.clone(), title.clone());
                            move |_| app.save_artifact(src.clone(), title.clone())
                        },
                        {icon(I::Download)}
                    }
                    if urls {
                        a {
                            class: "artifact-btn",
                            title: "Open in a new tab",
                            href: "{base}{s2}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            {icon(I::Open)}
                        }
                    }
                }
                button {
                    class: "artifact-btn artifact-expand",
                    title: if full { "Exit full screen (esc)" } else { "Full screen" },
                    onclick: toggle,
                    if full { {icon(I::Shrink)} } else { {icon(I::Expand)} }
                }
            }
            div { class: "artifact-body",
                OpenUi { source, done, send: Some(send), talk }
            }
        }
    }
}

/// What the reader sent from the lesson `title` (see `openui::lesson`), each with the
/// replies that followed it. The latest waits while the agent is `running`, or until its
/// reply begins.
fn lesson_talk(entries: &[Item], title: &str, running: bool) -> Vec<Exchange> {
    let mut out: Vec<Exchange> = Vec::new();
    // The exchange the replies so far belong to.
    let mut open: Option<usize> = None;
    for item in entries {
        match &*item.0 {
            Entry::User { text } => {
                open = None;
                if let Some(m) = openui::lesson::parse(text).filter(|m| m.title == title) {
                    out.push(Exchange {
                        part: m.part,
                        asked: m.body.to_string(),
                        ..Exchange::default()
                    });
                    open = Some(out.len() - 1);
                }
            }
            Entry::Assistant { html, .. } => {
                if let Some(e) = open.map(|k| &mut out[k]) {
                    if !e.reply.is_empty() {
                        e.reply.push('\n');
                    }
                    e.reply.push_str(html);
                }
            }
            Entry::Error { .. } => open = None,
            _ => {}
        }
    }
    if let Some(e) = open.map(|k| &mut out[k]) {
        e.waiting = running || e.reply.is_empty();
    }
    out
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A video a reply shows, asked of tamale as it comes into view.
#[component]
fn VideoBox(path: String, title: String) -> Element {
    let app = use_context::<App>();
    let id = use_hook(|| format!("video-{}", html::hash(&format!("{path}{}", host::now_ms()))));
    let mut tries = use_signal(|| 0u32);
    let got = use_resource({
        let (id, path) = (id.clone(), path.clone());
        move || {
            let (id, path) = (id.clone(), path.clone());
            let _ = tries();
            async move {
                host::visible(&id).await;
                app.video(path).await
            }
        }
    });
    let state = got.read().clone();
    rsx! {
        div { class: "video", id: "{id}",
            div { class: "artifact-bar",
                span { class: "artifact-icon video-icon", {icon(I::Play)} }
                span { class: "artifact-title", title: "{path}", "{title}" }
                span { class: "spacer" }
                if let Some(Ok((url, _))) = &state {
                    a { class: "artifact-btn", title: "Download", "aria-label": "Download", href: "{url}", download: "{title}", {icon(I::Download)} }
                }
            }
            div { class: "video-stage",
                match state {
                    None => rsx! { div { class: "video-wait", "Loading the video…" } },
                    Some(Ok((url, mime))) if mime.starts_with("image/") => rsx! { img { src: "{url}", alt: "{title}" } },
                    Some(Ok((url, _))) => rsx! { video { src: "{url}", controls: true, playsinline: true, "loop": true, preload: "metadata" } },
                    Some(Err(e)) => rsx! {
                        div { class: "video-wait error",
                            span { "{e}" }
                            button { class: "btn", onclick: move |_| tries += 1, span { "Try again" } }
                        }
                    },
                }
            }
        }
    }
}

/// A run of tool calls, the earlier ones folded away when there are many.
#[component]
fn ToolRun(run: Vec<(usize, Item)>) -> Element {
    let mut collapsed = use_signal(|| true);
    let count = run.len();
    let more = if collapsed() {
        format!(
            "Show {}",
            plural(
                count.saturating_sub(SHOWN_STEPS),
                "earlier step",
                "earlier steps"
            )
        )
    } else {
        "Show fewer steps".into()
    };
    rsx! {
        div { class: if collapsed() { "tools collapsed" } else { "tools" },
            button { class: "tools-more", hidden: count <= SHOWN_STEPS, onclick: move |_| collapsed.toggle(), "{more}" }
            div { class: "tool-list",
                for (i, item) in run {
                    ToolView { key: "{i}", item }
                }
            }
        }
    }
}

#[component]
fn ToolView(item: Item) -> Element {
    let app = use_context::<App>();
    let mut open = use_signal(|| false);
    let Entry::Tool {
        name,
        summary,
        description,
        output,
        error,
        cut,
    } = &*item.0
    else {
        return rsx! {};
    };
    let pending = output.is_none();
    let chef = *app.chef.read();
    let class = format!(
        "tool{}{}{}",
        if CHEF_TOOLS.contains(&name.as_str()) {
            " chef-step"
        } else {
            ""
        },
        if pending {
            " pending"
        } else if *error {
            " error"
        } else {
            " done"
        },
        if open() { " open" } else { "" }
    );
    let first = summary.lines().next().unwrap_or("").to_string();
    // The summary is usually the description's subject ("Read src/main.rs"); skip it then.
    let show_summary = !first.is_empty() && !description.contains(&first);
    // A step on a file leads to it in the code view: what it changed, or the file it read.
    let opens = FILE_TOOLS.contains(&name.as_str()) && !first.is_empty() && !chef;
    let changes = opens && name != "read_file";
    let command = name == "bash" || summary.contains('\n');
    let path = first.clone();
    rsx! {
        div { class: "{class}",
            button {
                class: "tool-head",
                title: "{name}: {summary}",
                onclick: move |_| open.toggle(),
                span { class: "tool-icon", if *error { {icon(I::Cross)} } else { {icon(I::Check)} } }
                span { class: "tool-desc", "{description}" }
                span { class: "tool-sum", if show_summary { "{first}" } }
                if opens {
                    span {
                        class: "tool-open",
                        title: if changes { "See what changed in this file" } else { "Open this file" },
                        onclick: move |ev| {
                            ev.stop_propagation();
                            let (p, line) = html::file_path(&path).unwrap_or((path.clone(), None));
                            app.open_in_code(&p, line, changes);
                        },
                        {icon(I::Open)}
                        if changes { "diff" } else { "open" }
                    }
                }
                span { class: "tool-chev", {icon(I::Chev)} }
            }
            if open() {
                div { class: "tool-out",
                    if command { pre { class: "cmd", "{summary}" } }
                    match output {
                        None => rsx! { pre { "Running…" } },
                        Some(out) if html::looks_like_diff(out) => rsx! {
                            pre {
                                div { class: "difflines",
                                    for line in out.split('\n') {
                                        span {
                                            class: if line.starts_with("@@") { "hunk" } else if line.starts_with('+') && !line.starts_with("+++") { "add" } else if line.starts_with('-') && !line.starts_with("---") { "del" } else { "" },
                                            if line.is_empty() { " " } else { "{line}" }
                                        }
                                    }
                                }
                                if *cut { span { class: "cut", "… output cut here" } }
                            }
                        },
                        Some(out) => rsx! {
                            pre {
                                if out.is_empty() { "(no output)" } else { "{out}" }
                                if *cut { span { class: "cut", "… output cut here" } }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// After this long on one step, the activity line says what it's waiting on.
const LONG_STEP: u64 = 180;

/// Why a long step is still going, in words.
fn long_step_note(kind: &str, steps: usize, t: &str) -> String {
    match kind {
        "think" => format!(
            "No output from the model for {t}. It may be reasoning through a big step; interrupt it if it looks stuck."
        ),
        "subagent" => format!(
            "The subagent has been on its own for {t}{}. Only its final report goes back to the agent.",
            if steps > 0 {
                format!(" ({} so far)", plural(steps, "step", "steps"))
            } else {
                String::new()
            }
        ),
        "tool" => {
            format!("This step has been running for {t}. Its output shows up when it finishes.")
        }
        "wait" => format!("Waiting on you for {t}. The run is paused until you answer."),
        _ => String::new(),
    }
}

/// What the agent is doing now, and for how long.
#[component]
pub fn Activity() -> Element {
    let app = use_context::<App>();
    let _ = app.tick.read();
    let status = app.status.read().clone();
    let Some(s) = status.filter(|s| s.running) else {
        return rsx! { div { class: "activity", id: "activity", hidden: true } };
    };
    let now = (host::now_ms() / 1000.0) as u64;
    let total = s.since.map(|since| now.saturating_sub(since));
    // The server says what the run is doing (see `app::activity::step` in tamale).
    let label = if s.step.is_empty() {
        "Thinking".to_string()
    } else {
        s.step.clone()
    };
    let kind = if s.step_kind.is_empty() {
        "think".to_string()
    } else {
        s.step_kind.clone()
    };
    let step_time = s.step_since.map(|t| now.saturating_sub(t)).or(total);
    let long = step_time.is_some_and(|t| t >= LONG_STEP);
    let class = format!(
        "activity{}{}",
        if kind == "wait" { " waiting" } else { "" },
        if long { " long" } else { "" }
    );
    // The run's totals, once there's more to it than this one step.
    let mut meta = Vec::new();
    if s.steps > 0 {
        meta.push(plural(s.steps, "step", "steps"));
    }
    if let (Some(total), Some(step)) = (total, step_time)
        && total - step.min(total) >= 5
    {
        meta.push(format!("{} total", fmt_elapsed(total)));
    }
    let note = if long {
        long_step_note(&kind, s.sub_steps, &fmt_elapsed(step_time.unwrap_or(0)))
    } else {
        String::new()
    };
    rsx! {
        div { class: "{class}", id: "activity",
            span { class: "spark" }
            span { class: "act-label", id: "act-label", "{label}" }
            span { class: "act-time", id: "act-time", if let Some(t) = step_time { "{fmt_elapsed(t)}" } }
            span { class: "act-meta", id: "act-meta", "{meta.join(\" · \")}" }
            span { class: "act-hint keys-only", kbd { "esc" } " to interrupt" }
            p { class: "act-note", id: "act-note", hidden: note.is_empty(), "{note}" }
        }
    }
}
