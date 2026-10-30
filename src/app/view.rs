use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Wrap,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::help::help_lines;
use super::{
    App, EFFORT_CHOICES, QUIET_SUMMARY_CHARS, QUIET_TOOL_CALLS, SPINNER, TOOL_PREVIEW_LINES,
    effort_label,
};
use crate::permissions;
use crate::session;
use crate::session::Entry;

impl App {
    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let input_width = area.width.saturating_sub(4).max(1) as usize;
        let (rows, cursor) = wrap_input(&self.input, self.cursor, input_width);
        let input_height = (rows.len() as u16).clamp(1, 10) + 2;

        let [chat_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(2),
        ])
        .areas(area);

        self.draw_chat(frame, chat_area);
        self.draw_input(frame, input_area, rows, cursor);
        self.draw_status(frame, status_area);
        if self.help_scroll.is_some() {
            self.draw_help(frame, chat_area);
        }
        if self.picker.is_some() {
            self.draw_picker(frame, chat_area);
        }
        if self.effort_picker.is_some() {
            self.draw_effort_picker(frame, chat_area);
        }
        if self.resume_picker.is_some() {
            self.draw_resume_picker(frame, chat_area);
        }
        if self.rewind_picker.is_some() {
            self.draw_rewind_picker(frame, chat_area);
        }
        if self.approval.is_some() {
            self.draw_approval(frame, chat_area);
        }
    }

    fn draw_resume_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.resume_picker else {
            return;
        };
        let popup = area.centered(Constraint::Percentage(80), Constraint::Max(20));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Resume session ".bold())
            .title_bottom(
                Line::from(" type to filter · ↑/↓ move · enter resume · esc cancel ")
                    .right_aligned(),
            );
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);

        let [filter_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::from(" Filter: ").fg(Color::DarkGray),
                Span::from(picker.filter.as_str()),
            ])),
            filter_area,
        );
        frame.set_cursor_position(Position::new(
            filter_area.x + 9 + picker.filter.width() as u16,
            filter_area.y,
        ));

        let sessions = picker.filtered();
        if sessions.is_empty() {
            frame.render_widget(
                Paragraph::new(" No matching sessions".fg(Color::DarkGray)),
                list_area,
            );
            return;
        }
        let ages: Vec<String> = sessions.iter().map(|m| session::ago(m.updated)).collect();
        let age_width = ages.iter().map(|a| a.width()).max().unwrap_or(0);
        let items: Vec<ListItem> = sessions
            .iter()
            .zip(&ages)
            .map(|(m, age)| {
                let s = if m.prompts == 1 { "" } else { "s" };
                ListItem::new(Line::from(vec![
                    Span::from(format!("{age:>age_width$}  ")).fg(Color::DarkGray),
                    Span::from(if m.title.is_empty() {
                        "(untitled)".to_string()
                    } else {
                        m.title.clone()
                    }),
                    Span::from(format!("  {} prompt{s} · {}", m.prompts, m.model))
                        .fg(Color::DarkGray),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(list, list_area, &mut state);
    }

    fn draw_effort_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(selected) = self.effort_picker else {
            return;
        };
        let label_width = EFFORT_CHOICES
            .iter()
            .map(|(e, _)| effort_label(*e).len())
            .max()
            .unwrap_or(0);
        let items: Vec<ListItem> = EFFORT_CHOICES
            .iter()
            .enumerate()
            .map(|(i, (effort, desc))| {
                let current = *effort == self.settings.model.effort;
                ListItem::new(Line::from(vec![
                    Span::from(if current { "● " } else { "  " }).fg(Color::Green),
                    Span::from(format!("{} ", i + 1)).fg(Color::DarkGray),
                    Span::from(format!("{:<label_width$}  ", effort_label(*effort))).bold(),
                    Span::from(*desc).fg(Color::DarkGray),
                ]))
            })
            .collect();
        let width = (items.iter().map(ListItem::width).max().unwrap_or(0) + 6).max(48) as u16;
        let height = EFFORT_CHOICES.len() as u16 + 2;
        let popup = area.centered(Constraint::Max(width), Constraint::Length(height));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Select effort ".bold())
            .title_bottom(Line::from(" ↑/↓ move · enter select · esc cancel ").right_aligned());
        frame.render_widget(Clear, popup);
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(list, popup, &mut state);
    }

    fn draw_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.picker else {
            return;
        };
        let popup = area.centered(Constraint::Max(70), Constraint::Max(20));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Select model ".bold())
            .title_bottom(
                Line::from(" type to filter · ↑/↓ move · enter select · esc cancel ")
                    .right_aligned(),
            );
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);

        let [filter_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::from(" Filter: ").fg(Color::DarkGray),
                Span::from(picker.filter.as_str()),
            ])),
            filter_area,
        );
        frame.set_cursor_position(Position::new(
            filter_area.x + 9 + picker.filter.width() as u16,
            filter_area.y,
        ));

        if self.models.is_none() {
            frame.render_widget(
                Paragraph::new(
                    format!(" {} Loading models…", SPINNER[self.tick % SPINNER.len()])
                        .fg(Color::Yellow),
                ),
                list_area,
            );
            return;
        }
        let models = self.filtered_models();
        if models.is_empty() {
            frame.render_widget(
                Paragraph::new(" No matching models".fg(Color::DarkGray)),
                list_area,
            );
            return;
        }
        let id_width = models.iter().map(|m| m.id.len()).max().unwrap_or(0);
        let items: Vec<ListItem> = models
            .iter()
            .map(|m| {
                let current = m.id == self.settings.model.name;
                ListItem::new(Line::from(vec![
                    Span::from(if current { "● " } else { "  " }).fg(Color::Green),
                    Span::from(format!("{:<id_width$}  ", m.id)),
                    Span::from(m.name.as_str()).fg(Color::DarkGray),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(picker.selected));
        frame.render_stateful_widget(list, list_area, &mut state);
    }

    fn draw_help(&mut self, frame: &mut Frame, area: Rect) {
        let popup = area.centered(Constraint::Percentage(80), Constraint::Percentage(90));
        let lines = help_lines();
        let inner_height = popup.height.saturating_sub(2) as usize;
        let max_scroll = lines.len().saturating_sub(inner_height);
        let scroll = self.help_scroll.unwrap_or(0).min(max_scroll);
        self.help_scroll = Some(scroll);
        self.help_page = inner_height.saturating_sub(1);

        let position = if max_scroll == 0 {
            "all".to_string()
        } else {
            format!("{}%", scroll * 100 / max_scroll)
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Help ".bold())
            .title_bottom(
                Line::from(format!(" ↑/↓ scroll · esc close · {position} ")).right_aligned(),
            );

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((scroll as u16, 0)),
            popup,
        );
        if max_scroll > 0 {
            let mut state = ScrollbarState::new(max_scroll).position(scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                popup.inner(Margin::new(0, 1)),
                &mut state,
            );
        }
    }

    fn draw_chat(&mut self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        self.transcript
            .sync(&self.entries, area.width, self.settings.ui.verbose);
        let running = self.is_running();
        let spinner = Line::from(
            format!(
                "{} {}",
                SPINNER[self.tick % SPINNER.len()],
                if self.compacting {
                    "Compacting…"
                } else if self.approval.is_some() {
                    "Waiting for approval…"
                } else {
                    "Thinking…"
                }
            )
            .fg(Color::Yellow),
        );
        let spinner_rows = if running {
            Paragraph::new(spinner.clone())
                .wrap(Wrap { trim: false })
                .line_count(area.width)
        } else {
            0
        };
        let total = self.transcript.rows() + spinner_rows;
        let max_scroll = total.saturating_sub(area.height as usize);
        self.scroll_back = self.scroll_back.min(max_scroll);
        let offset = max_scroll - self.scroll_back;
        let (mut lines, local_offset) = self
            .transcript
            .visible(offset.min(self.transcript.rows()), area.height as usize);
        if running {
            lines.push(spinner);
        }
        let local_offset = if offset > self.transcript.rows() {
            (offset - self.transcript.rows()) as u16
        } else {
            local_offset
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((local_offset, 0)),
            area,
        );
        if self.scroll_back > 0 {
            let mut state = ScrollbarState::new(max_scroll).position(offset);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area,
                &mut state,
            );
            let hint = format!(" ↓ {} more lines ", self.scroll_back);
            let width = (hint.width() as u16).min(area.width);
            let hint_area = Rect::new(
                area.right().saturating_sub(width + 1),
                area.bottom().saturating_sub(1),
                width,
                1,
            );
            frame.render_widget(Clear, hint_area);
            frame.render_widget(
                Paragraph::new(hint).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
                hint_area,
            );
        }
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect, rows: Vec<String>, cursor: (usize, usize)) {
        let visible = area.height.saturating_sub(2) as usize;
        let skip = (cursor.0 + 1).saturating_sub(visible);
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .skip(skip)
            .map(|(i, row)| {
                let prefix = if i == 0 { "› " } else { "  " };
                Line::from(vec![Span::from(prefix).fg(Color::Cyan), Span::from(row)])
            })
            .collect();
        let border = if self.is_running() {
            Color::DarkGray
        } else {
            Color::Cyan
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border));
        frame.render_widget(Paragraph::new(lines).block(block), area);
        frame.set_cursor_position(Position::new(
            area.x + 3 + cursor.1 as u16,
            area.y + 1 + (cursor.0 - skip) as u16,
        ));
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let cwd = self.workspace.cwd.display().to_string();
        let mut cwd_spans = vec![
            Span::from(" cwd: ").fg(Color::DarkGray),
            Span::from(cwd).fg(Color::Cyan),
        ];
        if !self.workspace.additional_dirs.is_empty() {
            cwd_spans.push(
                Span::from(format!(" +{} dirs", self.workspace.additional_dirs.len()))
                    .fg(Color::DarkGray),
            );
        }
        let cwd_line = Line::from(cwd_spans);
        let hint = if self.approval.is_some() {
            "approve the tool call · esc to interrupt"
        } else if self.rewind_picker.is_some() {
            "↑/↓ move · enter rewind · esc cancel"
        } else if self.picker.is_some() || self.resume_picker.is_some() {
            "type to filter · ↑/↓ move · enter select · esc cancel"
        } else if self.effort_picker.is_some() {
            "↑/↓ move · 1–6 jump · enter select · esc cancel"
        } else if self.help_scroll.is_some() {
            "↑/↓ j/k scroll · pgup/pgdn page · esc close"
        } else if self.is_running() {
            "esc to interrupt"
        } else {
            "enter send · shift+tab mode · /help"
        };
        let mode = self.permissions.lock().unwrap().mode;
        let mode_color = match mode {
            permissions::Mode::Ask => Color::Green,
            permissions::Mode::AcceptEdits => Color::Yellow,
            permissions::Mode::Auto => Color::Red,
        };
        let window = self.settings.model.context_window();
        let pct = self.context_tokens * 100 / window.max(1);
        let ctx_color = match pct {
            0..60 => Color::DarkGray,
            60..80 => Color::Yellow,
            _ => Color::Red,
        };
        let cached = if self.input_tokens > 0 {
            format!(
                " ({}% cached)",
                self.cached_tokens * 100 / self.input_tokens
            )
        } else {
            String::new()
        };
        let line = Line::from(vec![
            Span::from(format!(" {} ", mode.as_str()))
                .fg(mode_color)
                .bold(),
            Span::from(format!("· {} ", self.settings.model.name)).fg(Color::Magenta),
            Span::from(if self.settings.ui.verbose {
                "· verbose "
            } else {
                ""
            })
            .fg(Color::Magenta),
            Span::from(format!(
                "· thinking {} ",
                self.settings.model.thinking.as_str()
            ))
            .fg(Color::Magenta),
            Span::from(match self.settings.model.effort {
                Some(e) => format!("· {} ", e.as_str()),
                None => String::new(),
            })
            .fg(Color::Magenta),
            Span::from(format!(
                "· ctx {}/{} {pct}% ",
                fmt_tokens(self.context_tokens),
                fmt_tokens(window)
            ))
            .fg(ctx_color),
            Span::from(format!(
                "· ↑{}{cached} ↓{} ",
                fmt_tokens(self.input_tokens),
                fmt_tokens(self.output_tokens)
            ))
            .fg(Color::DarkGray),
            Span::from(format!(
                "· ${:.2}{} ",
                self.cost,
                if self.cost_partial { "+" } else { "" }
            ))
            .fg(Color::Green),
            Span::from(format!("· {hint}")).fg(Color::DarkGray),
        ]);
        frame.render_widget(Paragraph::new(vec![cwd_line, line]), area);
    }
}

/// Compact token count, e.g. 950, 12.3k, 1.2M.
pub(super) fn fmt_tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1e3),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// Compact chat rendering: each tool call is one line showing what the step is for (its
/// description) without output, and long runs of consecutive steps are collapsed to the
/// most recent few.
pub(super) fn render_quiet<'a>(entries: &'a [Entry], lines: &mut Vec<Line<'a>>) {
    let mut i = 0;
    while i < entries.len() {
        if !matches!(entries[i], Entry::Tool { .. }) {
            render_entry(&entries[i], false, lines);
            lines.push(Line::default());
            i += 1;
            continue;
        }
        let end = entries[i..]
            .iter()
            .position(|e| !matches!(e, Entry::Tool { .. }))
            .map_or(entries.len(), |n| i + n);
        let hidden = (end - i).saturating_sub(QUIET_TOOL_CALLS);
        if hidden > 0 {
            let s = if hidden == 1 { "" } else { "s" };
            lines.push(Line::from(
                format!("  … {hidden} earlier step{s} (/verbose to show)").fg(Color::DarkGray),
            ));
        }
        for entry in &entries[i + hidden..end] {
            render_entry(entry, false, lines);
        }
        lines.push(Line::default());
        i = end;
    }
}

pub(super) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

pub(super) fn render_entry<'a>(entry: &'a Entry, verbose: bool, lines: &mut Vec<Line<'a>>) {
    match entry {
        Entry::User(text) => {
            for (i, l) in text.lines().enumerate() {
                let prefix = if i == 0 { "› " } else { "  " };
                lines.push(Line::from(vec![
                    Span::from(prefix).fg(Color::Cyan).bold(),
                    Span::from(l).bold(),
                ]));
            }
        }
        Entry::Assistant(text) => render_markdown(text, lines),
        Entry::Tool {
            name,
            summary,
            description,
            output,
        } => {
            let dot_color = match output {
                None => Color::Yellow,
                Some((_, true)) => Color::Red,
                Some((_, false)) => Color::Green,
            };
            let summary = summary.lines().next().unwrap_or("");
            let mut spans = vec![
                Span::from("⏺ ").fg(dot_color),
                Span::from(description.as_str()),
            ];
            if verbose {
                lines.push(Line::from(spans));
                lines.push(Line::from(vec![
                    Span::from("  ").fg(Color::DarkGray),
                    Span::from(name.as_str()).fg(Color::Gray).bold(),
                    Span::from(format!("({summary})")).fg(Color::DarkGray),
                ]));
            } else {
                let detail = truncate_chars(summary, QUIET_SUMMARY_CHARS);
                spans.push(Span::from(format!("  {name} {detail}")).fg(Color::DarkGray));
                lines.push(Line::from(spans));
            }
            if !verbose {
                return;
            }
            if let Some((out, is_error)) = output {
                let style = if *is_error {
                    Style::new().fg(Color::Red)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                let all: Vec<&str> = out.trim_end().lines().collect();
                for (i, l) in all.iter().take(TOOL_PREVIEW_LINES).enumerate() {
                    let prefix = if i == 0 { "  ⎿ " } else { "    " };
                    lines.push(Line::from(vec![
                        Span::from(prefix).fg(Color::DarkGray),
                        Span::styled(*l, style),
                    ]));
                }
                if all.len() > TOOL_PREVIEW_LINES {
                    lines.push(Line::from(
                        format!("    … +{} lines", all.len() - TOOL_PREVIEW_LINES)
                            .fg(Color::DarkGray),
                    ));
                }
            }
        }
        Entry::Info(text) => {
            for l in text.lines() {
                lines.push(Line::from(l.fg(Color::DarkGray).italic()));
            }
        }
        Entry::Error(text) => {
            for l in text.lines() {
                lines.push(Line::from(l.fg(Color::Red)));
            }
        }
    }
}

/// Very small markdown styling: code fences, headings, bullets and `inline code`.
fn render_markdown<'a>(text: &'a str, lines: &mut Vec<Line<'a>>) {
    render_markdown_from(text, lines, false);
}

pub(super) struct MarkdownTail {
    pub byte: usize,
    pub in_code: bool,
}

/// Return the start/state of the last source line, which is the only previously
/// rendered line that can change when another streaming chunk is appended.
pub(super) fn render_markdown_from<'a>(
    text: &'a str,
    lines: &mut Vec<Line<'a>>,
    mut in_code: bool,
) -> MarkdownTail {
    let mut tail = MarkdownTail { byte: 0, in_code };
    let mut byte = 0;
    for segment in text.split_inclusive('\n') {
        tail = MarkdownTail { byte, in_code };
        byte += segment.len();
        let l = segment
            .strip_suffix('\n')
            .map_or(segment, |line| line.strip_suffix('\r').unwrap_or(line));
        if l.trim_start().starts_with("```") {
            in_code = !in_code;
            lines.push(Line::from(l.fg(Color::DarkGray)));
            continue;
        }
        if in_code {
            lines.push(Line::from(
                Span::from(format!("  {l}")).fg(Color::LightYellow),
            ));
            continue;
        }
        if let Some(h) = l.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim_start();
            lines.push(Line::from(h.bold().add_modifier(Modifier::UNDERLINED)));
            continue;
        }
        let mut spans = Vec::new();
        for (i, part) in l.split('`').enumerate() {
            if i % 2 == 1 {
                spans.push(part.fg(Color::LightYellow));
            } else {
                for (j, p) in part.split("**").enumerate() {
                    spans.push(if j % 2 == 1 { p.bold() } else { Span::from(p) });
                }
            }
        }
        lines.push(Line::from(spans));
    }
    tail
}

/// Wraps input text to `width` columns, returning visual rows and the
/// (row, col) of the byte offset `cursor`.
fn wrap_input(input: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let mut rows = vec![String::new()];
    let mut col = 0;
    let mut pos = None;
    for (i, ch) in input.char_indices() {
        if ch == '\n' {
            if i == cursor {
                pos = Some((rows.len() - 1, col));
            }
            rows.push(String::new());
            col = 0;
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if col + w > width {
            rows.push(String::new());
            col = 0;
        }
        if i == cursor {
            pos = Some((rows.len() - 1, col));
        }
        rows.last_mut().unwrap().push(ch);
        col += w;
    }
    let pos = pos.unwrap_or_else(|| {
        if col >= width {
            rows.push(String::new());
            (rows.len() - 1, 0)
        } else {
            (rows.len() - 1, col)
        }
    });
    (rows, pos)
}
