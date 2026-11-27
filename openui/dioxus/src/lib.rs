//! Artifacts, drawn: an OpenUI Lang document (see the `openui` crate) rendered with Dioxus
//! as part of the page, not a page of its own in a frame. The web UI (`web/`) draws them in
//! its thread; the viewer (`src/main.rs`) draws one on its own, in a tab or in the iOS app. Each component of tamale's
//! library (`openui::library`) is drawn here from its arguments; the `$state` a document
//! declares lives in this component, and changing it evaluates the document again.
//! A document that's still streaming in is drawn as far as it has come.

use dioxus::prelude::*;
use openui::text::{self as t, DiffKind, Span};
use openui::{Act, Eval, Node, State, Value};

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
/// the reader sent from it and the replies (see `openui::lesson`).
#[component]
pub fn OpenUi(
    source: String,
    done: bool,
    send: Option<Callback<String>>,
    #[props(default)] talk: Vec<Exchange>,
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
            rsx! {
                div { class: "oui-grid", style: "--min: {min}px",
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
            let label = n.opt("filename").unwrap_or_else(|| n.str("language"));
            rsx! {
                figure { class: "oui-code",
                    if !label.is_empty() { figcaption { "{label}" } }
                    CopyButton { text: code.clone() }
                    div { class: "oui-lines",
                        for (i, line) in code.trim_end_matches('\n').split('\n').enumerate() {
                            div { class: "oui-line",
                                span { class: "oui-ln", "{start + i}" }
                                code { "{line}" }
                            }
                        }
                    }
                }
            }
        }
        "Diff" => {
            let rows = t::diff_rows(&n.str("patch"));
            let (adds, dels) = rows.iter().fold((0, 0), |(a, d), r| match r.kind {
                DiffKind::Add => (a + 1, d),
                DiffKind::Del => (a, d + 1),
                _ => (a, d),
            });
            rsx! {
                figure { class: "oui-diff",
                    figcaption {
                        span { class: "oui-diff-file", "{n.str(\"file\")}" }
                        span { class: "oui-add", "+{adds}" }
                        span { class: "oui-del", "−{dels}" }
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
                                    DiffKind::Context => "ctx",
                                };
                                let sign = match r.kind { DiffKind::Add => "+", DiffKind::Del => "−", _ => "" };
                                let old = r.old.map(|o| o.to_string()).unwrap_or_default();
                                let new = r.new.map(|o| o.to_string()).unwrap_or_default();
                                rsx! {
                                    div { class: "oui-line d-{kind}",
                                        span { class: "oui-ln", "{old}" }
                                        span { class: "oui-ln", "{new}" }
                                        span { class: "oui-sign", "{sign}" }
                                        code { "{r.text}" }
                                    }
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
                    div { class: "oui-metric-value", "{n.str(\"value\")}" }
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
                for t in n.arg("tags").items() {
                    match t {
                        Value::Node(tn) => view_node(&tn, cx),
                        other => tag(&other.text(), ""),
                    }
                }
            }
        },
        "ListBlock" => {
            let variant = n.opt("variant").unwrap_or_else(|| "bullet".into());
            let items = n.arg("items").items();
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
            let last = steps.len().saturating_sub(1);
            rsx! {
                div { class: "oui-flow {dir}",
                    for (i, f) in steps.into_iter().enumerate() {
                        div { class: "oui-flow-node t-{f.opt(\"tone\").unwrap_or_else(|| \"neutral\".into())}",
                            div { class: "oui-flow-label", {rich(&f.str("label"))} }
                            if let Some(d) = f.opt("detail") { div { class: "oui-flow-detail", {rich(&d)} } }
                        }
                        if i < last {
                            div { class: "oui-flow-arrow", "aria-hidden": "true",
                                svg { view_box: "0 0 24 24", path { d: "M5 12h14M13 6l6 6-6 6" } }
                            }
                        }
                    }
                }
            }
        }
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
        "BarChart" => rsx! { BarChart { node } },
        "HorizontalBarChart" => rsx! { HBarChart { node } },
        "LineChart" | "AreaChart" => rsx! { LineChart { node } },
        "PieChart" => rsx! { PieChart { node } },
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
    rsx! { {spans.into_iter().map(span)} }
}

fn span(s: Span) -> Element {
    let Span {
        text,
        bold,
        italic,
        code,
        link,
    } = s;
    let inner = if code {
        rsx! { code { "{text}" } }
    } else if bold {
        rsx! { strong { "{text}" } }
    } else if italic {
        rsx! { em { "{text}" } }
    } else {
        rsx! { "{text}" }
    };
    match link {
        Some(href) => {
            rsx! { a { href: "{href}", target: "_blank", rel: "noopener noreferrer", {inner} } }
        }
        None => inner,
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
                    t::Block::Code { code, .. } => rsx! { pre { code { "{code}" } } },
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
    // Wide tables become cards on phones; narrow ones fit as they are.
    let stack = if cols.len() > 3 { "stack" } else { "" };
    rsx! {
        div { class: "oui-table-wrap",
            table { class: "oui-table {stack}",
                if let Some(c) = n.opt("caption") { caption { {rich(&c)} } }
                thead { tr { for (l, ty) in labels.iter().zip(&types) { th { class: "c-{ty}", "{l}" } } } }
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
                                            match ty.as_str() {
                                                "code" => rsx! { code { "{v.text()}" } },
                                                "tag" => tag(&v.text(), ""),
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

// ---------------------------------------------------------------- charts

/// Labels and each series' name and values.
fn chart_data(node: &Node) -> (Vec<String>, Vec<(String, Vec<f64>)>) {
    let labels: Vec<String> = node.arg("labels").items().iter().map(Value::text).collect();
    let series = nodes(node.arg("series"), "Series")
        .iter()
        .map(|s| {
            (
                s.str("name"),
                s.arg("values").items().iter().map(Value::num).collect(),
            )
        })
        .collect();
    (labels, series)
}

fn legend(series: &[(String, Vec<f64>)]) -> Element {
    rsx! {
        if series.len() > 1 {
            div { class: "oui-legend",
                for (i, (name, _)) in series.iter().enumerate() {
                    span { span { class: "oui-swatch c{i % 6}" } "{name}" }
                }
            }
        }
    }
}

/// The line above a chart that says what was tapped.
fn readout(
    picked: Option<(usize, usize)>,
    labels: &[String],
    series: &[(String, Vec<f64>)],
) -> Element {
    let text = picked.and_then(|(l, s)| {
        let (name, values) = series.get(s)?;
        let v = values.get(l)?;
        let label = labels.get(l).cloned().unwrap_or_default();
        Some(if series.len() > 1 {
            format!("{label} · {name}: {}", openui::eval::num_text(*v))
        } else {
            format!("{label}: {}", openui::eval::num_text(*v))
        })
    });
    let text = text.unwrap_or_default();
    rsx! { div { class: "oui-readout", "aria-live": "polite", "{text}" } }
}

fn y_axis(ticks: &[f64], pct: impl Fn(f64) -> f64) -> Element {
    rsx! {
        for t in ticks {
            div { class: "oui-gridline", style: "bottom: {pct(*t)}%",
                span { "{t::short_num(*t)}" }
            }
        }
    }
}

#[component]
fn BarChart(node: Node) -> Element {
    let (labels, series) = chart_data(&node);
    let stacked = node.str("variant") == "stacked";
    let mut picked = use_signal(|| None::<(usize, usize)>);
    let n = labels
        .len()
        .max(series.iter().map(|s| s.1.len()).max().unwrap_or(0));
    let at = |s: &(String, Vec<f64>), i: usize| s.1.get(i).copied().unwrap_or(0.0);
    let (mut lo, mut hi) = (0f64, 0f64);
    for i in 0..n {
        if stacked {
            hi = hi.max(series.iter().map(|s| at(s, i).max(0.0)).sum());
            lo = lo.min(series.iter().map(|s| at(s, i).min(0.0)).sum());
        } else {
            for s in &series {
                hi = hi.max(at(s, i));
                lo = lo.min(at(s, i));
            }
        }
    }
    let (ticks, bottom, top) = t::ticks(lo, hi);
    let pct = move |v: f64| (v - bottom) / (top - bottom) * 100.0;
    let zero = pct(0.0);
    rsx! {
        figure { class: "oui-chart",
            {readout(picked(), &labels, &series)}
            div { class: "oui-plot",
                {y_axis(&ticks, pct)}
                div { class: "oui-bars",
                    for i in 0..n {
                        div { class: "oui-slot",
                            if stacked {
                                div { class: "oui-col",
                                    {
                                        let mut up = 0.0;
                                        let mut down = 0.0;
                                        let segs: Vec<(usize, f64, f64)> = series.iter().enumerate().map(|(s, data)| {
                                            let v = at(data, i);
                                            let (from, to) = if v >= 0.0 { let f = up; up += v; (f, up) } else { let f = down; down += v; (down, f) };
                                            (s, pct(from), pct(to) - pct(from))
                                        }).collect();
                                        rsx! {
                                            for (s, b, h) in segs {
                                                button { class: "oui-bar c{s % 6}", style: "bottom: {b}%; height: {h}%",
                                                    "aria-label": "{labels.get(i).cloned().unwrap_or_default()}: {at(&series[s], i)}",
                                                    onclick: move |_| picked.set(Some((i, s))),
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                for (s, data) in series.iter().enumerate() {
                                    {
                                        let v = at(data, i);
                                        let (b, h) = if v >= 0.0 { (zero, pct(v) - zero) } else { (pct(v), zero - pct(v)) };
                                        let on = picked() == Some((i, s));
                                        rsx! {
                                            div { class: "oui-col",
                                                button { class: if on { "oui-bar on c{s % 6}" } else { "oui-bar c{s % 6}" },
                                                    style: "bottom: {b}%; height: {h.max(0.4)}%",
                                                    "aria-label": "{labels.get(i).cloned().unwrap_or_default()}: {v}",
                                                    onclick: move |_| picked.set(Some((i, s))),
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
            div { class: "oui-xlabels", for l in labels.iter() { span { title: "{l}", "{l}" } } }
            {axis_titles(&node)}
            {legend(&series)}
        }
    }
}

fn axis_titles(node: &Node) -> Element {
    let (x, y) = (node.str("xLabel"), node.str("yLabel"));
    rsx! {
        if !x.is_empty() || !y.is_empty() {
            div { class: "oui-axis-titles",
                span { "{y}" }
                span { "{x}" }
            }
        }
    }
}

#[component]
fn HBarChart(node: Node) -> Element {
    let (labels, series) = chart_data(&node);
    let stacked = node.str("variant") == "stacked";
    let at = |s: &(String, Vec<f64>), i: usize| s.1.get(i).copied().unwrap_or(0.0).max(0.0);
    let n = labels.len();
    let top = (0..n)
        .map(|i| {
            if stacked {
                series.iter().map(|s| at(s, i)).sum::<f64>()
            } else {
                series.iter().map(|s| at(s, i)).fold(0.0, f64::max)
            }
        })
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    rsx! {
        figure { class: "oui-chart",
            div { class: "oui-hbars",
                for i in 0..n {
                    div { class: "oui-hrow",
                        div { class: "oui-hlabel", title: "{labels[i]}", "{labels[i]}" }
                        div { class: if stacked { "oui-htrack stacked" } else { "oui-htrack" },
                            for (s, data) in series.iter().enumerate() {
                                {
                                    let v = at(data, i);
                                    let w = v / top * 100.0;
                                    rsx! {
                                        div { class: "oui-hbar-line",
                                            div { class: "oui-hbar c{s % 6}", style: "width: {w}%" }
                                            if !stacked { span { class: "oui-num", "{t::short_num(v)}" } }
                                        }
                                    }
                                }
                            }
                        }
                        if stacked {
                            span { class: "oui-num", "{t::short_num(series.iter().map(|s| at(s, i)).sum())}" }
                        }
                    }
                }
            }
            {legend(&series)}
        }
    }
}

#[component]
fn LineChart(node: Node) -> Element {
    let (labels, series) = chart_data(&node);
    let area = node.name == "AreaChart";
    let mut picked = use_signal(|| None::<(usize, usize)>);
    let n = labels
        .len()
        .max(series.iter().map(|s| s.1.len()).max().unwrap_or(0));
    let all = series.iter().flat_map(|s| s.1.iter().copied());
    let (lo, hi) = all.fold((0f64, 0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let (ticks, bottom, top) = t::ticks(lo, hi);
    let y = move |v: f64| (v - bottom) / (top - bottom) * 100.0;
    let x = move |i: usize| {
        if n <= 1 {
            50.0
        } else {
            i as f64 / (n - 1) as f64 * 100.0
        }
    };
    rsx! {
        figure { class: "oui-chart",
            {readout(picked(), &labels, &series)}
            div { class: "oui-plot",
                {y_axis(&ticks, y)}
                svg { class: "oui-lines-svg", view_box: "0 0 100 100", preserve_aspect_ratio: "none", "aria-hidden": "true",
                    for (s, (_, values)) in series.iter().enumerate() {
                        {
                            let points: Vec<String> = values.iter().enumerate().map(|(i, v)| format!("{:.3},{:.3}", x(i), 100.0 - y(*v))).collect();
                            let line = points.join(" ");
                            let fill = format!("{:.3},{:.3} {line} {:.3},{:.3}", x(0), 100.0 - y(bottom.max(0.0).min(top)), x(values.len().saturating_sub(1)), 100.0 - y(bottom.max(0.0).min(top)));
                            rsx! {
                                if area { polygon { class: "oui-area c{s % 6}", points: "{fill}" } }
                                polyline { class: "oui-line-path c{s % 6}", points: "{line}" }
                            }
                        }
                    }
                }
                for (s, (_, values)) in series.iter().enumerate() {
                    for (i, v) in values.iter().enumerate() {
                        {
                            let on = picked() == Some((i, s));
                            rsx! {
                                button { class: if on { "oui-dot on c{s % 6}" } else { "oui-dot c{s % 6}" },
                                    style: "left: {x(i)}%; bottom: {y(*v)}%",
                                    "aria-label": "{labels.get(i).cloned().unwrap_or_default()}: {v}",
                                    onclick: move |_| picked.set(Some((i, s))),
                                }
                            }
                        }
                    }
                }
            }
            div { class: "oui-xlabels spread", for l in labels.iter() { span { title: "{l}", "{l}" } } }
            {axis_titles(&node)}
            {legend(&series)}
        }
    }
}

#[component]
fn PieChart(node: Node) -> Element {
    let labels: Vec<String> = node.arg("labels").items().iter().map(Value::text).collect();
    let values: Vec<f64> = node
        .arg("values")
        .items()
        .iter()
        .map(|v| v.num().max(0.0))
        .collect();
    let donut = node.str("variant") == "donut";
    let total: f64 = values.iter().sum();
    let mut picked = use_signal(|| None::<usize>);
    let mut a0 = -std::f64::consts::FRAC_PI_2;
    let slices: Vec<(usize, String)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let sweep = if total > 0.0 {
                v / total * std::f64::consts::TAU
            } else {
                0.0
            };
            let path = arc(a0, a0 + sweep, if donut { 0.6 } else { 0.0 });
            a0 += sweep;
            (i, path)
        })
        .collect();
    rsx! {
        figure { class: "oui-chart oui-pie",
            svg { view_box: "-1.05 -1.05 2.1 2.1", class: "oui-pie-svg", role: "img",
                for (i, d) in slices {
                    path { class: if picked() == Some(i) { "on c{i % 6}" } else { "c{i % 6}" }, d: "{d}",
                        onclick: move |_| picked.set(Some(i)),
                    }
                }
            }
            div { class: "oui-pie-legend",
                for (i, l) in labels.iter().enumerate() {
                    {
                        let v = values.get(i).copied().unwrap_or(0.0);
                        let share = if total > 0.0 { v / total * 100.0 } else { 0.0 };
                        rsx! {
                            button { class: if picked() == Some(i) { "on" } else { "" }, onclick: move |_| picked.set(Some(i)),
                                span { class: "oui-swatch c{i % 6}" }
                                span { class: "oui-pie-label", "{l}" }
                                span { class: "oui-num", "{t::short_num(v)} · {share:.0}%" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// An SVG path for the slice from angle `a0` to `a1` of a unit circle, a ring when `inner`
/// is above 0.
fn arc(a0: f64, a1: f64, inner: f64) -> String {
    // A whole circle can't be one arc.
    let a1 = if a1 - a0 >= std::f64::consts::TAU {
        a0 + std::f64::consts::TAU - 1e-4
    } else {
        a1
    };
    let large = if a1 - a0 > std::f64::consts::PI { 1 } else { 0 };
    let (x0, y0, x1, y1) = (a0.cos(), a0.sin(), a1.cos(), a1.sin());
    if inner <= 0.0 {
        format!("M0 0 L{x0:.4} {y0:.4} A1 1 0 {large} 1 {x1:.4} {y1:.4} Z")
    } else {
        let r = inner;
        format!(
            "M{:.4} {:.4} L{x0:.4} {y0:.4} A1 1 0 {large} 1 {x1:.4} {y1:.4} L{:.4} {:.4} A{r} {r} 0 {large} 0 {:.4} {:.4} Z",
            x0 * r,
            y0 * r,
            x1 * r,
            y1 * r,
            x0 * r,
            y0 * r
        )
    }
}
