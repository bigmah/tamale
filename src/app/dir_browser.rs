//! `/add-dir` with no path: browse the file tree and pick a directory to add to the scope.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use unicode_width::UnicodeWidthStr;

use super::App;
use super::view::{tilde, truncate_chars};

/// Rows kept between the selection and the list's top or bottom edge, like vim's `scrolloff`.
const SCROLLOFF: usize = 3;

/// The browser's state: the directory being shown and its subdirectories.
pub(super) struct Browser {
    dir: PathBuf,
    /// Subdirectory names, sorted; hidden ones are listed only with `.` toggled on or when
    /// the filter starts with `.`.
    subdirs: Vec<String>,
    filter: String,
    /// Whether keys are typing into the filter (after `/`), rather than moving, as in vim.
    filtering: bool,
    show_hidden: bool,
    /// Row 0 is the shown directory itself; rows after it are the filtered subdirectories.
    pub(super) selected: usize,
    /// The first row shown. It only moves when the selection nears an edge, so the
    /// selection moves over the list rather than the list under it.
    pub(super) offset: usize,
    /// Set when a directory is shown, so the next draw centres the selection (vim's `zz`).
    center: bool,
    /// Rows the list had when last drawn, for half-page and page moves.
    pub(super) height: usize,
    /// `g` was pressed, waiting for a second `g` to go to the top.
    pending_g: bool,
    /// Why the directory couldn't be listed, if it couldn't.
    error: Option<String>,
}

impl Browser {
    /// Opens at `dir` with `select` (a subdirectory name) highlighted, if it's there.
    pub(super) fn new(dir: PathBuf, select: Option<&str>) -> Self {
        let mut browser = Self {
            dir,
            subdirs: Vec::new(),
            filter: String::new(),
            filtering: false,
            show_hidden: false,
            selected: 0,
            offset: 0,
            center: true,
            height: 10,
            pending_g: false,
            error: None,
        };
        browser.load();
        if let Some(name) = select
            && let Some(i) = browser.filtered().iter().position(|n| *n == name)
        {
            browser.selected = i + 1;
        }
        browser
    }

    fn load(&mut self) {
        self.subdirs.clear();
        self.error = None;
        match std::fs::read_dir(&self.dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    // `metadata` follows symlinks, so a link to a directory counts as one.
                    if std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
                        self.subdirs
                            .push(entry.file_name().to_string_lossy().into_owned());
                    }
                }
                self.subdirs
                    .sort_by_key(|name| (name.to_lowercase(), name.clone()));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Subdirectories matching the filter (case-insensitive substring).
    pub(super) fn filtered(&self) -> Vec<&str> {
        let filter = self.filter.to_lowercase();
        let hidden = self.show_hidden || self.filter.starts_with('.');
        self.subdirs
            .iter()
            .filter(|n| hidden || !n.starts_with('.'))
            .filter(|n| n.to_lowercase().contains(&filter))
            .map(String::as_str)
            .collect()
    }

    /// The directory on the highlighted row.
    pub(super) fn highlighted(&self) -> PathBuf {
        match self.selected.checked_sub(1) {
            Some(i) => match self.filtered().get(i) {
                Some(name) => self.dir.join(name),
                None => self.dir.clone(),
            },
            None => self.dir.clone(),
        }
    }

    /// Whether a filter is being typed (after `/`).
    pub(super) fn filtering(&self) -> bool {
        self.filtering
    }

    fn rows(&self) -> usize {
        self.filtered().len() + 1
    }

    fn set_filter(&mut self, filter: String) {
        let before = self
            .selected
            .checked_sub(1)
            .and_then(|i| self.filtered().get(i).map(|n| n.to_string()));
        self.filter = filter;
        let names = self.filtered();
        // The highlighted directory stays highlighted while it matches; otherwise the first
        // match is, so Enter or → takes what was typed.
        self.selected = match before.and_then(|b| names.iter().position(|n| *n == b)) {
            Some(i) => i + 1,
            None if self.filter.is_empty() || names.is_empty() => 0,
            None => 1,
        };
    }

    /// Shows the highlighted subdirectory's contents.
    pub(super) fn open(&mut self) {
        if self.selected == 0 {
            return;
        }
        self.dir = self.highlighted();
        self.filter.clear();
        self.selected = 0;
        self.offset = 0;
        self.center = true;
        self.load();
    }

    /// Shows the parent directory, with the one just left highlighted.
    pub(super) fn up(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let left = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let (filtering, show_hidden, height) = (self.filtering, self.show_hidden, self.height);
        *self = Self::new(parent, None);
        self.filtering = filtering;
        self.show_hidden = show_hidden;
        self.height = height;
        self.select_name(left.as_deref());
    }

    /// Shows `dir` from the top, keeping how the browser was set.
    fn go_to(&mut self, dir: PathBuf) {
        let (show_hidden, height) = (self.show_hidden, self.height);
        *self = Self::new(dir, None);
        self.show_hidden = show_hidden;
        self.height = height;
    }

    fn select_name(&mut self, name: Option<&str>) {
        if let Some(name) = name
            && let Some(i) = self.filtered().iter().position(|n| *n == name)
        {
            self.selected = i + 1;
        }
    }

    /// Moves the selection by `by` rows, stopping at either end.
    fn step(&mut self, by: isize) {
        let last = self.rows() - 1;
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    fn toggle_hidden(&mut self) {
        let name = self
            .selected
            .checked_sub(1)
            .and_then(|i| self.filtered().get(i).map(|n| n.to_string()));
        self.show_hidden = !self.show_hidden;
        self.selected = 0;
        self.select_name(name.as_deref());
    }

    /// The first row to show in a list `height` rows tall: unchanged unless the selection
    /// came within `SCROLLOFF` rows of an edge, or centred on a newly shown directory.
    fn scroll(&mut self, height: usize) -> usize {
        self.height = height.max(1);
        let rows = self.rows();
        let max = rows.saturating_sub(height);
        let margin = SCROLLOFF.min(height.saturating_sub(1) / 2);
        if std::mem::take(&mut self.center) {
            self.offset = self.selected.saturating_sub(height / 2);
        } else if self.selected < self.offset + margin {
            self.offset = self.selected.saturating_sub(margin);
        } else if self.selected + margin >= self.offset + height {
            self.offset = self.selected + margin + 1 - height;
        }
        self.offset = self.offset.min(max);
        self.offset
    }
}

impl App {
    /// Opens the browser at the working directory's parent, where the directories one
    /// usually wants to add (siblings of the project) are.
    pub(super) fn open_dir_browser(&mut self) {
        let cwd = &self.workspace.cwd;
        let name = cwd.file_name().map(|n| n.to_string_lossy().into_owned());
        let browser = match cwd.parent() {
            Some(parent) => Browser::new(parent.to_path_buf(), name.as_deref()),
            None => Browser::new(cwd.clone(), None),
        };
        self.dir_browser = Some(browser);
    }

    /// Keys work as in vim: j/k move, h/l go up or into a directory, gg/G, Ctrl+D/U and
    /// Ctrl+F/B jump, and `/` starts typing a filter, which Enter keeps and Esc clears.
    pub(super) fn on_dir_browser_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(browser) = &mut self.dir_browser else {
            return;
        };
        let half = (browser.height / 2).max(1) as isize;
        let page = browser.height.saturating_sub(1).max(1) as isize;
        // Keys that move the same way whether or not a filter is being typed.
        match key.code {
            KeyCode::Char('c') if ctrl => {
                self.dir_browser = None;
                return;
            }
            KeyCode::Up => return browser.step(-1),
            KeyCode::Down => return browser.step(1),
            KeyCode::Char('p') if ctrl => return browser.step(-1),
            KeyCode::Char('n') if ctrl => return browser.step(1),
            KeyCode::Char('d') if ctrl => return browser.step(half),
            KeyCode::Char('u') if ctrl && !browser.filtering => return browser.step(-half),
            KeyCode::Char('f') if ctrl => return browser.step(page),
            KeyCode::Char('b') if ctrl => return browser.step(-page),
            KeyCode::PageUp => return browser.step(-page),
            KeyCode::PageDown => return browser.step(page),
            KeyCode::Right | KeyCode::Tab => return browser.open(),
            KeyCode::Left => return browser.up(),
            _ => {}
        }
        if browser.filtering {
            match key.code {
                KeyCode::Esc => {
                    browser.filtering = false;
                    browser.set_filter(String::new());
                }
                KeyCode::Enter => browser.filtering = false,
                KeyCode::Backspace if browser.filter.is_empty() => browser.filtering = false,
                KeyCode::Backspace => {
                    let mut filter = browser.filter.clone();
                    filter.pop();
                    browser.set_filter(filter);
                }
                KeyCode::Char('u') if ctrl => browser.set_filter(String::new()),
                KeyCode::Char(c) if !ctrl => {
                    let filter = format!("{}{c}", browser.filter);
                    browser.set_filter(filter);
                }
                _ => {}
            }
            return;
        }
        let pending_g = std::mem::take(&mut browser.pending_g);
        match key.code {
            KeyCode::Esc if !browser.filter.is_empty() => browser.set_filter(String::new()),
            KeyCode::Esc | KeyCode::Char('q') => self.dir_browser = None,
            KeyCode::Char('j') => browser.step(1),
            KeyCode::Char('k') => browser.step(-1),
            KeyCode::Char('l') => browser.open(),
            KeyCode::Char('h' | '-') | KeyCode::Backspace => browser.up(),
            KeyCode::Char('g') if pending_g => browser.selected = 0,
            KeyCode::Char('g') => browser.pending_g = true,
            KeyCode::Home => browser.selected = 0,
            KeyCode::Char('G') | KeyCode::End => browser.selected = browser.rows() - 1,
            KeyCode::Char('/') => browser.filtering = true,
            KeyCode::Char('.') => browser.toggle_hidden(),
            KeyCode::Char('~') => {
                if let Some(home) = std::env::var_os("HOME") {
                    browser.go_to(PathBuf::from(home));
                }
            }
            KeyCode::Char('\\') => browser.go_to(PathBuf::from("/")),
            KeyCode::Enter => {
                let dir = browser.highlighted();
                self.dir_browser = None;
                self.add_dir_path(&dir);
            }
            _ => {}
        }
    }

    pub(super) fn on_dir_browser_paste(&mut self, text: &str) {
        if let Some(browser) = &mut self.dir_browser {
            browser.filtering = true;
            let filter = format!("{}{}", browser.filter, text.replace('\n', " "));
            browser.set_filter(filter);
        }
    }

    /// Whether `dir` is already in scope: the working directory, an added one, or inside either.
    fn in_scope(&self, dir: &Path) -> bool {
        let Ok(dir) = dir.canonicalize() else {
            return false;
        };
        let cwd = self
            .workspace
            .cwd
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.cwd.clone());
        std::iter::once(&cwd)
            .chain(&self.workspace.additional_dirs)
            .any(|root| dir.starts_with(root))
    }

    pub(super) fn draw_dir_browser(&mut self, frame: &mut Frame, area: Rect) {
        let Some(browser) = &self.dir_browser else {
            return;
        };
        let hint = if browser.filtering {
            " type to filter · ↑/↓ move · tab open · enter done · esc clear "
        } else {
            " j/k move · l/h open/up · / filter · . hidden · ~ home · enter add · q quit "
        };
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(24));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Add directory to scope ".bold())
            .title_bottom(Line::from(hint).right_aligned());
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        let [path_row, filter_row, list_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(inner);

        let position = format!("{}/{} ", browser.selected + 1, browser.rows());
        let width = path_row.width.saturating_sub(2 + position.len() as u16) as usize;
        let path = tilde(&browser.dir);
        // Long paths keep their end, which is the part that tells directories apart.
        let count = path.chars().count();
        let path = if count > width && width > 1 {
            let tail: String = path.chars().skip(count - (width - 1)).collect();
            format!("…{tail}")
        } else {
            path
        };
        frame.render_widget(Paragraph::new(Line::from(path.bold())), path_row);
        frame.render_widget(
            Line::from(position.fg(Color::DarkGray)).right_aligned(),
            path_row,
        );
        let hidden = if browser.show_hidden {
            Span::from("  · showing hidden").fg(Color::DarkGray)
        } else {
            Span::from("")
        };
        let filter = if browser.filtering {
            frame.set_cursor_position(Position::new(
                filter_row.x + 1 + browser.filter.width() as u16,
                filter_row.y,
            ));
            Line::from(vec![
                Span::from("/").fg(Color::Cyan).bold(),
                browser.filter.clone().into(),
            ])
        } else if browser.filter.is_empty() {
            Line::from(vec![
                Span::from("/ to filter").fg(Color::DarkGray),
                hidden.clone(),
            ])
        } else {
            Line::from(vec![
                Span::from("/").fg(Color::DarkGray),
                Span::from(browser.filter.clone()).fg(Color::Cyan),
                Span::from("  esc clears").fg(Color::DarkGray),
                hidden.clone(),
            ])
        };
        frame.render_widget(Paragraph::new(filter), filter_row);

        let note = |dir: &Path| {
            if self.in_scope(dir) {
                Span::from("  in scope").fg(Color::DarkGray)
            } else {
                Span::from("")
            }
        };
        let mut items = vec![ListItem::new(Line::from(vec![
            Span::from(". (this directory)"),
            note(&browser.dir),
        ]))];
        if let Some(error) = &browser.error {
            items.push(ListItem::new(Line::from(
                format!("  can't list: {error}").fg(Color::Red),
            )));
        } else {
            let names = browser.filtered();
            if names.is_empty() {
                let empty = if browser.filter.is_empty() {
                    "  no subdirectories"
                } else {
                    "  no matches"
                };
                items.push(ListItem::new(Line::from(empty.fg(Color::DarkGray))));
            }
            for name in names {
                items.push(ListItem::new(Line::from(vec![
                    Span::from(format!("{}/", truncate_chars(name, 70))),
                    note(&browser.dir.join(name)),
                ])));
            }
        }
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let selected = browser.selected;
        let Some(browser) = &mut self.dir_browser else {
            return;
        };
        let offset = browser.scroll(list_area.height as usize);
        let mut state = ListState::default()
            .with_offset(offset)
            .with_selected(Some(selected));
        frame.render_stateful_widget(list, list_area, &mut state);
    }
}
