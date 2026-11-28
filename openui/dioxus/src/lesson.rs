//! A lesson (`root = Lesson(goal, parts)`, see `openui::lesson`): one part at a time, with
//! a rail of every part and the reader's progress. Each part ends with the questions that
//! check it and a discussion: what the reader sent the agent from this part (answers,
//! questions, replies to its follow-ups) and the agent's feedback, which the page hands
//! back as [`Exchange`]s.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use dioxus::prelude::*;
use openui::lesson::{self, says};
use openui::{Node, Value};

use super::{Ctx, Cx, nodes, rich, view};

/// One message the reader sent from a lesson and the agent's reply to it, as the page
/// found them in the conversation.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Exchange {
    /// The part it's about, from 1.
    pub part: usize,
    /// What the reader sent, without the marker line.
    pub asked: String,
    /// The agent's reply as HTML (tamale's rendering of its markdown); empty until it
    /// begins.
    pub reply: String,
    /// Whether the agent is still on it.
    pub waiting: bool,
}

/// Sends what the reader wrote about part `part` (from 1), marked as the lesson's.
fn send(ctx: Ctx, part: usize, part_title: &str, body: &str) -> bool {
    let Some(send) = ctx.send else {
        return false;
    };
    let title = ctx.title.peek().clone();
    send.call(format!(
        "{}\n{body}",
        lesson::marker(&title, part, part_title)
    ));
    true
}

fn scroll_to(id: &str) {
    let id = serde_json::Value::from(id);
    let _ = dioxus::document::eval(&format!(
        "document.getElementById({id})?.scrollIntoView({{ block: 'start', behavior: 'smooth' }});"
    ));
}

#[component]
pub(crate) fn Lesson(node: Node) -> Element {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = use_hook(|| format!("oui-lesson-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    let ctx = use_context::<Ctx>();
    let parts = nodes(node.arg("parts"), "LessonPart");
    let mut at = use_signal(|| 0usize);
    let mut understood = use_signal(BTreeSet::<usize>::new);
    let n = parts.len();
    let i = at().min(n.saturating_sub(1));
    let talk = ctx.talk.read();
    // A part the reader got a reply in counts as started.
    let talked: BTreeSet<usize> = talk.iter().map(|e| e.part).collect();
    drop(talk);
    let go = {
        let id = id.clone();
        move |to: usize| {
            at.set(to);
            scroll_to(&id);
        }
    };
    let goal = node.str("goal");
    let Some(part) = parts.get(i).cloned() else {
        return rsx! {
            div { class: "oui-lesson", id: "{id}",
                if !goal.is_empty() { LessonGoal { goal } }
                div { class: "oui-empty", "Writing the first part…" }
            }
        };
    };
    let last = i + 1 == n;
    let done_all = n > 0 && (0..n).all(|k| understood.read().contains(&k));
    rsx! {
        div { class: "oui-lesson", id: "{id}",
            if !goal.is_empty() { LessonGoal { goal } }
            nav { class: "oui-lesson-rail", "aria-label": "Parts of the lesson",
                for (k, p) in parts.iter().enumerate() {
                    {
                        let state = if k == i {
                            "on"
                        } else if understood.read().contains(&k) {
                            "done"
                        } else if talked.contains(&(k + 1)) {
                            "started"
                        } else {
                            ""
                        };
                        let mut go = go.clone();
                        rsx! {
                            button {
                                class: "oui-lesson-stop {state}",
                                "aria-current": if k == i { "step" } else { "false" },
                                title: "{p.str(\"title\")}",
                                onclick: move |_| go(k),
                                span { class: "n", if understood.read().contains(&k) { "✓" } else { "{k + 1}" } }
                                span { class: "t", "{p.str(\"title\")}" }
                            }
                        }
                    }
                }
            }
            LessonPart { key: "{i}", node: part, number: i + 1, of: n }
            div { class: "oui-lesson-nav",
                button {
                    class: "oui-btn v-secondary",
                    disabled: i == 0,
                    onclick: {
                        let mut go = go.clone();
                        move |_| go(i.saturating_sub(1))
                    },
                    "← Back"
                }
                span { class: "oui-lesson-count", "{understood.read().len()} of {n} understood" }
                if last {
                    button {
                        class: "oui-btn v-primary",
                        disabled: done_all,
                        onclick: move |_| { understood.write().insert(i); },
                        if done_all { "✓ You've got it all" } else { "I understand this part" }
                    }
                } else {
                    button {
                        class: "oui-btn v-primary",
                        onclick: {
                            let mut go = go.clone();
                            move |_| {
                                understood.write().insert(i);
                                go(i + 1);
                            }
                        },
                        "I've got this — next →"
                    }
                }
            }
            if ctx.send.is_none() {
                p { class: "oui-lesson-offline", "Open this lesson in tamale's chat to have your answers checked and your questions answered." }
            }
        }
    }
}

#[component]
fn LessonGoal(goal: String) -> Element {
    rsx! {
        div { class: "oui-lesson-goal",
            span { class: "k", "By the end" }
            span { class: "v", {rich(&goal)} }
        }
    }
}

#[component]
fn LessonPart(node: Node, number: usize, of: usize) -> Element {
    let title = node.str("title");
    let level = node.str("level");
    let checks: Vec<Node> = node
        .arg("checks")
        .items()
        .into_iter()
        .filter_map(|v| match v {
            Value::Node(n) if n.name == "Check" || n.name == "QuickCheck" => Some(n),
            _ => None,
        })
        .collect();
    rsx! {
        section { class: "oui-lesson-part",
            div { class: "oui-lesson-level",
                span { "Part {number} of {of}" }
                if !level.is_empty() { span { class: "lvl", {rich(&level)} } }
            }
            h2 { class: "oui-lesson-title", {rich(&title)} }
            div { class: "oui-lesson-content", {view(node.arg("content"), &Cx::default())} }
            if !checks.is_empty() {
                div { class: "oui-lesson-checks",
                    h3 { "Check your understanding" }
                    for (j, c) in checks.into_iter().enumerate() {
                        if c.name == "QuickCheck" {
                            QuickCheck { key: "q{j}", node: c }
                        } else {
                            Check { key: "c{j}", node: c, part: number, part_title: title.clone() }
                        }
                    }
                }
            }
            Discussion { part: number, part_title: title.clone() }
        }
    }
}

/// An open question: the reader's answer goes to the agent, whose feedback shows in the
/// part's discussion.
#[component]
fn Check(node: Node, part: usize, part_title: String) -> Element {
    let ctx = use_context::<Ctx>();
    let mut answer = use_signal(String::new);
    let question = node.str("question");
    let answered = ctx.talk.read().iter().any(|e| {
        e.part == part && says::answered(&e.asked).is_some_and(|(q, _)| q == question.trim())
    });
    let mut submit = {
        let (question, part_title) = (question.clone(), part_title.clone());
        move || {
            let text = answer.peek().trim().to_string();
            if !text.is_empty() && send(ctx, part, &part_title, &says::answer(&question, &text)) {
                answer.set(String::new());
            }
        }
    };
    let mut submit2 = submit.clone();
    let can = ctx.send.is_some();
    rsx! {
        div { class: if answered { "oui-check answered" } else { "oui-check" },
            div { class: "oui-check-q",
                span { class: "mark", if answered { "✓" } else { "?" } }
                span { {rich(&question)} }
            }
            if let Some(h) = node.opt("hint") {
                details { class: "oui-check-more", summary { "Hint" } div { {rich(&h)} } }
            }
            if can {
                textarea {
                    class: "oui-input",
                    rows: "3",
                    placeholder: if answered { "Answer again, or differently…" } else { "Your answer, in your own words…" },
                    value: "{answer}",
                    oninput: move |e: FormEvent| answer.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Enter && (e.modifiers().meta() || e.modifiers().ctrl()) {
                            e.prevent_default();
                            submit2();
                        }
                    },
                }
                div { class: "oui-check-row",
                    if answered { span { class: "oui-check-note", "Feedback is in the discussion below." } }
                    span { class: "spacer" }
                    button {
                        class: "oui-btn v-primary",
                        disabled: answer.read().trim().is_empty(),
                        onclick: move |_| submit(),
                        "Check my answer"
                    }
                }
            }
            if let Some(k) = node.opt("keyPoints") {
                if answered || !can {
                    details { class: "oui-check-more", open: answered,
                        summary { "What a good answer covers" }
                        div { {rich(&k)} }
                    }
                }
            }
        }
    }
}

/// A multiple-choice question, answered on the spot. With one right answer a tap answers
/// it; with several, the reader ticks the ones they think are right and then checks.
#[component]
fn QuickCheck(node: Node) -> Element {
    let mut picked = use_signal(Vec::<usize>::new);
    let mut checked = use_signal(|| false);
    let options: Vec<String> = node
        .arg("options")
        .items()
        .iter()
        .map(Value::text)
        .collect();
    // The right options, by index: as numbers, or as they're sometimes given instead, by
    // letter ("B") or by the option's own words.
    let right: Vec<usize> = node
        .arg("answer")
        .items()
        .iter()
        .filter_map(|v| match v {
            Value::Num(n) if *n >= 0.0 => Some(*n as usize),
            Value::Str(s) => {
                let s = s.trim();
                let letter = match s.as_bytes() {
                    [c] if c.is_ascii_alphabetic() => {
                        Some((c.to_ascii_uppercase() - b'A') as usize)
                    }
                    _ => None,
                };
                s.parse::<usize>()
                    .ok()
                    .or_else(|| options.iter().position(|o| o.trim() == s))
                    .or(letter)
            }
            _ => None,
        })
        .filter(|i| *i < options.len())
        .collect();
    let several = right.len() > 1;
    let why = node.opt("why");
    let answered = checked();
    let chosen = picked();
    let correct = answered && {
        let mut a = chosen.clone();
        let mut b = right.clone();
        a.sort_unstable();
        b.sort_unstable();
        a == b
    };
    rsx! {
        div { class: if answered && correct { "oui-check quick answered" } else { "oui-check quick" },
            div { class: "oui-check-q",
                span { class: "mark", if answered && correct { "✓" } else { "?" } }
                span { {rich(&node.str("question"))} }
            }
            if several && !answered { div { class: "oui-check-note", "More than one is right: pick every one that is." } }
            div { class: "oui-quick-options", role: if several { "group" } else { "radiogroup" },
                for (k, o) in options.into_iter().enumerate() {
                    {
                        let on = chosen.contains(&k);
                        let state = match (answered, right.contains(&k), on) {
                            (true, true, _) => "right",
                            (true, false, true) => "wrong",
                            (true, false, false) => "dim",
                            (false, _, true) => "on",
                            (false, _, false) => "",
                        };
                        rsx! {
                            button {
                                class: "oui-quick {state}",
                                role: if several { "checkbox" } else { "radio" },
                                "aria-checked": "{on}",
                                onclick: move |_| {
                                    if several {
                                        if checked() {
                                            return;
                                        }
                                        let mut now = picked.write();
                                        match now.iter().position(|p| *p == k) {
                                            Some(at) => { now.remove(at); }
                                            None => now.push(k),
                                        }
                                    } else {
                                        picked.set(vec![k]);
                                        checked.set(true);
                                    }
                                },
                                span { class: "l", "{(b'A' + (k as u8 % 26)) as char}" }
                                span { {rich(&o)} }
                            }
                        }
                    }
                }
            }
            if several && !answered {
                div { class: "oui-check-row",
                    span { class: "spacer" }
                    button { class: "oui-btn v-primary", disabled: chosen.is_empty(), onclick: move |_| checked.set(true), "Check" }
                }
            }
            if answered {
                div { class: if correct { "oui-quick-why right" } else { "oui-quick-why wrong" },
                    strong { if correct { "Right. " } else { "Not quite. " } }
                    if let Some(w) = why { {rich(&w)} }
                }
            }
        }
    }
}

/// What the reader and the agent said about this part, and where to say more.
#[component]
fn Discussion(part: usize, part_title: String) -> Element {
    let ctx = use_context::<Ctx>();
    let mut text = use_signal(String::new);
    let talk: Vec<Exchange> = ctx
        .talk
        .read()
        .iter()
        .filter(|e| e.part == part)
        .cloned()
        .collect();
    if ctx.send.is_none() && talk.is_empty() {
        return rsx! {};
    }
    let mut submit = move || {
        let body = says::note(&text.peek());
        if !body.is_empty() && send(ctx, part, &part_title, &body) {
            text.set(String::new());
        }
    };
    let mut submit2 = submit.clone();
    rsx! {
        div { class: "oui-talk",
            h3 { if talk.is_empty() { "Questions?" } else { "Discussion" } }
            for (k, e) in talk.into_iter().enumerate() {
                div { key: "{k}", class: "oui-talk-x",
                    div { class: "oui-talk-you",
                        span { class: "who", "You" }
                        match says::answered(&e.asked) {
                            Some((q, a)) => rsx! {
                                div { class: "about", "Answering: " {rich(q)} }
                                div { class: "said", "{a}" }
                            },
                            None => rsx! { div { class: "said", "{e.asked}" } },
                        }
                    }
                    div { class: "oui-talk-tutor",
                        span { class: "who", "Tutor" }
                        if e.reply.is_empty() {
                            if e.waiting {
                                div { class: "oui-talk-wait", span {} span {} span {} }
                            } else {
                                div { class: "oui-talk-none", "No reply." }
                            }
                        } else {
                            div { class: "said prose", dangerous_inner_html: "{e.reply}" }
                        }
                    }
                }
            }
            if ctx.send.is_some() {
                div { class: "oui-talk-ask",
                    textarea {
                        class: "oui-input",
                        rows: "2",
                        placeholder: "Ask about this part, or answer the tutor's question…",
                        value: "{text}",
                        oninput: move |e: FormEvent| text.set(e.value()),
                        onkeydown: move |e: KeyboardEvent| {
                            if e.key() == Key::Enter && (e.modifiers().meta() || e.modifiers().ctrl()) {
                                e.prevent_default();
                                submit2();
                            }
                        },
                    }
                    button {
                        class: "oui-btn v-secondary",
                        disabled: text.read().trim().is_empty(),
                        onclick: move |_| submit(),
                        "Send"
                    }
                }
            }
        }
    }
}
