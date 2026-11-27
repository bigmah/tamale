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
/// `[text](https://…)`. Anything else, including unclosed markers, is text.
pub fn inline(s: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let mut rest = s;
    let flush = |plain: &mut String, out: &mut Vec<Span>| {
        if !plain.is_empty() {
            out.push(Span {
                text: std::mem::take(plain),
                ..Default::default()
            });
        }
    };
    while let Some(c) = rest.chars().next() {
        let marked = match c {
            '`' => rest[1..].find('`').map(|end| {
                (
                    Span {
                        text: rest[1..1 + end].to_string(),
                        code: true,
                        ..Default::default()
                    },
                    end + 2,
                )
            }),
            '*' if rest.starts_with("**") => rest[2..].find("**").filter(|e| *e > 0).map(|end| {
                (
                    Span {
                        text: rest[2..2 + end].to_string(),
                        bold: true,
                        ..Default::default()
                    },
                    end + 4,
                )
            }),
            '*' | '_' => rest[1..]
                .find(c)
                .filter(|e| *e > 0 && !rest[1..].starts_with(' '))
                // `snake_case_names` aren't emphasis.
                .filter(|_| c == '*' || !plain.ends_with(|p: char| p.is_alphanumeric()))
                .map(|end| {
                    (
                        Span {
                            text: rest[1..1 + end].to_string(),
                            italic: true,
                            ..Default::default()
                        },
                        end + 2,
                    )
                }),
            '[' => link(rest),
            _ => None,
        };
        match marked {
            Some((span, len)) => {
                flush(&mut plain, &mut out);
                out.push(span);
                rest = &rest[len..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut plain, &mut out);
    out
}

fn link(rest: &str) -> Option<(Span, usize)> {
    let close = rest.find("](")?;
    let end = close + 2 + rest[close + 2..].find(')')?;
    let url = rest[close + 2..end].trim();
    if !safe_url(url) {
        return None;
    }
    Some((
        Span {
            text: rest[1..close].to_string(),
            link: Some(url.to_string()),
            ..Default::default()
        },
        end + 1,
    ))
}

/// Whether a link may be followed: http(s) and mailto only.
pub fn safe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
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
}

#[derive(Clone, PartialEq, Debug)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// A unified diff's rows, with old and new line numbers from its hunk headers.
pub fn diff_rows(patch: &str) -> Vec<DiffRow> {
    let (mut old, mut new) = (1u32, 1u32);
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
            let start = |p: Option<&str>| -> u32 {
                // After its sign, which a model may write as any dash.
                p.and_then(|p| {
                    p.chars()
                        .skip(1)
                        .collect::<String>()
                        .split(',')
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap_or(1)
            };
            old = start(parts.next());
            new = start(parts.next());
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
            out.push(row(DiffKind::Add, None, Some(new), t));
            new += 1;
        } else if let Some(t) = line.strip_prefix('-') {
            out.push(row(DiffKind::Del, Some(old), None, t));
            old += 1;
        } else if line.starts_with('\\') {
            out.push(row(DiffKind::Meta, None, None, line));
        } else {
            let t = line.strip_prefix(' ').unwrap_or(line);
            out.push(row(DiffKind::Context, Some(old), Some(new), t));
            old += 1;
            new += 1;
        }
    }
    out
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
    }
}
