//! The bits of HTML the page handles itself. Replies come rendered by tamale (its
//! `src/web/render.rs`: markdown, escaped, with code highlighted); the page splits them into
//! their top-level blocks so it can put live parts (artifacts, videos, copy buttons) where
//! their placeholders are, and redraw only the blocks that changed as a reply streams in.

/// One top-level block of a reply.
#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    /// An artifact: its OpenUI Lang document, drawn in place. `src` is where it's kept
    /// once `done` (`/artifact/<id>`), empty while it's still streaming in.
    Artifact {
        src: String,
        title: String,
        source: String,
        done: bool,
    },
    /// A video a reply shows, to be asked of tamale by its path.
    Video { path: String, title: String },
    /// A code block, its inside (the language label and the highlighted code).
    Code { inner: String },
    /// Anything else, as it is.
    Html(String),
}

const VOID: &[&str] = &[
    "br", "hr", "img", "input", "wbr", "col", "meta", "link", "area", "source",
];

/// Splits `html` into its top-level elements. Text between them (there's none in what tamale
/// renders) goes with the element after it.
pub fn blocks(html: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(end) = html[i..].find('>').map(|e| i + e) else {
            break;
        };
        let tag = &html[i + 1..end];
        if let Some(name) = tag.strip_prefix('/') {
            let _ = name;
            depth = depth.saturating_sub(1);
            if depth == 0 {
                out.push(block(&html[start..=end]));
                start = end + 1;
            }
        } else if !tag.starts_with('!') {
            let name: String = tag
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            let selfclosing = tag.ends_with('/') || VOID.contains(&name.as_str());
            if selfclosing {
                if depth == 0 {
                    out.push(block(&html[start..=end]));
                    start = end + 1;
                }
            } else {
                depth += 1;
            }
        }
        i = end + 1;
    }
    let rest = html[start..].trim();
    if !rest.is_empty() {
        out.push(Block::Html(rest.to_string()));
    }
    out
}

fn block(source: &str) -> Block {
    let source = source.trim_start();
    if source.starts_with("<div class=\"artifact\" data-src=\"") {
        let src = attr(source, "data-src").unwrap_or_default();
        let title = attr(source, "data-title").unwrap_or_default();
        let document = attr(source, "data-source").unwrap_or_default();
        return Block::Artifact {
            src: unescape(&src),
            title: unescape(&title),
            source: unescape(&document),
            done: attr(source, "data-done").as_deref() == Some("1"),
        };
    }
    if source.starts_with("<div class=\"video\" data-path=\"") {
        let path = attr(source, "data-path").unwrap_or_default();
        let title = attr(source, "data-title").unwrap_or_default();
        return Block::Video {
            path: unescape(&path),
            title: unescape(&title),
        };
    }
    if let Some(rest) = source.strip_prefix("<div class=\"codeblock\">")
        && let Some(inner) = rest.strip_suffix("</div>")
    {
        return Block::Code {
            inner: inner.to_string(),
        };
    }
    Block::Html(decorate(source))
}

/// The value of attribute `name` in the start tag `tag` begins with (still escaped).
fn attr(tag: &str, name: &str) -> Option<String> {
    let head = &tag[..tag.find('>')?];
    let at = head.find(&format!(" {name}=\""))? + name.len() + 3;
    Some(head[at..].split('"').next()?.to_string())
}

/// The text of some HTML: its tags dropped, its entities read.
pub fn text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(&out)
}

/// The code a code block holds, for its copy button.
pub fn code_text(inner: &str) -> String {
    match inner.find("<code>") {
        Some(at) => text(&inner[at..]),
        None => text(inner),
    }
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // An entity is short; look for its end in the first few characters (not bytes: a
        // cut inside a character would panic, and text near an `&` is often not ASCII).
        let window = rest.char_indices().nth(12).map_or(rest.len(), |(i, _)| i);
        let Some(end) = rest[..window].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Plain text with `code` spans and links, for info lines, errors and toasts.
pub fn inline(text: &str) -> String {
    let mut out = String::new();
    let mut code = false;
    for (i, part) in split_ticks(text).into_iter().enumerate() {
        let _ = i;
        match part {
            Tick::Code(c) => {
                out.push_str("<code>");
                out.push_str(&escape(c));
                out.push_str("</code>");
            }
            Tick::Text(t) => out.push_str(&links(t)),
        }
        code = !code;
    }
    out
}

enum Tick<'a> {
    Text(&'a str),
    Code(&'a str),
}

/// `text` cut at its `code` spans: a backtick, then one or more characters other than a
/// backtick or a line break, then a backtick.
fn split_ticks(text: &str) -> Vec<Tick<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let Some(open) = rest.find('`') else { break };
        let after = &rest[open + 1..];
        let close = after.find(['`', '\n']);
        match close {
            Some(c) if c > 0 && after.as_bytes()[c] == b'`' => {
                out.push(Tick::Text(&rest[..open]));
                out.push(Tick::Code(&after[..c]));
                rest = &after[c + 1..];
            }
            _ => {
                out.push(Tick::Text(&rest[..open + 1]));
                rest = after;
            }
        }
    }
    out.push(Tick::Text(rest));
    out
}

/// `text`, escaped, with its http(s) addresses as links.
fn links(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let at = [rest.find("https://"), rest.find("http://")]
            .into_iter()
            .flatten()
            .min();
        let Some(at) = at else { break };
        // A word boundary before it.
        if at > 0
            && rest[..at]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            out.push_str(&escape(&rest[..at + 4]));
            rest = &rest[at + 4..];
            continue;
        }
        out.push_str(&escape(&rest[..at]));
        let url_end = rest[at..]
            .find(|c: char| c.is_whitespace() || c == '<' || c == ')')
            .map_or(rest.len(), |e| at + e);
        let url = escape(&rest[at..url_end]);
        out.push_str(&format!(
            "<a href=\"{url}\" target=\"_blank\" rel=\"noopener noreferrer\">{url}</a>"
        ));
        rest = &rest[url_end..];
    }
    out.push_str(&escape(rest));
    out
}

/// A user message: its text, with fenced blocks (quoted code) set as code.
pub fn user_html(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.split_inclusive('\n').peekable();
    let mut plain = String::new();
    while let Some(line) = lines.next() {
        if line.starts_with("```") {
            // Look for its closing fence.
            let mut body = String::new();
            let mut closed = false;
            let mut taken: Vec<&str> = Vec::new();
            for next in lines.by_ref() {
                taken.push(next);
                if next.starts_with("```")
                    && next.trim_end().trim_start_matches('`').trim().is_empty()
                {
                    closed = true;
                    break;
                }
                body.push_str(next);
            }
            if closed {
                out.push_str(&inline(plain.trim_end_matches('\n')));
                plain.clear();
                out.push_str("<pre><code>");
                out.push_str(&escape(body.strip_suffix('\n').unwrap_or(&body)));
                out.push_str("</code></pre>");
                // Line breaks right after a block are the block's.
                while lines.peek().is_some_and(|l| l.trim().is_empty()) {
                    lines.next();
                }
            } else {
                plain.push_str(line);
                for t in taken {
                    plain.push_str(t);
                }
            }
        } else {
            plain.push_str(line);
        }
    }
    out.push_str(&inline(&plain));
    out
}

/// The file some text names (see `openui::text::file_path`): replies and artifacts read
/// paths the same way.
pub use openui::text::file_path;

/// Marks inline `code` that names a file as a link to the code view.
fn decorate(html: &str) -> String {
    if !html.contains("<code>") {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    let mut pre = 0usize;
    loop {
        let next_code = rest.find("<code>");
        let next_pre = rest.find("<pre");
        let next_pre_end = rest.find("</pre>");
        let Some(at) = next_code else { break };
        // Code inside a `pre` is a block, not a name.
        if let Some(p) = next_pre.filter(|p| *p < at) {
            out.push_str(&rest[..p + 4]);
            rest = &rest[p + 4..];
            pre += 1;
            continue;
        }
        if let Some(p) = next_pre_end.filter(|p| *p < at) {
            out.push_str(&rest[..p + 6]);
            rest = &rest[p + 6..];
            pre = pre.saturating_sub(1);
            continue;
        }
        out.push_str(&rest[..at]);
        rest = &rest[at + 6..];
        let Some(end) = rest.find("</code>") else {
            out.push_str("<code>");
            continue;
        };
        let inner = &rest[..end];
        match (pre, file_path(&text(inner))) {
            (0, Some((path, line))) => {
                let title = match line {
                    Some(l) => format!("Open {path}, line {l} in the code view"),
                    None => format!("Open {path} in the code view"),
                };
                out.push_str(&format!(
                    "<code class=\"pathlink\" data-path=\"{}\" title=\"{}\">",
                    escape(&text(inner)),
                    escape(&title)
                ));
            }
            _ => out.push_str("<code>"),
        }
        out.push_str(inner);
        out.push_str("</code>");
        rest = &rest[end + 7..];
    }
    out.push_str(rest);
    out
}

/// A file name from an artifact's title.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(60).collect();
    if out.is_empty() {
        "artifact".into()
    } else {
        out
    }
}

/// A short stable hash, for keys and element ids.
pub fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:x}")
}

/// Whether tool output reads as a diff.
pub fn looks_like_diff(output: &str) -> bool {
    output
        .lines()
        .any(|l| l.starts_with("@@ ") || l.starts_with("+++ ") || l.starts_with("--- "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_beside_text_that_isnt_ascii() {
        // An artifact's document, in an attribute: entities next to arrows and dashes.
        assert_eq!(
            unescape("&quot;Model reply → render.rs &amp; −943 “x” &bogus→→→→→→→→ &#8594;"),
            "\"Model reply → render.rs & −943 “x” &bogus→→→→→→→→ →"
        );
        assert_eq!(unescape("&→→→→→→→→→→→→→→;"), "&→→→→→→→→→→→→→→;");
    }

    #[test]
    fn splits_a_reply_into_its_blocks() {
        let html = "<p>Hi <code>src/web.rs:12</code></p><div class=\"codeblock\"><div class=\"codeblock-lang\">rust</div><pre><code><span class=\"t-kw\">fn</span> x &lt;</code></pre></div>\
            <hr /><div class=\"artifact\" data-src=\"/artifact/abc\" data-title=\"A &lt;b&gt;\" data-done=\"1\" data-source=\"title = &quot;A &lt;b&gt;&quot;\nroot = Card([TextContent(&#39;x &gt; y&#39;)])\"></div><ul><li>a<ul><li>b</li></ul></li></ul>";
        let b = blocks(html);
        assert_eq!(b.len(), 5, "{b:?}");
        assert!(
            matches!(&b[0], Block::Html(h) if h.contains("class=\"pathlink\" data-path=\"src/web.rs:12\""))
        );
        assert!(matches!(&b[1], Block::Code { inner } if code_text(inner) == "fn x <"));
        assert!(
            matches!(&b[3], Block::Artifact { src, title, source, done: true }
                if src == "/artifact/abc" && title == "A <b>"
                    && source == "title = \"A <b>\"\nroot = Card([TextContent('x > y')])")
        );
        assert!(
            matches!(&b[4], Block::Html(h) if h.ends_with("</ul></ul>") || h.ends_with("</li></ul>"))
        );
    }

    #[test]
    fn reads_file_paths() {
        assert_eq!(
            file_path("src/web.rs:120"),
            Some(("src/web.rs".into(), Some(120)))
        );
        assert_eq!(file_path("app.js"), Some(("app.js".into(), None)));
        assert_eq!(file_path("a.b"), None);
        assert_eq!(file_path("x/y.rs#L4-L9"), Some(("x/y.rs".into(), Some(4))));
        assert_eq!(file_path("./a/b.toml"), Some(("./a/b.toml".into(), None)));
        assert_eq!(file_path("hello world.rs"), None);
        assert_eq!(
            file_path("src/x.rs:12:4"),
            Some(("src/x.rs".into(), Some(12)))
        );
    }

    #[test]
    fn inline_text_and_user_messages() {
        assert_eq!(inline("run `ls` <now>"), "run <code>ls</code> &lt;now&gt;");
        assert!(inline("see https://a.b/c).").contains("<a href=\"https://a.b/c\""));
        assert_eq!(
            user_html("look:\n```\nfn x() {}\n```\nok"),
            "look:<pre><code>fn x() {}</code></pre>ok"
        );
        assert_eq!(slug("A Big Title!"), "a-big-title");
    }
}
