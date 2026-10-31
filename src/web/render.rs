//! HTML for the web UI: assistant markdown and syntax-highlighted code.
//!
//! Highlighting emits flat `<span class="t-…">` runs per line, named after a small set of
//! token kinds rather than a theme's colours, so the page's stylesheet decides how code
//! looks. Raw HTML in markdown is shown as text: model output never becomes markup.

use std::fmt::Write;
use std::path::Path;
use std::sync::OnceLock;

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Larger files are shown without highlighting, which would take too long.
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Token kinds by scope prefix, most specific first. Comments and strings win over
/// whatever is nested inside them (quotes, escapes aside); everything else goes by the
/// innermost scope that matches.
const KINDS: &[(&str, &str)] = &[
    ("constant.character.escape", "e"),
    ("constant.numeric", "n"),
    ("constant.language", "k"),
    ("constant", "k"),
    ("keyword.operator", "o"),
    ("keyword", "kw"),
    ("storage.type", "kw"),
    ("storage.modifier", "kw"),
    ("storage", "kw"),
    ("entity.name.function", "fn"),
    ("support.function", "fn"),
    ("variable.function", "fn"),
    ("meta.function-call", "fn"),
    ("entity.name.type", "ty"),
    ("entity.name.class", "ty"),
    ("entity.name.struct", "ty"),
    ("entity.name.enum", "ty"),
    ("entity.name.trait", "ty"),
    ("entity.name.impl", "ty"),
    ("entity.other.inherited-class", "ty"),
    ("support.type", "ty"),
    ("support.class", "ty"),
    ("entity.name.tag", "tag"),
    ("entity.other.attribute-name", "attr"),
    ("entity.name.namespace", "ty"),
    ("entity.name", "fn"),
    ("variable.parameter", "param"),
    ("variable.language", "k"),
    ("support.constant", "k"),
    ("markup.heading", "kw"),
    ("markup.bold", "b"),
    ("markup.italic", "i"),
    ("markup.inserted", "add"),
    ("markup.deleted", "del"),
    ("meta.diff.range", "hd"),
    ("meta.diff.header", "hd"),
    ("markup.raw", "s"),
    ("punctuation", "p"),
];

struct Kinds {
    comment: Scope,
    string: Scope,
    escape: Scope,
    quote: Scope,
    others: Vec<(Scope, &'static str)>,
}

fn kinds() -> &'static Kinds {
    static KINDS_SCOPES: OnceLock<Kinds> = OnceLock::new();
    KINDS_SCOPES.get_or_init(|| Kinds {
        comment: Scope::new("comment").unwrap(),
        string: Scope::new("string").unwrap(),
        escape: Scope::new("constant.character.escape").unwrap(),
        quote: Scope::new("punctuation.definition.string").unwrap(),
        others: KINDS
            .iter()
            .map(|(s, k)| (Scope::new(s).unwrap(), *k))
            .collect(),
    })
}

/// The token kind for text under `stack`, if it has one.
fn kind(stack: &ScopeStack) -> Option<&'static str> {
    let k = kinds();
    let scopes = stack.as_slice();
    if scopes.iter().any(|s| k.comment.is_prefix_of(*s)) {
        return Some("c");
    }
    if scopes.iter().any(|s| k.string.is_prefix_of(*s)) {
        let top = scopes.last()?;
        return Some(
            if k.escape.is_prefix_of(*top) && !k.quote.is_prefix_of(*top) {
                "e"
            } else {
                "s"
            },
        );
    }
    scopes.iter().rev().find_map(|scope| {
        k.others
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(*scope))
            .map(|(_, kind)| *kind)
    })
}

/// The syntax for a file, by extension, then whole name, then first line.
fn syntax_for_path(path: &Path, content: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|e| ss.find_syntax_by_extension(e))
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| ss.find_syntax_by_first_line(content.lines().next().unwrap_or("")))
        .filter(|s| s.name != "Plain Text")
}

fn syntax_for_token(token: &str) -> Option<&'static SyntaxReference> {
    let token = token.split([',', ' ', '{']).next().unwrap_or("").trim();
    if token.is_empty() {
        return None;
    }
    let ss = syntaxes();
    let alias = match token.to_ascii_lowercase().as_str() {
        "ts" | "typescript" | "tsx" => "js",
        "jsx" => "js",
        "shell" | "zsh" | "console" => "sh",
        "toml" => "ini",
        "jsonc" | "json5" => "json",
        _ => token,
    }
    .to_string();
    ss.find_syntax_by_token(&alias)
        .filter(|s| s.name != "Plain Text")
}

/// A file as highlighted HTML, one string per line, and the syntax's name.
pub fn highlight_file(path: &Path, content: &str) -> (Vec<String>, Option<String>) {
    let syntax = if content.len() <= MAX_HIGHLIGHT_BYTES {
        syntax_for_path(path, content)
    } else {
        None
    };
    (highlight(syntax, content), syntax.map(|s| s.name.clone()))
}

/// `code` as HTML lines; plain (escaped) text without a syntax or if parsing fails.
fn highlight(syntax: Option<&SyntaxReference>, code: &str) -> Vec<String> {
    let plain = || code.lines().map(escape).collect();
    let Some(syntax) = syntax else {
        return plain();
    };
    let ss = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let Ok(ops) = state.parse_line(line, ss) else {
            return plain();
        };
        let text = line.trim_end_matches(['\n', '\r']);
        // Runs of one kind, merged where scopes change without changing the kind.
        let mut runs: Vec<(Option<&str>, &str)> = Vec::new();
        let mut push = |kind: Option<&'static str>, start: usize, end: usize| match runs.last_mut()
        {
            Some((last, run)) if *last == kind => {
                *run = &text[start - run.len()..end];
            }
            _ => runs.push((kind, &text[start..end])),
        };
        let mut start = 0;
        for (at, op) in ops {
            let at = at.min(text.len());
            if at > start {
                push(kind(&stack), start, at);
                start = at;
            }
            if stack.apply(&op).is_err() {
                return plain();
            }
        }
        if start < text.len() {
            push(kind(&stack), start, text.len());
        }
        let mut html = String::with_capacity(text.len() * 2);
        for (kind, run) in runs {
            push_run(&mut html, kind, run);
        }
        lines.push(html);
    }
    lines
}

fn push_run(html: &mut String, kind: Option<&str>, text: &str) {
    match kind {
        Some(kind) => {
            let _ = write!(html, "<span class=\"t-{kind}\">");
            escape_into(html, text);
            html.push_str("</span>");
        }
        None => escape_into(html, text),
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Links that can't run script: web, mail, in-page and relative ones.
fn safe_href(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    match lower.find(':') {
        Some(colon) => {
            let scheme = &lower[..colon];
            // A colon after a path or query character isn't a scheme.
            scheme.contains(['/', '?', '#']) || matches!(scheme, "http" | "https" | "mailto")
        }
        None => true,
    }
}

/// Assistant markdown as HTML.
pub fn markdown(text: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let mut out = String::with_capacity(text.len() * 3 / 2);
    // The code block being collected: its language token and text.
    let mut code: Option<(String, String)> = None;
    let mut table: Vec<Alignment> = Vec::new();
    let mut in_head = false;
    let mut cell = 0usize;
    for event in Parser::new_ext(text, options) {
        if let Some((_, body)) = &mut code {
            match event {
                Event::Text(t) => body.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, body) = code.take().unwrap();
                    push_code_block(&mut out, &lang, &body);
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => out.push_str("<p>"),
                Tag::Heading { level, .. } => {
                    let _ = write!(out, "<{level}>");
                }
                Tag::BlockQuote(kind) => match kind {
                    Some(kind) => {
                        let (class, label) = match kind {
                            BlockQuoteKind::Note => ("note", "Note"),
                            BlockQuoteKind::Tip => ("tip", "Tip"),
                            BlockQuoteKind::Important => ("important", "Important"),
                            BlockQuoteKind::Warning => ("warning", "Warning"),
                            BlockQuoteKind::Caution => ("caution", "Caution"),
                        };
                        let _ = write!(
                            out,
                            "<blockquote class=\"callout {class}\"><div class=\"callout-title\">{label}</div>"
                        );
                    }
                    None => out.push_str("<blockquote>"),
                },
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    code = Some((lang, String::new()));
                }
                Tag::List(Some(1)) => out.push_str("<ol>"),
                Tag::List(Some(start)) => {
                    let _ = write!(out, "<ol start=\"{start}\">");
                }
                Tag::List(None) => out.push_str("<ul>"),
                Tag::Item => out.push_str("<li>"),
                Tag::Table(alignments) => {
                    table = alignments;
                    out.push_str("<div class=\"table\"><table>");
                }
                Tag::TableHead => {
                    in_head = true;
                    cell = 0;
                    out.push_str("<thead><tr>");
                }
                Tag::TableRow => {
                    cell = 0;
                    out.push_str("<tr>");
                }
                Tag::TableCell => {
                    let tag = if in_head { "th" } else { "td" };
                    let align = match table.get(cell) {
                        Some(Alignment::Left) => " class=\"al-l\"",
                        Some(Alignment::Center) => " class=\"al-c\"",
                        Some(Alignment::Right) => " class=\"al-r\"",
                        _ => "",
                    };
                    let _ = write!(out, "<{tag}{align}>");
                }
                Tag::Emphasis => out.push_str("<em>"),
                Tag::Strong => out.push_str("<strong>"),
                Tag::Strikethrough => out.push_str("<del>"),
                Tag::Link {
                    dest_url, title, ..
                } => {
                    if safe_href(&dest_url) {
                        out.push_str("<a href=\"");
                        escape_into(&mut out, &dest_url);
                        out.push('"');
                        if !title.is_empty() {
                            out.push_str(" title=\"");
                            escape_into(&mut out, &title);
                            out.push('"');
                        }
                        out.push_str(" target=\"_blank\" rel=\"noopener noreferrer\">");
                    } else {
                        out.push_str("<a>");
                    }
                }
                // Shown as a link labelled with the alt text, not loaded.
                Tag::Image { dest_url, .. } => {
                    if safe_href(&dest_url) {
                        out.push_str("<a class=\"img\" href=\"");
                        escape_into(&mut out, &dest_url);
                        out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
                    } else {
                        out.push_str("<a class=\"img\">");
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => out.push_str("</p>"),
                TagEnd::Heading(level) => {
                    let _ = write!(out, "</{level}>");
                }
                TagEnd::BlockQuote(_) => out.push_str("</blockquote>"),
                TagEnd::List(true) => out.push_str("</ol>"),
                TagEnd::List(false) => out.push_str("</ul>"),
                TagEnd::Item => out.push_str("</li>"),
                TagEnd::Table => out.push_str("</tbody></table></div>"),
                TagEnd::TableHead => {
                    in_head = false;
                    out.push_str("</tr></thead><tbody>");
                }
                TagEnd::TableRow => out.push_str("</tr>"),
                TagEnd::TableCell => {
                    out.push_str(if in_head { "</th>" } else { "</td>" });
                    cell += 1;
                }
                TagEnd::Emphasis => out.push_str("</em>"),
                TagEnd::Strong => out.push_str("</strong>"),
                TagEnd::Strikethrough => out.push_str("</del>"),
                TagEnd::Link | TagEnd::Image => out.push_str("</a>"),
                _ => {}
            },
            Event::Text(t) => escape_into(&mut out, &t),
            Event::Code(t) => {
                out.push_str("<code>");
                escape_into(&mut out, &t);
                out.push_str("</code>");
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                out.push_str("<code class=\"math\">");
                escape_into(&mut out, &t);
                out.push_str("</code>");
            }
            // Shown as written rather than interpreted.
            Event::Html(t) | Event::InlineHtml(t) => escape_into(&mut out, &t),
            Event::FootnoteReference(name) => {
                out.push_str("<sup>[");
                escape_into(&mut out, &name);
                out.push_str("]</sup>");
            }
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr>"),
            Event::TaskListMarker(done) => out.push_str(if done {
                "<input type=\"checkbox\" checked disabled> "
            } else {
                "<input type=\"checkbox\" disabled> "
            }),
        }
    }
    // A code block still streaming in.
    if let Some((lang, body)) = code {
        push_code_block(&mut out, &lang, &body);
    }
    out
}

fn push_code_block(out: &mut String, lang: &str, body: &str) {
    let syntax = syntax_for_token(lang);
    let label = lang.split([',', ' ', '{']).next().unwrap_or("").trim();
    out.push_str("<div class=\"codeblock\">");
    if !label.is_empty() {
        out.push_str("<div class=\"codeblock-lang\">");
        escape_into(out, label);
        out.push_str("</div>");
    }
    out.push_str("<pre><code>");
    let body = body.strip_suffix('\n').unwrap_or(body);
    let lines = highlight(syntax, body);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    out.push_str("</code></pre></div>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_escapes_raw_html_and_unsafe_links() {
        let html = markdown(
            "<script>alert(1)</script>\n\n[x](javascript:alert(1)) [y](https://a.b/?q=\"1\")",
        );
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("<a>x</a>"), "{html}");
        assert!(
            html.contains("href=\"https://a.b/?q=&quot;1&quot;\""),
            "{html}"
        );
    }

    #[test]
    fn markdown_renders_blocks_tables_and_callouts() {
        let html = markdown(
            "# Title\n\n- a\n- [x] b\n\n| l | r |\n|:--|--:|\n| 1 | 2 |\n\n> [!WARNING]\n> careful",
        );
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<ul><li>a</li>"), "{html}");
        assert!(html.contains("checked disabled"), "{html}");
        assert!(html.contains("<th class=\"al-l\">l</th>"), "{html}");
        assert!(html.contains("<td class=\"al-r\">2</td>"), "{html}");
        assert!(html.contains("callout warning"), "{html}");
    }

    #[test]
    fn code_blocks_are_highlighted_even_while_streaming() {
        let html = markdown("```rust\nfn main() { let s = \"<hi>\"; }\n```");
        assert!(html.contains("codeblock-lang\">rust<"), "{html}");
        assert!(html.contains("<span class=\"t-kw\">fn</span>"), "{html}");
        assert!(
            html.contains("<span class=\"t-s\">&quot;&lt;hi&gt;&quot;</span>"),
            "{html}"
        );
        let open = markdown("```python\nx = 1");
        assert!(open.contains("<span class=\"t-n\">1</span>"), "{open}");
    }

    #[test]
    fn files_highlight_per_line_and_name_their_syntax() {
        let (lines, syntax) = highlight_file(Path::new("a.rs"), "// hi\nlet x = 1;\n");
        assert_eq!(syntax.as_deref(), Some("Rust"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "<span class=\"t-c\">// hi</span>");
        let (lines, syntax) = highlight_file(Path::new("notes.unknownext"), "a < b\n");
        assert_eq!(syntax, None);
        assert_eq!(lines, vec!["a &lt; b"]);
    }

    #[test]
    fn only_script_free_links_are_safe() {
        assert!(safe_href("https://x.y"));
        assert!(safe_href("src/main.rs"));
        assert!(safe_href("#top"));
        assert!(safe_href("a/b:c"));
        assert!(!safe_href("javascript:alert(1)"));
        assert!(!safe_href(" JavaScript:alert(1)"));
        assert!(!safe_href("data:text/html,x"));
    }
}
