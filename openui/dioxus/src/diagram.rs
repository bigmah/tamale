//! Diagrams with more than one way through them: `Diagram`, boxes joined by arrows that
//! branch, merge and loop back, laid out in rows by `openui::graph`; and `Sequence`, who
//! sends what to whom, in order. (`Flow`, a straight chain, is drawn in `lib.rs`.)

use dioxus::prelude::*;
use openui::Node;
use openui::graph::{self, Slot};

use super::{nodes, rich};

/// How tall a box is and how far apart rows are, in px: fixed, so arrows can be drawn
/// between boxes without measuring them.
const BOX: f64 = 58.0;
const GAP: f64 = 30.0;
/// Rows are further apart when the arrows between them carry words.
const GAP_WITH_LABELS: f64 = 42.0;
/// How far short of a box an arrow's line stops, where its head is.
const HEAD: f64 = 5.0;
/// How far aside an arrow that points up runs, in hundredths of the width, so it isn't
/// drawn over one that points down between the same boxes.
const ASIDE: f64 = 3.0;

struct BoxInfo {
    id: String,
    label: String,
    detail: Option<String>,
    tone: String,
}

/// The index of the box an edge names: by its id, or else by its label; a name no box has
/// gets a box of its own, since an arrow to nothing helps no one.
fn find(boxes: &mut Vec<BoxInfo>, name: &str) -> usize {
    let name = name.trim();
    let same = |a: &str| a.eq_ignore_ascii_case(name);
    if let Some(i) = boxes.iter().position(|b| b.id == name) {
        return i;
    }
    if let Some(i) = boxes.iter().position(|b| same(&b.id) || same(&b.label)) {
        return i;
    }
    boxes.push(BoxInfo {
        id: name.to_string(),
        label: name.to_string(),
        detail: None,
        tone: "neutral".into(),
    });
    boxes.len() - 1
}

#[component]
pub(crate) fn Diagram(node: Node) -> Element {
    let mut picked = use_signal(|| None::<usize>);
    let mut boxes: Vec<BoxInfo> = nodes(node.arg("nodes"), "DiagramNode")
        .iter()
        .map(|b| BoxInfo {
            id: b.str("id"),
            label: b.opt("label").unwrap_or_else(|| b.str("id")),
            detail: b.opt("detail"),
            tone: b.opt("tone").unwrap_or_else(|| "neutral".into()),
        })
        .collect();
    // (from, to, its words, dashed)
    let arrows: Vec<(usize, usize, Option<String>, bool)> = nodes(node.arg("edges"), "DiagramEdge")
        .iter()
        .filter(|e| !e.str("from").trim().is_empty() && !e.str("to").trim().is_empty())
        .map(|e| {
            let from = find(&mut boxes, &e.str("from"));
            let to = find(&mut boxes, &e.str("to"));
            (from, to, e.opt("label"), e.str("style") == "dashed")
        })
        .collect();
    if boxes.is_empty() {
        return rsx! {};
    }
    let pairs: Vec<(usize, usize)> = arrows.iter().map(|a| (a.0, a.1)).collect();
    let layout = graph::layout(boxes.len(), &pairs);
    let gap = if arrows.iter().any(|a| a.2.is_some()) {
        GAP_WITH_LABELS
    } else {
        GAP
    };
    let top = |row: usize| row as f64 * (BOX + gap);
    let height = layout.rows.len() as f64 * BOX + layout.rows.len().saturating_sub(1) as f64 * gap;

    // Each arrow: its line, where its head goes (and which way it points), and where its
    // words go.
    struct Drawn {
        path: String,
        dashed: bool,
        head: (f64, f64, bool),
        label: Option<(f64, f64, String)>,
    }
    let drawn: Vec<Drawn> = layout
        .routes
        .iter()
        .filter(|r| r.points.len() >= 2)
        .map(|r| {
            let aside = if r.up { ASIDE } else { 0.0 };
            let pts: Vec<(f64, f64, f64)> = r
                .points
                .iter()
                .map(|(row, place)| {
                    (
                        layout.x(*row, *place) * 100.0 + aside,
                        top(*row),
                        top(*row) + BOX,
                    )
                })
                .collect();
            let last = pts.len() - 1;
            // The line leaves the first box's bottom and reaches the last one's top, short
            // of whichever end has the head.
            let start_y = pts[0].2 + if r.up { HEAD } else { 0.0 };
            let end_y = pts[last].1 - if r.up { 0.0 } else { HEAD };
            let mut path = format!("M{:.2} {:.1}", pts[0].0, start_y);
            let mut from = (pts[0].0, start_y);
            for (i, p) in pts.iter().enumerate().skip(1) {
                let to_y = if i == last { end_y } else { p.1 };
                let bend = (to_y - from.1) / 2.0;
                path.push_str(&format!(
                    " C{:.2} {:.1} {:.2} {:.1} {:.2} {:.1}",
                    from.0,
                    from.1 + bend,
                    p.0,
                    to_y - bend,
                    p.0,
                    to_y
                ));
                if i < last {
                    // Straight down past the boxes of a row it only passes.
                    path.push_str(&format!(" L{:.2} {:.1}", p.0, p.2));
                    from = (p.0, p.2);
                }
            }
            let head = if r.up {
                (pts[0].0, pts[0].2, true)
            } else {
                (pts[last].0, pts[last].1, false)
            };
            // The words sit on the stretch next to the box the arrow leaves.
            let (a, b) = if r.up {
                (pts[last - 1], pts[last])
            } else {
                (pts[0], pts[1])
            };
            let label = arrows[r.edge]
                .2
                .clone()
                .map(|text| ((a.0 + b.0) / 2.0, (a.2 + b.1) / 2.0, text));
            Drawn {
                path,
                dashed: arrows[r.edge].3,
                head,
                label,
            }
        })
        .collect();
    let cols = layout.width().max(1);
    let shown = picked().and_then(|i| boxes.get(i));
    rsx! {
        figure { class: "oui-diagram", style: "--cols: {cols}",
            div { class: "oui-dg-canvas", style: "height: {height}px",
                svg { class: "oui-dg-edges", view_box: "0 0 100 {height}", preserve_aspect_ratio: "none", "aria-hidden": "true",
                    for d in drawn.iter() {
                        path { class: if d.dashed { "dashed" } else { "" }, d: "{d.path}" }
                    }
                }
                for (r, row) in layout.rows.iter().enumerate() {
                    div { class: "oui-dg-row", style: "top: {top(r)}px",
                        for slot in row.iter() {
                            match slot {
                                Slot::Node(i) => {
                                    let i = *i;
                                    let b = &boxes[i];
                                    rsx! {
                                        button {
                                            class: if picked() == Some(i) { "oui-dg-node on t-{b.tone}" } else { "oui-dg-node t-{b.tone}" },
                                            onclick: move |_| picked.set(if picked() == Some(i) { None } else { Some(i) }),
                                            span { class: "oui-dg-label", {rich(&b.label)} }
                                            if let Some(d) = &b.detail { span { class: "oui-dg-detail", {rich(d)} } }
                                        }
                                    }
                                }
                                Slot::Pass => rsx! { span { class: "oui-dg-pass" } },
                            }
                        }
                    }
                }
                for d in drawn.iter() {
                    span { class: if d.head.2 { "oui-dg-head up" } else { "oui-dg-head" }, style: "left: {d.head.0}%; top: {d.head.1}px" }
                }
                for d in drawn.iter() {
                    if let Some((x, y, text)) = &d.label {
                        span { class: "oui-dg-edge-label", style: "left: {x}%; top: {y}px", "{text}" }
                    }
                }
            }
            if let Some(b) = shown {
                figcaption { class: "oui-dg-readout", "aria-live": "polite",
                    strong { {rich(&b.label)} }
                    if let Some(d) = &b.detail { span { {rich(d)} } }
                }
            }
        }
    }
}

/// The index of the actor a message names; one that isn't listed joins the others.
fn actor(actors: &mut Vec<String>, name: &str) -> usize {
    let name = name.trim();
    match actors.iter().position(|a| a.eq_ignore_ascii_case(name)) {
        Some(i) => i,
        None => {
            actors.push(name.to_string());
            actors.len() - 1
        }
    }
}

#[component]
pub(crate) fn Sequence(node: Node) -> Element {
    let mut actors: Vec<String> = node
        .arg("actors")
        .items()
        .iter()
        .map(|a| a.text().trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();
    // (from, to, the message)
    let messages: Vec<(usize, usize, Node)> = nodes(node.arg("messages"), "Message")
        .into_iter()
        .map(|m| {
            let from = actor(&mut actors, &m.str("from"));
            let to = match m.opt("to") {
                Some(to) => actor(&mut actors, &to),
                None => from,
            };
            (from, to, m)
        })
        .collect();
    if actors.is_empty() {
        return rsx! {};
    }
    let n = actors.len() as f64;
    // Where an actor's line is, in hundredths of the width.
    let lane = |i: usize| (i as f64 + 0.5) / n * 100.0;
    rsx! {
        figure { class: "oui-seq", style: "--n: {actors.len()}",
            div { class: "oui-seq-actors",
                for a in actors.iter() { div { class: "oui-seq-actor", span { {rich(a)} } } }
            }
            div { class: "oui-seq-body",
                div { class: "oui-seq-lanes", "aria-hidden": "true",
                    for _ in actors.iter() { span {} }
                }
                for (i, (from, to, m)) in messages.iter().enumerate() {
                    {
                        // A tone that's a style is a slip worth reading as it was meant.
                        let dashed = m.str("style") == "dashed" || m.str("tone") == "dashed";
                        let tone = m.opt("tone").filter(|t| t != "dashed").unwrap_or_else(|| "neutral".into());
                        let (left, right) = (lane(*from.min(to)), lane(*from.max(to)));
                        // Words start at the arrow's left end, or end at its right one,
                        // whichever leaves them the most room.
                        let text_style = if (left + right) / 2.0 <= 50.0 {
                            format!("margin-left: {left}%; text-align: left")
                        } else {
                            format!("margin-right: {}%; text-align: right", 100.0 - right)
                        };
                        let text = rsx! {
                            div { class: "oui-seq-text", style: "{text_style}",
                                div { class: "oui-seq-label",
                                    span { class: "oui-seq-n", "{i + 1}" }
                                    span { {rich(&m.str("label"))} }
                                }
                                if let Some(d) = m.opt("detail") { div { class: "oui-seq-detail", {rich(&d)} } }
                            }
                        };
                        if from == to {
                            // What an actor does on its own: a note on its line, which
                            // runs through the note's near edge.
                            let note_style = if left <= 50.0 {
                                format!("margin-left: calc({left}% - 8px)")
                            } else {
                                format!("margin-left: auto; margin-right: calc({}% - 8px)", 100.0 - left)
                            };
                            rsx! {
                                div { class: "oui-seq-msg note t-{tone}",
                                    div { class: "oui-seq-text", style: "{note_style}",
                                        div { class: "oui-seq-label",
                                            span { class: "oui-seq-n", "{i + 1}" }
                                            span { {rich(&m.str("label"))} }
                                        }
                                        if let Some(d) = m.opt("detail") { div { class: "oui-seq-detail", {rich(&d)} } }
                                    }
                                }
                            }
                        } else {
                            rsx! {
                                div { class: "oui-seq-msg t-{tone}",
                                    {text}
                                    div {
                                        class: if from < to { "oui-seq-arrow right" } else { "oui-seq-arrow left" },
                                        class: if dashed { "dashed" },
                                        style: "margin-left: {left}%; width: {right - left}%",
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
