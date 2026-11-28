//! Text the renderer lays out: inline markdown in labels, block markdown, unified diffs,
//! and chart axes. Kept here, away from Dioxus, so it's tested natively.

/// A run of text with one style.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

/// Inline markdown: `**bold**`, `*italic*` or `_italic_`, `` `code` `` and
/// `[text](https://…)`, one inside another (`**the `cap`**`). Anything else, including
/// unclosed markers, is text.
pub fn inline(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    inline_into(s, &Span::default(), &mut out);
    out
}

/// `s` as spans that each add their own style to `style`.
fn inline_into(s: &str, style: &Span, out: &mut Vec<Span>) {
    let mut plain = String::new();
    let mut rest = s;
    let flush = |plain: &mut String, out: &mut Vec<Span>| {
        if !plain.is_empty() {
            out.push(Span {
                text: std::mem::take(plain),
                ..style.clone()
            });
        }
    };
    while let Some(c) = rest.chars().next() {
        // What the marker here wraps and styles, and how much of `rest` it takes.
        let marked: Option<(&str, Span, usize)> = match c {
            '`' => rest[1..].find('`').map(|end| {
                (
                    &rest[1..1 + end],
                    Span {
                        code: true,
                        ..style.clone()
                    },
                    end + 2,
                )
            }),
            '*' if rest.starts_with("**") => closing(&rest[2..], "**").map(|end| {
                (
                    &rest[2..2 + end],
                    Span {
                        bold: true,
                        ..style.clone()
                    },
                    end + 4,
                )
            }),
            '*' | '_' => closing(&rest[1..], &rest[..1])
                .filter(|_| !rest[1..].starts_with(char::is_whitespace))
                // `snake_case_names` aren't emphasis.
                .filter(|end| {
                    c == '*'
                        || (!plain.ends_with(char::is_alphanumeric)
                            && !rest[2 + end..].starts_with(char::is_alphanumeric))
                })
                .map(|end| {
                    (
                        &rest[1..1 + end],
                        Span {
                            italic: true,
                            ..style.clone()
                        },
                        end + 2,
                    )
                }),
            '[' => link(rest).map(|(text, url, len)| {
                (
                    text,
                    Span {
                        link: Some(url),
                        ..style.clone()
                    },
                    len,
                )
            }),
            _ => None,
        };
        match marked {
            Some((inner, inner_style, len)) => {
                flush(&mut plain, out);
                if inner_style.code {
                    out.push(Span {
                        text: inner.to_string(),
                        ..inner_style
                    });
                } else {
                    inline_into(inner, &inner_style, out);
                }
                rest = &rest[len..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut plain, out);
}

/// Where `marker` closes what it opened just before `s`: its first occurrence that isn't
/// inside `` `code` `` (and, for `*`, isn't half of a `**`), past at least one character.
fn closing(s: &str, marker: &str) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(code) = rest.strip_prefix('`') {
            // Code keeps its markers to itself.
            match code.find('`') {
                Some(end) => i += end + 2,
                None => i += 1,
            }
        } else if marker.len() == 1 && rest.starts_with(marker) && rest[1..].starts_with(marker) {
            i += 2;
        } else if rest.starts_with(marker) {
            return (i > 0).then_some(i);
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// A link at the start of `rest`: its text, its address and how much of `rest` it takes.
fn link(rest: &str) -> Option<(&str, String, usize)> {
    let close = rest.find("](")?;
    let end = close + 2 + rest[close + 2..].find(')')?;
    let url = rest[close + 2..end].trim();
    if !safe_url(url) {
        return None;
    }
    Some((&rest[1..close], url.to_string(), end + 1))
}

/// Whether a link may be followed: http(s) and mailto only.
pub fn safe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

/// Extensions that make a bare name (`app.js`, no directory) a file rather than `a.b`.
const FILE_EXT: &[&str] = &[
    "rs", "js", "mjs", "cjs", "ts", "tsx", "jsx", "py", "go", "rb", "java", "kt", "swift", "c",
    "h", "cc", "cpp", "hpp", "cs", "php", "sh", "bash", "zsh", "toml", "yaml", "yml", "json", "md",
    "txt", "html", "css", "scss", "sql", "lock", "xml", "svg", "proto", "vue", "svelte", "lua",
    "ex", "exs", "zig", "nix", "tf", "ini", "cfg", "conf",
];

/// The file some text names, if it reads as one: a path with an extension, optionally with
/// `:line`, `:line-line` or `#Lline`. Whether there is such a file is pullspace's to say.
pub fn file_path(raw: &str) -> Option<(String, Option<u64>)> {
    let mut text = raw.trim();
    for q in ['\'', '"', '`'] {
        text = text.strip_prefix(q).unwrap_or(text);
        text = text.strip_suffix(q).unwrap_or(text);
    }
    // The line, if any.
    let (path, line) = if let Some((path, l)) = text.split_once("#L") {
        let first: String = l.chars().take_while(char::is_ascii_digit).collect();
        let tail = &l[first.len()..];
        let ok = !first.is_empty()
            && (tail.is_empty()
                || tail
                    .strip_prefix('-')
                    .map(|t| t.trim_start_matches('L'))
                    .is_some_and(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())));
        if !ok {
            return None;
        }
        (path, first.parse().ok())
    } else if let Some(colon) = text.find(':') {
        let (path, tail) = (&text[..colon], &text[colon + 1..]);
        let first: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if first.is_empty() {
            return None;
        }
        let mut more = &tail[first.len()..];
        while let Some(rest) = more.strip_prefix(['-', ':']) {
            let n = rest.chars().take_while(char::is_ascii_digit).count();
            if n == 0 {
                return None;
            }
            more = &rest[n..];
        }
        if !more.is_empty() {
            return None;
        }
        (path, first.parse().ok())
    } else {
        (text, None)
    };
    let word = |c: char| c.is_alphanumeric() || "_.@+-".contains(c);
    let body = path
        .strip_prefix('~')
        .or_else(|| path.strip_prefix(".."))
        .or_else(|| path.strip_prefix('.'))
        .unwrap_or(path);
    let body = body.strip_prefix('/').unwrap_or(body);
    if body.is_empty()
        || body
            .split('/')
            .any(|seg| seg.is_empty() || !seg.chars().all(word))
    {
        return None;
    }
    let name = body.rsplit('/').next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty()
        || ext.is_empty()
        || ext.len() > 8
        || !ext.chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        return None;
    }
    if !path.contains('/') && !FILE_EXT.contains(&ext.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some((path.to_string(), line.filter(|l| *l > 0)))
}

/// A block of markdown.
#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    Heading(u8, String),
    Para(String),
    List { ordered: bool, items: Vec<String> },
    Quote(String),
    Code { lang: String, code: String },
}

/// Block markdown, as much of it as prose in an artifact needs.
pub fn blocks(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut lines = md.lines().peekable();
    let end_para = |para: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Para(para.join(" ")));
            para.clear();
        }
    };
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.is_empty() {
            end_para(&mut para, &mut out);
            continue;
        }
        if let Some(lang) = t.strip_prefix("```") {
            end_para(&mut para, &mut out);
            let mut code = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                code.push(l);
            }
            out.push(Block::Code {
                lang: lang.trim().to_string(),
                code: code.join("\n"),
            });
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            end_para(&mut para, &mut out);
            out.push(Block::Heading(hashes as u8, t[hashes..].trim().to_string()));
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            end_para(&mut para, &mut out);
            let mut quote = vec![q.trim()];
            while let Some(next) = lines.peek().and_then(|l| l.trim().strip_prefix('>')) {
                quote.push(next.trim());
                lines.next();
            }
            out.push(Block::Quote(quote.join(" ")));
            continue;
        }
        if let Some((ordered, item)) = list_item(t) {
            end_para(&mut para, &mut out);
            let mut items = vec![item.to_string()];
            while let Some(next) = lines.peek() {
                match list_item(next.trim()) {
                    Some((o, item)) if o == ordered => items.push(item.to_string()),
                    // A line indented under an item continues it.
                    None if next.starts_with("  ") && !next.trim().is_empty() => {
                        let last = items.last_mut().unwrap();
                        last.push(' ');
                        last.push_str(next.trim());
                    }
                    _ => break,
                }
                lines.next();
            }
            out.push(Block::List { ordered, items });
            continue;
        }
        para.push(t);
    }
    end_para(&mut para, &mut out);
    out
}

fn list_item(t: &str) -> Option<(bool, &str)> {
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some((false, rest));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &t[digits..];
        if let Some(item) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((true, item));
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    Context,
    Add,
    Del,
    Hunk,
    /// `diff --git`, `---`, `+++`, `index`: shown muted.
    Meta,
    /// Lines left out (`...` on a line of its own).
    Gap,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// A unified diff's rows, with old and new line numbers from its hunk headers. Lines are
/// numbered only where the numbers are known: after a header that gives them
/// (`@@ -12,7 +12,9 @@`), and until lines are left out (`...`).
pub fn diff_rows(patch: &str) -> Vec<DiffRow> {
    // The next old and new line numbers, while they're known.
    let mut at: Option<(u32, u32)> = None;
    let mut in_hunk = false;
    let mut out = Vec::new();
    for line in patch.lines() {
        let row = |kind, o, n, text: &str| DiffRow {
            kind,
            old: o,
            new: n,
            text: text.to_string(),
        };
        if line.starts_with("@@") {
            // @@ -12,7 +12,9 @@ context
            let mut parts = line.split_whitespace().skip(1);
            let start = |p: Option<&str>| -> Option<u32> {
                // After its sign, which a model may write as any dash.
                p?.chars()
                    .skip(1)
                    .collect::<String>()
                    .split(',')
                    .next()?
                    .parse()
                    .ok()
            };
            at = start(parts.next()).zip(start(parts.next()));
            in_hunk = true;
            out.push(row(DiffKind::Hunk, None, None, line));
        } else if !in_hunk
            && (line.starts_with("diff ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("index "))
        {
            out.push(row(DiffKind::Meta, None, None, line));
        } else if let Some(t) = line.strip_prefix('+') {
            out.push(row(DiffKind::Add, None, at.map(|(_, n)| n), t));
            at = at.map(|(o, n)| (o, n + 1));
        } else if let Some(t) = line.strip_prefix('-') {
            out.push(row(DiffKind::Del, at.map(|(o, _)| o), None, t));
            at = at.map(|(o, n)| (o + 1, n));
        } else if line.starts_with('\\') {
            out.push(row(DiffKind::Meta, None, None, line));
        } else if matches!(line.trim(), "..." | "…" | "⋮") {
            // Lines left out: what follows can't be numbered.
            out.push(row(DiffKind::Gap, None, None, ""));
            at = None;
        } else {
            let t = line.strip_prefix(' ').unwrap_or(line);
            out.push(row(
                DiffKind::Context,
                at.map(|(o, _)| o),
                at.map(|(_, n)| n),
                t,
            ));
            at = at.map(|(o, n)| (o + 1, n + 1));
        }
    }
    // The indentation every line shares says nothing: without it more of each line fits.
    let code = |r: &DiffRow| matches!(r.kind, DiffKind::Context | DiffKind::Add | DiffKind::Del);
    let shared = shared_indent(out.iter().filter(|r| code(r)).map(|r| r.text.as_str()));
    if shared > 0 {
        for r in out.iter_mut().filter(|r| code(r)) {
            r.text = r.text.chars().skip(shared).collect();
        }
    }
    out
}

/// How many characters of leading whitespace every line that isn't blank starts with.
pub fn shared_indent<'a>(lines: impl Iterator<Item = &'a str>) -> usize {
    lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').count())
        .min()
        .unwrap_or(0)
}

/// `code` without the indentation all its lines share, and without blank lines at its ends.
pub fn dedent(code: &str) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let shared = shared_indent(lines.iter().copied());
    let first = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let last = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    lines[first..last.max(first)]
        .iter()
        .map(|l| l.chars().skip(shared).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// How wide a line's leading whitespace is, in characters (a tab is four).
pub fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// The lines a `CodeBlock` marks: numbers (`12`) and ranges (`"12-15"`), as the block
/// numbers them.
pub fn line_set(spec: &[String]) -> Vec<(usize, usize)> {
    spec.iter()
        .flat_map(|s| s.split(','))
        .filter_map(|part| {
            let part = part.trim();
            let (a, b) = match part.split_once(['-', '–', '—']) {
                Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
                None => {
                    let n: usize = part.parse().ok()?;
                    (n, n)
                }
            };
            Some((a.min(b), a.max(b)))
        })
        .collect()
}

/// Round numbers for a chart's axis from 0 (or `min`, when below 0) to past `max`: the
/// ticks and the axis's top and bottom.
pub fn ticks(min: f64, max: f64) -> (Vec<f64>, f64, f64) {
    let lo = min.min(0.0);
    let hi = if max <= lo { lo + 1.0 } else { max };
    let raw = (hi - lo) / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    let bottom = (lo / step).floor() * step;
    let top = (hi / step).ceil() * step;
    let mut out = Vec::new();
    let mut t = bottom;
    while t <= top + step / 2.0 {
        out.push((t / step).round() * step);
        t += step;
    }
    (out, bottom, top)
}

/// Round numbers for an axis that doesn't start at zero, around values from `min` to
/// `max`: for a line that's read by its shape. The ticks and the axis's top and bottom.
pub fn ticks_near(min: f64, max: f64) -> (Vec<f64>, f64, f64) {
    let (lo, hi) = if max <= min {
        (min - 1.0, min + 1.0)
    } else {
        (min, max)
    };
    let raw = (hi - lo) / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    let bottom = (lo / step).floor() * step;
    let top = (hi / step).ceil() * step;
    let mut out = Vec::new();
    let mut t = bottom;
    while t <= top + step / 2.0 {
        out.push((t / step).round() * step);
        t += step;
    }
    (out, bottom, top)
}

/// A figure short enough for an axis or a bar label: 1.2k, 3.4M.
pub fn short_num(n: f64) -> String {
    let a = n.abs();
    let (v, unit) = if a >= 1e9 {
        (n / 1e9, "B")
    } else if a >= 1e6 {
        (n / 1e6, "M")
    } else if a >= 1e4 {
        (n / 1e3, "k")
    } else {
        (n, "")
    };
    let s = if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else if v.abs() >= 100.0 {
        format!("{v:.0}")
    } else if v.abs() >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    format!("{s}{unit}")
}

/// A number as a table or a metric shows it: as written, with its digits in groups of
/// three from 10,000 up (so a year, a port or a line number stays as it is).
pub fn grouped(n: f64) -> String {
    let text = crate::eval::num_text(n);
    if !n.is_finite() || n.abs() < 10_000.0 || text.contains('e') {
        return text;
    }
    let (sign, digits) = match text.strip_prefix('-') {
        Some(d) => ("-", d),
        None => ("", text.as_str()),
    };
    let (whole, frac) = match digits.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (digits, None),
    };
    let mut out = String::from(sign);
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markdown() {
        let s = inline("Fix **the race** in `sync.rs`, see [PR](https://x.y/1) and my_var_name.");
        let parts: Vec<(&str, bool, bool, bool)> = s
            .iter()
            .map(|s| (s.text.as_str(), s.bold, s.code, s.link.is_some()))
            .collect();
        assert_eq!(
            parts,
            [
                ("Fix ", false, false, false),
                ("the race", true, false, false),
                (" in ", false, false, false),
                ("sync.rs", false, true, false),
                (", see ", false, false, false),
                ("PR", false, false, true),
                (" and my_var_name.", false, false, false),
            ]
        );
    }

    #[test]
    fn inline_styles_nest() {
        let s = inline("*read the file `src/main.rs`* and **the `cap` is [here](https://x.y)**");
        let parts: Vec<(&str, bool, bool, bool, bool)> = s
            .iter()
            .map(|s| (s.text.as_str(), s.bold, s.italic, s.code, s.link.is_some()))
            .collect();
        assert_eq!(
            parts,
            [
                ("read the file ", false, true, false, false),
                ("src/main.rs", false, true, true, false),
                (" and ", false, false, false, false),
                ("the ", true, false, false, false),
                ("cap", true, false, true, false),
                (" is ", true, false, false, false),
                ("here", true, false, false, true),
            ]
        );
    }

    #[test]
    fn markers_inside_code_dont_close_emphasis() {
        let s = inline("*allow `bash:cargo test*` first*, 2 * 3 * 4, a_b_c and _so_");
        assert_eq!(s[0].text, "allow ");
        assert!(s[0].italic);
        assert_eq!(s[1].text, "bash:cargo test*");
        assert!(s[1].code && s[1].italic);
        assert_eq!(s[2].text, " first");
        assert_eq!(s[3].text, ", 2 * 3 * 4, a_b_c and ");
        assert!(!s[3].italic);
        assert_eq!(s[4].text, "so");
        assert!(s[4].italic);
    }

    #[test]
    fn unsafe_links_stay_text() {
        let s = inline("[x](javascript:alert(1))");
        assert!(s.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn markdown_blocks() {
        let b = blocks(
            "## Why\nIt was\nslow.\n\n- one\n- two\n  more\n1. a\n2. b\n> note\n```rust\nfn x() {}\n```",
        );
        assert_eq!(
            b,
            [
                Block::Heading(2, "Why".into()),
                Block::Para("It was slow.".into()),
                Block::List {
                    ordered: false,
                    items: vec!["one".into(), "two more".into()]
                },
                Block::List {
                    ordered: true,
                    items: vec!["a".into(), "b".into()]
                },
                Block::Quote("note".into()),
                Block::Code {
                    lang: "rust".into(),
                    code: "fn x() {}".into()
                },
            ]
        );
    }

    #[test]
    fn diffs_number_their_lines() {
        let rows = diff_rows("--- a/x\n+++ b/x\n@@ -10,3 +10,3 @@ fn f\n a\n-b\n+c\n d");
        let nums: Vec<(DiffKind, Option<u32>, Option<u32>)> =
            rows.iter().map(|r| (r.kind, r.old, r.new)).collect();
        assert_eq!(
            nums,
            [
                (DiffKind::Meta, None, None),
                (DiffKind::Meta, None, None),
                (DiffKind::Hunk, None, None),
                (DiffKind::Context, Some(10), Some(10)),
                (DiffKind::Del, Some(11), None),
                (DiffKind::Add, None, Some(11)),
                (DiffKind::Context, Some(12), Some(12)),
            ]
        );
    }

    #[test]
    fn diffs_number_only_what_they_know_and_drop_shared_indentation() {
        // A header without numbers, then lines left out.
        let rows = diff_rows(
            "@@ fn read @@\n         let a = 1;\n-        old();\n+        new();\n ...\n         end();",
        );
        assert!(rows.iter().all(|r| r.old.is_none() && r.new.is_none()));
        assert_eq!(rows[1].text, "let a = 1;");
        assert_eq!(rows[2].text, "old();");
        assert_eq!(rows[4].kind, DiffKind::Gap);
        // Numbers stop where lines are left out.
        let rows = diff_rows("@@ -5,3 +5,3 @@\n a\n...\n b");
        assert_eq!(rows[1].new, Some(5));
        assert_eq!(rows[3].new, None);
        // No header at all: nothing to number from.
        assert_eq!(diff_rows("-a\n+b")[0].old, None);
    }

    #[test]
    fn code_loses_the_indentation_its_lines_share() {
        assert_eq!(
            dedent("\n        if x {\n            y();\n\n        }\n"),
            "if x {\n    y();\n\n}"
        );
        assert_eq!(indent_width("\t  x"), 6);
        assert_eq!(
            line_set(&["3".into(), "7-9".into(), "x".into()]),
            [(3, 3), (7, 9)]
        );
    }

    #[test]
    fn big_numbers_are_grouped() {
        assert_eq!(grouped(2026.0), "2026");
        assert_eq!(grouped(85654.0), "85,654");
        assert_eq!(grouped(-1234567.5), "-1,234,567.5");
        assert_eq!(grouped(0.25), "0.25");
    }

    #[test]
    fn reads_file_paths() {
        assert_eq!(
            file_path("src/web.rs:120"),
            Some(("src/web.rs".into(), Some(120)))
        );
        assert_eq!(file_path("app.js"), Some(("app.js".into(), None)));
        assert_eq!(file_path("a.b"), None);
        assert_eq!(file_path("Policy::check"), None);
    }

    #[test]
    fn a_hunk_header_with_a_unicode_minus_still_numbers_its_lines() {
        let rows = diff_rows("@@ −7,2 +7,2 @@\n a");
        assert_eq!((rows[1].old, rows[1].new), (Some(7), Some(7)));
    }

    #[test]
    fn axis_ticks_are_round() {
        let (t, lo, hi) = ticks(0.0, 87.0);
        assert_eq!((lo, hi), (0.0, 100.0));
        assert_eq!(t, [0.0, 25.0, 50.0, 75.0, 100.0]);
        assert_eq!(short_num(25_000.0), "25k");
        assert_eq!(short_num(1_250_000.0), "1.25M");
        assert_eq!(short_num(0.5), "0.5");
        let (t, lo, hi) = ticks_near(411.0, 470.0);
        assert_eq!((lo, hi), (400.0, 480.0));
        assert_eq!(t, [400.0, 420.0, 440.0, 460.0, 480.0]);
    }
}
