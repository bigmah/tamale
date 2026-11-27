//! Tokens. Lexing never fails: unknown characters are skipped, and a string still being
//! written when the text ends is closed there, so a reply that's streaming in reads as far
//! as it has come.

#[derive(Clone, PartialEq, Debug)]
pub enum Tok {
    /// A name: a component when it starts with an uppercase letter and is called, else a
    /// reference to a statement.
    Ident(String),
    /// `$name`, without the `$`.
    State(String),
    /// `@Name`, without the `@`.
    Func(String),
    Str(String),
    Num(f64),
    /// An operator or punctuation: `=`, `==`, `&&`, `(`, ...
    Op(&'static str),
    Newline,
}

const OPS: &[&str] = &[
    "==", "!=", ">=", "<=", "&&", "||", "=", ">", "<", "+", "-", "*", "/", "%", "!", "?", ":", ".",
    ",", "(", ")", "[", "]", "{", "}",
];

pub fn lex(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                out.push(Tok::Newline);
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '#' => i = line_end(&chars, i),
            '/' if chars.get(i + 1) == Some(&'/') => i = line_end(&chars, i),
            '"' | '\'' => {
                let (s, next) = string(&chars, i + 1, c);
                out.push(Tok::Str(s));
                i = next;
            }
            c if c.is_ascii_digit()
                || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) =>
            {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if matches!(chars.get(i), Some('e' | 'E')) {
                    let mut j = i + 1;
                    if matches!(chars.get(j), Some('+' | '-')) {
                        j += 1;
                    }
                    if chars.get(j).is_some_and(char::is_ascii_digit) {
                        i = j;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let text: String = chars[start..i].iter().collect();
                out.push(Tok::Num(text.parse().unwrap_or(0.0)));
            }
            '$' | '@' => {
                let (name, next) = ident(&chars, i + 1);
                i = next;
                if name.is_empty() {
                    continue;
                }
                out.push(if c == '$' {
                    Tok::State(name)
                } else {
                    Tok::Func(name)
                });
            }
            c if c.is_alphabetic() || c == '_' => {
                let (name, next) = ident(&chars, i);
                i = next;
                out.push(Tok::Ident(name));
            }
            '&' | '|' => {
                // A single `&` or `|` means `&&` or `||`.
                i += if chars.get(i + 1) == Some(&c) { 2 } else { 1 };
                out.push(Tok::Op(if c == '&' { "&&" } else { "||" }));
            }
            _ => {
                let rest: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                match OPS.iter().find(|op| rest.starts_with(**op)) {
                    Some(op) => {
                        out.push(Tok::Op(op));
                        i += op.len();
                    }
                    None => i += 1,
                }
            }
        }
    }
    out
}

fn line_end(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i] != '\n' {
        i += 1;
    }
    i
}

fn ident(chars: &[char], mut i: usize) -> (String, usize) {
    let start = i;
    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
        i += 1;
    }
    (chars[start..i].iter().collect(), i)
}

/// A string from just after its opening `quote`: its text, and where the text after it starts.
/// Double-quoted strings take JSON's escapes; single-quoted ones only `\'`, `\\`, `\n`, `\t`.
fn string(chars: &[char], mut i: usize, quote: char) -> (String, usize) {
    let mut out = String::new();
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == quote {
            return (out, i);
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(&e) = chars.get(i) else { break };
        i += 1;
        match (quote, e) {
            (_, '\\') => out.push('\\'),
            (_, 'n') => out.push('\n'),
            (_, 't') => out.push('\t'),
            ('\'', '\'') => out.push('\''),
            ('"', '"') => out.push('"'),
            ('"', '/') => out.push('/'),
            ('"', 'r') => out.push('\r'),
            ('"', 'b') => out.push('\u{8}'),
            ('"', 'f') => out.push('\u{c}'),
            ('"', 'u') => {
                let hex: String = chars[i..(i + 4).min(chars.len())].iter().collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) if hex.len() == 4 => {
                        out.push(ch);
                        i += 4;
                    }
                    _ => out.push_str("\\u"),
                }
            }
            (_, other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    (out, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = lex("a = Card([\"x\\n\", 'it\\'s', -1.5e2]) // note\n$n = @Count(a) # c");
        assert_eq!(
            t,
            vec![
                Tok::Ident("a".into()),
                Tok::Op("="),
                Tok::Ident("Card".into()),
                Tok::Op("("),
                Tok::Op("["),
                Tok::Str("x\n".into()),
                Tok::Op(","),
                Tok::Str("it's".into()),
                Tok::Op(","),
                Tok::Op("-"),
                Tok::Num(150.0),
                Tok::Op("]"),
                Tok::Op(")"),
                Tok::Newline,
                Tok::State("n".into()),
                Tok::Op("="),
                Tok::Func("Count".into()),
                Tok::Op("("),
                Tok::Ident("a".into()),
                Tok::Op(")"),
            ]
        );
    }

    #[test]
    fn an_unfinished_string_closes_at_the_end() {
        assert_eq!(
            lex("t = \"Half wr"),
            vec![
                Tok::Ident("t".into()),
                Tok::Op("="),
                Tok::Str("Half wr".into())
            ]
        );
    }

    #[test]
    fn single_ampersand_and_pipe_mean_the_double_ones() {
        assert_eq!(lex("a & b | c")[1], Tok::Op("&&"));
        assert_eq!(lex("a & b | c")[3], Tok::Op("||"));
    }
}
