//! `/diff`: a full-screen viewer of `git diff` with a file tree and the selected file's diff.

use std::collections::{BTreeMap, HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

use super::{App, AppEvent, SPINNER, WHEEL_LINES};
use crate::diff::{self, FileDiff, Kind, Status, Target};
use crate::session::Entry;

pub(super) const ADDED_BG: Color = Color::Rgb(18, 48, 28);
const ADDED_EMPHASIS: Color = Color::Rgb(36, 100, 52);
const REMOVED_BG: Color = Color::Rgb(58, 20, 24);
const REMOVED_EMPHASIS: Color = Color::Rgb(122, 38, 46);
const HUNK_BG: Color = Color::Rgb(24, 34, 52);
const CURSOR_BG: Color = Color::Rgb(48, 48, 62);
const SELECTION_BG: Color = Color::Rgb(52, 64, 112);
/// Columns moved per ←/→ in the diff pane.
const HSCROLL_STEP: usize = 8;
const TAB: &str = "    ";
/// Unchanged lines shown around each change.
const CONTEXT: usize = 3;
/// Hidden lines revealed per expand.
const EXPAND_STEP: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Files,
    Diff,
}

enum Node {
    /// A directory, by its full path.
    Dir(String),
    File(usize),
}

struct Row {
    depth: usize,
    name: String,
    node: Node,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    /// A diff line, by index into the file's lines.
    Line(usize),
    /// Hidden unchanged lines `start..end` (indexes into the file's lines).
    Gap {
        start: usize,
        end: usize,
    },
    Other,
}

/// Unchanged lines the user expanded in a file, by new line number.
#[derive(Default)]
struct Reveal {
    all: bool,
    lines: HashSet<u32>,
}

/// The selected file's diff, laid out once: a fixed gutter and horizontally scrolled text.
struct Rendered {
    file: usize,
    gutter: Vec<Line<'static>>,
    text: Vec<Line<'static>>,
    /// Background filling each row across the pane.
    backgrounds: Vec<Option<Color>>,
    rows: Vec<RowKind>,
    /// Row of each hunk header.
    hunks: Vec<usize>,
    gutter_width: u16,
}

impl Rendered {
    fn push(
        &mut self,
        gutter: Line<'static>,
        text: Line<'static>,
        bg: Option<Color>,
        kind: RowKind,
    ) {
        self.gutter.push(gutter);
        self.text.push(text);
        self.backgrounds.push(bg);
        self.rows.push(kind);
    }
}

pub(super) struct Viewer {
    target: Target,
    /// Increments per load, so only the newest result is applied.
    generation: u64,
    loading: bool,
    /// `None` until the first load finishes.
    files: Option<Vec<FileDiff>>,
    error: Option<String>,
    collapsed: HashSet<String>,
    rows: Vec<Row>,
    selected: usize,
    /// The file shown in the diff pane; stays put while a directory is selected.
    file: Option<usize>,
    focus: Focus,
    scroll: usize,
    /// The diff pane's largest scroll offset at the last draw.
    max_scroll: usize,
    /// The diff pane's cursor row, moved with j/k.
    cursor: usize,
    /// Where a visual line selection (`v`) started; it runs to the cursor.
    visual: Option<usize>,
    /// Set by keyboard moves so the next draw scrolls the cursor into view; otherwise
    /// (after the wheel) the cursor is kept on screen instead.
    follow_cursor: bool,
    hscroll: usize,
    tree_scroll: usize,
    /// Set by keyboard moves so the next draw scrolls the selection into view.
    follow: bool,
    rendered: Option<Rendered>,
    /// Expanded unchanged lines per file path; kept across reloads.
    reveals: HashMap<String, Reveal>,
    /// Pane areas from the last draw, for the mouse and page sizes.
    tree_area: Rect,
    diff_area: Rect,
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, usize)>,
}

impl Viewer {
    fn new(target: Target) -> Self {
        Self {
            target,
            generation: 0,
            loading: false,
            files: None,
            error: None,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            file: None,
            focus: Focus::Files,
            scroll: 0,
            max_scroll: 0,
            cursor: 0,
            visual: None,
            follow_cursor: true,
            hscroll: 0,
            tree_scroll: 0,
            follow: true,
            rendered: None,
            reveals: HashMap::new(),
            tree_area: Rect::default(),
            diff_area: Rect::default(),
        }
    }

    fn files(&self) -> &[FileDiff] {
        self.files.as_deref().unwrap_or(&[])
    }

    /// Rebuilds the visible tree rows: directories first, single-child chains merged.
    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        flatten(&tree(self.files()), "", 0, &self.collapsed, &mut rows);
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    /// Files in tree order, ignoring collapsed directories.
    fn file_order(&self) -> Vec<usize> {
        let mut rows = Vec::new();
        flatten(&tree(self.files()), "", 0, &HashSet::new(), &mut rows);
        rows.into_iter()
            .filter_map(|r| match r.node {
                Node::File(i) => Some(i),
                Node::Dir(_) => None,
            })
            .collect()
    }

    fn select_row(&mut self, row: usize) {
        self.selected = row.min(self.rows.len().saturating_sub(1));
        if let Some(Row {
            node: Node::File(i),
            ..
        }) = self.rows.get(self.selected)
        {
            self.show_file(*i);
        }
    }

    fn show_file(&mut self, file: usize) {
        if self.file != Some(file) {
            self.file = Some(file);
            self.scroll = 0;
            self.cursor = 0;
            self.visual = None;
            self.hscroll = 0;
        }
    }

    /// Shows a file, expanding the directories it is in.
    fn select_file(&mut self, file: usize) {
        let Some(path) = self.files().get(file).map(|f| f.path.clone()) else {
            return;
        };
        self.collapsed
            .retain(|dir| !path.starts_with(&format!("{dir}/")));
        self.rebuild_rows();
        if let Some(row) = self
            .rows
            .iter()
            .position(|r| matches!(r.node, Node::File(i) if i == file))
        {
            self.selected = row;
        }
        self.show_file(file);
    }

    fn step_file(&mut self, forward: bool) {
        let order = self.file_order();
        if order.is_empty() {
            return;
        }
        let at = self.file.and_then(|f| order.iter().position(|&i| i == f));
        let next = match (at, forward) {
            (None, _) => 0,
            (Some(at), true) => (at + 1).min(order.len() - 1),
            (Some(at), false) => at.saturating_sub(1),
        };
        self.select_file(order[next]);
    }

    fn toggle_dir(&mut self, expand: Option<bool>) {
        let Some(Row {
            node: Node::Dir(path),
            ..
        }) = self.rows.get(self.selected)
        else {
            return;
        };
        let path = path.clone();
        let collapse = expand.map_or(!self.collapsed.contains(&path), |e| !e);
        if collapse {
            self.collapsed.insert(path);
        } else {
            self.collapsed.remove(&path);
        }
        self.rebuild_rows();
    }

    /// Selects the directory containing the selected row; false at the top level.
    fn select_parent(&mut self) -> bool {
        let Some(depth) = self.rows.get(self.selected).map(|r| r.depth) else {
            return false;
        };
        match self.rows[..self.selected]
            .iter()
            .rposition(|r| r.depth < depth)
        {
            Some(row) => {
                self.selected = row;
                true
            }
            None => false,
        }
    }

    fn page(&self) -> usize {
        (self.diff_area.height as usize).saturating_sub(2).max(1)
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    fn move_cursor(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta);
        self.follow_cursor = true;
    }

    /// Moves the cursor and the view together, like Ctrl+D in vim.
    fn page_by(&mut self, delta: isize) {
        self.scroll_by(delta);
        self.move_cursor(delta);
    }

    fn step_hunk(&mut self, forward: bool) {
        let Some(rendered) = &self.rendered else {
            return;
        };
        let target = if forward {
            rendered.hunks.iter().find(|&&h| h > self.cursor)
        } else {
            rendered.hunks.iter().rev().find(|&&h| h < self.cursor)
        };
        match target {
            Some(&row) => {
                self.cursor = row;
                self.follow_cursor = true;
                // Show the hunk from its header down.
                self.scroll = row;
            }
            // Past the last hunk, continue with the next file.
            None if forward => self.step_file(true),
            None => {}
        }
    }

    /// Applies a finished load, keeping the selected file (by path) and its position.
    fn on_loaded(&mut self, files: Vec<FileDiff>) {
        let path = self
            .file
            .and_then(|i| self.files().get(i))
            .map(|f| f.path.clone());
        let first_load = self.files.is_none();
        self.files = Some(files);
        self.rendered = None;
        self.error = None;
        self.rebuild_rows();
        let same = path.and_then(|p| self.files().iter().position(|f| f.path == p));
        match same {
            Some(i) => {
                self.file = Some(i);
                if let Some(row) = self
                    .rows
                    .iter()
                    .position(|r| matches!(r.node, Node::File(f) if f == i))
                {
                    self.selected = row;
                }
            }
            None => {
                self.file = None;
                if first_load || self.files().is_empty() {
                    self.selected = 0;
                }
                match self.file_order().first() {
                    Some(&first) if first_load => self.select_file(first),
                    _ => self.select_row(self.selected),
                }
            }
        }
    }

    fn render(&mut self) {
        let Some(index) = self.file else {
            self.rendered = None;
            return;
        };
        if self.rendered.as_ref().is_some_and(|r| r.file == index) {
            return;
        }
        let Some(file) = self.files().get(index) else {
            return;
        };
        self.rendered = Some(render_file(index, file, self.reveals.get(&file.path)));
    }

    /// The selected rows (cursor and visual anchor), clamped to the file's rows.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.visual?;
        let last = self.rendered.as_ref()?.rows.len().checked_sub(1)?;
        let (cursor, anchor) = (self.cursor.min(last), anchor.min(last));
        Some((cursor.min(anchor), cursor.max(anchor)))
    }

    /// The selected lines quoted for a prompt: the path, line numbers and a diff block.
    fn selection_text(&self) -> Option<String> {
        let (first, last) = self.selection()?;
        let rendered = self.rendered.as_ref()?;
        let file = self.files().get(self.file?)?;
        let lines: Vec<_> = file.hunks.iter().flat_map(|h| &h.lines).collect();
        let mut body = String::new();
        let (mut new, mut old) = (Vec::new(), Vec::new());
        for kind in &rendered.rows[first..=last] {
            match *kind {
                RowKind::Line(i) => {
                    let line = lines[i];
                    let sign = match line.kind {
                        Kind::Added => '+',
                        Kind::Removed => '-',
                        Kind::Context | Kind::NoNewline => ' ',
                    };
                    body.push(sign);
                    body.push_str(line.text.trim_end_matches('\r'));
                    body.push('\n');
                    new.extend(line.new);
                    old.extend(line.old);
                }
                RowKind::Gap { start, end } => {
                    body.push_str(&format!(" … {} unchanged lines\n", end - start));
                }
                RowKind::Other => {}
            }
        }
        if body.is_empty() {
            return None;
        }
        let range = |nums: &[u32], what: &str| {
            let (a, b) = (nums.iter().min().unwrap(), nums.iter().max().unwrap());
            if a == b {
                format!("{what} {a}")
            } else {
                format!("{what}s {a}–{b}")
            }
        };
        let range = if !new.is_empty() {
            range(&new, "line")
        } else {
            range(&old, "removed line")
        };
        Some(format!(
            "In `{}` ({range}):\n```diff\n{body}```\n\n",
            file.path
        ))
    }

    /// Reveals up to `EXPAND_STEP` hidden lines of the gap at `row`, next to the changes
    /// around it; small gaps open fully.
    fn expand_gap(&mut self, row: usize) {
        let (Some(index), Some(rendered)) = (self.file, &self.rendered) else {
            return;
        };
        let Some(&RowKind::Gap { start, end }) = rendered.rows.get(row) else {
            return;
        };
        let Some(file) = self.files.as_ref().and_then(|f| f.get(index)) else {
            return;
        };
        let lines: Vec<_> = file.hunks.iter().flat_map(|h| &h.lines).collect();
        let half = EXPAND_STEP / 2;
        let indexes: Vec<usize> = if end - start <= EXPAND_STEP {
            (start..end).collect()
        } else if start == 0 {
            (end - EXPAND_STEP..end).collect()
        } else if end == lines.len() {
            (start..start + EXPAND_STEP).collect()
        } else {
            (start..start + half).chain(end - half..end).collect()
        };
        let reveal = self.reveals.entry(file.path.clone()).or_default();
        reveal
            .lines
            .extend(indexes.into_iter().filter_map(|i| lines[i].new));
        self.rendered = None;
    }

    /// Shows the whole file, or goes back to just the changes.
    fn toggle_full_file(&mut self) {
        let Some(path) = self
            .file
            .and_then(|i| self.files().get(i))
            .map(|f| f.path.clone())
        else {
            return;
        };
        let reveal = self.reveals.entry(path).or_default();
        if reveal.all {
            *reveal = Reveal::default();
        } else {
            reveal.all = true;
        }
        self.rendered = None;
    }
}

fn tree(files: &[FileDiff]) -> Dir {
    let mut root = Dir::default();
    for (i, file) in files.iter().enumerate() {
        let mut dir = &mut root;
        let mut parts: Vec<&str> = file.path.split('/').collect();
        let name = parts.pop().unwrap_or_default().to_string();
        for part in parts {
            dir = dir.dirs.entry(part.to_string()).or_default();
        }
        dir.files.push((name, i));
    }
    root
}

fn flatten(
    dir: &Dir,
    prefix: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    rows: &mut Vec<Row>,
) {
    for (name, mut child) in &dir.dirs {
        let mut name = name.clone();
        // Merge chains like `src/app` into one row.
        while child.files.is_empty() && child.dirs.len() == 1 {
            let (next, grandchild) = child.dirs.iter().next().unwrap();
            name = format!("{name}/{next}");
            child = grandchild;
        }
        let path = format!("{prefix}{name}");
        rows.push(Row {
            depth,
            name: name.clone(),
            node: Node::Dir(path.clone()),
        });
        if !collapsed.contains(&path) {
            flatten(child, &format!("{path}/"), depth + 1, collapsed, rows);
        }
    }
    let mut files: Vec<_> = dir.files.iter().collect();
    files.sort();
    for (name, i) in files {
        rows.push(Row {
            depth,
            name: name.clone(),
            node: Node::File(*i),
        });
    }
}

fn status_color(status: Status) -> Color {
    match status {
        Status::Added | Status::Untracked => Color::Green,
        Status::Deleted => Color::Red,
        Status::Modified => Color::Yellow,
        Status::Renamed => Color::Magenta,
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Added => "added",
        Status::Deleted => "deleted",
        Status::Modified => "modified",
        Status::Renamed => "renamed",
        Status::Untracked => "untracked",
    }
}

/// Expands tabs and hides carriage returns so columns line up.
fn clean(text: &str) -> String {
    text.trim_end_matches('\r').replace('\t', TAB)
}

/// The header for the visible run of lines starting at `start`: git's `@@` ranges and the
/// closest earlier line that starts a definition (git's default section heading).
fn run_header(lines: &[&diff::DiffLine], shown: &[bool], start: usize) -> (String, String) {
    let end = (start..lines.len())
        .find(|&i| !shown[i])
        .unwrap_or(lines.len());
    let run = &lines[start..end];
    let first = |side: fn(&diff::DiffLine) -> Option<u32>| {
        // Before an insertion at the top of a side, git counts from the previous line.
        run.iter().find_map(|l| side(l)).unwrap_or_else(|| {
            lines[..start]
                .iter()
                .rev()
                .find_map(|l| side(l))
                .unwrap_or(0)
        })
    };
    let count =
        |side: fn(&diff::DiffLine) -> Option<u32>| run.iter().filter(|l| side(l).is_some()).count();
    let header = format!(
        "@@ -{},{} +{},{} @@",
        first(|l| l.old),
        count(|l| l.old),
        first(|l| l.new),
        count(|l| l.new)
    );
    let section = lines[..start]
        .iter()
        .rev()
        .filter(|l| l.kind != Kind::Removed)
        .find(|l| {
            l.text
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        })
        .map_or(String::new(), |l| clean(&l.text));
    (header, section)
}

/// Which lines are shown: those near a change, plus any the user revealed.
fn shown_lines(lines: &[&diff::DiffLine], reveal: Option<&Reveal>) -> Vec<bool> {
    if reveal.is_some_and(|r| r.all) {
        return vec![true; lines.len()];
    }
    let mut shown = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if matches!(line.kind, Kind::Added | Kind::Removed) {
            let end = (i + CONTEXT + 1).min(lines.len());
            shown[i.saturating_sub(CONTEXT)..end].fill(true);
        } else if let (Some(reveal), Some(new)) = (reveal, line.new)
            && reveal.lines.contains(&new)
        {
            shown[i] = true;
        }
    }
    for i in 1..lines.len() {
        // `\ No newline at end of file` belongs to the line before it.
        if lines[i].kind == Kind::NoNewline {
            shown[i] = shown[i - 1];
        }
    }
    shown
}

fn render_file(index: usize, file: &FileDiff, reveal: Option<&Reveal>) -> Rendered {
    let lines: Vec<&diff::DiffLine> = file.hunks.iter().flat_map(|h| &h.lines).collect();
    let max_line = lines
        .iter()
        .flat_map(|l| [l.old, l.new])
        .flatten()
        .max()
        .unwrap_or(0);
    let width = max_line.to_string().len().max(3);
    let mut rendered = Rendered {
        file: index,
        gutter: Vec::new(),
        text: Vec::new(),
        backgrounds: Vec::new(),
        rows: Vec::new(),
        hunks: Vec::new(),
        gutter_width: (width * 2 + 4) as u16,
    };
    let note = |rendered: &mut Rendered, text: String| {
        rendered.push(
            Line::default(),
            Line::from(text).fg(Color::DarkGray).italic(),
            None,
            RowKind::Other,
        );
    };
    if let Some(old) = &file.old_path {
        note(&mut rendered, format!("Renamed from {old}"));
    }
    if let Some((old, new)) = &file.mode_change {
        note(&mut rendered, format!("Mode changed {old} → {new}"));
    }
    if file.binary {
        note(&mut rendered, "Binary file not shown".into());
    } else if lines.is_empty() && file.old_path.is_none() && file.mode_change.is_none() {
        note(&mut rendered, "Empty file".into());
    }
    if !rendered.text.is_empty() && !lines.is_empty() {
        rendered.push(Line::default(), Line::default(), None, RowKind::Other);
    }

    let shown = shown_lines(&lines, reveal);
    let texts: Vec<String> = lines.iter().map(|l| clean(&l.text)).collect();
    let emphasis = pair_changes(&lines, &texts);
    let hunk_style = Style::new().bg(HUNK_BG);
    let header_spans = |start: usize, shown: &[bool]| {
        let (header, section) = run_header(&lines, shown, start);
        let mut spans = vec![Span::from(header).fg(Color::Cyan)];
        if !section.is_empty() {
            spans.push(Span::from(format!(" {section}")).fg(Color::Gray));
        }
        spans
    };
    let mut i = 0;
    while i < lines.len() {
        if !shown[i] {
            // Hidden unchanged lines collapse into one row that expands them.
            let start = i;
            while i < lines.len() && !shown[i] {
                i += 1;
            }
            let hidden = i - start;
            let mut text = vec![
                Span::from(format!(
                    "↕ {hidden} unchanged line{}",
                    if hidden == 1 { "" } else { "s" }
                ))
                .fg(Color::Blue),
            ];
            if i < lines.len() {
                rendered.hunks.push(rendered.text.len());
                text.push(Span::from("  "));
                text.extend(header_spans(i, &shown));
            }
            rendered.push(
                Line::from(format!("{:>w$}   ", "⋯", w = width * 2 + 1))
                    .fg(Color::Blue)
                    .style(hunk_style),
                Line::from(text).style(hunk_style),
                Some(HUNK_BG),
                RowKind::Gap { start, end: i },
            );
            continue;
        }
        if i == 0 {
            rendered.hunks.push(rendered.text.len());
            rendered.push(
                Line::from("").style(hunk_style),
                Line::from(header_spans(0, &shown)).style(hunk_style),
                Some(HUNK_BG),
                RowKind::Other,
            );
        }

        let line = lines[i];
        i += 1;
        if line.kind == Kind::NoNewline {
            rendered.push(
                Line::default(),
                Line::from(line.text.trim_start_matches("\\ ").to_string())
                    .fg(Color::DarkGray)
                    .italic(),
                None,
                RowKind::Other,
            );
            continue;
        }
        let num = |n: Option<u32>| n.map_or(" ".repeat(width), |n| format!("{n:>width$}"));
        let (sign, sign_color, bg, strong) = match line.kind {
            Kind::Added => ("+", Color::Green, Some(ADDED_BG), ADDED_EMPHASIS),
            Kind::Removed => ("-", Color::Red, Some(REMOVED_BG), REMOVED_EMPHASIS),
            Kind::Context | Kind::NoNewline => (" ", Color::DarkGray, None, Color::Reset),
        };
        let line_style = bg.map_or(Style::new(), |bg| Style::new().bg(bg));
        let gutter = Line::from(vec![
            Span::from(format!("{} {} ", num(line.old), num(line.new))).fg(Color::DarkGray),
            Span::from(format!("{sign} ")).fg(sign_color).bold(),
        ])
        .style(line_style);
        let text = &texts[i - 1];
        let spans = match emphasis[i - 1] {
            Some((start, end)) => {
                let chars: Vec<char> = text.chars().collect();
                let part = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
                vec![
                    Span::from(part(0, start)),
                    Span::from(part(start, end)).bg(strong),
                    Span::from(part(end, chars.len())),
                ]
            }
            None => vec![Span::from(text.clone())],
        };
        let mut text_line = Line::from(spans).style(line_style);
        if line.kind == Kind::Context {
            text_line = text_line.fg(Color::Gray);
        }
        rendered.push(gutter, text_line, bg, RowKind::Line(i - 1));
    }
    rendered
}

/// For each line, the character range to emphasize: removed lines directly followed by
/// added lines are paired in order, and their differing middles highlighted.
fn pair_changes(lines: &[&diff::DiffLine], texts: &[String]) -> Vec<Option<(usize, usize)>> {
    let mut out = vec![None; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != Kind::Removed {
            i += 1;
            continue;
        }
        let removed_start = i;
        while i < lines.len() && lines[i].kind == Kind::Removed {
            i += 1;
        }
        let added_start = i;
        while i < lines.len() && lines[i].kind == Kind::Added {
            i += 1;
        }
        let pairs = (added_start - removed_start).min(i - added_start);
        for k in 0..pairs {
            let (old, new) = (removed_start + k, added_start + k);
            if let Some((a, b)) = diff::inline_change(&texts[old], &texts[new]) {
                // An empty range on one side (pure insertion) needs no emphasis.
                out[old] = (a.0 < a.1).then_some(a);
                out[new] = (b.0 < b.1).then_some(b);
            }
        }
    }
    out
}

/// Truncates to `max` columns, ending with `…` when shortened.
fn fit(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if out.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) + 1 > max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

impl App {
    /// `/diff [args]`: shows the Diff tab, comparing what `args` asks for.
    pub(super) fn open_diff(&mut self, args: &str) {
        let target = Target::parse(args);
        if self.diff.as_ref().is_none_or(|v| v.target != target) {
            self.diff = Some(Viewer::new(target));
        }
        self.diff_open = true;
        self.reload_diff();
    }

    /// Switches to the Diff tab, keeping its place from last time.
    pub(super) fn show_diff(&mut self) {
        if self.diff.is_none() {
            self.diff = Some(Viewer::new(Target::parse("")));
        }
        self.diff_open = true;
        self.reload_diff();
    }

    /// Loads uncommitted changes in the background so the tab bar can show them.
    pub fn preload_diff(&mut self) {
        if self.diff.is_none() {
            self.diff = Some(Viewer::new(Target::parse("")));
            self.reload_diff();
        }
    }

    /// Reloads the viewer in the background, whether or not it's showing.
    pub(super) fn reload_diff(&mut self) {
        let Some(viewer) = &mut self.diff else {
            return;
        };
        viewer.generation += 1;
        viewer.loading = true;
        let (generation, target) = (viewer.generation, viewer.target.clone());
        let dir = self.workspace.cwd.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = diff::load(&dir, &target).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Diff(generation, result));
        });
    }

    pub(super) fn diff_loading(&self) -> bool {
        self.diff_open && self.diff.as_ref().is_some_and(|v| v.loading)
    }

    pub(super) fn on_diff(&mut self, generation: u64, result: Result<Vec<FileDiff>, String>) {
        let Some(viewer) = &mut self.diff else {
            return;
        };
        if generation != viewer.generation {
            return;
        }
        viewer.loading = false;
        match result {
            Ok(files) => viewer.on_loaded(files),
            // Nothing to show yet, so report it in the chat instead.
            Err(error) if viewer.files.is_none() && self.diff_open => {
                viewer.error = Some(error.clone());
                self.diff_open = false;
                self.entries.push(Entry::Error(format!("Diff: {error}")));
            }
            Err(error) => viewer.error = Some(error),
        }
    }

    /// Goes back to the chat with `quote` added to the prompt, ready for a question.
    fn ask_about(&mut self, quote: &str) {
        self.diff_open = false;
        if !self.input.trim().is_empty() && !self.input.ends_with("\n\n") {
            self.input.push_str(if self.input.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            });
        }
        self.input.push_str(quote);
        self.cursor = self.input.len();
        self.scroll_back = 0;
    }

    /// The diff viewer's Chat/Diff tab bar; `None` until there's something to show.
    pub(super) fn tab_spans(&self) -> Option<Vec<Span<'static>>> {
        self.diff.as_ref()?.files.as_ref()?;
        Some(vec![
            Span::from(" ← ").fg(Color::DarkGray),
            Span::styled(" Chat ", Style::new().fg(Color::Gray)),
            Span::styled(" Diff ", Style::new().fg(Color::Black).bg(Color::Cyan).bold()),
        ])
    }

    /// What the footer's Diff tab says about the changes; `None` outside a git repository.
    pub(super) fn diff_stats(&self) -> Option<Vec<Span<'static>>> {
        let files = self.diff.as_ref()?.files.as_ref()?;
        if files.is_empty() {
            return Some(vec![Span::from("clean ").fg(Color::DarkGray)]);
        }
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        Some(vec![
            Span::from(format!("{} ", files.len())).fg(Color::Yellow),
            Span::from(format!("+{added} ")).fg(Color::Green),
            Span::from(format!("−{removed} ")).fg(Color::Red),
        ])
    }

    pub(super) fn on_diff_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(viewer) = &mut self.diff else {
            return;
        };
        // Keys act on the laid-out rows, even before the first draw.
        viewer.render();
        let page = viewer.page() as isize;
        viewer.follow = true;
        // Visual line mode: j/k extend the selection, which goes to the chat or clipboard.
        if viewer.visual.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('v' | 'V') => {
                    viewer.visual = None;
                    return;
                }
                KeyCode::Char('c') if ctrl => {
                    viewer.visual = None;
                    return;
                }
                KeyCode::Enter | KeyCode::Char('a') => {
                    if let Some(quote) = viewer.selection_text() {
                        viewer.visual = None;
                        self.ask_about(&quote);
                    }
                    return;
                }
                KeyCode::Char('y') => {
                    if let Some(quote) = viewer.selection_text() {
                        viewer.visual = None;
                        self.write_clipboard(quote);
                    }
                    return;
                }
                // Moving files or hunks ends the selection; scrolling keys extend it.
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('[' | ']') => viewer.visual = None,
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.diff_open = false,
            KeyCode::Char('c') if ctrl => self.diff_open = false,
            KeyCode::Char('r') => self.reload_diff(),
            KeyCode::Tab | KeyCode::BackTab => {
                viewer.focus = match viewer.focus {
                    Focus::Files => Focus::Diff,
                    Focus::Diff => Focus::Files,
                }
            }
            KeyCode::Char(']') => viewer.step_file(true),
            KeyCode::Char('[') => viewer.step_file(false),
            KeyCode::Char('E') => viewer.toggle_full_file(),
            KeyCode::Char('n') => viewer.step_hunk(true),
            KeyCode::Char('p' | 'N') => viewer.step_hunk(false),
            KeyCode::PageDown | KeyCode::Char(' ') => viewer.page_by(page),
            KeyCode::PageUp => viewer.page_by(-page),
            KeyCode::Char('d') if ctrl => viewer.page_by(page / 2),
            KeyCode::Char('u') if ctrl => viewer.page_by(-page / 2),
            _ if viewer.focus == Focus::Files => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    viewer.select_row(viewer.selected.saturating_sub(1))
                }
                KeyCode::Down | KeyCode::Char('j') => viewer.select_row(viewer.selected + 1),
                KeyCode::Home | KeyCode::Char('g') => viewer.select_row(0),
                KeyCode::End | KeyCode::Char('G') => viewer.select_row(usize::MAX),
                KeyCode::Left | KeyCode::Char('h') => match viewer.rows.get(viewer.selected) {
                    Some(Row {
                        node: Node::Dir(path),
                        ..
                    }) if !viewer.collapsed.contains(path) => viewer.toggle_dir(Some(false)),
                    // At the top of the tree, ← goes back to the Chat tab.
                    _ => {
                        if !viewer.select_parent() {
                            self.diff_open = false;
                        }
                    }
                },
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                    match viewer.rows.get(viewer.selected).map(|r| &r.node) {
                        Some(Node::Dir(_)) if key.code == KeyCode::Enter => viewer.toggle_dir(None),
                        Some(Node::Dir(_)) => viewer.toggle_dir(Some(true)),
                        Some(Node::File(_)) => viewer.focus = Focus::Diff,
                        None => {}
                    }
                }
                _ => {}
            },
            _ => match key.code {
                KeyCode::Up | KeyCode::Char('k') => viewer.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => viewer.move_cursor(1),
                KeyCode::Home | KeyCode::Char('g') => {
                    viewer.cursor = 0;
                    viewer.follow_cursor = true;
                }
                // Clamped to the real maximum when drawn.
                KeyCode::End | KeyCode::Char('G') => {
                    viewer.cursor = usize::MAX;
                    viewer.follow_cursor = true;
                }
                KeyCode::Char('v' | 'V') => viewer.visual = Some(viewer.cursor),
                KeyCode::Left | KeyCode::Char('h') if viewer.hscroll == 0 => {
                    viewer.focus = Focus::Files
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    viewer.hscroll = viewer.hscroll.saturating_sub(HSCROLL_STEP)
                }
                KeyCode::Right | KeyCode::Char('l') => viewer.hscroll += HSCROLL_STEP,
                KeyCode::Enter | KeyCode::Char('e') => viewer.expand_gap(viewer.cursor),
                _ => {}
            },
        }
    }

    pub(super) fn on_diff_mouse(&mut self, mouse: MouseEvent) {
        let Some(viewer) = &mut self.diff else {
            return;
        };
        viewer.render();
        let at = Position::new(mouse.column, mouse.row);
        let in_tree = viewer.tree_area.contains(at);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if in_tree {
                    viewer.tree_scroll = if up {
                        viewer.tree_scroll.saturating_sub(WHEEL_LINES)
                    } else {
                        viewer.tree_scroll.saturating_add(WHEEL_LINES)
                    };
                } else {
                    viewer.scroll_by(if up {
                        -(WHEEL_LINES as isize)
                    } else {
                        WHEEL_LINES as isize
                    });
                    viewer.follow_cursor = false;
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_tree => {
                viewer.focus = Focus::Files;
                let row = viewer.tree_scroll + (mouse.row - viewer.tree_area.y) as usize;
                if row < viewer.rows.len() {
                    viewer.select_row(row);
                    viewer.toggle_dir(None);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if viewer.diff_area.contains(at) => {
                viewer.focus = Focus::Diff;
                let top = viewer.scroll.min(viewer.max_scroll);
                viewer.cursor = top + (mouse.row - viewer.diff_area.y) as usize;
                if viewer.visual.is_none() {
                    viewer.expand_gap(viewer.cursor);
                }
            }
            _ => {}
        }
    }

    pub(super) fn draw_diff(&mut self, frame: &mut Frame) {
        let spinner = SPINNER[self.tick % SPINNER.len()];
        let tabs = self.tab_spans().unwrap_or_default();
        let Some(viewer) = &mut self.diff else {
            return;
        };
        let area = frame.area();
        frame.render_widget(Clear, area);
        let [title_area, body, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(area);

        // Title: what is compared and the totals.
        let files = viewer.files();
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let mut title = tabs;
        title.push(Span::from(format!(" {} ", viewer.target.label())).fg(Color::Gray));
        if viewer.files.is_some() {
            title.extend([
                Span::from(format!(
                    "· {} file{} ",
                    files.len(),
                    if files.len() == 1 { "" } else { "s" }
                ))
                .fg(Color::DarkGray),
                Span::from(format!("+{added} ")).fg(Color::Green),
                Span::from(format!("−{removed}")).fg(Color::Red),
            ]);
        }
        if viewer.loading {
            title.push(Span::from(format!("  {spinner}")).fg(Color::Cyan));
        }
        if let Some(error) = &viewer.error {
            title.push(Span::from(format!("  {error}")).fg(Color::Red));
        }
        frame.render_widget(Line::from(title), title_area);

        let hint = match viewer.focus {
            _ if viewer.visual.is_some() => {
                " -- VISUAL LINE --  j/k extend · enter ask the agent · y copy · esc cancel "
            }
            Focus::Files => {
                " ↑/↓ select · ←/→ fold · enter open · tab diff pane · [/] file · n/p hunk · r refresh · esc chat "
            }
            Focus::Diff => {
                " j/k move · v select · ←/→ pan · enter expand · E whole file · tab files · [/] file · n/p hunk · r refresh · esc chat "
            }
        };
        frame.render_widget(Line::from(hint).fg(Color::DarkGray), hint_area);

        let tree_width = (body.width / 3).clamp(24.min(body.width), 48);
        let [tree_area, diff_area] =
            Layout::horizontal([Constraint::Length(tree_width), Constraint::Min(1)]).areas(body);
        let border = |focused: bool| {
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(if focused {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }))
        };

        // File tree.
        let tree_block = border(viewer.focus == Focus::Files).title(" Files ".bold());
        let tree_inner = tree_block.inner(tree_area);
        frame.render_widget(tree_block, tree_area);
        viewer.tree_area = tree_inner;
        let height = tree_inner.height as usize;
        if viewer.files.is_some() && viewer.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from("No changes").fg(Color::DarkGray).italic()),
                tree_inner,
            );
        }
        // Keyboard moves bring the selection into view; the wheel may scroll it away.
        if std::mem::take(&mut viewer.follow) && height > 0 {
            viewer.tree_scroll = viewer.tree_scroll.clamp(
                (viewer.selected + 1).saturating_sub(height),
                viewer.selected,
            );
        }
        viewer.tree_scroll = viewer
            .tree_scroll
            .min(viewer.rows.len().saturating_sub(height));
        let width = tree_inner.width as usize;
        let tree_lines: Vec<Line> = viewer
            .rows
            .iter()
            .enumerate()
            .skip(viewer.tree_scroll)
            .take(height)
            .map(|(i, row)| {
                let indent = "  ".repeat(row.depth);
                let mut spans = vec![Span::from(indent.clone())];
                let mut used = indent.width();
                match &row.node {
                    Node::Dir(path) => {
                        let icon = if viewer.collapsed.contains(path) {
                            "▸ "
                        } else {
                            "▾ "
                        };
                        spans.push(Span::from(icon).fg(Color::DarkGray));
                        let name = fit(&format!("{}/", row.name), width.saturating_sub(used + 2));
                        spans.push(Span::from(name).fg(Color::Blue).bold());
                    }
                    Node::File(f) => {
                        let file = &viewer.files()[*f];
                        let color = status_color(file.status);
                        spans.push(
                            Span::from(format!("{} ", file.status.letter()))
                                .fg(color)
                                .bold(),
                        );
                        used += 2;
                        let stats = if file.binary {
                            "bin".to_string()
                        } else {
                            format!("+{} −{}", file.added, file.removed)
                        };
                        let name = fit(&row.name, width.saturating_sub(used + stats.width() + 1));
                        used += name.width();
                        let name_style = if file.status == Status::Deleted {
                            Style::new().fg(color).add_modifier(Modifier::CROSSED_OUT)
                        } else {
                            Style::new().fg(color)
                        };
                        spans.push(Span::styled(name, name_style));
                        let pad = width.saturating_sub(used + stats.width());
                        spans.push(Span::from(" ".repeat(pad)));
                        if file.binary {
                            spans.push(Span::from(stats).fg(Color::DarkGray));
                        } else {
                            spans.push(Span::from(format!("+{}", file.added)).fg(Color::Green));
                            spans.push(Span::from(format!(" −{}", file.removed)).fg(Color::Red));
                        }
                    }
                }
                let mut line = Line::from(spans);
                if i == viewer.selected {
                    line = line.style(if viewer.focus == Focus::Files {
                        Style::new()
                            .bg(Color::Rgb(40, 60, 90))
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().bg(Color::Rgb(45, 45, 50))
                    });
                }
                line
            })
            .collect();
        frame.render_widget(Paragraph::new(tree_lines), tree_inner);

        // Diff pane.
        viewer.render();
        let file = viewer.file.and_then(|i| viewer.files().get(i));
        let mut diff_block = border(viewer.focus == Focus::Diff);
        if let Some(file) = file {
            let path = fit(&file.path, (diff_area.width as usize).saturating_sub(20));
            diff_block = diff_block.title(Line::from(vec![
                Span::from(" "),
                Span::from(path).bold(),
                Span::from(format!(" {} ", status_name(file.status))).fg(status_color(file.status)),
            ]));
        }
        let inner = diff_block.inner(diff_area);
        viewer.diff_area = inner;
        let Some(rendered) = &viewer.rendered else {
            frame.render_widget(diff_block, diff_area);
            return;
        };
        let total = rendered.text.len();
        let height = inner.height as usize;
        viewer.max_scroll = total.saturating_sub(height);
        viewer.cursor = viewer.cursor.min(total.saturating_sub(1));
        if height > 0 {
            if std::mem::take(&mut viewer.follow_cursor) {
                viewer.scroll = viewer
                    .scroll
                    .clamp((viewer.cursor + 1).saturating_sub(height), viewer.cursor);
            } else {
                let scroll = viewer.scroll.min(viewer.max_scroll);
                viewer.cursor = viewer.cursor.clamp(scroll, scroll + height - 1);
            }
        }
        viewer.scroll = viewer.scroll.min(viewer.max_scroll);
        let selection = viewer.selection();
        let hunks = rendered.hunks.len();
        let position = if hunks == 0 {
            None
        } else if total <= height {
            Some(format!(
                " {hunks} hunk{} ",
                if hunks == 1 { "" } else { "s" }
            ))
        } else {
            let hunk = rendered
                .hunks
                .iter()
                .filter(|&&h| h <= viewer.cursor)
                .count()
                .max(1);
            Some(format!(
                " hunk {hunk}/{hunks} · lines {}–{} of {total} ",
                viewer.scroll + 1,
                (viewer.scroll + height).min(total)
            ))
        };
        let position = match selection {
            Some((first, last)) => Some(format!(
                " {} row{} selected · enter ask · y copy ",
                last - first + 1,
                if first == last { "" } else { "s" }
            )),
            None => position,
        };
        if let Some(position) = position {
            diff_block =
                diff_block.title_bottom(Line::from(position).right_aligned().fg(Color::DarkGray));
        }
        frame.render_widget(diff_block, diff_area);

        let [gutter_area, text_area] = Layout::horizontal([
            Constraint::Length(rendered.gutter_width.min(inner.width)),
            Constraint::Min(1),
        ])
        .areas(inner);
        let visible = viewer.scroll..(viewer.scroll + height).min(total);
        let focused = viewer.focus == Focus::Diff;
        for (y, bg) in rendered.backgrounds[visible.clone()].iter().enumerate() {
            let row = visible.start + y;
            let selected = selection.is_some_and(|(a, b)| (a..=b).contains(&row));
            let bg = if selected {
                Some(SELECTION_BG)
            } else if focused && row == viewer.cursor {
                Some(CURSOR_BG)
            } else {
                *bg
            };
            if let Some(bg) = bg {
                let area = Rect::new(inner.x, inner.y + y as u16, inner.width, 1);
                frame.buffer_mut().set_style(area, Style::new().bg(bg));
            }
        }
        frame.render_widget(
            Paragraph::new(rendered.gutter[visible.clone()].to_vec()),
            gutter_area,
        );
        let mut text = rendered.text[visible.clone()].to_vec();
        // Mark a gap under the cursor, which Enter expands.
        if focused
            && selection.is_none()
            && matches!(rendered.rows.get(viewer.cursor), Some(RowKind::Gap { .. }))
            && let Some(line) = text.get_mut(viewer.cursor - visible.start)
        {
            line.spans
                .push(Span::from("  ⏎ expand").fg(Color::Cyan).bold());
        }
        frame.render_widget(
            Paragraph::new(text).scroll((0, viewer.hscroll.min(u16::MAX as usize) as u16)),
            text_area,
        );
        // A bar down the left edge of the selection.
        if let Some((first, last)) = selection {
            for row in first.max(visible.start)..=last.min(visible.end.saturating_sub(1)) {
                let y = inner.y + (row - visible.start) as u16;
                frame.buffer_mut()[(inner.x, y)]
                    .set_symbol("▌")
                    .set_fg(Color::Cyan);
            }
        }
        if total > height {
            let mut state =
                ScrollbarState::new(total.saturating_sub(height)).position(viewer.scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                diff_area.inner(ratatui::layout::Margin::new(0, 1)),
                &mut state,
            );
        }
    }
}
