//! `/add-dir` with no path: browse the file tree and pick a directory to add to the scope.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use super::App;
use super::view::{tilde, truncate_chars};

/// The browser's state: the directory being shown and its subdirectories.
pub(super) struct Browser {
    dir: PathBuf,
    /// Subdirectory names, sorted; hidden ones are listed only when the filter starts with `.`.
    subdirs: Vec<String>,
    filter: String,
    /// Row 0 is the shown directory itself; rows after it are the filtered subdirectories.
    selected: usize,
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
            selected: 0,
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
        let hidden = self.filter.starts_with('.');
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
        *self = Self::new(parent, left.as_deref());
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

    pub(super) fn on_dir_browser_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(browser) = &mut self.dir_browser else {
            return;
        };
        let last = browser.rows() - 1;
        match key.code {
            KeyCode::Esc => self.dir_browser = None,
            KeyCode::Char('c') if ctrl => self.dir_browser = None,
            KeyCode::Up => browser.selected = browser.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => browser.selected = browser.selected.saturating_sub(1),
            KeyCode::Down => browser.selected = (browser.selected + 1).min(last),
            KeyCode::Char('n') if ctrl => browser.selected = (browser.selected + 1).min(last),
            KeyCode::PageUp => browser.selected = browser.selected.saturating_sub(10),
            KeyCode::PageDown => browser.selected = (browser.selected + 10).min(last),
            KeyCode::Home => browser.selected = 0,
            KeyCode::End => browser.selected = last,
            KeyCode::Right | KeyCode::Tab => browser.open(),
            KeyCode::Left => browser.up(),
            KeyCode::Backspace if browser.filter.is_empty() => browser.up(),
            KeyCode::Backspace => {
                let mut filter = browser.filter.clone();
                filter.pop();
                browser.set_filter(filter);
            }
            KeyCode::Char('u') if ctrl => browser.set_filter(String::new()),
            KeyCode::Char('~') if browser.filter.is_empty() => {
                if let Some(home) = std::env::var_os("HOME") {
                    *browser = Browser::new(PathBuf::from(home), None);
                }
            }
            KeyCode::Char('/') if browser.filter.is_empty() => {
                *browser = Browser::new(PathBuf::from("/"), None);
            }
            KeyCode::Char(c) if !ctrl => {
                let filter = format!("{}{c}", browser.filter);
                browser.set_filter(filter);
            }
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

    pub(super) fn draw_dir_browser(&self, frame: &mut Frame, area: Rect) {
        let Some(browser) = &self.dir_browser else {
            return;
        };
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(24));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Add directory to scope ".bold())
            .title_bottom(
                Line::from(" type to filter · →/← open/up · ~ home · enter add · esc cancel ")
                    .right_aligned(),
            );
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        let [path_row, filter_row, list_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(inner);

        let width = path_row.width.saturating_sub(2) as usize;
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
        let filter = if browser.filter.is_empty() {
            Line::from("› type to filter (. shows hidden)".fg(Color::DarkGray))
        } else {
            Line::from(vec!["› ".into(), browser.filter.clone().into()])
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
        let mut state = ListState::default().with_selected(Some(browser.selected));
        frame.render_stateful_widget(list, list_area, &mut state);
    }
}
