//! Artifacts, drawn: an OpenUI Lang document (see the `openui` crate) rendered with Dioxus
//! as part of the page, not a page of its own in a frame. The web UI (`web/`) draws them in
//! its thread; the viewer (`src/main.rs`) draws one on its own, in a tab or in the iOS app. Each component of tamale's
//! library (`openui::library`) is drawn here from its arguments; the `$state` a document
//! declares lives in this component, and changing it evaluates the document again.
//! A document that's still streaming in is drawn as far as it has come.

use dioxus::prelude::*;
use openui::code;
use openui::text::{self as t, DiffKind, Span};
use openui::{Act, Eval, Node, State, Value};

mod charts;
mod diagram;
mod lesson;
pub use lesson::Exchange;

/// What the renderer asks of the page, through JavaScript so it's the same in a browser and
/// in a desktop window.
mod page {
    use dioxus::document::eval;

    pub async fn copy_text(text: &str) -> bool {
        let text = serde_json::Value::from(text);
        eval(&format!(
            "try {{ await navigator.clipboard.writeText({text}); return true; }} catch (e) {{ return false; }}"
        ))
        .join::<bool>()
        .await
        .unwrap_or(false)
    }

    pub async fn sleep(ms: u32) {
        let _ = eval(&format!(
            "await new Promise((r) => setTimeout(r, {ms})); return true;"
        ))
        .join::<bool>()
        .await;
    }

    /// Opens an http(s) or mailto link in a new tab, without a handle on this page.
    pub fn open_tab(url: &str) {
        let url = serde_json::Value::from(url);
        let _ = eval(&format!(
            "window.open({url}, '_blank', 'noopener,noreferrer');"
        ));
    }
}

/// What a drawn artifact shares with its parts.
#[derive(Clone, Copy)]
struct Ctx {
    /// The values the reader set; each declared variable's first value is used until then.
    set: Signal<State>,
    /// Every variable's current value.
    state: Memo<State>,
    /// Sends a message to the agent as the user's (none in a tab of its own).
    send: Option<Callback<String>>,
    /// The artifact's title (see `openui::title`), which a lesson's messages name it by.
    title: Memo<String>,
    /// For a lesson: what the reader sent from it and the agent's replies.
    talk: Memo<Vec<Exchange>>,
    /// Whether the page opens files: then a file's name is a link to it (see [`path_link`]).
    paths: bool,
}

/// The form a part is in, for the buttons that send its values.
#[derive(Clone, PartialEq, Debug, Default)]
struct FormInfo {
    name: String,
    /// Each field's variable, label and rules.
    fields: Vec<(String, String, Value)>,
}

/// Whether the reader tried to send the form, so its fields show what's wrong.
#[derive(Clone, Copy)]
struct Tried(Signal<bool>);

/// An artifact. `send` is how its actions talk to the agent; `talk`, for a lesson, is what
/// the reader sent from it and the replies (see `openui::lesson`). With `paths`, the files
/// it names are marked as links (`.pathlink`, with the path in `data-path`) for the page to
/// open: tamale's web UI shows them in its code view.
#[component]
pub fn OpenUi(
    source: String,
    done: bool,
    send: Option<Callback<String>>,
    #[props(default)] talk: Vec<Exchange>,
    #[props(default)] paths: bool,
) -> Element {
    let program = use_memo(use_reactive!(|source| openui::parse(&source)));
    let title = use_memo(use_reactive!(
        |source| openui::title(&source).unwrap_or_default()
    ));
    let talk = use_memo(use_reactive!(|talk| talk));
    let set = use_signal(State::new);
    let state = use_memo(move || {
        let mut st = set();
        openui::init_state(&program.read(), &mut st);
        st
    });
    use_context_provider(|| Ctx {
        set,
        state,
        send,
        title,
        talk,
        paths,
    });
    let program = program.read();
    let st = state.read();
    let mut eval = Eval::new(&program, &st);
    let root = eval.root();
    let mut problems = std::mem::take(&mut eval.problems);
    if let Some(root) = &root {
        openui::walk(root, &mut |n| {
            for p in openui::library::check(n) {
                if !problems.contains(&p) {
                    problems.push(p);
                }
            }
        });
    }
    let cx = Cx::default();
    rsx! {
        div { class: "oui",
            match root {
                Some(root) => view_node(&root, &cx),
                None if done => rsx! { div { class: "oui-empty", "This artifact has nothing to show." } },
                None => rsx! { div { class: "oui-empty", "Drawing…" } },
            }
            if done && !problems.is_empty() {
                details { class: "oui-problems",
                    summary { "Some parts couldn't be drawn" }
                    ul { for p in problems { li { "{p}" } } }
                }
            }
        }
    }
}

/// An artifact on its own, as the viewer shows it: its title, and the document, which
/// nothing here can send messages from.
#[component]
pub fn Standalone(title: String, source: String) -> Element {
    rsx! {
        div { class: "oui-page",
            header { class: "oui-page-bar", span { "{title}" } }
            OpenUi { source, done: true, send: None }
        }
    }
}

/// What the parts being drawn are inside of.
#[derive(Clone, PartialEq, Default)]
struct Cx {
    form: Option<FormInfo>,
}

fn view(v: &Value, cx: &Cx) -> Element {
    match v {
        Value::Node(n) => view_node(n, cx),
        Value::Array(items) => rsx! { {items.iter().map(|i| view(i, cx))} },
        Value::Null | Value::Action(_) | Value::Object(_) => rsx! {},
        // A bare string where a component goes is a paragraph.
        other => rsx! { p { class: "oui-text", {rich(&other.text())} } },
    }
}

fn view_node(n: &Node, cx: &Cx) -> Element {
    let node = n.clone();
    let cx2 = cx.clone();
    match n.name.as_str() {
        // ------------------------------------------------------------ layout
        "Stack" => {
            let dir = if n.str("direction") == "row" {
                "row"
            } else {
                "column"
            };
            let gap = n.opt("gap").unwrap_or_else(|| "m".into());
            let align = n.opt("align").unwrap_or_default();
            rsx! {
                div { class: "oui-stack {dir} gap-{gap} align-{align}",
                    {view(n.arg("children"), cx)}
                }
            }
        }
        "Grid" => {
            let min = match n.arg("minWidth").num() {
                w if w >= 80.0 => w.min(480.0),
                _ => 220.0,
            };
            // A row of figures lays itself out: two across on a phone, never one left over.
            let children = n.arg("children").items();
            let metrics = !children.is_empty()
                && children
                    .iter()
                    .all(|c| matches!(c, Value::Node(m) if m.name == "Metric"));
            let class = if metrics {
                "oui-grid metrics"
            } else {
                "oui-grid"
            };
            rsx! {
                div { class, style: "--min: {min}px",
                    {view(n.arg("children"), cx)}
                }
            }
        }
        "Card" => {
            let variant = n.opt("variant").unwrap_or_else(|| "default".into());
            rsx! { div { class: "oui-card v-{variant}", {view(n.arg("children"), cx)} } }
        }
        "CardHeader" => rsx! {
            div { class: "oui-header",
                h2 { {rich(&n.str("title"))} }
                if let Some(s) = n.opt("subtitle") { p { {rich(&s)} } }
            }
        },
        "InlineHeader" => rsx! {
            div { class: "oui-inline-header",
                h3 { {rich(&n.str("title"))} }
                if let Some(s) = n.opt("description") { p { {rich(&s)} } }
            }
        },
        "Separator" => rsx! { hr { class: "oui-sep" } },
        "Tabs" => rsx! { Tabs { node, cx: cx2 } },
        "Accordion" => rsx! {
            div { class: "oui-accordion",
                for item in nodes(n.arg("items"), "AccordionItem") {
                    AccordionItem { node: item, cx: cx.clone() }
                }
            }
        },
        "Steps" if n.str("mode") == "walk" => rsx! { Walk { node, cx: cx2 } },
        "Steps" => rsx! {
            ol { class: "oui-steps",
                for (i, item) in nodes(n.arg("items"), "StepsItem").into_iter().enumerate() {
                    {
                        let status = item.opt("status").unwrap_or_default();
                        rsx! {
                            li { class: "oui-step s-{status}",
                                span { class: "oui-step-n", if status == "done" { "✓" } else { "{i + 1}" } }
                                div { class: "oui-step-body",
                                    div { class: "oui-step-title",
                                        span { {rich(&item.str("title"))} }
                                        if !status.is_empty() { span { class: "oui-pill s-{status}", "{status}" } }
                                    }
                                    {content(item.arg("details"), cx)}
                                }
                            }
                        }
                    }
                }
            }
        },
        // ------------------------------------------------------------ content
        "TextContent" => {
            let size = n.opt("size").unwrap_or_else(|| "default".into());
            rsx! { p { class: "oui-text size-{size}", {rich(&n.str("text"))} } }
        }
        "Markdown" => markdown(&n.str("text")),
        "Callout" => {
            let variant = n.opt("variant").unwrap_or_else(|| "info".into());
            rsx! {
                div { class: "oui-callout v-{variant}", role: "note",
                    strong { {rich(&n.str("title"))} }
                    if let Some(d) = n.opt("description") { div { {rich(&d)} } }
                }
            }
        }
        "CodeBlock" => {
            let start = match n.arg("startLine").num() {
                s if s >= 1.0 => s as usize,
                _ => 1,
            };
            let code = n.str("code");
            let filename = n.opt("filename");
            let language = n.str("language");
            // The lines as shown: without the indentation they share, coloured by the
            // language, or by the file's when the language doesn't say.
            let shown = t::dedent(code.trim_matches('\n'));
            let lines = match code::highlight(&language, &shown) {
                plain if plain.iter().flatten().all(|(k, _)| *k == code::Kind::Plain) => {
                    match &filename {
                        Some(f) => code::highlight(&code::language_of(f), &shown),
                        None => plain,
                    }
                }
                coloured => coloured,
            };
            let marks = t::line_set(
                &n.arg("highlight")
                    .items()
                    .iter()
                    .map(Value::text)
                    .collect::<Vec<_>>(),
            );
            let marked = |no: usize| marks.iter().any(|(a, b)| *a <= no && no <= *b);
            let has_marks = (0..lines.len()).any(|i| marked(start + i));
            let width = (start + lines.len()).to_string().len().max(2);
            rsx! {
                figure { class: if has_marks { "oui-code has-marks" } else { "oui-code" }, style: "--ln: {width}ch",
                    match &filename {
                        Some(f) => rsx! { figcaption { {path_link(f, Some(start).filter(|s| *s > 1))} } },
                        None if !language.is_empty() => rsx! { figcaption { "{language}" } },
                        None => rsx! {},
                    }
                    CopyButton { text: code.clone() }
                    div { class: "oui-lines",
                        for (i, runs) in lines.into_iter().enumerate() {
                            {
                                let ind = indent_of(&runs);
                                rsx! {
                                    div { class: if marked(start + i) { "oui-line mark" } else { "oui-line" },
                                        span { class: "oui-ln", "{start + i}" }
                                        code { style: "--ind: {ind}", {code_runs(runs)} }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "Diff" => {
            let file = n.str("file");
            let rows = t::diff_rows(&n.str("patch"));
            let language = code::language_of(&file);
            let (adds, dels) = rows.iter().fold((0, 0), |(a, d), r| match r.kind {
                DiffKind::Add => (a + 1, d),
                DiffKind::Del => (a, d + 1),
                _ => (a, d),
            });
            // Lines are numbered only when the hunks said where they are.
            let numbered = rows.iter().any(|r| r.old.is_some() || r.new.is_some());
            let width = rows
                .iter()
                .filter_map(|r| r.old.max(r.new))
                .max()
                .map_or(2, |n| n.to_string().len().max(2));
            // Where the first change is, for the link to open the file at.
            let first = rows
                .iter()
                .find(|r| matches!(r.kind, DiffKind::Add | DiffKind::Del))
                .and_then(|r| r.new.or(r.old))
                .map(|l| l as usize);
            rsx! {
                figure { class: if numbered { "oui-diff" } else { "oui-diff nonum" }, style: "--ln: {width}ch",
                    figcaption {
                        span { class: "oui-diff-file", {path_link(&file, first)} }
                        if adds > 0 { span { class: "oui-add", "+{adds}" } }
                        if dels > 0 { span { class: "oui-del", "−{dels}" } }
                    }
                    if let Some(note) = n.opt("note") { div { class: "oui-diff-note", {rich(&note)} } }
                    div { class: "oui-lines",
                        for r in rows {
                            {
                                let kind = match r.kind {
                                    DiffKind::Add => "add",
                                    DiffKind::Del => "del",
                                    DiffKind::Hunk => "hunk",
                                    DiffKind::Meta => "meta",
                                    DiffKind::Gap => "gap",
                                    DiffKind::Context => "ctx",
                                };
                                let sign = match r.kind { DiffKind::Add => "+", DiffKind::Del => "−", _ => "" };
                                let old = r.old.map(|o| o.to_string()).unwrap_or_default();
                                let new = r.new.map(|o| o.to_string()).unwrap_or_default();
                                // The one number a narrow screen has room for.
                                let one = if new.is_empty() { old.clone() } else { new.clone() };
                                let runs = code::highlight_line(&language, &r.text);
                                let ind = indent_of(&runs);
                                match r.kind {
                                    DiffKind::Hunk => rsx! { div { class: "oui-line d-hunk", code { {hunk_label(&r.text)} } } },
                                    DiffKind::Gap => rsx! { div { class: "oui-line d-gap", "aria-label": "Lines left out", code { "⋯" } } },
                                    DiffKind::Meta => rsx! { div { class: "oui-line d-meta", code { "{r.text}" } } },
                                    _ => rsx! {
                                        div { class: "oui-line d-{kind}",
                                            span { class: "oui-ln two", "{old}" }
                                            span { class: "oui-ln two", "{new}" }
                                            span { class: "oui-ln one", "{one}" }
                                            span { class: "oui-sign", "{sign}" }
                                            code { style: "--ind: {ind}", {code_runs(runs)} }
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
        "Table" => table(n),
        "Metric" => {
            let tone = n.opt("tone").unwrap_or_else(|| "neutral".into());
            rsx! {
                div { class: "oui-metric",
                    div { class: "oui-metric-value", "{figure(n.arg(\"value\"))}" }
                    div { class: "oui-metric-label",
                        span { {rich(&n.str("label"))} }
                        if let Some(d) = n.opt("delta") { span { class: "oui-pill t-{tone}", "{d}" } }
                    }
                }
            }
        }
        "Progress" => {
            let max = match n.arg("max").num() {
                m if m > 0.0 => m,
                _ => 100.0,
            };
            let value = n.arg("value").num();
            let pct = (value / max * 100.0).clamp(0.0, 100.0);
            let tone = n.opt("tone").unwrap_or_else(|| "accent".into());
            let figure = format!(
                "{} / {}",
                openui::eval::num_text(value),
                openui::eval::num_text(max)
            );
            rsx! {
                div { class: "oui-progress",
                    div { class: "oui-progress-label",
                        span { {rich(&n.str("label"))} }
                        span { class: "oui-num", "{figure}" }
                    }
                    div { class: "oui-track", role: "progressbar", "aria-valuenow": "{value}", "aria-valuemax": "{max}",
                        div { class: "oui-fill t-{tone}", style: "width: {pct}%" }
                    }
                }
            }
        }
        "Tag" => tag(&n.str("text"), &n.opt("variant").unwrap_or_default()),
        "TagBlock" => rsx! {
            div { class: "oui-tags",
                for t in n.arg("tags").items().into_iter().filter(|t| !t.is_null()) {
                    match t {
                        Value::Node(tn) => view_node(&tn, cx),
                        other => tag(&other.text(), ""),
                    }
                }
            }
        },
        "ListBlock" => {
            let variant = match n.str("variant").as_str() {
                v @ ("number" | "plain" | "pros" | "cons" | "check") => v.to_string(),
                _ => "bullet".into(),
            };
            let items: Vec<Value> = n
                .arg("items")
                .items()
                .into_iter()
                .filter(|i| !i.is_null())
                .collect();
            let body = rsx! {
                for item in items {
                    match item {
                        Value::Node(li) if li.name == "ListItem" => rsx! { ListItem { node: li } },
                        other => rsx! { li { class: "oui-li", div { class: "oui-li-title", {rich(&other.text())} } } },
                    }
                }
            };
            if variant == "number" {
                rsx! { ol { class: "oui-list v-number", {body} } }
            } else {
                rsx! { ul { class: "oui-list v-{variant}", {body} } }
            }
        }
        "ListItem" => rsx! { ul { class: "oui-list v-plain", ListItem { node } } },
        "Tree" => rsx! {
            ul { class: "oui-tree", role: "tree",
                for item in nodes(n.arg("items"), "TreeItem") {
                    TreeItem { node: item }
                }
            }
        },
        "TreeItem" => rsx! { ul { class: "oui-tree", TreeItem { node } } },
        "Flow" => {
            let dir = if n.str("direction") == "column" {
                "column"
            } else {
                "row"
            };
            let steps = nodes(n.arg("nodes"), "FlowNode");
            // The arrows between the nodes, and the timeline's rail, are the stylesheet's.
            rsx! {
                div { class: "oui-flow {dir}",
                    for (i, f) in steps.into_iter().enumerate() {
                        div { class: "oui-flow-node t-{f.opt(\"tone\").unwrap_or_else(|| \"neutral\".into())}",
                            span { class: "oui-flow-n", "aria-hidden": "true", "{i + 1}" }
                            div { class: "oui-flow-label", {rich(&f.str("label"))} }
                            if let Some(d) = f.opt("detail") { div { class: "oui-flow-detail", {rich(&d)} } }
                        }
                    }
                }
            }
        }
        "Diagram" => rsx! { diagram::Diagram { node } },
        "Sequence" => rsx! { diagram::Sequence { node } },
        "FlowNode" => view_node(
            &Node {
                name: "Flow".into(),
                args: vec![Value::Array(vec![Value::Node(n.clone())])],
                binds: vec![],
            },
            cx,
        ),
        "Image" => {
            let src = n.str("src");
            let ok = src.starts_with("https://") || src.starts_with("data:image/");
            rsx! {
                if ok {
                    img { class: "oui-img", src: "{src}", alt: "{n.str(\"alt\")}", loading: "lazy", referrerpolicy: "no-referrer" }
                }
            }
        }
        // ------------------------------------------------------------ charts
        "BarChart" => rsx! { charts::BarChart { node } },
        "HorizontalBarChart" => rsx! { charts::HBarChart { node } },
        "LineChart" | "AreaChart" => rsx! { charts::LineChart { node } },
        "PieChart" => rsx! { charts::PieChart { node } },
        // ------------------------------------------------------------ interactive
        "Button" => rsx! { Button { node, cx: cx2 } },
        "Buttons" => {
            let dir = if n.str("direction") == "column" {
                "column"
            } else {
                "row"
            };
            rsx! { div { class: "oui-buttons {dir}", {view(n.arg("items"), cx)} } }
        }
        "FollowUpBlock" => rsx! {
            div { class: "oui-followups",
                for item in nodes(n.arg("items"), "FollowUpItem") {
                    FollowUp { text: item.str("text") }
                }
            }
        },
        "FollowUpItem" => rsx! { div { class: "oui-followups", FollowUp { text: n.str("text") } } },
        "Form" => rsx! { Form { node } },
        "FormControl" => {
            let control = n.arg("control").clone();
            rsx! {
                div { class: "oui-field",
                    span { class: "oui-field-label", {rich(&n.str("label"))} }
                    {view(&control, cx)}
                    if let Some(h) = n.opt("hint") { span { class: "oui-field-hint", {rich(&h)} } }
                    if let Value::Node(c) = &control { FieldError { node: c.clone() } }
                }
            }
        }
        "Input" | "TextArea" | "Select" | "Switch" | "Slider" | "SegmentedControl" => {
            rsx! { Control { node } }
        }
        // ------------------------------------------------------------ lessons
        "Lesson" => rsx! { lesson::Lesson { node } },
        // Parts of something else, drawn by it; and components tamale doesn't have.
        _ => rsx! {},
    }
}

/// The nodes of `list` that are `name`s.
fn nodes(list: &Value, name: &str) -> Vec<Node> {
    list.items()
        .into_iter()
        .filter_map(|v| match v {
            Value::Node(n) if n.name == name => Some(n),
            _ => None,
        })
        .collect()
}

/// Content that may be text, a component or several.
fn content(v: &Value, cx: &Cx) -> Element {
    match v {
        Value::Str(s) => rsx! { div { class: "oui-text", {rich(s)} } },
        other => view(other, cx),
    }
}

/// Text with inline markdown.
fn rich(s: &str) -> Element {
    let spans = t::inline(s);
    let paths = links();
    rsx! { {spans.into_iter().map(|s| span(s, paths))} }
}

/// Whether the page opens the files an artifact names.
fn links() -> bool {
    try_consume_context::<Ctx>().is_some_and(|c| c.paths)
}

fn span(s: Span, paths: bool) -> Element {
    let Span {
        text,
        bold,
        italic,
        code,
        link,
    } = s;
    let mut inner = if code {
        code_span(&text, paths)
    } else {
        rsx! { "{text}" }
    };
    if italic {
        inner = rsx! { em { {inner} } };
    }
    if bold {
        inner = rsx! { strong { {inner} } };
    }
    match link {
        Some(href) => {
            rsx! { a { href: "{href}", target: "_blank", rel: "noopener noreferrer", {inner} } }
        }
        None => inner,
    }
}

/// A name in code's type. One that names a file opens it, where the page can (`paths`).
fn code_span(text: &str, paths: bool) -> Element {
    match t::file_path(text).filter(|_| paths) {
        Some((path, _)) => rsx! {
            code { class: "pathlink", "data-path": "{text}", title: "Open {path} in the code view", "{text}" }
        },
        None => rsx! { code { "{text}" } },
    }
}

/// A file's name, as the head of its code or its diff: a link to the file (at `line`, when
/// it's known) where the page opens files.
fn path_link(file: &str, line: Option<usize>) -> Element {
    let known = t::file_path(file).filter(|_| links());
    match known {
        Some((path, at)) => {
            let to = match line.filter(|_| at.is_none()) {
                Some(l) => format!("{path}:{l}"),
                None => file.to_string(),
            };
            rsx! { span { class: "pathlink", "data-path": "{to}", title: "Open {path} in the code view", "{file}" } }
        }
        None => rsx! { "{file}" },
    }
}

/// How far a line of code is indented, in characters: the width of the whitespace it
/// starts with, which [`code_runs`] sets apart.
fn indent_of(runs: &[code::Run]) -> usize {
    match runs.first() {
        Some((code::Kind::Plain, text)) => t::indent_width(text),
        _ => 0,
    }
}

/// A line of code in its colours. Its indentation is a part of its own, which a narrow
/// artifact draws as half as much padding (see `.oui-ind`).
fn code_runs(mut runs: Vec<code::Run>) -> Element {
    let indent = match runs.first_mut() {
        Some((code::Kind::Plain, text)) => {
            let at = text.len() - text.trim_start_matches([' ', '\t']).len();
            let indent: String = text.drain(..at).collect();
            Some(indent).filter(|i| !i.is_empty())
        }
        _ => None,
    };
    rsx! {
        if let Some(indent) = indent { span { class: "oui-ind", "{indent}" } }
        for (kind, text) in runs {
            if kind == code::Kind::Plain { "{text}" } else { span { class: "t-{kind.class()}", "{text}" } }
        }
    }
}

/// A hunk's header as a reader wants it: where it is, without the counts.
/// `@@ -40,7 +40,9 @@ impl Link {` reads `impl Link {`, or `Line 40` when it names nothing.
fn hunk_label(header: &str) -> String {
    let rest = header.trim_start_matches('@').trim();
    let (range, context) = match rest.split_once("@@") {
        Some((range, context)) => (range.trim(), context.trim()),
        None => ("", rest),
    };
    if !context.is_empty() {
        return context.to_string();
    }
    let line = range
        .split_whitespace()
        .find_map(|p| p.strip_prefix('+'))
        .and_then(|p| p.split(',').next())
        .filter(|l| l.parse::<u32>().is_ok());
    match line {
        Some(l) => format!("Line {l}"),
        // A header that names its place in words: `@@ fn read @@`.
        None => range.to_string(),
    }
}

/// A figure as a metric or a table shows it: a number's digits grouped from 10,000 up.
fn figure(v: &Value) -> String {
    match v {
        Value::Num(n) => t::grouped(*n),
        other => other.text(),
    }
}

fn markdown(md: &str) -> Element {
    rsx! {
        div { class: "oui-md",
            for b in t::blocks(md) {
                match b {
                    t::Block::Heading(level, h) => match level {
                        1 | 2 => rsx! { h3 { {rich(&h)} } },
                        _ => rsx! { h4 { {rich(&h)} } },
                    },
                    t::Block::Para(p) => rsx! { p { {rich(&p)} } },
                    t::Block::Quote(q) => rsx! { blockquote { {rich(&q)} } },
                    t::Block::List { ordered: true, items } => rsx! { ol { for i in items { li { {rich(&i)} } } } },
                    t::Block::List { ordered: false, items } => rsx! { ul { for i in items { li { {rich(&i)} } } } },
                    t::Block::Code { lang, code } => rsx! {
                        pre { code {
                            for (i, runs) in code::highlight(&lang, &code).into_iter().enumerate() {
                                if i > 0 { "\n" }
                                {code_runs(runs)}
                            }
                        } }
                    },
                }
            }
        }
    }
}

fn tag(text: &str, variant: &str) -> Element {
    let variant = if variant.is_empty() {
        "neutral"
    } else {
        variant
    };
    rsx! { span { class: "oui-tag v-{variant}", "{text}" } }
}

fn table(n: &Node) -> Element {
    let cols = nodes(n.arg("columns"), "Col");
    let data: Vec<Vec<Value>> = cols.iter().map(|c| c.arg("data").items()).collect();
    let rows = data.iter().map(Vec::len).max().unwrap_or(0);
    let labels: Vec<String> = cols.iter().map(|c| c.str("label")).collect();
    let types: Vec<String> = cols.iter().map(|c| c.str("type")).collect();
    let paths = links();
    // Wide tables become cards on phones; narrow ones fit as they are.
    let stack = if cols.len() > 3 { "stack" } else { "" };
    rsx! {
        div { class: "oui-table-wrap",
            table { class: "oui-table {stack}",
                if let Some(c) = n.opt("caption") { caption { {rich(&c)} } }
                thead { tr { for (l, ty) in labels.iter().zip(&types) { th { class: "c-{ty}", {rich(l)} } } } }
                tbody {
                    for r in 0..rows {
                        tr {
                            for (c, col) in data.iter().enumerate() {
                                {
                                    let v = col.get(r).cloned().unwrap_or_default();
                                    let ty = types[c].clone();
                                    let label = labels[c].clone();
                                    rsx! {
                                        td { class: "c-{ty}", "data-label": "{label}",
                                            match (&v, ty.as_str()) {
                                                (Value::Null, _) => rsx! {},
                                                (Value::Node(cell), _) => view_node(cell, &Cx::default()),
                                                (_, "code") => code_span(&v.text(), paths),
                                                (_, "tag") => tag(&v.text(), ""),
                                                (_, "number") => rsx! { "{figure(&v)}" },
                                                _ => rich(&v.text()),
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
    }
}

// ---------------------------------------------------------------- parts with state

#[component]
fn Tabs(node: Node, cx: Cx) -> Element {
    let items = nodes(node.arg("items"), "TabItem");
    let mut at = use_signal(|| 0usize);
    let current = at().min(items.len().saturating_sub(1));
    let count = items.len();
    rsx! {
        div { class: "oui-tabs",
            div { class: "oui-tablist", role: "tablist",
                onkeydown: move |e: KeyboardEvent| match e.key() {
                    Key::ArrowRight => at.set((current + 1) % count.max(1)),
                    Key::ArrowLeft => at.set((current + count.max(1) - 1) % count.max(1)),
                    _ => {}
                },
                for (i, item) in items.iter().enumerate() {
                    button {
                        role: "tab",
                        class: if i == current { "oui-tab on" } else { "oui-tab" },
                        "aria-selected": "{i == current}",
                        tabindex: if i == current { "0" } else { "-1" },
                        onclick: move |_| at.set(i),
                        {rich(&item.str("trigger"))}
                    }
                }
            }
            if let Some(item) = items.get(current) {
                div { class: "oui-tabpanel", role: "tabpanel", {content(item.arg("content"), &cx)} }
            }
        }
    }
}

/// Steps one at a time (`Steps(items, "walk")`): the reader moves through them, so each
/// step's state is what they're looking at before they see the next.
#[component]
fn Walk(node: Node, cx: Cx) -> Element {
    let items = nodes(node.arg("items"), "StepsItem");
    let mut at = use_signal(|| 0usize);
    let count = items.len();
    let current = at().min(count.saturating_sub(1));
    let Some(item) = items.get(current) else {
        return rsx! {};
    };
    rsx! {
        div { class: "oui-walk",
            onkeydown: move |e: KeyboardEvent| match e.key() {
                Key::ArrowRight if current + 1 < count => at.set(current + 1),
                Key::ArrowLeft if current > 0 => at.set(current - 1),
                _ => {}
            },
            div { class: "oui-walk-bar",
                for i in 0..count {
                    button {
                        class: if i == current { "on" } else if i < current { "seen" } else { "" },
                        "aria-label": "Step {i + 1}",
                        "aria-current": if i == current { "step" } else { "false" },
                        onclick: move |_| at.set(i),
                    }
                }
            }
            div { key: "{current}", class: "oui-walk-step",
                div { class: "oui-walk-title",
                    span { class: "oui-step-n", "{current + 1}" }
                    span { {rich(&item.str("title"))} }
                    span { class: "oui-walk-of", "of {count}" }
                }
                {content(item.arg("details"), &cx)}
            }
            div { class: "oui-walk-nav",
                button { class: "oui-btn v-secondary", disabled: current == 0, onclick: move |_| at.set(current.saturating_sub(1)), "← Back" }
                button { class: "oui-btn v-primary", disabled: current + 1 >= count, onclick: move |_| at.set(current + 1), "Next →" }
            }
        }
    }
}

#[component]
fn AccordionItem(node: Node, cx: Cx) -> Element {
    let mut open = use_signal(|| node.arg("open").truthy());
    rsx! {
        div { class: if open() { "oui-acc open" } else { "oui-acc" },
            button { class: "oui-acc-head", "aria-expanded": "{open()}", onclick: move |_| open.toggle(),
                span { {rich(&node.str("trigger"))} }
                Chevron {}
            }
            if open() { div { class: "oui-acc-body", {content(node.arg("content"), &cx)} } }
        }
    }
}

#[component]
fn Chevron() -> Element {
    rsx! { svg { class: "oui-chev", view_box: "0 0 24 24", "aria-hidden": "true", path { d: "M9 6l6 6-6 6" } } }
}

#[component]
fn TreeItem(node: Node) -> Element {
    let children = nodes(node.arg("children"), "TreeItem");
    let mut open = use_signal(|| node.arg("open").truthy());
    let has_children = !children.is_empty();
    let detail = node.arg("detail").clone();
    let has_more = has_children || !detail.is_null();
    rsx! {
        li { class: "oui-tree-item", role: "treeitem", "aria-expanded": if has_more { "{open()}" } else { "" },
            button {
                class: if has_more { "oui-tree-row more" } else { "oui-tree-row" },
                onclick: move |_| if has_more { open.toggle() },
                if has_more { span { class: if open() { "oui-tree-chev open" } else { "oui-tree-chev" }, Chevron {} } } else { span { class: "oui-tree-chev" } }
                span { class: "oui-tree-label", {rich(&node.str("label"))} }
            }
            if open() {
                if !detail.is_null() { div { class: "oui-tree-detail", {content(&detail, &Cx::default())} } }
                if has_children {
                    ul { role: "group", for c in children { TreeItem { node: c } } }
                }
            }
        }
    }
}

#[component]
fn ListItem(node: Node) -> Element {
    let ctx = use_context::<Ctx>();
    let action = node.arg("action").clone();
    let acts = actions(&action);
    let body = rsx! {
        div { class: "oui-li-main",
            div { class: "oui-li-title", {rich(&node.str("title"))} }
            if let Some(s) = node.opt("subtitle") { div { class: "oui-li-sub", {rich(&s)} } }
        }
        if let Some(m) = node.opt("meta") { div { class: "oui-li-meta", "{m}" } }
    };
    rsx! {
        li { class: "oui-li",
            if acts.is_empty() {
                div { class: "oui-li-row", {body} }
            } else {
                button { class: "oui-li-row tap", onclick: move |_| run(ctx, &acts, None, None), {body} }
            }
        }
    }
}

#[component]
fn CopyButton(text: String) -> Element {
    let mut copied = use_signal(|| false);
    rsx! {
        button {
            class: if copied() { "oui-copy done" } else { "oui-copy" },
            onclick: move |_| {
                let text = text.clone();
                spawn(async move {
                    if page::copy_text(&text).await {
                        copied.set(true);
                        page::sleep(1400).await;
                        copied.set(false);
                    }
                });
            },
            if copied() { "Copied" } else { "Copy" }
        }
    }
}

// ---------------------------------------------------------------- actions

/// The steps of an action argument. A string is a message to the agent.
fn actions(v: &Value) -> Vec<Act> {
    match v {
        Value::Action(steps) => steps.clone(),
        Value::Str(s) if !s.trim().is_empty() => vec![Act::ToAssistant {
            message: s.clone(),
            context: Value::Null,
        }],
        _ => Vec::new(),
    }
}

/// Does what an action says. A message to the agent from a form's button carries the
/// form's values, and isn't sent while a field is wrong.
fn run(mut ctx: Ctx, acts: &[Act], form: Option<&FormInfo>, tried: Option<Tried>) {
    let sends = acts.iter().any(|a| matches!(a, Act::ToAssistant { .. }));
    let state = ctx.state.peek().clone();
    if sends && let Some(form) = form {
        let wrong = form.fields.iter().any(|(key, _, rules)| {
            t_validate(&state.get(key).cloned().unwrap_or_default(), rules).is_some()
        });
        if wrong {
            if let Some(Tried(mut tried)) = tried {
                tried.set(true);
            }
            return;
        }
    }
    for act in acts {
        match act {
            Act::Set(var, v) => {
                ctx.set.write().insert(var.clone(), v.clone());
            }
            Act::Reset(vars) => {
                let mut set = ctx.set.write();
                for v in vars {
                    set.remove(v);
                }
            }
            Act::OpenUrl(url) => {
                if t::safe_url(url) {
                    page::open_tab(url);
                }
            }
            Act::ToAssistant { message, context } => {
                let mut text = message.clone();
                let mut extra: Vec<(String, Value)> = Vec::new();
                if let Some(form) = form {
                    let values = form
                        .fields
                        .iter()
                        .map(|(key, _, _)| {
                            (key.clone(), state.get(key).cloned().unwrap_or_default())
                        })
                        .collect();
                    extra.push(("form".into(), Value::Str(form.name.clone())));
                    extra.push(("values".into(), Value::Object(values)));
                }
                if !context.is_null() {
                    extra.push(("context".into(), context.clone()));
                }
                if !extra.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&Value::Object(extra).json());
                }
                if let Some(send) = ctx.send {
                    send.call(text);
                }
            }
        }
    }
}

fn t_validate(v: &Value, rules: &Value) -> Option<String> {
    let text = v.text();
    let rule = |name: &str| rules.field(name);
    if rule("required").truthy() && (text.trim().is_empty() || matches!(v, Value::Bool(false))) {
        return Some("Required".into());
    }
    if text.is_empty() {
        return None;
    }
    if rule("email").truthy() {
        let ok = text.split_once('@').is_some_and(|(a, b)| {
            !a.is_empty()
                && !a.contains(char::is_whitespace)
                && b.contains('.')
                && !b.starts_with('.')
                && !b.ends_with('.')
        });
        if !ok {
            return Some("Enter an email address".into());
        }
    }
    if rule("url").truthy() && !t::safe_url(&text) {
        return Some("Enter a link".into());
    }
    let numeric = rule("numeric").truthy() || !rule("min").is_null() || !rule("max").is_null();
    if numeric {
        let Ok(n) = text.trim().parse::<f64>() else {
            return Some("Enter a number".into());
        };
        if !rule("min").is_null() && n < rule("min").num() {
            return Some(format!("At least {}", rule("min").text()));
        }
        if !rule("max").is_null() && n > rule("max").num() {
            return Some(format!("At most {}", rule("max").text()));
        }
    }
    let len = text.chars().count() as f64;
    if !rule("minLength").is_null() && len < rule("minLength").num() {
        return Some(format!("At least {} characters", rule("minLength").text()));
    }
    if !rule("maxLength").is_null() && len > rule("maxLength").num() {
        return Some(format!("At most {} characters", rule("maxLength").text()));
    }
    None
}

#[component]
fn Button(node: Node, cx: Cx) -> Element {
    let ctx = use_context::<Ctx>();
    let tried = try_use_context::<Tried>();
    let acts = actions(node.arg("action"));
    let variant = node.opt("variant").unwrap_or_else(|| "secondary".into());
    let dead = acts.is_empty()
        || (ctx.send.is_none() && acts.iter().all(|a| matches!(a, Act::ToAssistant { .. })));
    rsx! {
        button {
            class: "oui-btn v-{variant}",
            disabled: dead,
            onclick: move |_| run(ctx, &acts, cx.form.as_ref(), tried),
            {rich(&node.str("label"))}
        }
    }
}

#[component]
fn FollowUp(text: String) -> Element {
    let ctx = use_context::<Ctx>();
    let acts = vec![Act::ToAssistant {
        message: text.clone(),
        context: Value::Null,
    }];
    rsx! {
        button { class: "oui-followup", disabled: ctx.send.is_none(), onclick: move |_| run(ctx, &acts, None, None),
            span { {rich(&text)} }
            svg { view_box: "0 0 24 24", "aria-hidden": "true", path { d: "M5 12h14M13 6l6 6-6 6" } }
        }
    }
}

#[component]
fn Form(node: Node) -> Element {
    use_context_provider(|| Tried(Signal::new(false)));
    let mut fields = Vec::new();
    for control in nodes(node.arg("fields"), "FormControl") {
        if let Value::Node(c) = control.arg("control") {
            fields.push((var_of(c), control.str("label"), c.arg("rules").clone()));
        }
    }
    let cx = Cx {
        form: Some(FormInfo {
            name: node.str("name"),
            fields,
        }),
    };
    rsx! {
        form { class: "oui-form", onsubmit: move |e| e.prevent_default(),
            div { class: "oui-form-fields", {view(node.arg("fields"), &cx)} }
            {view(node.arg("buttons"), &cx)}
        }
    }
}

/// The variable a control reads and writes: its bound `$variable`, else its name.
fn var_of(n: &Node) -> String {
    n.bind("value")
        .map(str::to_string)
        .unwrap_or_else(|| n.str("name"))
}

#[component]
fn FieldError(node: Node) -> Element {
    let ctx = use_context::<Ctx>();
    let tried = try_use_context::<Tried>();
    let shown = tried.is_some_and(|Tried(t)| t());
    let wrong = shown
        .then(|| {
            let v = ctx
                .state
                .read()
                .get(&var_of(&node))
                .cloned()
                .unwrap_or_default();
            t_validate(&v, node.arg("rules"))
        })
        .flatten();
    rsx! { if let Some(w) = wrong { span { class: "oui-field-error", "{w}" } } }
}

#[component]
fn Control(node: Node) -> Element {
    let mut ctx = use_context::<Ctx>();
    let var = var_of(&node);
    let value = ctx.state.read().get(&var).cloned().unwrap_or_default();
    let mut write = {
        let var = var.clone();
        move |v: Value| {
            ctx.set.write().insert(var.clone(), v);
        }
    };
    let name = node.str("name");
    let placeholder = node.str("placeholder");
    match node.name.as_str() {
        "TextArea" => {
            let rows = match node.arg("rows").num() {
                r if r >= 1.0 => r as i64,
                _ => 4,
            };
            rsx! {
                textarea { class: "oui-input", name: "{name}", placeholder: "{placeholder}", rows: "{rows}",
                    value: "{value.text()}",
                    oninput: move |e: FormEvent| write(Value::Str(e.value())),
                }
            }
        }
        "Select" => {
            let items = nodes(node.arg("items"), "SelectItem");
            let current = value.text();
            rsx! {
                select { class: "oui-input", name: "{name}",
                    oninput: move |e: FormEvent| write(Value::Str(e.value())),
                    if current.is_empty() || !placeholder.is_empty() {
                        option { value: "", disabled: true, selected: current.is_empty(), "{placeholder}" }
                    }
                    for i in items {
                        option { value: "{i.str(\"value\")}", selected: i.str("value") == current, "{i.str(\"label\")}" }
                    }
                }
            }
        }
        "SegmentedControl" => {
            let items = nodes(node.arg("items"), "SelectItem");
            let current = value.text();
            rsx! {
                div { class: "oui-segmented", role: "radiogroup",
                    for i in items {
                        {
                            let v = i.str("value");
                            let on = v == current;
                            let mut write = write.clone();
                            rsx! {
                                button { role: "radio", "aria-checked": "{on}", class: if on { "on" } else { "" },
                                    onclick: move |_| write(Value::Str(v.clone())),
                                    {rich(&i.str("label"))}
                                }
                            }
                        }
                    }
                }
            }
        }
        "Switch" => {
            let on = value.truthy();
            rsx! {
                button { class: if on { "oui-switch on" } else { "oui-switch" }, role: "switch", "aria-checked": "{on}",
                    onclick: move |_| write(Value::Bool(!on)),
                    span { class: "oui-knob" }
                    span { {rich(&node.str("label"))} }
                }
            }
        }
        "Slider" => {
            let (min, max) = (node.arg("min").num(), node.arg("max").num());
            let step = match node.arg("step").num() {
                s if s > 0.0 => s,
                _ => 1.0,
            };
            let v = if value.is_null() { min } else { value.num() };
            let shown = openui::eval::num_text(v);
            rsx! {
                div { class: "oui-slider",
                    if let Some(l) = node.opt("label") { span { {rich(&l)} } }
                    input { r#type: "range", min: "{min}", max: "{max}", step: "{step}", value: "{v}",
                        oninput: move |e: FormEvent| write(Value::Num(e.value().parse().unwrap_or(min))),
                    }
                    span { class: "oui-num", "{shown}" }
                }
            }
        }
        _ => {
            let ty = match node.str("type").as_str() {
                ty @ ("number" | "email" | "search" | "url") => ty.to_string(),
                _ => "text".into(),
            };
            let number = ty == "number";
            rsx! {
                input { class: "oui-input", r#type: "{ty}", name: "{name}", placeholder: "{placeholder}",
                    value: "{value.text()}",
                    oninput: move |e: FormEvent| {
                        let s = e.value();
                        write(match s.trim().parse::<f64>() {
                            Ok(n) if number => Value::Num(n),
                            _ => Value::Str(s),
                        });
                    },
                }
            }
        }
    }
}
