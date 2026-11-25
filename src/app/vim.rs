//! Vim-style modal editing for the prompt (`/vim`, or `vim = true` under `[ui]`).
//!
//! The prompt is edited like a small file: Enter in Insert mode starts a new line (with
//! the line's indent), and the prompt goes with Enter in Normal mode or `:w`. The editor
//! works on the prompt's text and cursor (a byte offset) and leaves every key it doesn't
//! use, like Ctrl+C or the arrows, to the prompt's usual handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Undo steps kept.
const UNDO_LIMIT: usize = 200;
/// What Tab inserts, and `>` and `<` add and take away.
const INDENT: &str = "    ";
/// Where `gq` wraps lines.
const TEXT_WIDTH: usize = 80;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Typing goes into the prompt as usual; Esc goes to Normal.
    #[default]
    Insert,
    Normal,
    /// Characters selected from the anchor to the cursor (`v`).
    Visual,
    /// Whole lines selected from the anchor's to the cursor's (`V`).
    VisualLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
    /// `>`
    Indent,
    /// `<`
    Outdent,
    /// `gq`: rewraps lines to `TEXT_WIDTH`.
    Format,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Find {
    /// `f`: onto the next given character.
    To,
    /// `t`: up to the next given character.
    Till,
    /// `F`
    BackTo,
    /// `T`
    BackTill,
}

impl Find {
    fn reversed(self) -> Self {
        match self {
            Find::To => Find::BackTo,
            Find::Till => Find::BackTill,
            Find::BackTo => Find::To,
            Find::BackTill => Find::Till,
        }
    }
}

/// A key that needs another before it does anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pending {
    #[default]
    None,
    /// `g`, waiting for the second `g`.
    G,
    /// An operator (`d`, `c`, `y`) waiting for its motion.
    Op(Op),
    /// An operator followed by `g`.
    OpG(Op),
    /// An operator followed by `i` (inner) or `a` (around), waiting for the object.
    Object(Op, bool),
    /// `i` or `a` in Visual mode.
    VisualObject(bool),
    /// `f`, `t`, `F` or `T` (maybe after an operator), waiting for the character.
    Find(Find, Option<Op>),
    /// `r`, waiting for the replacement.
    Replace,
}

/// Where a motion goes, and how an operator treats the text up to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// Up to, not including, the position.
    Exclusive(usize),
    /// Up to and including the character at the position.
    Inclusive(usize),
    /// Whole lines, from the cursor's to the position's.
    Lines(usize),
}

/// What the prompt should do after a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The key was used.
    Handled,
    /// Not a vim key here: the prompt handles it as usual.
    Pass,
    /// Send the prompt, as Enter does (`:w`).
    Send,
}

#[derive(Debug, Default)]
pub struct Vim {
    pub mode: Mode,
    pending: Pending,
    /// The count being typed, e.g. the 3 of `3w` (or of `d3w`).
    count: Option<usize>,
    /// The count typed before an operator, e.g. the 2 of `2d3w`.
    op_count: Option<usize>,
    register: String,
    /// Whether the register holds whole lines (from `dd`, `yy`, `V`...).
    register_lines: bool,
    /// Where a Visual selection started.
    anchor: usize,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// The prompt as it was when Insert mode began, so the whole insert undoes at once.
    insert_start: Option<(String, usize)>,
    last_find: Option<(Find, char)>,
    /// An Ex command being typed after `:`.
    ex: Option<String>,
    /// What the last Ex command had to say, until the next key.
    message: Option<String>,
    /// The keys of the change being made, and the prompt before it, for `.`.
    recording: Option<(Vec<KeyEvent>, String)>,
    /// The keys of the last change, which `.` makes again.
    last_change: Vec<KeyEvent>,
    replaying: bool,
}

impl Vim {
    /// The mode's name, for the prompt's border.
    pub fn label(&self) -> String {
        if let Some(ex) = &self.ex {
            return format!(":{ex}");
        }
        if let Some(message) = &self.message {
            return message.clone();
        }
        let mode = match self.mode {
            Mode::Insert => "INSERT",
            Mode::Normal => "NORMAL",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        };
        let mut keys = String::new();
        if let Some(n) = self.op_count {
            keys.push_str(&n.to_string());
        }
        match self.pending {
            Pending::None => {}
            Pending::G => keys.push('g'),
            Pending::Op(op) | Pending::OpG(op) | Pending::Object(op, _) => {
                keys.push_str(op_keys(op));
                match self.pending {
                    Pending::OpG(_) => keys.push('g'),
                    Pending::Object(_, inner) => keys.push(if inner { 'i' } else { 'a' }),
                    _ => {}
                }
            }
            Pending::VisualObject(inner) => keys.push(if inner { 'i' } else { 'a' }),
            Pending::Find(find, op) => {
                if let Some(op) = op {
                    keys.push_str(op_keys(op));
                }
                keys.push(match find {
                    Find::To => 'f',
                    Find::Till => 't',
                    Find::BackTo => 'F',
                    Find::BackTill => 'T',
                });
            }
            Pending::Replace => keys.push('r'),
        }
        if let Some(n) = self.count {
            keys.push_str(&n.to_string());
        }
        if keys.is_empty() {
            mode.to_string()
        } else {
            format!("{mode} {keys}")
        }
    }

    /// Back to Insert mode for a fresh prompt, as after sending one.
    pub fn reset(&mut self) {
        self.mode = Mode::Insert;
        self.pending = Pending::None;
        self.count = None;
        self.op_count = None;
        self.insert_start = None;
        self.ex = None;
        self.recording = None;
    }

    /// Whether Insert mode is on, where Enter starts a new line.
    pub fn inserting(&self) -> bool {
        self.mode == Mode::Insert
    }

    /// The selected bytes in Visual modes.
    pub fn selection(&self, text: &str, cursor: usize) -> Option<(usize, usize)> {
        let anchor = self.anchor_in(text, cursor);
        let (a, b) = (anchor.min(cursor), anchor.max(cursor));
        match self.mode {
            Mode::Visual => Some((a, next(text, b))),
            Mode::VisualLine => Some((line_start(text, a), line_end(text, b))),
            _ => None,
        }
    }

    /// Keeps the cursor on a character, as Normal and Visual modes do (not past a
    /// line's end, unless the line is empty).
    pub fn clamp(&self, text: &str, cursor: &mut usize) {
        *cursor = (*cursor).min(text.len());
        while !text.is_char_boundary(*cursor) {
            *cursor -= 1;
        }
        if self.mode != Mode::Insert
            && *cursor == line_end(text, *cursor)
            && *cursor > line_start(text, *cursor)
        {
            *cursor = prev(text, *cursor);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, text: &mut String, cursor: &mut usize) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.message = None;
        if self.mode == Mode::Insert {
            self.record(key);
            match key.code {
                KeyCode::Esc => {
                    self.enter_normal(text, cursor);
                    self.finish_recording(text);
                    return Outcome::Handled;
                }
                // A slash command still goes with Enter.
                KeyCode::Enter if !ctrl && text.starts_with('/') && !text.contains('\n') => {
                    self.recording = None;
                    return Outcome::Pass;
                }
                KeyCode::Enter if !ctrl => {
                    newline(text, cursor);
                    return Outcome::Handled;
                }
                KeyCode::Tab if key.modifiers.is_empty() => {
                    text.insert_str(*cursor, INDENT);
                    *cursor += INDENT.len();
                    return Outcome::Handled;
                }
                _ => return Outcome::Pass,
            }
        }
        if let Some(outcome) = self.ex_key(key, ctrl, text, cursor) {
            self.clamp(text, cursor);
            return outcome;
        }
        // Undo, redo and `.` work from snapshots and recordings rather than making them.
        if self.pending == Pending::None && self.mode == Mode::Normal {
            match key.code {
                KeyCode::Char('u') if !ctrl => {
                    let (n, _) = self.take_count();
                    self.step(n, true, text, cursor);
                    return Outcome::Handled;
                }
                KeyCode::Char('r') if ctrl => {
                    let (n, _) = self.take_count();
                    self.step(n, false, text, cursor);
                    return Outcome::Handled;
                }
                KeyCode::Char('.') if !ctrl => {
                    let (n, _) = self.take_count();
                    self.repeat(n, text, cursor);
                    return Outcome::Handled;
                }
                KeyCode::Char(':') if !ctrl => {
                    self.clear_pending();
                    self.ex = Some(String::new());
                    return Outcome::Handled;
                }
                _ => {}
            }
        }
        if self.mode == Mode::Normal
            && self.pending == Pending::None
            && self.count.is_none()
            && !self.replaying
        {
            self.recording = Some((Vec::new(), text.clone()));
        }
        self.record(key);
        let before = (text.clone(), *cursor);
        let outcome = if self.mode == Mode::Normal {
            self.normal_key(key, ctrl, text, cursor)
        } else {
            self.visual_key(key, ctrl, text, cursor)
        };
        if self.mode == Mode::Insert {
            if outcome == Outcome::Handled {
                self.insert_start = Some(before);
            }
        } else if *text != before.0 {
            self.push_undo(before);
        }
        if self.mode == Mode::Normal
            && self.pending == Pending::None
            && self.count.is_none()
            && self.op_count.is_none()
        {
            self.finish_recording(text);
        }
        self.clamp(text, cursor);
        outcome
    }

    fn record(&mut self, key: KeyEvent) {
        if !self.replaying
            && let Some((keys, _)) = &mut self.recording
        {
            keys.push(key);
        }
    }

    /// Keeps the keys just recorded for `.`, if they changed the prompt.
    fn finish_recording(&mut self, text: &str) {
        if let Some((keys, start)) = self.recording.take()
            && start != text
        {
            self.last_change = keys;
        }
    }

    /// `.`: makes the last change again, `n` times.
    fn repeat(&mut self, n: usize, text: &mut String, cursor: &mut usize) {
        let keys = self.last_change.clone();
        self.replaying = true;
        for _ in 0..n {
            for &key in &keys {
                if self.on_key(key, text, cursor) == Outcome::Pass && self.mode == Mode::Insert {
                    insert_key(key, text, cursor);
                }
            }
            if self.mode == Mode::Insert {
                self.enter_normal(text, cursor);
            }
        }
        self.replaying = false;
        self.clamp(text, cursor);
    }

    /// A key while an Ex command (`:…`) is being typed, if one is.
    fn ex_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        text: &mut String,
        cursor: &mut usize,
    ) -> Option<Outcome> {
        let ex = self.ex.as_mut()?;
        match key.code {
            KeyCode::Esc => self.ex = None,
            KeyCode::Backspace => {
                if ex.pop().is_none() {
                    self.ex = None;
                }
            }
            KeyCode::Char(c) if !ctrl => ex.push(c),
            KeyCode::Enter => {
                let command = self.ex.take().unwrap_or_default();
                return Some(self.run_ex(command.trim(), text, cursor));
            }
            _ => {}
        }
        Some(Outcome::Handled)
    }

    /// Runs Ex command `command`: `:w` sends, `:q` clears, `:N` goes to line N, and
    /// `:s` and `:%s` substitute.
    fn run_ex(&mut self, command: &str, text: &mut String, cursor: &mut usize) -> Outcome {
        match command {
            "" => {}
            "w" | "w!" | "wq" | "wq!" | "x" | "x!" | "send" => {
                self.reset();
                return Outcome::Send;
            }
            "q" | "q!" => {
                if !text.is_empty() {
                    self.push_undo((text.clone(), *cursor));
                    text.clear();
                    *cursor = 0;
                }
            }
            "$" => *cursor = first_non_blank(text, line_start(text, text.len())),
            _ if command.chars().all(|c| c.is_ascii_digit()) => {
                let line: usize = command.parse().unwrap_or(1);
                *cursor = first_non_blank(text, nth_line(text, line.saturating_sub(1)));
            }
            _ => match self.substitute(command, text, cursor) {
                Ok(0) => self.message = Some("Pattern not found".into()),
                Ok(n) => {
                    let s = if n == 1 { "" } else { "s" };
                    self.message = Some(format!("{n} substitution{s}"));
                }
                Err(e) => self.message = Some(e),
            },
        }
        Outcome::Handled
    }

    /// `:s/pattern/replacement/flags` on the cursor's line, or `:%s/…` on every line:
    /// how many replacements it made. Patterns are regular expressions; `\1` and `&` in
    /// the replacement stand for groups and the match, and the flags `g` and `i` replace
    /// every match on a line and ignore case.
    fn substitute(
        &mut self,
        command: &str,
        text: &mut String,
        cursor: &mut usize,
    ) -> Result<usize, String> {
        let unknown = || format!("Not an editor command: {command}");
        let (all, rest) = match command.strip_prefix('%') {
            Some(rest) => (true, rest),
            None => (false, command),
        };
        let rest = rest
            .strip_prefix("s")
            .or_else(|| rest.strip_prefix("substitute"))
            .ok_or_else(unknown)?;
        let delimiter = rest
            .chars()
            .next()
            .filter(|c| !c.is_alphanumeric() && *c != ' ' && *c != '\\');
        let delimiter = delimiter.ok_or_else(unknown)?;
        let parts = split_unescaped(&rest[delimiter.len_utf8()..], delimiter);
        let pattern = parts.first().cloned().unwrap_or_default();
        if pattern.is_empty() {
            return Err("No pattern".into());
        }
        let replacement = vim_replacement(parts.get(1).map_or("", String::as_str));
        let flags = parts.get(2).map_or("", String::as_str);
        let regex = regex::RegexBuilder::new(&pattern)
            .case_insensitive(flags.contains('i'))
            .build()
            .map_err(|e| {
                format!(
                    "Bad pattern: {}",
                    e.to_string().lines().last().unwrap_or("")
                )
            })?;
        let (start, end) = if all {
            (0, text.len())
        } else {
            (line_start(text, *cursor), line_end(text, *cursor))
        };
        let mut count = 0;
        let lines: Vec<String> = text[start..end]
            .split('\n')
            .map(|line| {
                let found = regex.find_iter(line).count();
                if flags.contains('g') {
                    count += found;
                    regex.replace_all(line, replacement.as_str()).into_owned()
                } else {
                    count += found.min(1);
                    regex.replace(line, replacement.as_str()).into_owned()
                }
            })
            .collect();
        if count > 0 {
            self.push_undo((text.clone(), *cursor));
            text.replace_range(start..end, &lines.join("\n"));
            *cursor = first_non_blank(text, line_start(text, (*cursor).min(text.len())));
        }
        Ok(count)
    }

    fn enter_normal(&mut self, text: &str, cursor: &mut usize) {
        if let Some(start) = self.insert_start.take()
            && start.0 != text
        {
            self.push_undo(start);
        }
        self.mode = Mode::Normal;
        self.clear_pending();
        if *cursor > line_start(text, *cursor) {
            *cursor = prev(text, *cursor);
        }
    }

    fn push_undo(&mut self, snapshot: (String, usize)) {
        self.undo.push(snapshot);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Undoes (or redoes) `n` changes.
    fn step(&mut self, n: usize, undo: bool, text: &mut String, cursor: &mut usize) {
        for _ in 0..n {
            let from = if undo { &mut self.undo } else { &mut self.redo };
            let Some((old, at)) = from.pop() else {
                break;
            };
            let current = (std::mem::replace(text, old), *cursor);
            *cursor = at;
            if undo {
                self.redo.push(current);
            } else {
                self.undo.push(current);
            }
        }
        self.clamp(text, cursor);
    }

    fn clear_pending(&mut self) {
        self.pending = Pending::None;
        self.count = None;
        self.op_count = None;
    }

    /// The count for the command being finished, and whether one was typed.
    fn take_count(&mut self) -> (usize, bool) {
        let explicit = self.count.is_some() || self.op_count.is_some();
        let n = self.op_count.take().unwrap_or(1) * self.count.take().unwrap_or(1);
        (n.max(1), explicit)
    }

    fn anchor_in(&self, text: &str, cursor: usize) -> usize {
        let anchor = self.anchor.min(text.len());
        if text.is_char_boundary(anchor) {
            anchor
        } else {
            cursor
        }
    }

    /// Takes a digit as part of a count, if it is one here.
    fn count_digit(&mut self, c: char) -> bool {
        let Some(d) = c.to_digit(10) else {
            return false;
        };
        if d == 0 && self.count.is_none() {
            return false; // `0` is the start-of-line motion.
        }
        let n = self
            .count
            .unwrap_or(0)
            .saturating_mul(10)
            .saturating_add(d as usize);
        self.count = Some(n.min(99_999));
        true
    }

    fn normal_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        text: &mut String,
        cursor: &mut usize,
    ) -> Outcome {
        let c = match key.code {
            KeyCode::Char(c) if !ctrl => c,
            KeyCode::Backspace if self.pending == Pending::None => 'h',
            KeyCode::Delete if self.pending == Pending::None => 'x',
            KeyCode::Esc if self.pending != Pending::None || self.count.is_some() => {
                self.clear_pending();
                return Outcome::Handled;
            }
            KeyCode::Enter => {
                // Sent (or a newline added): the next prompt starts in Insert mode.
                self.clear_pending();
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
                {
                    self.mode = Mode::Insert;
                }
                return Outcome::Pass;
            }
            _ => {
                self.clear_pending();
                return Outcome::Pass;
            }
        };

        match std::mem::take(&mut self.pending) {
            Pending::Replace => {
                let (n, _) = self.take_count();
                replace_chars(text, cursor, c, n);
            }
            Pending::Find(find, op) => {
                self.last_find = Some((find, c));
                let (n, _) = self.take_count();
                if let Some(target) = find_char(text, *cursor, find, c, n, false) {
                    self.apply(op, target, text, cursor);
                }
            }
            Pending::Object(op, inner) => {
                self.take_count();
                if let Some((a, b, lines)) = object(text, *cursor, c, inner) {
                    self.operate(op, a, b, lines, text, cursor);
                }
            }
            Pending::VisualObject(_) => {}
            Pending::G if c == 'q' => {
                self.op_count = self.count.take();
                self.pending = Pending::Op(Op::Format);
                return Outcome::Handled;
            }
            Pending::G => {
                let (n, explicit) = self.take_count();
                if c == 'g' {
                    let line = if explicit { n - 1 } else { 0 };
                    *cursor = first_non_blank(text, nth_line(text, line));
                }
            }
            Pending::OpG(op) => {
                let (n, explicit) = self.take_count();
                if c == 'q' && op == Op::Format {
                    let last = lines_down(text, *cursor, n - 1);
                    self.operate(op, *cursor, last, true, text, cursor);
                } else if c == 'g' {
                    let line = if explicit { n - 1 } else { 0 };
                    let target = Target::Lines(nth_line(text, line));
                    self.apply(Some(op), target, text, cursor);
                }
            }
            Pending::Op(op) => {
                if self.count_digit(c) {
                    self.pending = Pending::Op(op);
                    return Outcome::Handled;
                }
                match c {
                    _ if c == op_key(op) => {
                        let (n, _) = self.take_count();
                        let last = lines_down(text, *cursor, n - 1);
                        self.operate(op, *cursor, last, true, text, cursor);
                    }
                    'i' | 'a' => self.pending = Pending::Object(op, c == 'i'),
                    'g' => self.pending = Pending::OpG(op),
                    'f' | 't' | 'F' | 'T' => self.pending = Pending::Find(find_kind(c), Some(op)),
                    _ => {
                        let (n, explicit) = self.take_count();
                        if let Some(target) = self.motion(c, n, explicit, Some(op), text, *cursor) {
                            self.apply(Some(op), target, text, cursor);
                        }
                    }
                }
            }
            Pending::None => {
                if self.count_digit(c) {
                    return Outcome::Handled;
                }
                return self.normal_command(c, text, cursor);
            }
        }
        if self.pending == Pending::None {
            self.count = None;
            self.op_count = None;
        }
        Outcome::Handled
    }

    fn normal_command(&mut self, c: char, text: &mut String, cursor: &mut usize) -> Outcome {
        match c {
            'd' | 'c' | 'y' | '>' | '<' => {
                self.op_count = self.count.take();
                self.pending = Pending::Op(match c {
                    'd' => Op::Delete,
                    'c' => Op::Change,
                    'y' => Op::Yank,
                    '>' => Op::Indent,
                    _ => Op::Outdent,
                });
                return Outcome::Handled;
            }
            'g' => {
                self.pending = Pending::G;
                return Outcome::Handled;
            }
            'r' => {
                self.pending = Pending::Replace;
                return Outcome::Handled;
            }
            'f' | 't' | 'F' | 'T' => {
                self.pending = Pending::Find(find_kind(c), None);
                return Outcome::Handled;
            }
            _ => {}
        }
        let (n, explicit) = self.take_count();
        match c {
            'i' => self.mode = Mode::Insert,
            'a' => {
                if *cursor < line_end(text, *cursor) {
                    *cursor = next(text, *cursor);
                }
                self.mode = Mode::Insert;
            }
            'I' => {
                *cursor = first_non_blank(text, *cursor);
                self.mode = Mode::Insert;
            }
            'A' => {
                *cursor = line_end(text, *cursor);
                self.mode = Mode::Insert;
            }
            'o' => {
                let indent = indent_of(text, *cursor).to_string();
                let end = line_end(text, *cursor);
                text.insert_str(end, &format!("\n{indent}"));
                *cursor = end + 1 + indent.len();
                self.mode = Mode::Insert;
            }
            'O' => {
                let indent = indent_of(text, *cursor).to_string();
                let start = line_start(text, *cursor);
                text.insert_str(start, &format!("{indent}\n"));
                *cursor = start + indent.len();
                self.mode = Mode::Insert;
            }
            'x' => self.apply_motion(Op::Delete, 'l', n, text, cursor),
            'X' => self.apply_motion(Op::Delete, 'h', n, text, cursor),
            'D' => self.apply_motion(Op::Delete, '$', n, text, cursor),
            'C' => self.apply_motion(Op::Change, '$', n, text, cursor),
            's' => self.apply_motion(Op::Change, 'l', n, text, cursor),
            'S' => {
                let last = lines_down(text, *cursor, n - 1);
                self.operate(Op::Change, *cursor, last, true, text, cursor);
            }
            'Y' => {
                let last = lines_down(text, *cursor, n - 1);
                let at = *cursor;
                self.operate(Op::Yank, at, last, true, text, cursor);
                *cursor = at;
            }
            'p' => self.put(true, n, text, cursor),
            'P' => self.put(false, n, text, cursor),
            'J' => join_lines(text, cursor, n.max(2) - 1),
            '~' => toggle_case(text, cursor, n),
            'v' => {
                self.mode = Mode::Visual;
                self.anchor = *cursor;
            }
            'V' => {
                self.mode = Mode::VisualLine;
                self.anchor = *cursor;
            }
            _ => {
                if let Some(target) = self.motion(c, n, explicit, None, text, *cursor) {
                    self.apply(None, target, text, cursor);
                }
            }
        }
        Outcome::Handled
    }

    fn visual_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        text: &mut String,
        cursor: &mut usize,
    ) -> Outcome {
        let c = match key.code {
            KeyCode::Char(c) if !ctrl => c,
            KeyCode::Left => 'h',
            KeyCode::Right => 'l',
            KeyCode::Up => 'k',
            KeyCode::Down => 'j',
            KeyCode::Esc => {
                self.clear_pending();
                self.mode = Mode::Normal;
                return Outcome::Handled;
            }
            KeyCode::Enter => {
                self.clear_pending();
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
                {
                    self.mode = Mode::Insert;
                }
                return Outcome::Pass;
            }
            _ => {
                self.clear_pending();
                return Outcome::Pass;
            }
        };
        let linewise = self.mode == Mode::VisualLine;
        let anchor = self.anchor_in(text, *cursor);
        let (a, b) = (anchor.min(*cursor), anchor.max(*cursor));

        match std::mem::take(&mut self.pending) {
            Pending::VisualObject(inner) => {
                self.take_count();
                match object(text, *cursor, c, inner) {
                    Some((start, end, true)) => {
                        self.mode = Mode::VisualLine;
                        self.anchor = start;
                        *cursor = end;
                    }
                    Some((start, end, false)) if end > start => {
                        self.mode = Mode::Visual;
                        self.anchor = start;
                        *cursor = prev(text, end);
                    }
                    _ => {}
                }
                return Outcome::Handled;
            }
            Pending::Find(find, _) => {
                self.last_find = Some((find, c));
                let (n, _) = self.take_count();
                if let Some(target) = find_char(text, *cursor, find, c, n, false) {
                    self.apply(None, target, text, cursor);
                }
                return Outcome::Handled;
            }
            Pending::G => {
                let (n, explicit) = self.take_count();
                if c == 'g' {
                    let line = if explicit { n - 1 } else { 0 };
                    *cursor = first_non_blank(text, nth_line(text, line));
                } else if c == 'q' {
                    self.mode = Mode::Normal;
                    self.operate(Op::Format, a, b, true, text, cursor);
                }
                return Outcome::Handled;
            }
            _ => {}
        }
        if self.count_digit(c) {
            return Outcome::Handled;
        }
        match c {
            'i' | 'a' => {
                self.pending = Pending::VisualObject(c == 'i');
                return Outcome::Handled;
            }
            'g' => {
                self.pending = Pending::G;
                return Outcome::Handled;
            }
            'f' | 't' | 'F' | 'T' => {
                self.pending = Pending::Find(find_kind(c), None);
                return Outcome::Handled;
            }
            _ => {}
        }
        let (n, explicit) = self.take_count();
        let end = if linewise { b } else { next(text, b) };
        match c {
            'v' | 'V' => {
                let wanted = if c == 'v' {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                self.mode = if self.mode == wanted {
                    Mode::Normal
                } else {
                    wanted
                };
            }
            'o' => {
                self.anchor = *cursor;
                *cursor = anchor;
            }
            'd' | 'x' => {
                self.operate(Op::Delete, a, end, linewise, text, cursor);
                self.mode = Mode::Normal;
            }
            'y' => {
                self.operate(Op::Yank, a, end, linewise, text, cursor);
                self.mode = Mode::Normal;
            }
            'c' | 's' => {
                self.mode = Mode::Normal;
                self.operate(Op::Change, a, end, linewise, text, cursor);
            }
            '>' | '<' => {
                self.mode = Mode::Normal;
                let op = if c == '>' { Op::Indent } else { Op::Outdent };
                for _ in 0..n {
                    self.operate(op, a, b, true, text, cursor);
                }
            }
            'D' | 'X' | 'Y' | 'C' | 'S' => {
                let op = match c {
                    'Y' => Op::Yank,
                    'C' | 'S' => Op::Change,
                    _ => Op::Delete,
                };
                self.mode = Mode::Normal;
                self.operate(op, a, b, true, text, cursor);
            }
            'p' | 'P' => {
                let register = (self.register.clone(), self.register_lines);
                self.operate(Op::Delete, a, end, linewise, text, cursor);
                (self.register, self.register_lines) = register;
                self.mode = Mode::Normal;
                // Where the selection was.
                self.put(false, n, text, cursor);
            }
            '~' | 'u' | 'U' => {
                let (s, e) = if linewise {
                    (line_start(text, a), line_end(text, b))
                } else {
                    (a, end)
                };
                let changed: String = text[s..e]
                    .chars()
                    .map(|ch| match c {
                        'u' => ch.to_lowercase().collect::<String>(),
                        'U' => ch.to_uppercase().collect(),
                        _ => flip_case(ch),
                    })
                    .collect();
                text.replace_range(s..e, &changed);
                *cursor = s;
                self.mode = Mode::Normal;
            }
            'J' => {
                *cursor = a;
                let lines = text[a..b].matches('\n').count();
                join_lines(text, cursor, lines.max(1));
                self.mode = Mode::Normal;
            }
            _ => {
                if let Some(target) = self.motion(c, n, explicit, None, text, *cursor) {
                    self.apply(None, target, text, cursor);
                }
            }
        }
        Outcome::Handled
    }

    fn apply_motion(
        &mut self,
        op: Op,
        motion: char,
        n: usize,
        text: &mut String,
        cursor: &mut usize,
    ) {
        if let Some(target) = self.motion(motion, n, false, Some(op), text, *cursor) {
            self.apply(Some(op), target, text, cursor);
        }
    }

    /// Where motion key `c` goes from `at`, if it's a motion.
    fn motion(
        &self,
        c: char,
        n: usize,
        explicit: bool,
        op: Option<Op>,
        text: &str,
        at: usize,
    ) -> Option<Target> {
        let t = text;
        Some(match c {
            'h' => {
                let start = line_start(t, at);
                let mut p = at;
                for _ in 0..n {
                    if p > start {
                        p = prev(t, p);
                    }
                }
                Target::Exclusive(p)
            }
            'l' | ' ' => {
                let end = line_end(t, at);
                let mut p = at;
                for _ in 0..n {
                    if p < end {
                        p = next(t, p);
                    }
                }
                Target::Exclusive(p)
            }
            'j' | '+' => Target::Lines(lines_down(t, at, n)),
            'k' | '-' => Target::Lines(lines_up(t, at, n)),
            'w' | 'W' => {
                let big = c == 'W';
                let on_word = char_at(t, at).is_some_and(|ch| !ch.is_whitespace());
                if op == Some(Op::Change) && on_word {
                    // `cw` changes to the end of the word, like `ce`.
                    let mut p = run_end(t, at, big);
                    for _ in 1..n {
                        p = word_end(t, p, big);
                    }
                    return Some(Target::Inclusive(p));
                }
                let mut p = at;
                for _ in 0..n {
                    p = word_forward(t, p, big);
                }
                // An operator stops at the end of the line rather than taking the next
                // line's indent.
                if op.is_some()
                    && let Some(nl) = t[at..p].rfind('\n')
                    && t[at + nl..p].trim().is_empty()
                {
                    p = at + nl;
                }
                Target::Exclusive(p)
            }
            'b' | 'B' => {
                let mut p = at;
                for _ in 0..n {
                    p = word_back(t, p, c == 'B');
                }
                Target::Exclusive(p)
            }
            'e' | 'E' => {
                let mut p = at;
                for _ in 0..n {
                    p = word_end(t, p, c == 'E');
                }
                Target::Inclusive(p)
            }
            '0' => Target::Exclusive(line_start(t, at)),
            '^' | '_' if c == '^' || op.is_none() => Target::Exclusive(first_non_blank(t, at)),
            '_' => Target::Lines(lines_down(t, at, n - 1)),
            '$' => Target::Exclusive(line_end(t, lines_down(t, at, n - 1))),
            'G' => {
                let line = if explicit {
                    nth_line(t, n - 1)
                } else {
                    line_start(t, t.len())
                };
                Target::Lines(first_non_blank(t, line))
            }
            ';' | ',' => {
                let (find, ch) = self.last_find?;
                let find = if c == ',' { find.reversed() } else { find };
                return find_char(t, at, find, ch, n, true);
            }
            '%' => Target::Inclusive(matching_bracket(t, at)?),
            '}' => {
                let mut p = at;
                for _ in 0..n {
                    p = paragraph_forward(t, p);
                }
                Target::Exclusive(p)
            }
            '{' => {
                let mut p = at;
                for _ in 0..n {
                    p = paragraph_back(t, p);
                }
                Target::Exclusive(p)
            }
            _ => return None,
        })
    }

    fn apply(&mut self, op: Option<Op>, target: Target, text: &mut String, cursor: &mut usize) {
        let at = *cursor;
        match (op, target) {
            (None, Target::Exclusive(p) | Target::Inclusive(p) | Target::Lines(p)) => *cursor = p,
            (Some(op), Target::Exclusive(p)) => {
                self.operate(op, at.min(p), at.max(p), false, text, cursor)
            }
            (Some(op), Target::Inclusive(p)) => {
                let end = next(text, at.max(p));
                self.operate(op, at.min(p), end, false, text, cursor)
            }
            (Some(op), Target::Lines(p)) => {
                self.operate(op, at.min(p), at.max(p), true, text, cursor)
            }
        }
    }

    /// Runs `op` on bytes `a..b`, or with `lines` on the lines from `a`'s to `b`'s.
    fn operate(
        &mut self,
        op: Op,
        a: usize,
        b: usize,
        lines: bool,
        text: &mut String,
        cursor: &mut usize,
    ) {
        if matches!(op, Op::Indent | Op::Outdent | Op::Format) {
            let start = line_start(text, a);
            let end = line_end(text, b);
            let lines = text[start..end].split('\n');
            let reshaped = match op {
                Op::Indent => lines
                    .map(|line| {
                        if line.trim().is_empty() {
                            line.to_string()
                        } else {
                            format!("{INDENT}{line}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Op::Outdent => lines.map(outdent).collect::<Vec<_>>().join("\n"),
                _ => reflow(&text[start..end], TEXT_WIDTH),
            };
            text.replace_range(start..end, &reshaped);
            *cursor = first_non_blank(text, start);
            return;
        }
        if lines {
            let start = line_start(text, a);
            let end = line_end(text, b);
            self.register = format!("{}\n", &text[start..end]);
            self.register_lines = true;
            match op {
                Op::Yank => *cursor = a,
                Op::Delete => {
                    let range = if end < text.len() {
                        start..end + 1
                    } else if start > 0 {
                        start - 1..end
                    } else {
                        start..end
                    };
                    let from = range.start;
                    text.replace_range(range, "");
                    *cursor = first_non_blank(text, line_start(text, from.min(text.len())));
                }
                Op::Change => {
                    text.replace_range(start..end, "");
                    *cursor = start;
                    self.mode = Mode::Insert;
                }
                _ => {}
            }
        } else {
            self.register = text[a..b].to_string();
            self.register_lines = false;
            if op != Op::Yank {
                text.replace_range(a..b, "");
            }
            *cursor = a;
            if op == Op::Change {
                self.mode = Mode::Insert;
            }
        }
    }

    /// Puts the register after (`p`) or before (`P`) the cursor, `n` times.
    fn put(&mut self, after: bool, n: usize, text: &mut String, cursor: &mut usize) {
        if self.register.is_empty() {
            return;
        }
        let register = self.register.repeat(n);
        if self.register_lines {
            if after {
                let end = line_end(text, *cursor);
                let lines = register.strip_suffix('\n').unwrap_or(&register);
                text.insert_str(end, &format!("\n{lines}"));
                *cursor = end + 1;
            } else {
                let start = line_start(text, *cursor);
                text.insert_str(start, &register);
                *cursor = start;
            }
            *cursor = first_non_blank(text, *cursor);
        } else {
            let at = if after && *cursor < line_end(text, *cursor) {
                next(text, *cursor)
            } else {
                *cursor
            };
            text.insert_str(at, &register);
            *cursor = prev(text, at + register.len());
        }
    }
}

/// The key that, typed again after the operator, makes it work on whole lines (`dd`).
fn op_key(op: Op) -> char {
    match op {
        Op::Delete => 'd',
        Op::Change => 'c',
        Op::Yank => 'y',
        Op::Indent => '>',
        Op::Outdent => '<',
        Op::Format => 'q',
    }
}

/// The operator's keys, as the mode label shows them while it waits.
fn op_keys(op: Op) -> &'static str {
    match op {
        Op::Delete => "d",
        Op::Change => "c",
        Op::Yank => "y",
        Op::Indent => ">",
        Op::Outdent => "<",
        Op::Format => "gq",
    }
}

fn find_kind(c: char) -> Find {
    match c {
        'f' => Find::To,
        't' => Find::Till,
        'F' => Find::BackTo,
        _ => Find::BackTill,
    }
}

// ------------------------------------------------------------------ text helpers

fn char_at(t: &str, i: usize) -> Option<char> {
    t.get(i..)?.chars().next()
}

fn next(t: &str, i: usize) -> usize {
    char_at(t, i).map_or(i, |c| i + c.len_utf8())
}

fn prev(t: &str, i: usize) -> usize {
    t[..i].chars().next_back().map_or(i, |c| i - c.len_utf8())
}

fn line_start(t: &str, i: usize) -> usize {
    t[..i].rfind('\n').map_or(0, |p| p + 1)
}

fn line_end(t: &str, i: usize) -> usize {
    t[i..].find('\n').map_or(t.len(), |p| i + p)
}

fn first_non_blank(t: &str, i: usize) -> usize {
    let start = line_start(t, i);
    let line = &t[start..line_end(t, i)];
    start + line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// The start of line `n` (from 0), or of the last line.
fn nth_line(t: &str, n: usize) -> usize {
    let mut start = 0;
    for _ in 0..n {
        match t[start..].find('\n') {
            Some(p) => start += p + 1,
            None => break,
        }
    }
    start
}

fn at_column(t: &str, start: usize, column: usize) -> usize {
    let end = line_end(t, start);
    t[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(j, _)| start + j)
}

fn lines_down(t: &str, i: usize, n: usize) -> usize {
    let column = t[line_start(t, i)..i].chars().count();
    let mut start = line_start(t, i);
    for _ in 0..n {
        let end = line_end(t, start);
        if end >= t.len() {
            break;
        }
        start = end + 1;
    }
    at_column(t, start, column)
}

fn lines_up(t: &str, i: usize, n: usize) -> usize {
    let column = t[line_start(t, i)..i].chars().count();
    let mut start = line_start(t, i);
    for _ in 0..n {
        if start == 0 {
            break;
        }
        start = line_start(t, start - 1);
    }
    at_column(t, start, column)
}

/// 0 for blanks, 1 for word characters (any non-blank in a WORD), 2 for punctuation.
fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// The last character of the run of one class that `i` is in.
fn run_end(t: &str, i: usize, big: bool) -> usize {
    let Some(c) = char_at(t, i) else {
        return i;
    };
    let k = class(c, big);
    let mut p = i;
    while let Some(c) = char_at(t, next(t, p))
        && class(c, big) == k
    {
        p = next(t, p);
    }
    p
}

/// `w`: the start of the next word.
fn word_forward(t: &str, i: usize, big: bool) -> usize {
    let mut p = i;
    if let Some(c) = char_at(t, p) {
        let k = class(c, big);
        if k != 0 {
            while let Some(c) = char_at(t, p)
                && class(c, big) == k
            {
                p = next(t, p);
            }
        }
    }
    while let Some(c) = char_at(t, p)
        && c.is_whitespace()
    {
        // An empty line counts as a word.
        if c == '\n' && p > i && t[..p].ends_with('\n') {
            break;
        }
        p = next(t, p);
    }
    p
}

/// `b`: the start of this word, or of the one before.
fn word_back(t: &str, i: usize, big: bool) -> usize {
    let mut p = i;
    while p > 0 && t[..p].ends_with(char::is_whitespace) {
        p = prev(t, p);
    }
    let Some(c) = t[..p].chars().next_back() else {
        return 0;
    };
    let k = class(c, big);
    while let Some(c) = t[..p].chars().next_back()
        && class(c, big) == k
    {
        p = prev(t, p);
    }
    p
}

/// `e`: the end of this word, or of the next.
fn word_end(t: &str, i: usize, big: bool) -> usize {
    let mut p = next(t, i);
    while let Some(c) = char_at(t, p)
        && c.is_whitespace()
    {
        p = next(t, p);
    }
    if p >= t.len() {
        return prev(t, t.len()).min(t.len());
    }
    run_end(t, p, big)
}

/// `f`, `t`, `F` or `T` for `c`, `n` times; `repeat` for `;` and `,`, which look past a
/// character right next to the cursor for `t` and `T`, as Vim does.
fn find_char(t: &str, i: usize, find: Find, c: char, n: usize, repeat: bool) -> Option<Target> {
    let start = line_start(t, i);
    let end = line_end(t, i);
    let mut p = i;
    match find {
        Find::To | Find::Till => {
            for k in 0..n {
                let from = if repeat
                    && find == Find::Till
                    && k == 0
                    && char_at(t, next(t, p)) == Some(c)
                {
                    next(t, next(t, p))
                } else {
                    next(t, p)
                };
                if from > end {
                    return None;
                }
                p = from + t[from..end].find(c)?;
            }
            Some(Target::Inclusive(if find == Find::Till {
                prev(t, p)
            } else {
                p
            }))
        }
        Find::BackTo | Find::BackTill => {
            for k in 0..n {
                let upto = if repeat && find == Find::BackTill && k == 0 && t[..p].ends_with(c) {
                    prev(t, p)
                } else {
                    p
                };
                p = start + t[start..upto].rfind(c)?;
            }
            Some(Target::Exclusive(if find == Find::BackTill {
                next(t, p)
            } else {
                p
            }))
        }
    }
}

/// The bracket matching the one at (or after, on the line) `i`, for `%`.
fn matching_bracket(t: &str, i: usize) -> Option<usize> {
    const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
    let end = line_end(t, i);
    let (at, c) = t[i..end]
        .char_indices()
        .map(|(j, c)| (i + j, c))
        .find(|(_, c)| PAIRS.iter().any(|&(o, cl)| *c == o || *c == cl))?;
    let &(open, close) = PAIRS.iter().find(|&&(o, cl)| c == o || c == cl)?;
    let mut depth = 0usize;
    if c == open {
        for (j, ch) in t[at..].char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(at + j);
                }
            }
        }
    } else {
        for (j, ch) in t[..=at].char_indices().rev() {
            if ch == close {
                depth += 1;
            } else if ch == open {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
        }
    }
    None
}

/// The bytes of text object `c` (`w`, `W`, `p`, quotes, brackets) around `i`, and
/// whether it's whole lines (a paragraph, `p`).
fn object(t: &str, i: usize, c: char, inner: bool) -> Option<(usize, usize, bool)> {
    let chars = |range: Option<(usize, usize)>| range.map(|(a, b)| (a, b, false));
    match c {
        'w' | 'W' => chars(Some(word_object(t, i, inner, c == 'W'))),
        'p' => Some(paragraph_object(t, i, inner)),
        '"' | '\'' | '`' => chars(quote_object(t, i, c, inner)),
        '(' | ')' | 'b' => chars(bracket_object(t, i, '(', ')', inner)),
        '[' | ']' => chars(bracket_object(t, i, '[', ']', inner)),
        '{' | '}' | 'B' => chars(bracket_object(t, i, '{', '}', inner)),
        '<' | '>' => chars(bracket_object(t, i, '<', '>', inner)),
        _ => None,
    }
}

fn is_blank_line(t: &str, start: usize) -> bool {
    t[start..line_end(t, start)].trim().is_empty()
}

/// `}`: the blank line after this paragraph, or the end.
fn paragraph_forward(t: &str, i: usize) -> usize {
    let mut start = line_start(t, i);
    let mut seen_text = !is_blank_line(t, start);
    loop {
        let end = line_end(t, start);
        if end >= t.len() {
            return t.len();
        }
        start = end + 1;
        if !is_blank_line(t, start) {
            seen_text = true;
        } else if seen_text {
            return start;
        }
    }
}

/// `{`: the blank line before this paragraph, or the start.
fn paragraph_back(t: &str, i: usize) -> usize {
    let mut start = line_start(t, i);
    let mut seen_text = !is_blank_line(t, start);
    loop {
        if start == 0 {
            return 0;
        }
        start = line_start(t, start - 1);
        if !is_blank_line(t, start) {
            seen_text = true;
        } else if seen_text {
            return start;
        }
    }
}

/// `ip` (the lines of this paragraph, or of these blank lines) and `ap` (with the blank
/// lines after it): where its first and last lines start.
fn paragraph_object(t: &str, i: usize, inner: bool) -> (usize, usize, bool) {
    let here = line_start(t, i);
    let blank = is_blank_line(t, here);
    let mut first = here;
    while first > 0 && is_blank_line(t, line_start(t, first - 1)) == blank {
        first = line_start(t, first - 1);
    }
    let following = |last: usize, blank: bool| {
        let end = line_end(t, last);
        (end < t.len() && is_blank_line(t, end + 1) == blank).then_some(end + 1)
    };
    let mut last = here;
    while let Some(next) = following(last, blank) {
        last = next;
    }
    if !inner && !blank {
        while let Some(next) = following(last, true) {
            last = next;
        }
    }
    (first, last, true)
}

fn word_object(t: &str, i: usize, inner: bool, big: bool) -> (usize, usize) {
    let start = line_start(t, i);
    let end = line_end(t, i);
    let Some(c) = char_at(t, i).filter(|&c| c != '\n') else {
        return (i, i);
    };
    let k = class(c, big);
    let mut a = i;
    while a > start
        && t[..a]
            .chars()
            .next_back()
            .is_some_and(|c| class(c, big) == k)
    {
        a = prev(t, a);
    }
    let mut b = next(t, i);
    while b < end && char_at(t, b).is_some_and(|c| class(c, big) == k) {
        b = next(t, b);
    }
    if !inner {
        // With the blanks after it, or before it if there are none after.
        let mut e = b;
        while e < end && char_at(t, e).is_some_and(char::is_whitespace) {
            e = next(t, e);
        }
        if e > b {
            b = e;
        } else {
            while a > start && t[..a].ends_with([' ', '\t']) {
                a = prev(t, a);
            }
        }
    }
    (a, b)
}

fn quote_object(t: &str, i: usize, q: char, inner: bool) -> Option<(usize, usize)> {
    let start = line_start(t, i);
    let end = line_end(t, i);
    let quotes: Vec<usize> = t[start..end]
        .match_indices(q)
        .map(|(p, _)| start + p)
        .collect();
    let (open, close) = quotes
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        // The pair around the cursor, or else the next one on the line.
        .find(|&(_, close)| i <= close)?;
    Some(if inner {
        (open + 1, close)
    } else {
        (open, close + 1)
    })
}

fn bracket_object(
    t: &str,
    i: usize,
    open: char,
    close: char,
    inner: bool,
) -> Option<(usize, usize)> {
    let from = if char_at(t, i) == Some(open) {
        i
    } else {
        let mut depth = 0usize;
        let mut found = None;
        for (j, c) in t[..i].char_indices().rev() {
            if c == close {
                depth += 1;
            } else if c == open {
                if depth == 0 {
                    found = Some(j);
                    break;
                }
                depth -= 1;
            }
        }
        found?
    };
    let mut depth = 0usize;
    let to = t[from + 1..].char_indices().find_map(|(j, c)| {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some(from + 1 + j);
            }
            depth -= 1;
        }
        None
    })?;
    Some(if inner {
        (from + 1, to)
    } else {
        (from, to + 1)
    })
}

fn replace_chars(text: &mut String, cursor: &mut usize, c: char, n: usize) {
    let end = line_end(text, *cursor);
    let chars: Vec<(usize, char)> = text[*cursor..end].char_indices().take(n).collect();
    if chars.len() < n || c == '\n' {
        return;
    }
    let (last, ch) = chars[n - 1];
    let stop = *cursor + last + ch.len_utf8();
    let replacement = c.to_string().repeat(n);
    text.replace_range(*cursor..stop, &replacement);
    *cursor += replacement.len() - c.len_utf8();
}

fn flip_case(c: char) -> String {
    if c.is_uppercase() {
        c.to_lowercase().collect()
    } else {
        c.to_uppercase().collect()
    }
}

fn toggle_case(text: &mut String, cursor: &mut usize, n: usize) {
    let end = line_end(text, *cursor);
    let stop = text[*cursor..end]
        .char_indices()
        .nth(n)
        .map_or(end, |(j, _)| *cursor + j);
    let flipped: String = text[*cursor..stop].chars().map(flip_case).collect();
    text.replace_range(*cursor..stop, &flipped);
    *cursor += flipped.len();
}

/// `J`: joins `joins` following lines onto the cursor's with single spaces.
fn join_lines(text: &mut String, cursor: &mut usize, joins: usize) {
    for _ in 0..joins {
        let end = line_end(text, *cursor);
        if end >= text.len() {
            break;
        }
        let rest = &text[end + 1..];
        let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let next_empty = rest[blanks..].is_empty() || rest[blanks..].starts_with('\n');
        let sep =
            if next_empty || text[..end].ends_with([' ', '\t']) || end == line_start(text, end) {
                ""
            } else {
                " "
            };
        text.replace_range(end..end + 1 + blanks, sep);
        *cursor = end;
    }
}

fn indent_of(t: &str, i: usize) -> &str {
    let start = line_start(t, i);
    &t[start..first_non_blank(t, i)]
}

/// Enter in Insert mode: a new line, indented as this one is.
fn newline(text: &mut String, cursor: &mut usize) {
    let start = line_start(text, *cursor);
    let indent = indent_of(text, *cursor);
    // Not more indent than the cursor is past, when it's in the indent.
    let indent = indent[..indent.len().min(*cursor - start)].to_string();
    text.insert_str(*cursor, &format!("\n{indent}"));
    *cursor += 1 + indent.len();
}

/// A key typed in Insert mode, as `.` makes it again.
fn insert_key(key: KeyEvent, text: &mut String, cursor: &mut usize) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return;
    }
    match key.code {
        KeyCode::Char(c) => {
            text.insert(*cursor, c);
            *cursor += c.len_utf8();
        }
        KeyCode::Backspace if *cursor > 0 => {
            let from = prev(text, *cursor);
            text.replace_range(from..*cursor, "");
            *cursor = from;
        }
        KeyCode::Delete if *cursor < text.len() => {
            let to = next(text, *cursor);
            text.replace_range(*cursor..to, "");
        }
        _ => {}
    }
}

/// A line with one level of indent taken away.
fn outdent(line: &str) -> String {
    if let Some(rest) = line.strip_prefix('\t') {
        return rest.to_string();
    }
    let spaces = line.len() - line.trim_start_matches(' ').len();
    line[spaces.min(INDENT.len())..].to_string()
}

/// A list item's marker (`- `, `* `, `+ `, `1. `, `1) `), if the line starts with one.
fn list_marker(body: &str) -> Option<&str> {
    for bullet in ["- ", "* ", "+ "] {
        if body.starts_with(bullet) {
            return Some(bullet);
        }
    }
    let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = &body[digits..];
    (digits > 0 && (rest.starts_with(". ") || rest.starts_with(") "))).then(|| &body[..digits + 2])
}

/// `gq`: rewraps each paragraph of `block` to `width` columns, keeping its indent. List
/// items start their own paragraphs (continued under their text), and blank lines,
/// headings, table rows and code blocks stay as they are.
fn reflow(block: &str, width: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    // The paragraph being gathered: its first line's start, the other lines' and its words.
    let mut paragraph: Option<(String, String, Vec<&str>)> = None;
    let flush = |paragraph: &mut Option<(String, String, Vec<&str>)>, out: &mut Vec<String>| {
        let Some((first, rest, words)) = paragraph.take() else {
            return;
        };
        let mut line = first;
        let mut has_word = false;
        for word in words {
            let len = line.chars().count();
            if has_word && len + 1 + word.chars().count() > width {
                out.push(std::mem::replace(&mut line, rest.clone()));
                has_word = false;
            }
            if has_word {
                line.push(' ');
            }
            line.push_str(word);
            has_word = true;
        }
        out.push(line);
    };
    let mut in_code = false;
    for line in block.split('\n') {
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        let fence = body.starts_with("```") || body.starts_with("~~~");
        if in_code || fence || body.is_empty() || body.starts_with('#') || body.starts_with('|') {
            flush(&mut paragraph, &mut out);
            if fence {
                in_code = !in_code;
            }
            out.push(if body.is_empty() {
                String::new()
            } else {
                line.to_string()
            });
            continue;
        }
        if let Some(marker) = list_marker(body) {
            flush(&mut paragraph, &mut out);
            let hanging = " ".repeat(indent.chars().count() + marker.chars().count());
            let words = body[marker.len()..].split_whitespace().collect();
            paragraph = Some((format!("{indent}{marker}"), hanging, words));
            continue;
        }
        match &mut paragraph {
            Some((_, _, words)) => words.extend(body.split_whitespace()),
            None => {
                paragraph = Some((
                    indent.to_string(),
                    indent.to_string(),
                    body.split_whitespace().collect(),
                ))
            }
        }
    }
    flush(&mut paragraph, &mut out);
    out.join("\n")
}

/// Splits `s` at each `delimiter` not escaped with a backslash (`\/` becomes `/`).
fn split_unescaped(s: &str, delimiter: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        let part = parts.last_mut().unwrap();
        match c {
            '\\' => match chars.next() {
                Some(d) if d == delimiter => part.push(d),
                Some(d) => {
                    part.push('\\');
                    part.push(d);
                }
                None => part.push('\\'),
            },
            c if c == delimiter => parts.push(String::new()),
            c => part.push(c),
        }
    }
    parts
}

/// A Vim replacement (`\1` for a group, `&` for the match, `\n` for a newline) as the
/// `regex` crate's.
fn vim_replacement(replacement: &str) -> String {
    let mut out = String::new();
    let mut chars = replacement.chars();
    while let Some(c) = chars.next() {
        match c {
            '$' => out.push_str("$$"),
            '&' => out.push_str("${0}"),
            '\\' => match chars.next() {
                Some(d @ '0'..='9') => out.push_str(&format!("${{{d}}}")),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('$') => out.push_str("$$"),
                Some(d) => out.push(d),
                None => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs keys (one char each; `⎋` is Esc) on `text` with the cursor at `at`, starting
    /// in Normal mode, and returns the text and cursor.
    fn run(text: &str, at: usize, keys: &str) -> (String, usize, Vim) {
        let mut vim = Vim {
            mode: Mode::Normal,
            ..Vim::default()
        };
        let mut text = text.to_string();
        let mut cursor = at;
        for c in keys.chars() {
            let key = match c {
                '⎋' => KeyEvent::from(KeyCode::Esc),
                '⏎' => KeyEvent::from(KeyCode::Enter),
                '⇥' => KeyEvent::from(KeyCode::Tab),
                c => KeyEvent::from(KeyCode::Char(c)),
            };
            let outcome = vim.on_key(key, &mut text, &mut cursor);
            if vim.mode == Mode::Insert && outcome == Outcome::Pass {
                text.insert(cursor, c);
                cursor += c.len_utf8();
            }
        }
        (text, cursor, vim)
    }

    fn edit(text: &str, at: usize, keys: &str) -> String {
        run(text, at, keys).0
    }

    #[test]
    fn motions_move_the_cursor() {
        assert_eq!(run("foo bar baz", 0, "w").1, 4);
        assert_eq!(run("foo bar baz", 0, "2w").1, 8);
        assert_eq!(run("foo bar baz", 0, "e").1, 2);
        assert_eq!(run("foo bar baz", 8, "b").1, 4);
        assert_eq!(run("foo.bar baz", 0, "w").1, 3);
        assert_eq!(run("foo.bar baz", 0, "W").1, 8);
        assert_eq!(run("foo bar", 0, "$").1, 6, "on the last character");
        assert_eq!(run("  foo", 4, "0").1, 0);
        assert_eq!(run("  foo", 0, "^").1, 2);
        assert_eq!(run("a\nb\nc", 4, "gg").1, 0);
        assert_eq!(run("a\nb\nc", 0, "G").1, 4);
        assert_eq!(run("a\nb\nc", 0, "2G").1, 2);
        assert_eq!(run("a,b,c", 0, "f,").1, 1);
        assert_eq!(run("a,b,c", 0, "f,;").1, 3);
        assert_eq!(run("a,b,c", 0, "t,").1, 0);
        assert_eq!(run("a,b,c", 4, "F,").1, 3);
        assert_eq!(run("f(a, (b))", 0, "%").1, 8);
        assert_eq!(run("héllo wörld", 0, "w").1, "héllo ".len());
    }

    #[test]
    fn operators_take_motions_and_counts() {
        assert_eq!(edit("foo bar baz", 0, "dw"), "bar baz");
        assert_eq!(edit("foo bar baz", 0, "d2w"), "baz");
        assert_eq!(edit("foo bar baz", 0, "2dw"), "baz");
        assert_eq!(edit("foo bar baz", 4, "D"), "foo ");
        assert_eq!(edit("foo bar", 0, "x"), "oo bar");
        assert_eq!(edit("foo bar", 0, "3x"), " bar");
        assert_eq!(edit("foo bar", 2, "X"), "fo bar");
        assert_eq!(edit("foo bar", 0, "de"), " bar");
        assert_eq!(edit("foo bar", 6, "db"), "foo r");
        assert_eq!(edit("a,b,c", 0, "dt,"), ",b,c");
        assert_eq!(edit("a,b,c", 0, "df,"), "b,c");
        assert_eq!(
            edit("foo\nbar", 0, "dw"),
            "\nbar",
            "dw stops at the line's end"
        );
        assert_eq!(edit("foo bar", 0, "cwqux⎋"), "qux bar");
        assert_eq!(edit("foo bar", 0, "c$x⎋"), "x");
        assert_eq!(edit("ab", 0, "rx"), "xb");
        assert_eq!(edit("abc", 0, "2rx"), "xxc");
        assert_eq!(edit("abc", 0, "~~"), "ABc");
    }

    #[test]
    fn lines_are_deleted_changed_yanked_and_put() {
        assert_eq!(edit("a\nb\nc", 2, "dd"), "a\nc");
        assert_eq!(edit("a\nb\nc", 4, "dd"), "a\nb");
        assert_eq!(edit("a\nb\nc", 0, "2dd"), "c");
        assert_eq!(edit("a\nb\nc", 0, "dj"), "c");
        assert_eq!(edit("a\nb\nc", 4, "dk"), "a");
        assert_eq!(edit("a\nb\nc", 2, "dG"), "a");
        assert_eq!(edit("a\nb\nc", 2, "dgg"), "c");
        assert_eq!(edit("only", 0, "dd"), "");
        assert_eq!(edit("a\nb", 0, "ccx⎋"), "x\nb");
        assert_eq!(edit("a\nb", 0, "yyp"), "a\na\nb");
        assert_eq!(edit("a\nb", 2, "yyP"), "a\nb\nb");
        assert_eq!(edit("a\nb", 0, "ddp"), "b\na");
        assert_eq!(edit("ab", 0, "xp"), "ba");
        assert_eq!(edit("ab", 0, "yl3p"), "aaaab");
        assert_eq!(edit("a\n  b", 0, "J"), "a b");
        assert_eq!(edit("x", 0, "ofoo⎋"), "x\nfoo");
        assert_eq!(edit("x", 0, "Ofoo⎋"), "foo\nx");
    }

    #[test]
    fn insert_commands_and_undo() {
        assert_eq!(edit("bc", 0, "ia⎋"), "abc");
        assert_eq!(edit("ac", 0, "ab⎋"), "abc");
        assert_eq!(edit("  b", 2, "Ia⎋"), "  ab");
        assert_eq!(edit("a", 0, "Ab⎋"), "ab");
        // A whole insert undoes in one step, and redo brings it back.
        assert_eq!(edit("x", 0, "Afoo⎋u"), "x");
        let (text, cursor, mut vim) = run("foo bar", 0, "dwdwu");
        assert_eq!(text, "bar");
        let (mut text, mut cursor) = (text, cursor);
        let redo = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        vim.on_key(redo, &mut text, &mut cursor);
        assert_eq!(text, "");
        assert_eq!(edit("a b c", 0, "dwdw2u"), "a b c");
    }

    #[test]
    fn text_objects() {
        assert_eq!(edit("foo bar baz", 5, "diw"), "foo  baz");
        assert_eq!(edit("foo bar baz", 5, "daw"), "foo baz");
        assert_eq!(edit(r#"say "hi there" ok"#, 7, "di\""), r#"say "" ok"#);
        assert_eq!(edit(r#"say "hi" ok"#, 0, "ci\"yo⎋"), r#"say "yo" ok"#);
        assert_eq!(edit("f(a, (b), c)", 3, "di("), "f()");
        assert_eq!(edit("f(a, (b), c)", 6, "da("), "f(a, , c)");
        assert_eq!(edit("{\n  x\n}", 4, "diB"), "{}");
    }

    #[test]
    fn visual_mode_selects_and_operates() {
        let (_, _, vim) = run("foo bar", 0, "vl");
        assert_eq!(vim.selection("foo bar", 1), Some((0, 2)));
        assert_eq!(edit("foo bar", 0, "vld"), "o bar");
        assert_eq!(edit("foo bar", 0, "vey$vp"), "foo bafoo");
        assert_eq!(edit("a\nb\nc", 0, "Vjd"), "c");
        assert_eq!(edit("foo bar", 4, "viwcx⎋"), "foo x");
        assert_eq!(edit("foo bar", 0, "veU"), "FOO bar");
        assert_eq!(edit("foo bar", 0, "v⎋x"), "oo bar");
    }

    #[test]
    fn normal_mode_keeps_typing_out_of_the_prompt() {
        let mut vim = Vim::default();
        let mut text = "abc".to_string();
        let mut cursor = 3;
        assert_eq!(
            vim.on_key(KeyEvent::from(KeyCode::Esc), &mut text, &mut cursor),
            Outcome::Handled
        );
        assert_eq!(vim.mode, Mode::Normal);
        assert_eq!(cursor, 2, "Esc steps back onto the last character");
        // Unknown keys are swallowed; Esc and Enter go to the prompt.
        assert_eq!(
            vim.on_key(KeyEvent::from(KeyCode::Char('Q')), &mut text, &mut cursor),
            Outcome::Handled
        );
        assert_eq!(text, "abc");
        assert_eq!(
            vim.on_key(KeyEvent::from(KeyCode::Esc), &mut text, &mut cursor),
            Outcome::Pass
        );
        assert_eq!(
            vim.on_key(KeyEvent::from(KeyCode::Enter), &mut text, &mut cursor),
            Outcome::Pass
        );
        assert_eq!(
            vim.mode,
            Mode::Insert,
            "a sent prompt leaves the next in Insert mode"
        );
    }

    #[test]
    fn the_prompt_edits_like_a_file() {
        // Enter starts an indented new line instead of sending; j/k stay in the text.
        assert_eq!(edit("", 0, "i  one⏎two⎋"), "  one\n  two");
        assert_eq!(edit("x", 0, "a⇥y⎋"), "x    y");
        assert_eq!(run("ab\ncd\nef", 1, "jj").1, 7);
        assert_eq!(
            run("ab\ncd", 4, "kkk").1,
            1,
            "k on the first line stays put"
        );
        assert_eq!(edit("  a", 2, "ob⎋"), "  a\n  b", "o keeps the indent");
        // A slash command still goes with Enter in Insert mode.
        let mut vim = Vim::default();
        let (mut text, mut cursor) = ("/help".to_string(), 5);
        assert_eq!(
            vim.on_key(KeyEvent::from(KeyCode::Enter), &mut text, &mut cursor),
            Outcome::Pass
        );
    }

    #[test]
    fn indent_reflow_and_paragraphs() {
        assert_eq!(edit("a\nb", 0, ">>"), "    a\nb");
        assert_eq!(edit("a\nb", 0, ">j"), "    a\n    b");
        assert_eq!(
            edit("a\n\nb", 0, "2>>"),
            "    a\n\nb",
            "blank lines get no indent"
        );
        assert_eq!(edit("      a", 0, "<<"), "  a");
        assert_eq!(edit("a\nb\nc", 2, "Vj>"), "a\n    b\n    c");
        assert_eq!(run("a\nb\n\nc", 0, "}").1, 4);
        assert_eq!(run("a\n\nb\nc", 6, "{").1, 2);
        assert_eq!(edit("a\nb\n\nc", 0, "dap"), "c");
        assert_eq!(edit("a\nb\n\nc", 2, "dip"), "\nc");
        let long = "word ".repeat(30);
        let wrapped = edit(long.trim(), 0, "gqq");
        assert!(
            wrapped.lines().count() == 2 && wrapped.lines().all(|l| l.len() <= 80),
            "{wrapped}"
        );
        assert_eq!(
            edit("one\ntwo\n\n- three\n  four", 0, "gqG"),
            "one two\n\n- three four"
        );
        assert_eq!(
            edit("```\na\nb\n```", 0, "gqG"),
            "```\na\nb\n```",
            "code stays"
        );
    }

    #[test]
    fn dot_repeats_the_last_change() {
        assert_eq!(edit("a b c d", 0, "dw.."), "d");
        assert_eq!(edit("a\nb\nc", 0, "Ax⎋j.j."), "ax\nbx\ncx");
        assert_eq!(edit("foo foo", 0, "cwbar⎋w."), "bar bar");
        assert_eq!(edit("a\nb", 0, ">>j."), "    a\n    b");
        assert_eq!(edit("abcd", 0, "x2."), "d");
    }

    #[test]
    fn ex_commands() {
        assert_eq!(edit("a\nb\nc", 0, ":3⏎x"), "a\nb\n");
        assert_eq!(edit("foo foo\nfoo", 0, ":s/foo/bar/⏎"), "bar foo\nfoo");
        assert_eq!(edit("foo foo\nfoo", 0, ":%s/foo/bar/g⏎"), "bar bar\nbar");
        assert_eq!(
            edit("ab", 0, r":s/\(a\)/[&]/⏎"),
            "ab",
            "Vim's \\( isn't a group here"
        );
        assert_eq!(edit("ab", 0, ":s/(a)(b)/\\2\\1/⏎"), "ba");
        assert_eq!(edit("A", 0, ":s/a/b/i⏎"), "b");
        assert_eq!(edit("x/y", 0, ":s/\\//-/⏎"), "x-y");
        assert_eq!(edit("abc", 0, ":q⏎"), "");
        assert_eq!(edit("abc", 0, ":q⏎u"), "abc");
        let (_, _, vim) = run("abc", 0, ":s/z/y/⏎");
        assert_eq!(vim.label(), "Pattern not found");
        let (_, _, vim) = run("abc", 0, ":wq");
        assert_eq!(vim.label(), ":wq");
        let mut vim = Vim {
            mode: Mode::Normal,
            ..Vim::default()
        };
        let (mut text, mut cursor) = ("hi".to_string(), 0);
        for c in ":w".chars() {
            vim.on_key(KeyEvent::from(KeyCode::Char(c)), &mut text, &mut cursor);
        }
        assert_eq!(
            vim.on_key(KeyEvent::from(KeyCode::Enter), &mut text, &mut cursor),
            Outcome::Send
        );
        assert_eq!(vim.mode, Mode::Insert);
    }
}
