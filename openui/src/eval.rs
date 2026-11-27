//! Evaluation: a program and the current state give the tree to draw. The page evaluates
//! again whenever the state changes, so what's drawn always reads the current values.
//! Values follow JavaScript's rules, as the spec asks (truthiness, `+` on strings, loose
//! equality); division by zero gives 0.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::parse::{Expr, Program};

/// The `$variables`, by name without the `$`.
pub type State = BTreeMap<String, Value>;

#[derive(Clone, PartialEq, Debug, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
    /// A component to draw.
    Node(Node),
    /// What a button or link does when used.
    Action(Vec<Act>),
}

/// A component with its arguments, in the order its signature gives them.
#[derive(Clone, PartialEq, Debug)]
pub struct Node {
    pub name: String,
    pub args: Vec<Value>,
    /// For each argument written as a bare `$variable`, its name: inputs write to it.
    pub binds: Vec<Option<String>>,
}

impl Node {
    /// The argument called `param` in the component's signature (see [`crate::library`]).
    pub fn arg(&self, param: &str) -> &Value {
        static NULL: Value = Value::Null;
        crate::library::param_index(&self.name, param)
            .and_then(|i| self.args.get(i))
            .unwrap_or(&NULL)
    }

    /// The `$variable` the argument called `param` was written as, if it was one.
    pub fn bind(&self, param: &str) -> Option<&str> {
        let i = crate::library::param_index(&self.name, param)?;
        self.binds.get(i)?.as_deref()
    }

    pub fn str(&self, param: &str) -> String {
        self.arg(param).text()
    }

    /// The argument as text, if it was given (and isn't empty).
    pub fn opt(&self, param: &str) -> Option<String> {
        Some(self.arg(param).text()).filter(|s| !s.is_empty())
    }
}

/// One step of an action.
#[derive(Clone, PartialEq, Debug)]
pub enum Act {
    /// Sends a message to the agent, as if the user had typed it.
    ToAssistant {
        message: String,
        context: Value,
    },
    OpenUrl(String),
    /// Sets a state variable.
    Set(String, Value),
    /// Puts state variables back to their first values.
    Reset(Vec<String>),
}

impl Value {
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }

    pub fn num(&self) -> f64 {
        match self {
            Value::Num(n) => *n,
            Value::Bool(b) => *b as u8 as f64,
            Value::Str(s) => {
                let s = s.trim();
                if s.is_empty() {
                    0.0
                } else {
                    s.parse().unwrap_or(0.0)
                }
            }
            _ => 0.0,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The value as text: what a label shows.
    pub fn text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => num_text(*n),
            Value::Str(s) => s.clone(),
            Value::Array(items) => items.iter().map(Value::text).collect::<Vec<_>>().join(","),
            Value::Object(_) | Value::Node(_) | Value::Action(_) => String::new(),
        }
    }

    /// The items of an array; a single value is one item, null none.
    pub fn items(&self) -> Vec<Value> {
        match self {
            Value::Array(items) => items.clone(),
            Value::Null => Vec::new(),
            v => vec![v.clone()],
        }
    }

    pub fn field(&self, name: &str) -> Value {
        match self {
            Value::Object(kv) => kv
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default(),
            // On an array, a field plucks it from each item.
            Value::Array(items) => match name {
                "length" => Value::Num(items.len() as f64),
                _ => Value::Array(items.iter().map(|i| i.field(name)).collect()),
            },
            Value::Str(s) if name == "length" => Value::Num(s.chars().count() as f64),
            Value::Node(n) => n.arg(name).clone(),
            _ => Value::Null,
        }
    }

    /// The value at a dotted path, as `@Filter` and `@Sort` read their fields.
    pub fn path(&self, path: &str) -> Value {
        if path.is_empty() {
            return self.clone();
        }
        path.split('.').fold(self.clone(), |v, f| v.field(f))
    }

    /// As JSON, for what an action sends the agent.
    pub fn json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null | Value::Node(_) | Value::Action(_) => out.push_str("null"),
            Value::Bool(b) => out.push_str(&b.to_string()),
            Value::Num(n) if n.is_finite() => out.push_str(&num_text(*n)),
            Value::Num(_) => out.push_str("null"),
            Value::Str(s) => json_str(out, s),
            Value::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_json(out);
                }
                out.push(']');
            }
            Value::Object(kv) => {
                out.push('{');
                for (i, (k, v)) in kv.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    json_str(out, k);
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A number as JavaScript writes it: no `.0` on whole numbers.
pub fn num_text(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// JavaScript's loose equality, for the values a document has.
fn loose_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Null, Null) => true,
        (Null, _) | (_, Null) => false,
        (Str(x), Str(y)) => x == y,
        (Num(_) | Bool(_) | Str(_), Num(_) | Bool(_) | Str(_)) => a.num() == b.num(),
        _ => a == b,
    }
}

fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        _ => a
            .num()
            .partial_cmp(&b.num())
            .unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// The value of each statement, worked out once per evaluation.
pub struct Eval<'a> {
    program: &'a Program,
    state: &'a State,
    done: HashMap<String, Value>,
    busy: HashSet<String>,
    locals: Vec<(String, Value)>,
    /// What went wrong, for the page to show once the document has finished.
    pub problems: Vec<String>,
}

impl<'a> Eval<'a> {
    pub fn new(program: &'a Program, state: &'a State) -> Self {
        Eval {
            program,
            state,
            done: HashMap::new(),
            busy: HashSet::new(),
            locals: Vec::new(),
            problems: Vec::new(),
        }
    }

    /// The document's root, if it has one that's a component.
    pub fn root(&mut self) -> Option<Node> {
        let name = self.program.root()?;
        match self.statement(name) {
            Value::Node(n) => Some(n),
            _ => None,
        }
    }

    pub fn statement(&mut self, name: &str) -> Value {
        if let Some(v) = self.done.get(name) {
            return v.clone();
        }
        let Some(expr) = self.program.get(name) else {
            return Value::Null;
        };
        // A statement that needs itself is unresolved.
        if !self.busy.insert(name.to_string()) {
            return Value::Null;
        }
        // Statements don't see `@Each`'s variables: those belong to its template.
        let locals = std::mem::take(&mut self.locals);
        let v = self.expr(expr);
        self.locals = locals;
        self.busy.remove(name);
        self.done.insert(name.to_string(), v.clone());
        v
    }

    pub fn expr(&mut self, e: &Expr) -> Value {
        match e {
            Expr::Null => Value::Null,
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Str(s.clone()),
            // A reference that doesn't resolve drops out of a list rather than leave a gap.
            Expr::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|i| self.expr(i))
                    .filter(|v| !v.is_null())
                    .collect(),
            ),
            Expr::Object(kv) => {
                Value::Object(kv.iter().map(|(k, v)| (k.clone(), self.expr(v))).collect())
            }
            Expr::Ref(name) => {
                if let Some((_, v)) = self.locals.iter().rev().find(|(n, _)| n == name) {
                    return v.clone();
                }
                self.statement(name)
            }
            Expr::State(name) => self.state_value(name),
            Expr::Component(name, args) => {
                if crate::library::component(name).is_none() {
                    self.problem(format!("unknown component {name}"));
                }
                Value::Node(Node {
                    name: name.clone(),
                    args: args.iter().map(|a| self.expr(a)).collect(),
                    binds: args
                        .iter()
                        .map(|a| match a {
                            Expr::State(n) => Some(n.clone()),
                            _ => None,
                        })
                        .collect(),
                })
            }
            Expr::Func(name, args) => self.func(name, args),
            Expr::Unsupported(name) => {
                self.problem(format!("{name} isn't available in tamale's artifacts"));
                Value::Null
            }
            Expr::Unary(op, e) => {
                let v = self.expr(e);
                match *op {
                    "!" => Value::Bool(!v.truthy()),
                    _ => Value::Num(-v.num()),
                }
            }
            Expr::Binary(op, a, b) => {
                // `&&` and `||` give one of their sides, as in JavaScript.
                let a = self.expr(a);
                match *op {
                    "&&" if !a.truthy() => return a,
                    "||" if a.truthy() => return a,
                    "&&" | "||" => return self.expr(b),
                    _ => {}
                }
                let b = self.expr(b);
                binary(op, &a, &b)
            }
            Expr::Ternary(c, y, n) => {
                if self.expr(c).truthy() {
                    self.expr(y)
                } else {
                    self.expr(n)
                }
            }
            Expr::Member(e, f) => self.expr(e).field(f),
            Expr::Index(e, i) => {
                let v = self.expr(e);
                let i = self.expr(i);
                match (&v, &i) {
                    (Value::Array(items), Value::Num(n)) => {
                        items.get(*n as usize).cloned().unwrap_or_default()
                    }
                    (Value::Str(s), Value::Num(n)) => s
                        .chars()
                        .nth(*n as usize)
                        .map(|c| Value::Str(c.to_string()))
                        .unwrap_or_default(),
                    _ => v.field(&i.text()),
                }
            }
        }
    }

    fn state_value(&self, name: &str) -> Value {
        self.state.get(name).cloned().unwrap_or_default()
    }

    fn problem(&mut self, p: String) {
        if !self.problems.contains(&p) {
            self.problems.push(p);
        }
    }

    fn func(&mut self, name: &str, args: &[Expr]) -> Value {
        // Each's template is evaluated once per item, so it's read before its arguments are.
        if name == "Each" {
            let list = args.first().map(|a| self.expr(a)).unwrap_or_default();
            let var = args.get(1).map(|a| self.expr(a).text()).unwrap_or_default();
            let Some(template) = args.get(2) else {
                return Value::Array(Vec::new());
            };
            let mut out = Vec::new();
            for item in list.items() {
                self.locals.push((var.clone(), item));
                let v = self.expr(template);
                self.locals.pop();
                if !v.is_null() {
                    out.push(v);
                }
            }
            return Value::Array(out);
        }
        // `@Set($x, ...)` and `@Reset($x)` name their variables rather than read them.
        let state_name = |e: &Expr| match e {
            Expr::State(n) => Some(n.clone()),
            Expr::Str(s) => Some(s.trim_start_matches('$').to_string()),
            _ => None,
        };
        match name {
            "Set" => {
                let Some(var) = args.first().and_then(state_name) else {
                    return Value::Null;
                };
                let v = args.get(1).map(|a| self.expr(a)).unwrap_or_default();
                return Value::Action(vec![Act::Set(var, v)]);
            }
            "Reset" => {
                return Value::Action(vec![Act::Reset(
                    args.iter().filter_map(state_name).collect(),
                )]);
            }
            _ => {}
        }
        let v: Vec<Value> = args.iter().map(|a| self.expr(a)).collect();
        let arg = |i: usize| v.get(i).cloned().unwrap_or_default();
        let numbers = |list: &Value| -> Vec<f64> { list.items().iter().map(Value::num).collect() };
        match name {
            "Action" => Value::Action(
                v.iter()
                    .flat_map(Value::items)
                    .flat_map(|a| match a {
                        Value::Action(steps) => steps,
                        _ => Vec::new(),
                    })
                    .collect(),
            ),
            "ToAssistant" => Value::Action(vec![Act::ToAssistant {
                message: arg(0).text(),
                context: arg(1),
            }]),
            "OpenUrl" => Value::Action(vec![Act::OpenUrl(arg(0).text())]),
            "Run" => {
                self.problem(
                    "@Run needs a query or mutation, which tamale's artifacts don't make".into(),
                );
                Value::Action(Vec::new())
            }
            "Count" => Value::Num(arg(0).items().len() as f64),
            "First" => arg(0).items().first().cloned().unwrap_or_default(),
            "Last" => arg(0).items().last().cloned().unwrap_or_default(),
            "Sum" => Value::Num(numbers(&arg(0)).iter().sum()),
            "Avg" => {
                let n = numbers(&arg(0));
                Value::Num(if n.is_empty() {
                    0.0
                } else {
                    n.iter().sum::<f64>() / n.len() as f64
                })
            }
            "Min" => numbers(&arg(0))
                .into_iter()
                .reduce(f64::min)
                .map_or(Value::Null, Value::Num),
            "Max" => numbers(&arg(0))
                .into_iter()
                .reduce(f64::max)
                .map_or(Value::Null, Value::Num),
            "Filter" => {
                let field = arg(1).text();
                let op = arg(2).text();
                let want = arg(3);
                Value::Array(
                    arg(0)
                        .items()
                        .into_iter()
                        .filter(|item| {
                            let got = item.path(&field);
                            match op.as_str() {
                                "==" => loose_eq(&got, &want),
                                "!=" => !loose_eq(&got, &want),
                                ">" => got.num() > want.num(),
                                "<" => got.num() < want.num(),
                                ">=" => got.num() >= want.num(),
                                "<=" => got.num() <= want.num(),
                                "contains" => match &got {
                                    Value::Array(items) => items.iter().any(|i| loose_eq(i, &want)),
                                    g => g
                                        .text()
                                        .to_lowercase()
                                        .contains(&want.text().to_lowercase()),
                                },
                                _ => true,
                            }
                        })
                        .collect(),
                )
            }
            "Sort" => {
                let field = arg(1).text();
                let mut items = arg(0).items();
                items.sort_by(|a, b| compare(&a.path(&field), &b.path(&field)));
                if arg(2).text().eq_ignore_ascii_case("desc") {
                    items.reverse();
                }
                Value::Array(items)
            }
            "Round" | "Abs" | "Floor" | "Ceil" => {
                let places = if name == "Round" {
                    arg(1).num() as i32
                } else {
                    0
                };
                let f = |x: f64| -> f64 {
                    match name {
                        "Round" => {
                            let m = 10f64.powi(places);
                            (x * m + 0.5).floor() / m
                        }
                        "Abs" => x.abs(),
                        "Floor" => x.floor(),
                        _ => x.ceil(),
                    }
                };
                match arg(0) {
                    Value::Array(items) => {
                        Value::Array(items.iter().map(|i| Value::Num(f(i.num()))).collect())
                    }
                    x => Value::Num(f(x.num())),
                }
            }
            _ => {
                self.problem(format!("unknown function @{name}"));
                Value::Null
            }
        }
    }
}

fn binary(op: &str, a: &Value, b: &Value) -> Value {
    match op {
        "+" => match (a, b) {
            (Value::Str(_), _) | (_, Value::Str(_)) => Value::Str(a.text() + &b.text()),
            _ => Value::Num(a.num() + b.num()),
        },
        "-" => Value::Num(a.num() - b.num()),
        "*" => Value::Num(a.num() * b.num()),
        "/" | "%" => {
            let d = b.num();
            Value::Num(match (op, d == 0.0) {
                (_, true) => 0.0,
                ("/", _) => a.num() / d,
                _ => a.num() % d,
            })
        }
        "==" => Value::Bool(loose_eq(a, b)),
        "!=" => Value::Bool(!loose_eq(a, b)),
        ">" => Value::Bool(compare(a, b).is_gt()),
        "<" => Value::Bool(compare(a, b).is_lt()),
        ">=" => Value::Bool(compare(a, b).is_ge()),
        "<=" => Value::Bool(compare(a, b).is_le()),
        _ => Value::Null,
    }
}

/// The first values of a program's state variables: each declared one not already set.
/// Values the reader set are never overwritten.
pub fn init_state(program: &Program, state: &mut State) {
    let empty = State::new();
    for (name, expr) in program.state_decls() {
        if state.contains_key(name) {
            continue;
        }
        let v = Eval::new(program, &empty).expr(expr);
        state.insert(name.to_string(), v);
    }
}

/// The first value of each named variable, for `@Reset`.
pub fn reset(program: &Program, state: &mut State, names: &[String]) {
    let empty = State::new();
    for name in names {
        let first = program
            .state_decls()
            .find(|(n, _)| n == name)
            .map(|(_, e)| Eval::new(program, &empty).expr(e))
            .unwrap_or_default();
        state.insert(name.clone(), first);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn eval(src: &str, name: &str) -> Value {
        let p = parse(src);
        let mut state = State::new();
        init_state(&p, &mut state);
        Eval::new(&p, &state).statement(name)
    }

    #[test]
    fn arithmetic_and_strings() {
        assert_eq!(eval("x = 1 + 2 * 3", "x"), Value::Num(7.0));
        assert_eq!(eval("x = \"n=\" + 4", "x"), Value::Str("n=4".into()));
        assert_eq!(eval("x = 5 == \"5\"", "x"), Value::Bool(true));
        assert_eq!(eval("x = 3 / 0", "x"), Value::Num(0.0));
        assert_eq!(eval("x = null || \"d\"", "x"), Value::Str("d".into()));
    }

    #[test]
    fn references_resolve_in_any_order_and_missing_ones_drop_out() {
        let v = eval(
            "root = Stack([a, missing])\na = TextContent(\"hi\")",
            "root",
        );
        let Value::Node(n) = v else { panic!() };
        assert_eq!(n.arg("children").items().len(), 1);
    }

    #[test]
    fn cycles_are_null() {
        assert_eq!(eval("a = b\nb = a", "a"), Value::Null);
    }

    #[test]
    fn state_reads_and_binds() {
        let v = eval(
            "$q = \"x\"\nroot = Input(\"q\", \"Find\", \"text\", null, $q)",
            "root",
        );
        let Value::Node(n) = v else { panic!() };
        assert_eq!(n.bind("value"), Some("q"));
        assert_eq!(n.str("value"), "x");
    }

    #[test]
    fn builtins() {
        let src = "rows = [{n: \"a\", v: 3}, {n: \"b\", v: 1}, {n: \"c\", v: 2}]\n\
                   c = @Count(rows)\ns = @Sum(rows.v)\nbig = @Filter(rows, \"v\", \">=\", 2)\n\
                   sorted = @Sort(rows, \"v\", \"desc\")\nnames = @Each(rows, \"r\", r.n + \"!\")\n\
                   r = @Round(2.346, 2)";
        assert_eq!(eval(src, "c"), Value::Num(3.0));
        assert_eq!(eval(src, "s"), Value::Num(6.0));
        assert_eq!(eval(src, "big").items().len(), 2);
        assert_eq!(
            eval(src, "sorted").items()[0].field("n"),
            Value::Str("a".into())
        );
        assert_eq!(eval(src, "names").text(), "a!,b!,c!");
        assert_eq!(eval(src, "r"), Value::Num(2.35));
    }

    #[test]
    fn actions_collect_their_steps() {
        let v = eval(
            "$open = false\na = Action([@Set($open, !$open), @ToAssistant(\"Go\", {x: 1})])",
            "a",
        );
        assert_eq!(
            v,
            Value::Action(vec![
                Act::Set("open".into(), Value::Bool(true)),
                Act::ToAssistant {
                    message: "Go".into(),
                    context: Value::Object(vec![("x".into(), Value::Num(1.0))])
                }
            ])
        );
    }

    #[test]
    fn json_out() {
        assert_eq!(
            eval("x = {a: [1, 2.5, \"q\\\"\"], b: null}", "x").json(),
            r#"{"a":[1,2.5,"q\""],"b":null}"#
        );
    }
}
