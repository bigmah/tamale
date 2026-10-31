//! Render assistant markdown into styled lines already wrapped to the chat width.
//! Wrapping here (rather than in Paragraph) is what lets list items, quotes and code
//! blocks keep their indent or gutter on continuation rows.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::OnceLock;

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter, ScopeSelectors, StyleModifier,
    Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Cyan;
const FRAME: Style = Style::new().fg(Color::DarkGray);
const INLINE_CODE: Style = Style::new().fg(Color::LightYellow);
const PLAIN_CODE: Style = Style::new().fg(Color::LightYellow);

/// Where rendering can resume once more text is appended: the start of the last
/// top-level block, the only one that later text can still change.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tail {
    pub byte: usize,
    pub line: usize,
    /// Whether the block is separated from the one before it by a blank line.
    pub gap: bool,
}

/// Render `text` into `out`. `gap` asks for a blank line before the first block, as
/// when resuming from a [`Tail`] that followed earlier blocks.
pub(super) fn render<'a>(text: &str, width: u16, gap: bool, out: &mut Vec<Line<'a>>) -> Tail {
    let base = out.len();
    let mut r = Renderer {
        width: (width as usize).max(1),
        out,
        containers: Vec::new(),
        lists: Vec::new(),
        styles: vec![Style::new()],
        links: Vec::new(),
        inline: Vec::new(),
        code: None,
        table: None,
        gap,
    };
    let mut tail = Tail {
        byte: 0,
        line: 0,
        gap,
    };
    let mut depth = 0usize;
    let mut started = false;
    let last_line = text.rfind('\n').map_or(0, |i| i + 1);
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        // A block starting on the unfinished last line may still join the one before
        // it, as a partial table row does, so the tail stays on that earlier block.
        if depth == 0 && matches!(event, Event::Start(_) | Event::Rule) {
            let byte = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
            if byte < last_line || !started {
                started = true;
                tail = Tail {
                    byte,
                    line: r.out.len() - base,
                    gap: r.gap,
                };
            }
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth -= 1,
            _ => {}
        }
        r.event(event);
    }
    r.flush_inline(None);
    tail
}

enum Container {
    Quote(Style),
    /// A list item: its marker until the first row is emitted, then an indent.
    Item {
        marker: Option<Span<'static>>,
        indent: usize,
    },
}

struct Code {
    lang: String,
    text: String,
}

struct Table {
    aligns: Vec<Alignment>,
    head: usize,
    rows: Vec<Vec<Vec<Span<'static>>>>,
}

struct Renderer<'o, 'a> {
    width: usize,
    out: &'o mut Vec<Line<'a>>,
    containers: Vec<Container>,
    /// The next number of each open list, `None` for bullets.
    lists: Vec<Option<u64>>,
    styles: Vec<Style>,
    /// Destination and text so far of each open link.
    links: Vec<(String, String)>,
    /// Pending inline content, one entry per source line.
    inline: Vec<Vec<Span<'static>>>,
    code: Option<Code>,
    table: Option<Table>,
    gap: bool,
}

impl Renderer<'_, '_> {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some(code) = &mut self.code {
                    code.text.push_str(&text);
                } else {
                    let style = self.style();
                    for (i, part) in text.split('\n').enumerate() {
                        if i > 0 {
                            self.inline.push(Vec::new());
                        }
                        self.push_span(Span::styled(part.to_string(), style));
                    }
                }
            }
            Event::Code(text) => {
                let style = self.style().patch(INLINE_CODE);
                self.push_span(Span::styled(text.to_string(), style));
            }
            Event::InlineHtml(html) | Event::InlineMath(html) | Event::DisplayMath(html) => {
                let style = self.style().patch(FRAME);
                self.push_span(Span::styled(html.to_string(), style));
            }
            Event::Html(html) => {
                for line in html.lines() {
                    self.emit(vec![Span::styled(line.to_string(), FRAME)]);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if self.inline.is_empty() {
                    self.inline.push(Vec::new());
                }
                self.inline.push(Vec::new());
            }
            Event::Rule => {
                self.start_block();
                let rule = "─".repeat(self.avail());
                self.emit(vec![Span::styled(rule, FRAME)]);
                self.gap = true;
            }
            Event::TaskListMarker(done) => {
                if let Some(Container::Item { marker, .. }) = self.containers.last_mut() {
                    *marker = Some(if done {
                        Span::styled("☑ ", Style::new().fg(Color::Green))
                    } else {
                        Span::styled("☐ ", Style::new().fg(ACCENT))
                    });
                }
            }
            Event::FootnoteReference(name) => {
                let style = self.style().patch(FRAME);
                self.push_span(Span::styled(format!("[^{name}]"), style));
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::Heading { .. } | Tag::HtmlBlock => self.start_block(),
            Tag::BlockQuote(kind) => {
                self.start_block();
                let (label, color) = match kind {
                    None => ("", Color::DarkGray),
                    Some(BlockQuoteKind::Note) => ("Note", Color::Blue),
                    Some(BlockQuoteKind::Tip) => ("Tip", Color::Green),
                    Some(BlockQuoteKind::Important) => ("Important", Color::Magenta),
                    Some(BlockQuoteKind::Warning) => ("Warning", Color::Yellow),
                    Some(BlockQuoteKind::Caution) => ("Caution", Color::Red),
                };
                self.containers
                    .push(Container::Quote(Style::new().fg(color)));
                if !label.is_empty() {
                    let style = Style::new().fg(color).add_modifier(Modifier::BOLD);
                    self.emit(vec![Span::styled(label, style)]);
                }
            }
            Tag::CodeBlock(kind) => {
                self.start_block();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split([' ', ',', '{']).next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some(Code {
                    lang,
                    text: String::new(),
                });
            }
            Tag::List(start) => {
                self.start_block();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush_inline(None);
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => format!("{} ", ["•", "◦", "▪"][depth % 3]),
                };
                self.containers.push(Container::Item {
                    indent: marker.width(),
                    marker: Some(Span::styled(marker, Style::new().fg(ACCENT))),
                });
            }
            Tag::Table(aligns) => {
                self.start_block();
                self.table = Some(Table {
                    aligns,
                    head: 0,
                    rows: Vec::new(),
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => self.inline.clear(),
            Tag::Emphasis => self.push_style(Style::new().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::new().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push((dest_url.to_string(), String::new()));
                self.push_style(
                    Style::new()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { .. } => {
                let style = self.style().patch(FRAME);
                self.push_span(Span::styled("[image: ", style));
                self.push_style(FRAME.add_modifier(Modifier::ITALIC));
            }
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => self.push_style(self.style()),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_inline(None);
                self.gap = true;
            }
            TagEnd::Heading(level) => {
                self.flush_inline(Some(level));
                self.gap = true;
            }
            TagEnd::HtmlBlock => self.gap = true,
            TagEnd::BlockQuote(_) => {
                self.flush_inline(None);
                self.containers.pop();
                self.gap = true;
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    self.code_block(&code);
                }
                self.gap = true;
            }
            TagEnd::List(_) => {
                self.flush_inline(None);
                self.lists.pop();
                // Nested lists stay tight against the rest of their item.
                if !self
                    .containers
                    .iter()
                    .any(|c| matches!(c, Container::Item { .. }))
                {
                    self.gap = true;
                }
            }
            TagEnd::Item => {
                self.flush_inline(None);
                if let Some(Container::Item {
                    marker: Some(_), ..
                }) = self.containers.last()
                {
                    self.emit(Vec::new());
                }
                self.containers.pop();
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.table(table);
                }
                self.gap = true;
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.head = table.rows.len();
                }
            }
            TagEnd::TableRow => {}
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline)
                    .into_iter()
                    .reduce(|mut a, b| {
                        a.push(Span::raw(" "));
                        a.extend(b);
                        a
                    })
                    .unwrap_or_default();
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(cell);
                }
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some((url, text)) = self.links.pop() {
                    let bare = url.strip_prefix("mailto:").unwrap_or(&url);
                    if !url.is_empty() && bare != text {
                        let style = self.style().patch(FRAME);
                        self.push_span(Span::styled(format!(" ({url})"), style));
                    }
                }
            }
            TagEnd::Image => {
                self.styles.pop();
                let style = self.style().patch(FRAME);
                self.push_span(Span::styled("]", style));
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {
                self.styles.pop();
            }
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style().patch(style));
    }

    fn push_span(&mut self, span: Span<'static>) {
        for (_, text) in &mut self.links {
            text.push_str(&span.content);
        }
        if self.inline.is_empty() {
            self.inline.push(Vec::new());
        }
        self.inline.last_mut().unwrap().push(span);
    }

    /// Begin a block: emit text of a tight list item before it, then the blank
    /// line separating it from the previous block.
    fn start_block(&mut self) {
        self.flush_inline(None);
        if self.gap {
            self.gap = false;
            let mut spans: Vec<Span<'static>> = self
                .containers
                .iter()
                .map(|c| match c {
                    Container::Quote(style) => Span::styled("│ ", *style),
                    Container::Item { indent, .. } => Span::raw(" ".repeat(*indent)),
                })
                .collect();
            while spans.last().is_some_and(|s| s.content.trim().is_empty()) {
                spans.pop();
            }
            if let Some(last) = spans.last_mut() {
                last.content = last.content.trim_end().to_string().into();
            }
            self.out.push(Line::from(spans));
        }
    }

    fn prefix_width(&self) -> usize {
        self.containers
            .iter()
            .map(|c| match c {
                Container::Quote(_) => 2,
                Container::Item { indent, .. } => *indent,
            })
            .sum()
    }

    fn avail(&self) -> usize {
        self.width.saturating_sub(self.prefix_width()).max(1)
    }

    fn emit(&mut self, content: Vec<Span<'static>>) {
        let mut spans = Vec::with_capacity(self.containers.len() + content.len());
        for c in &mut self.containers {
            spans.push(match c {
                Container::Quote(style) => Span::styled("│ ", *style),
                Container::Item { marker, indent } => marker
                    .take()
                    .unwrap_or_else(|| Span::raw(" ".repeat(*indent))),
            });
        }
        spans.extend(content);
        self.out.push(Line::from(spans));
    }

    fn flush_inline(&mut self, heading: Option<HeadingLevel>) {
        if self.inline.is_empty() {
            return;
        }
        let (style, rule) = match heading {
            None => (Style::new(), None),
            Some(HeadingLevel::H1) => (
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                Some(("━", Style::new().fg(ACCENT))),
            ),
            Some(HeadingLevel::H2) => (
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                Some(("─", FRAME)),
            ),
            Some(HeadingLevel::H3) => (Style::new().fg(ACCENT).add_modifier(Modifier::BOLD), None),
            Some(_) => (
                Style::new().add_modifier(Modifier::BOLD | Modifier::ITALIC),
                None,
            ),
        };
        let avail = self.avail();
        let mut widest = 0;
        for segment in std::mem::take(&mut self.inline) {
            for mut row in wrap(&segment, avail) {
                for span in &mut row {
                    span.style = style.patch(span.style);
                }
                widest = widest.max(row.iter().map(|s| s.content.width()).sum::<usize>());
                self.emit(row);
            }
        }
        if let Some((glyph, style)) = rule {
            self.emit(vec![Span::styled(glyph.repeat(widest.max(1)), style)]);
        }
    }

    fn code_block(&mut self, code: &Code) {
        let label = if code.lang.is_empty() {
            "╭─".to_string()
        } else {
            format!("╭─ {}", code.lang)
        };
        self.emit(vec![Span::styled(label, FRAME)]);
        let avail = self.avail().saturating_sub(2).max(1);
        let text = code.text.replace('\t', "    ");
        for row in highlight(&code.lang, &text) {
            for part in wrap_hard(&row, avail) {
                let mut spans = vec![Span::styled("│ ", FRAME)];
                spans.extend(part);
                self.emit(spans);
            }
        }
        self.emit(vec![Span::styled("╰─", FRAME)]);
    }

    fn table(&mut self, table: Table) {
        let cols = table
            .rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(table.aligns.len());
        if cols == 0 {
            return;
        }
        let bold = |cell: &[Span<'static>], header: bool| -> Vec<Span<'static>> {
            cell.iter()
                .cloned()
                .map(|mut s| {
                    if header {
                        s.style = s.style.add_modifier(Modifier::BOLD);
                    }
                    s
                })
                .collect()
        };
        let mut widths = vec![1; cols];
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(spans_width(cell));
            }
        }
        let avail = self.avail();
        let overhead = 3 * cols + 1;
        if avail < overhead + 3 * cols {
            // Too narrow for a grid: one wrapped line per row.
            for (r, row) in table.rows.iter().enumerate() {
                let mut line = Vec::new();
                for (i, cell) in row.iter().enumerate() {
                    if i > 0 {
                        line.push(Span::styled(" │ ", FRAME));
                    }
                    line.extend(bold(cell, r < table.head));
                }
                for wrapped in wrap(&line, avail) {
                    self.emit(wrapped);
                }
            }
            return;
        }
        while widths.iter().sum::<usize>() + overhead > avail {
            let widest = (0..cols).max_by_key(|&i| widths[i]).unwrap();
            widths[widest] -= 1;
        }
        let border = |left: &str, mid: &str, right: &str| {
            let inner: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            Span::styled(format!("{left}{}{right}", inner.join(mid)), FRAME)
        };
        self.emit(vec![border("╭", "┬", "╮")]);
        for (r, row) in table.rows.iter().enumerate() {
            let header = r < table.head;
            let cells: Vec<Vec<Vec<Span<'static>>>> = (0..cols)
                .map(|i| {
                    let cell = row.get(i).map_or(&[][..], Vec::as_slice);
                    wrap(&bold(cell, header), widths[i])
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for h in 0..height {
                let mut line = vec![Span::styled("│", FRAME)];
                for (i, cell) in cells.iter().enumerate() {
                    let content = cell.get(h).cloned().unwrap_or_default();
                    let pad = widths[i].saturating_sub(spans_width(&content));
                    let left = match table.aligns.get(i) {
                        Some(Alignment::Right) => pad,
                        Some(Alignment::Center) => pad / 2,
                        _ => 0,
                    };
                    line.push(Span::raw(" ".repeat(left + 1)));
                    line.extend(content);
                    line.push(Span::raw(" ".repeat(pad - left + 1)));
                    line.push(Span::styled("│", FRAME));
                }
                self.emit(line);
            }
            if r + 1 == table.head && table.rows.len() > table.head {
                self.emit(vec![border("├", "┼", "┤")]);
            }
        }
        self.emit(vec![border("╰", "┴", "╯")]);
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// Append `text` to `row`, extending the last span when the style matches.
fn push_text(row: &mut Vec<Span<'static>>, text: &str, style: Style) {
    match row.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(text),
        _ => row.push(Span::styled(text.to_string(), style)),
    }
}

struct Word {
    space: bool,
    width: usize,
    parts: Vec<(String, Style)>,
}

/// Greedy word wrap across styled spans, splitting words longer than `width`.
fn wrap(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let mut words: Vec<Word> = Vec::new();
    for span in spans {
        for g in span.content.graphemes(true) {
            let space = g.chars().all(char::is_whitespace);
            if words.last().is_none_or(|w| w.space != space) {
                words.push(Word {
                    space,
                    width: 0,
                    parts: Vec::new(),
                });
            }
            let word = words.last_mut().unwrap();
            word.width += g.width();
            match word.parts.last_mut() {
                Some((text, style)) if *style == span.style => text.push_str(g),
                _ => word.parts.push((g.to_string(), span.style)),
            }
        }
    }
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut used = 0;
    let mut space: Option<&Word> = None;
    for word in &words {
        if word.space {
            if used > 0 {
                space = Some(word);
            }
            continue;
        }
        let gap = space.map_or(0, |s| s.width);
        if used > 0 && used + gap + word.width > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        } else if let Some(s) = space {
            for (text, style) in &s.parts {
                push_text(&mut row, text, *style);
            }
            used += gap;
        }
        space = None;
        for (text, style) in &word.parts {
            for g in text.graphemes(true) {
                let w = g.width();
                if used > 0 && used + w > width {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                push_text(&mut row, g, *style);
                used += w;
            }
        }
    }
    rows.push(row);
    rows
}

/// Split a row into chunks of at most `width` columns, keeping spaces.
fn wrap_hard(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut used = 0;
    for span in spans {
        for g in span.content.graphemes(true) {
            let w = g.width();
            if used > 0 && used + w > width {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            push_text(&mut row, g, span.style);
            used += w;
        }
    }
    rows.push(row);
    rows
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// A theme of terminal palette colors, so code follows the user's color scheme.
/// Colors are smuggled through syntect as palette indexes in `r` with `a == 0`.
fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let ansi = |index: u8| syntect::highlighting::Color {
            r: index,
            g: 0,
            b: 0,
            a: 0,
        };
        let rules: &[(&str, u8, FontStyle)] = &[
            (
                "comment, punctuation.definition.comment",
                8,
                FontStyle::ITALIC,
            ),
            ("string, constant.character", 2, FontStyle::empty()),
            (
                "constant.numeric, constant.language, constant.character.escape",
                3,
                FontStyle::empty(),
            ),
            (
                "keyword, storage, keyword.operator.word",
                5,
                FontStyle::empty(),
            ),
            ("keyword.operator, punctuation", 7, FontStyle::empty()),
            (
                "entity.name.function, support.function, meta.function-call variable.function",
                4,
                FontStyle::empty(),
            ),
            (
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
                 support.type, support.class, entity.other.inherited-class",
                6,
                FontStyle::empty(),
            ),
            (
                "entity.name.tag, entity.name.section, markup.heading",
                4,
                FontStyle::BOLD,
            ),
            (
                "entity.other.attribute-name, variable.parameter",
                6,
                FontStyle::empty(),
            ),
            ("markup.inserted", 2, FontStyle::empty()),
            ("markup.deleted, invalid", 1, FontStyle::empty()),
            ("meta.diff.range, meta.diff.header", 6, FontStyle::empty()),
        ];
        Theme {
            settings: ThemeSettings {
                foreground: Some(syntect::highlighting::Color::WHITE),
                ..ThemeSettings::default()
            },
            scopes: rules
                .iter()
                .map(|(scope, color, font)| ThemeItem {
                    scope: scope.parse::<ScopeSelectors>().unwrap(),
                    style: StyleModifier {
                        foreground: Some(ansi(*color)),
                        background: None,
                        font_style: Some(*font),
                    },
                })
                .collect(),
            ..Theme::default()
        }
    })
}

fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let lang = lang.to_ascii_lowercase();
    let token = match lang.as_str() {
        "" | "text" | "txt" | "plain" | "plaintext" => return None,
        "ts" | "tsx" | "typescript" | "jsx" | "mjs" | "cjs" => "js",
        "shell" | "zsh" | "console" | "sh" => "bash",
        "jsonc" | "json5" => "json",
        "yml" => "yaml",
        "golang" => "go",
        "c++" => "cpp",
        "patch" => "diff",
        other => other,
    };
    syntaxes().find_syntax_by_token(token)
}

/// Highlighted code blocks, kept so a resize or a streamed chunk resumes from the
/// last highlighted line instead of re-highlighting the whole block.
struct Highlighted {
    lang: String,
    /// The complete lines highlighted so far.
    done: String,
    rows: Vec<Vec<Span<'static>>>,
    parse: ParseState,
    state: HighlightState,
}

const HIGHLIGHT_CACHE: usize = 256;

thread_local! {
    static HIGHLIGHTED: RefCell<VecDeque<Highlighted>> = const { RefCell::new(VecDeque::new()) };
}

/// Style code by language, one row of spans per line.
fn highlight(lang: &str, code: &str) -> Vec<Vec<Span<'static>>> {
    let Some(syntax) = find_syntax(lang) else {
        let code = code.strip_suffix('\n').unwrap_or(code);
        return code
            .split('\n')
            .map(|l| vec![Span::styled(l.to_string(), PLAIN_CODE)])
            .collect();
    };
    let highlighter = Highlighter::new(theme());
    HIGHLIGHTED.with_borrow_mut(|cache| {
        let found = cache
            .iter()
            .enumerate()
            .filter(|(_, h)| h.lang == lang && code.starts_with(h.done.as_str()))
            .max_by_key(|(_, h)| h.done.len())
            .map(|(i, _)| i);
        let mut entry = match found.and_then(|i| cache.remove(i)) {
            Some(entry) => entry,
            None => Highlighted {
                lang: lang.to_string(),
                done: String::new(),
                rows: Vec::new(),
                parse: ParseState::new(syntax),
                state: HighlightState::new(&highlighter, ScopeStack::new()),
            },
        };
        let rest = &code[entry.done.len()..];
        let complete = rest.rfind('\n').map_or(0, |i| i + 1);
        for line in LinesWithEndings::from(&rest[..complete]) {
            let row = highlight_line(&mut entry.parse, &mut entry.state, &highlighter, line);
            entry.rows.push(row);
        }
        entry.done.push_str(&rest[..complete]);
        let mut rows = entry.rows.clone();
        let partial = &rest[complete..];
        if !partial.is_empty() {
            let (mut parse, mut state) = (entry.parse.clone(), entry.state.clone());
            rows.push(highlight_line(
                &mut parse,
                &mut state,
                &highlighter,
                partial,
            ));
        }
        if rows.is_empty() {
            rows.push(Vec::new());
        }
        cache.push_back(entry);
        if cache.len() > HIGHLIGHT_CACHE {
            cache.pop_front();
        }
        rows
    })
}

fn highlight_line(
    parse: &mut ParseState,
    state: &mut HighlightState,
    highlighter: &Highlighter<'_>,
    line: &str,
) -> Vec<Span<'static>> {
    let mut row = Vec::new();
    let ops = parse.parse_line(line, syntaxes()).unwrap_or_default();
    for (style, text) in HighlightIterator::new(state, &ops, line, highlighter) {
        let text = text.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            continue;
        }
        let mut s = Style::new();
        if style.foreground.a == 0 {
            s = s.fg(palette(style.foreground.r));
        }
        if style.font_style.contains(FontStyle::BOLD) {
            s = s.add_modifier(Modifier::BOLD);
        }
        if style.font_style.contains(FontStyle::ITALIC) {
            s = s.add_modifier(Modifier::ITALIC);
        }
        push_text(&mut row, text, s);
    }
    row
}

fn palette(index: u8) -> Color {
    match index {
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        _ => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(md: &str, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        render(md, width, false, &mut out);
        out.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn lists_keep_their_indent_when_wrapping() {
        assert_eq!(
            text(
                "- one two three four\n  - nested item\n- [x] done\n\n1. first",
                14
            ),
            [
                "• one two",
                "  three four",
                "  ◦ nested",
                "    item",
                "☑ done",
                "",
                "1. first",
            ]
        );
    }

    #[test]
    fn quotes_code_and_rules() {
        assert_eq!(
            text("> quoted\n> text\n\n```\nlet x = 1;\n```\n\n---", 12),
            [
                "│ quoted",
                "│ text",
                "",
                "╭─",
                "│ let x = 1;",
                "╰─",
                "",
                "────────────",
            ]
        );
    }

    #[test]
    fn tables_align_and_shrink_to_fit() {
        assert_eq!(
            text("| a | long header |\n|--:|---|\n| 1 | x |", 40),
            [
                "╭───┬─────────────╮",
                "│ a │ long header │",
                "├───┼─────────────┤",
                "│ 1 │ x           │",
                "╰───┴─────────────╯",
            ]
        );
        assert_eq!(
            text("| a | long header |\n|---|---|\n| 1 | x |", 15),
            [
                "╭───┬─────────╮",
                "│ a │ long    │",
                "│   │ header  │",
                "├───┼─────────┤",
                "│ 1 │ x       │",
                "╰───┴─────────╯",
            ]
        );
    }

    #[test]
    fn headings_links_and_soft_breaks() {
        assert_eq!(
            text(
                "# Title\nline one\nline two [site](https://x.y) <https://a.b>",
                80
            ),
            [
                "Title",
                "━━━━━",
                "",
                "line one",
                "line two site (https://x.y) https://a.b",
            ]
        );
    }

    #[test]
    fn highlights_known_languages() {
        let rows = highlight("rust", "fn main() {}\n// hi");
        assert_eq!(rows.len(), 2);
        assert!(rows[0].iter().any(|s| s.style.fg == Some(Color::Magenta)));
        assert_eq!(rows[1][0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn resuming_from_the_tail_matches_a_full_render() {
        let text = "Intro para\nwith soft break\n\n## Changes\n\n1. First item\n   with continuation\n\n\
                    2. Second item\n\n   ```diff\n   - old\n   + new\n   ```\n3. Third\n\n- loose a\n\n\
                    - loose b\n  - nested *em* **strong** ~~gone~~\n    - deeper `code`\n\n\
                    > outer\n> > inner quote\n> back\n\n***\n\n<details>\n<summary>x</summary>\n</details>\n\n\
                    | File | Lines | Notes |\n|:-----|------:|:-----:|\n\
                    | `src/app/markdown.rs` | 600 | a long note that wraps |\n| b | 2 | |\n\n\
                    Header\n======\n\n    indented code\n\nDone [link](http://example.com) and ![img](x.png).\n";
        for width in [1, 6, 13, 30, 100] {
            let mut tail = Tail {
                byte: 0,
                line: 0,
                gap: false,
            };
            let mut lines = Vec::new();
            for end in text
                .char_indices()
                .map(|(i, _)| i)
                .skip(1)
                .chain([text.len()])
            {
                let prefix = &text[..end];
                lines.truncate(tail.line);
                let next = render(&prefix[tail.byte..], width, tail.gap, &mut lines);
                tail = Tail {
                    byte: tail.byte + next.byte,
                    line: tail.line + next.line,
                    gap: next.gap,
                };
                let mut full = Vec::new();
                render(prefix, width, false, &mut full);
                let strings =
                    |lines: &[Line]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
                assert_eq!(
                    strings(&lines),
                    strings(&full),
                    "width {width}, prefix {prefix:?}"
                );
            }
        }
    }
}
