//! OpenUI Lang (<https://www.openui.com/docs/openui-lang>), as tamale's artifacts use it.
//!
//! A reply's artifact is a fenced block tagged ```` ```openui ```` holding a program: one
//! `name = expression` statement a line, `root` the component that's drawn, the others
//! referenced from it in any order, so it can be drawn while it's still streaming in.
//! Components are tamale's own ([`library`]); the page draws them with Dioxus (the
//! `openui-dioxus` crate in `dioxus/`), so an artifact is data, never a script.
//!
//! What's here: tokens ([`lex`]), statements and expressions ([`parse`]), evaluation against
//! the `$state` ([`eval`]), the component library, and the text helpers the renderer lays
//! out with ([`text`]). Tool calls (`Query`, `Mutation`, `@Run`) aren't supported: an
//! artifact embeds its data.

pub mod eval;
pub mod lesson;
pub mod lex;
pub mod library;
pub mod parse;
pub mod text;

pub use eval::{Act, Eval, Node, State, Value, init_state};
pub use parse::{Expr, Program, parse};

/// The info string of the fenced block an artifact is written in.
pub const FENCE: &str = "openui";

/// A statement that names the artifact (`title = "…"`); otherwise its first `CardHeader`
/// does.
pub const TITLE_STATEMENT: &str = "title";

/// The artifact's title: its `title` statement, else the title of the first `CardHeader`
/// drawn.
pub fn title(source: &str) -> Option<String> {
    let program = parse(source);
    let state = initial_state(&program);
    let mut eval = Eval::new(&program, &state);
    if let Value::Str(t) = eval.statement(TITLE_STATEMENT) {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let root = eval.root()?;
    let mut found = None;
    walk(&root, &mut |n| {
        if found.is_none() && n.name == "CardHeader" {
            found = Some(n.str("title")).filter(|t| !t.trim().is_empty());
        }
    });
    found.map(|t| t.trim().to_string())
}

/// The state a program starts with.
pub fn initial_state(program: &Program) -> State {
    let mut state = State::new();
    init_state(program, &mut state);
    state
}

/// Every node under `root` (itself included), depth first.
pub fn walk(root: &Node, f: &mut dyn FnMut(&Node)) {
    fn value(v: &Value, f: &mut dyn FnMut(&Node)) {
        match v {
            Value::Node(n) => walk(n, f),
            Value::Array(items) => items.iter().for_each(|i| value(i, f)),
            Value::Object(kv) => kv.iter().for_each(|(_, v)| value(v, f)),
            _ => {}
        }
    }
    f(root);
    for a in &root.args {
        value(a, f);
    }
}

/// The artifact's text, one piece a line, for reading it aloud or searching it: what's
/// drawn when it opens, every tab and section included.
pub fn plain_text(source: &str) -> String {
    let program = parse(source);
    let state = initial_state(&program);
    let Some(root) = Eval::new(&program, &state).root() else {
        return String::new();
    };
    let mut lines = Vec::new();
    fn strings(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Str(s) if !s.trim().is_empty() => out.push(s.trim().to_string()),
            Value::Num(n) => out.push(eval::num_text(*n)),
            Value::Array(items) => {
                // A table column or chart data reads as one line.
                if items
                    .iter()
                    .all(|i| matches!(i, Value::Str(_) | Value::Num(_)))
                {
                    let line = items.iter().map(Value::text).collect::<Vec<_>>().join(", ");
                    if !line.is_empty() {
                        out.push(line);
                    }
                } else {
                    items.iter().for_each(|i| strings(i, out));
                }
            }
            Value::Node(n) => n.args.iter().for_each(|a| strings(a, out)),
            _ => {}
        }
    }
    strings(&Value::Node(root), &mut lines);
    lines.join("\n")
}

/// What's wrong with a finished document: unknown components or functions, arguments that
/// don't fit, a missing root.
pub fn problems(source: &str) -> Vec<String> {
    let program = parse(source);
    let state = initial_state(&program);
    let mut eval = Eval::new(&program, &state);
    let Some(root) = eval.root() else {
        return vec!["no root component".into()];
    };
    let mut out = eval.problems;
    walk(&root, &mut |n| {
        for p in library::check(n) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"title = "Checkout latency"
root = Card([header, chart, tabs])
header = CardHeader("p95 down 38%", "after the cache fix")
chart = BarChart(["Before", "After"], [Series("p95 ms", [420, 260])])
tabs = Tabs([TabItem("a", "Why", TextContent("The **cache** was cold.")), TabItem("b", "Next", TextContent("Ship it"))])
"#;

    #[test]
    fn titles() {
        assert_eq!(title(DOC).as_deref(), Some("Checkout latency"));
        assert_eq!(
            title("root = Card([CardHeader(\"Plan\")])").as_deref(),
            Some("Plan")
        );
        assert_eq!(title("root = Card([])"), None);
    }

    #[test]
    fn text_for_reading() {
        let t = plain_text(DOC);
        assert!(t.contains("p95 down 38%"));
        assert!(t.contains("Before, After"));
        assert!(t.contains("Ship it"));
    }

    #[test]
    fn problems_are_named() {
        assert!(problems(DOC).is_empty(), "{:?}", problems(DOC));
        let p = problems("root = Card([Wat(1), CardHeader()])");
        assert!(p.contains(&"unknown component Wat".to_string()), "{p:?}");
        assert!(
            p.contains(&"CardHeader is missing title".to_string()),
            "{p:?}"
        );
    }
}
