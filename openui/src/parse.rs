//! Statements and expressions. Like lexing, parsing never fails as a whole: a statement
//! that doesn't read as `name = expression` is skipped, and brackets still open when the
//! text ends are closed there, so a document that's still streaming in shows what it has.

use crate::lex::{Tok, lex};

#[derive(Clone, PartialEq, Debug)]
pub enum Expr {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    /// Another statement, or a variable `@Each` gives its template.
    Ref(String),
    /// `$name`.
    State(String),
    /// `Name(args)`: a component.
    Component(String, Vec<Expr>),
    /// `@Name(args)`, and `Action([...])`.
    Func(String, Vec<Expr>),
    /// `Query(...)` or `Mutation(...)`: tool calls, which tamale's artifacts don't make.
    Unsupported(String),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
}

#[derive(Clone, PartialEq, Debug)]
pub struct Statement {
    /// Its name; a state variable's has its `$`.
    pub name: String,
    pub expr: Expr,
}

/// A parsed document.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Program {
    /// In the order written; for a name written twice, the last wins.
    pub statements: Vec<Statement>,
}

impl Program {
    pub fn get(&self, name: &str) -> Option<&Expr> {
        self.statements
            .iter()
            .rev()
            .find(|s| s.name == name)
            .map(|s| &s.expr)
    }

    /// The statement the document is drawn from: `root`, or else the first that's a
    /// component.
    pub fn root(&self) -> Option<&str> {
        if self.get("root").is_some() {
            return Some("root");
        }
        self.statements
            .iter()
            .find(|s| !s.name.starts_with('$') && matches!(s.expr, Expr::Component(..)))
            .map(|s| s.name.as_str())
    }

    /// The state variables and the expressions that give their first values.
    pub fn state_decls(&self) -> impl Iterator<Item = (&str, &Expr)> {
        self.statements
            .iter()
            .filter_map(|s| Some((s.name.strip_prefix('$')?, &s.expr)))
    }
}

pub fn parse(src: &str) -> Program {
    // Fences are the caller's to strip, but a stray one shouldn't become statements.
    let src: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    let toks = lex(&src);
    let mut p = Parser { toks, at: 0 };
    let mut statements = Vec::new();
    loop {
        p.skip_newlines();
        if p.at >= p.toks.len() {
            break;
        }
        let start = p.at;
        match p.statement() {
            Some(s) if p.at_statement_end() => statements.push(s),
            _ => {
                // Not a statement: skip to the next line that isn't inside brackets.
                p.at = start;
                p.skip_statement();
            }
        }
    }
    Program { statements }
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Some(Tok::Newline)) {
            self.at += 1;
        }
    }

    /// Whether the token after the next newlines is `op`, and if so skips to it.
    fn eat_across_lines(&mut self, op: &str) -> bool {
        let save = self.at;
        self.skip_newlines();
        if self.eat(op) {
            return true;
        }
        self.at = save;
        false
    }

    fn at_statement_end(&self) -> bool {
        matches!(self.peek(), None | Some(Tok::Newline))
    }

    fn skip_statement(&mut self) {
        let mut depth = 0i32;
        while let Some(t) = self.peek() {
            match t {
                Tok::Op("(" | "[" | "{") => depth += 1,
                Tok::Op(")" | "]" | "}") => depth = (depth - 1).max(0),
                Tok::Newline if depth == 0 => return,
                _ => {}
            }
            self.at += 1;
        }
    }

    fn statement(&mut self) -> Option<Statement> {
        let name = match self.peek()? {
            Tok::Ident(n) => n.clone(),
            Tok::State(n) => format!("${n}"),
            _ => return None,
        };
        self.at += 1;
        if !self.eat("=") {
            return None;
        }
        let expr = self.expr(false)?;
        Some(Statement { name, expr })
    }

    /// An expression. Inside brackets (`nested`) newlines are only whitespace.
    fn expr(&mut self, nested: bool) -> Option<Expr> {
        self.ternary(nested)
    }

    fn gap(&mut self, nested: bool) {
        if nested {
            self.skip_newlines();
        }
    }

    fn ternary(&mut self, nested: bool) -> Option<Expr> {
        let cond = self.binary(0, nested)?;
        self.gap(nested);
        // A ternary may go on over lines: the next line starts with `?` or `:`.
        if self.eat_across_lines("?") {
            self.skip_newlines();
            let yes = self.ternary(true)?;
            if !self.eat_across_lines(":") {
                return Some(Expr::Ternary(
                    Box::new(cond),
                    Box::new(yes),
                    Box::new(Expr::Null),
                ));
            }
            self.skip_newlines();
            let no = self.ternary(nested)?;
            return Some(Expr::Ternary(Box::new(cond), Box::new(yes), Box::new(no)));
        }
        Some(cond)
    }

    fn binary(&mut self, level: usize, nested: bool) -> Option<Expr> {
        const LEVELS: &[&[&str]] = &[
            &["||"],
            &["&&"],
            &["==", "!="],
            &[">", "<", ">=", "<="],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary(nested);
        }
        let mut left = self.binary(level + 1, nested)?;
        loop {
            self.gap(nested);
            let Some(Tok::Op(op)) = self.peek() else {
                break;
            };
            let Some(op) = LEVELS[level].iter().find(|o| **o == *op) else {
                break;
            };
            self.at += 1;
            self.gap(nested);
            let right = self.binary(level + 1, nested)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }

    fn unary(&mut self, nested: bool) -> Option<Expr> {
        if self.eat("!") {
            return Some(Expr::Unary("!", Box::new(self.unary(nested)?)));
        }
        if self.eat("-") {
            return Some(match self.unary(nested)? {
                Expr::Num(n) => Expr::Num(-n),
                e => Expr::Unary("-", Box::new(e)),
            });
        }
        self.postfix(nested)
    }

    fn postfix(&mut self, nested: bool) -> Option<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat(".") {
                // `m.0.1` reads as the field "0.1" in the spec; a number token gives that.
                match self.peek().cloned() {
                    Some(Tok::Ident(f)) => {
                        self.at += 1;
                        e = Expr::Member(Box::new(e), f);
                    }
                    Some(Tok::Num(n)) => {
                        self.at += 1;
                        e = Expr::Member(Box::new(e), crate::eval::num_text(n));
                    }
                    _ => return Some(e),
                }
            } else if self.is_op("[") {
                self.at += 1;
                self.skip_newlines();
                let index = self.expr(true).unwrap_or(Expr::Null);
                self.skip_newlines();
                self.eat("]");
                e = Expr::Index(Box::new(e), Box::new(index));
            } else {
                let _ = nested;
                return Some(e);
            }
        }
    }

    fn primary(&mut self) -> Option<Expr> {
        let tok = self.peek()?.clone();
        self.at += 1;
        Some(match tok {
            Tok::Str(s) => Expr::Str(s),
            Tok::Num(n) => Expr::Num(n),
            Tok::State(n) => Expr::State(n),
            Tok::Func(name) => {
                let args = if self.is_op("(") {
                    self.at += 1;
                    self.list(")")
                } else {
                    Vec::new()
                };
                Expr::Func(name, args)
            }
            Tok::Ident(name) => match name.as_str() {
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "null" => Expr::Null,
                _ if self.is_op("(") => {
                    self.at += 1;
                    let args = self.list(")");
                    match name.as_str() {
                        "Query" | "Mutation" => Expr::Unsupported(name),
                        "Action" => Expr::Func(name, args),
                        _ => Expr::Component(name, args),
                    }
                }
                _ => Expr::Ref(name),
            },
            Tok::Op("(") => {
                self.skip_newlines();
                let e = self.expr(true)?;
                self.skip_newlines();
                self.eat(")");
                e
            }
            Tok::Op("[") => Expr::Array(self.list("]")),
            Tok::Op("{") => self.object(),
            _ => return None,
        })
    }

    /// Expressions up to `close`, commas optional. The end of the text closes it too.
    fn list(&mut self, close: &str) -> Vec<Expr> {
        let mut out = Vec::new();
        loop {
            self.skip_newlines();
            while self.eat(",") {
                self.skip_newlines();
            }
            if self.peek().is_none() || self.eat(close) {
                return out;
            }
            let before = self.at;
            match self.expr(true) {
                Some(e) => out.push(e),
                None => {
                    // A token that starts nothing (a stray closer): step over it.
                    if self.at == before {
                        self.at += 1;
                    }
                }
            }
        }
    }

    fn object(&mut self) -> Expr {
        let mut out = Vec::new();
        loop {
            self.skip_newlines();
            while self.eat(",") {
                self.skip_newlines();
            }
            let key = match self.peek().cloned() {
                None => break,
                Some(Tok::Op("}")) => {
                    self.at += 1;
                    break;
                }
                Some(Tok::Ident(k) | Tok::Str(k) | Tok::State(k)) => k,
                Some(Tok::Num(n)) => crate::eval::num_text(n),
                Some(_) => "?".to_string(),
            };
            self.at += 1;
            self.skip_newlines();
            if !self.eat(":") {
                continue;
            }
            self.skip_newlines();
            match self.expr(true) {
                Some(e) => out.push((key, e)),
                None => break,
            }
        }
        Expr::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_and_calls() {
        let p = parse(
            "root = Card([header, chart])\nheader = CardHeader(\"Revenue\", \"6 months\")\n\
             chart = BarChart(labels, [Series(\"Revenue\", [1, 2])])\nlabels = [\"Jan\", \"Feb\"]",
        );
        assert_eq!(p.statements.len(), 4);
        assert_eq!(p.root(), Some("root"));
        assert_eq!(
            p.get("header"),
            Some(&Expr::Component(
                "CardHeader".into(),
                vec![Expr::Str("Revenue".into()), Expr::Str("6 months".into())]
            ))
        );
    }

    #[test]
    fn brackets_span_lines_and_commas_are_optional() {
        let p = parse("root = Stack([\n  a\n  b,\n])\na = TextContent(\"x\" \"large\")");
        assert_eq!(
            p.get("root"),
            Some(&Expr::Component(
                "Stack".into(),
                vec![Expr::Array(vec![
                    Expr::Ref("a".into()),
                    Expr::Ref("b".into())
                ])]
            ))
        );
        assert!(matches!(p.get("a"), Some(Expr::Component(_, args)) if args.len() == 2));
    }

    #[test]
    fn a_document_cut_off_mid_way_closes_what_is_open() {
        let p = parse("root = Stack([a, b\na = TextContent(\"Hel");
        // Inside the open bracket the next line is still part of `root`.
        let Some(Expr::Component(_, args)) = p.get("root") else {
            panic!("{p:?}")
        };
        let Expr::Array(items) = &args[0] else {
            panic!()
        };
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn junk_lines_are_skipped() {
        let p = parse("Here is the UI:\nroot = Stack([])\n!!\nx = 1 +");
        assert_eq!(p.statements.len(), 1);
    }

    #[test]
    fn precedence_and_ternaries() {
        let p = parse("x = 1 + 2 * 3 == 7 && !false ? \"y\" : \"n\"\nz = $open\n  ? A()\n  : B()");
        assert!(matches!(p.get("x"), Some(Expr::Ternary(..))));
        assert!(matches!(p.get("z"), Some(Expr::Ternary(..))));
        assert_eq!(p.statements.len(), 2);
    }

    #[test]
    fn state_members_and_functions() {
        let p =
            parse("$tab = \"a\"\nn = @Count(rows.items[0].x)\nact = Action([@Set($tab, \"b\")])");
        assert_eq!(p.state_decls().count(), 1);
        assert!(matches!(p.get("n"), Some(Expr::Func(name, _)) if name == "Count"));
        assert!(matches!(p.get("act"), Some(Expr::Func(name, _)) if name == "Action"));
    }

    #[test]
    fn objects() {
        let p = parse("o = {a: 1, \"b c\": [2] d: {e: true}}");
        let Some(Expr::Object(kv)) = p.get("o") else {
            panic!()
        };
        assert_eq!(
            kv.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["a", "b c", "d"]
        );
    }
}
