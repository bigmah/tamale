use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Wrap,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::activity;
use super::backends::Choice;
use super::help::help_lines;
use super::markdown;
use super::{
    App, EFFORT_CHOICES, FooterTab, QUIET_SUMMARY_CHARS, QUIET_TOOL_CALLS, SPINNER,
    TOOL_PREVIEW_LINES, effort_label,
};
use crate::backend;
use crate::permissions;
use crate::session;
use crate::session::Entry;

impl App {
    pub fn draw(&mut self, frame: &mut Frame) {
        #[cfg(unix)]
        if self.remote_view.is_some() {
            self.draw_remote_view(frame);
            return;
        }
        let area = frame.area();
        let input_width = area.width.saturating_sub(4).max(1) as usize;
        let (rows, cursor) = wrap_input(&self.input, self.cursor, input_width);
        let input_height = (rows.len() as u16).clamp(1, 10) + 2;
        let queued = self.queued_messages();
        let queued_height = queued.len().min(MAX_QUEUED_LINES) as u16;
        let goal_height = self.goal.is_some() as u16;

        let [chat_area, goal_area, queued_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(goal_height),
            Constraint::Length(queued_height),
            Constraint::Length(input_height),
            Constraint::Length(2),
        ])
        .areas(area);

        self.draw_chat(frame, chat_area);
        self.draw_goal(frame, goal_area);
        draw_queued(frame, queued_area, &queued);
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
        if self.backend_picker.is_some() {
            self.draw_backend_picker(frame, chat_area);
        }
        #[cfg(unix)]
        if self.sessions_picker.is_some() {
            self.draw_sessions_picker(frame, chat_area);
        }
        if self.resume_picker.is_some() {
            self.draw_resume_picker(frame, chat_area);
        }
        if self.rewind_picker.is_some() {
            self.draw_rewind_picker(frame, chat_area);
        }
        if self.diff_open {
            self.draw_diff(frame);
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

    fn draw_backend_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.backend_picker else {
            return;
        };
        let choices = self.backend_choices();
        let items: Vec<ListItem> = choices
            .iter()
            .enumerate()
            .map(|(i, choice)| {
                let (name, detail, signed_out) = match *choice {
                    Choice::Api if backend::api_key_set() => (
                        "api",
                        "Anthropic API · ANTHROPIC_API_KEY".to_string(),
                        false,
                    ),
                    Choice::Api => (
                        "api",
                        "Anthropic API · ANTHROPIC_API_KEY not set".to_string(),
                        true,
                    ),
                    Choice::Login(n) => {
                        let login = &self.logins[n];
                        let mut detail = login.label_with_account();
                        if !login.signed_in()
                            && let Some(dir) = login.login_dir()
                        {
                            detail.push_str(&format!(" · {}", tilde(dir)));
                        }
                        (login.kind().as_str(), detail, !login.signed_in())
                    }
                    Choice::AddLogin(kind) => (
                        "+",
                        format!("Sign in to another {} account…", kind.cli_name()),
                        false,
                    ),
                };
                let current = self.is_current(*choice);
                ListItem::new(Line::from(vec![
                    Span::from(if current { "● " } else { "  " }).fg(Color::Green),
                    Span::from(format!("{} ", i + 1)).fg(Color::DarkGray),
                    Span::from(format!("{name:<11}  ")).bold(),
                    Span::from(detail).fg(if signed_out {
                        Color::Red
                    } else {
                        Color::DarkGray
                    }),
                ]))
            })
            .collect();
        let width = (items.iter().map(ListItem::width).max().unwrap_or(0) + 6).max(60) as u16;
        let height = items.len() as u16 + 2;
        let popup = area.centered(Constraint::Max(width), Constraint::Length(height));
        let title = if picker.loading {
            format!(
                " Select backend {} checking logins… ",
                SPINNER[self.tick % SPINNER.len()]
            )
        } else {
            " Select backend ".to_string()
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(title.bold())
            .title_bottom(
                Line::from(" ↑/↓ move · enter select · d remove · esc cancel ").right_aligned(),
            );
        frame.render_widget(Clear, popup);
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black))
            .highlight_symbol("› ");
        let selected = picker.selected.min(choices.len() - 1);
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(list, popup, &mut state);
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
        let since = if running {
            *self.busy_since.get_or_insert_with(std::time::Instant::now)
        } else {
            self.busy_since = None;
            std::time::Instant::now()
        };
        let (label, palette) = if self.compacting {
            ("Compacting…", activity::COMPACTING)
        } else if self.approval.is_some() {
            ("Waiting for approval…", activity::WAITING)
        } else {
            ("Thinking…", activity::THINKING)
        };
        let spinner = activity::line(label, palette, since.elapsed(), self.approval.is_none());
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
        let border = if self.is_running() || self.tab_focus.is_some() {
            Color::DarkGray
        } else {
            Color::Cyan
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border));
        frame.render_widget(Paragraph::new(lines).block(block), area);
        // With the tabs focused there is no cursor in the prompt.
        if self.tab_focus.is_some() {
            return;
        }
        frame.set_cursor_position(Position::new(
            area.x + 3 + cursor.1 as u16,
            area.y + 1 + (cursor.0 - skip) as u16,
        ));
    }

    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        // Where inference comes from stays visible; a long cwd loses its start instead.
        let backend = self.backend_label();
        let dirs = match self.workspace.additional_dirs.len() {
            0 => String::new(),
            n => format!(" +{n} dirs"),
        };
        // Remote and Web sit left of Chat, Diff right of it, as ← and → reach them once ↓
        // has focused the tabs; a click on Remote or Web works too.
        // The highlighted tab is filled; the others stay quiet, Chat marked as current.
        let focus = self.tab_focus;
        let picked = |tab| focus == Some(tab);
        let style = |tab| {
            if picked(tab) {
                Style::new().fg(Color::Black).bg(Color::Cyan).bold()
            } else if tab == FooterTab::Chat {
                Style::new().fg(Color::Cyan).bold()
            } else {
                Style::new().fg(Color::Gray)
            }
        };
        let mut chat = self
            .tab_spans()
            .unwrap_or_else(|| vec![Span::from(" Chat ")]);
        let has_diff = chat.len() > 1;
        chat[0] = Span::styled(" Chat ", style(FooterTab::Chat));
        if has_diff {
            // A whole " Diff " fills evenly; the counts after it lose their leading gap.
            chat[1] = Span::styled(" Diff ", style(FooterTab::Diff));
            if let Some(next) = chat.get_mut(2) {
                next.content = next.content.trim_start().to_string().into();
            }
        }
        // Arrows show where ← and → can still go.
        let arrow =
            |s: &'static str, shown: bool| Span::from(if shown { s } else { " " }).fg(Color::Cyan);
        let remote_on = self.remote_on();
        let remote_tab = if remote_on { REMOTE_ON_TAB } else { REMOTE_OFF_TAB };
        let mut remote_style = style(FooterTab::Remote);
        if remote_on && !picked(FooterTab::Remote) {
            remote_style = remote_style.fg(Color::Green);
        }
        let mut tabs = vec![
            arrow("←", focus.is_some_and(|t| t != FooterTab::Remote)),
            Span::styled(remote_tab, remote_style),
            Span::styled(WEB_TAB, style(FooterTab::Web)),
        ];
        let remote_offset = tabs[0].content.width() as u16;
        tabs.extend(chat);
        tabs.push(Span::styled(SESSIONS_TAB, style(FooterTab::Sessions)));
        tabs.push(arrow("→", focus.is_some_and(|t| t != FooterTab::Sessions)));
        tabs.push(Span::from(" "));
        let tabs_width: usize = tabs.iter().map(|s| s.content.width()).sum();
        let fixed = " cwd: ".len()
            + dirs.width()
            + " · via ".len()
            + backend.width()
            + tabs_width
            + 1
            + if self.hub_viewers.is_some() { 18 } else { 0 };
        let cwd = truncate_start(
            &self.workspace.cwd.display().to_string(),
            (area.width as usize).saturating_sub(fixed).max(10),
        );
        let cwd_spans = vec![
            Span::from(" cwd: ").fg(Color::DarkGray),
            Span::from(cwd).fg(Color::Cyan),
            Span::from(dirs).fg(Color::DarkGray),
            Span::from(" · via ").fg(Color::DarkGray),
            Span::from(backend)
                .fg(if self.backend.signed_in() {
                    Color::Blue
                } else {
                    Color::Red
                })
                .bold(),
        ];
        let mut cwd_spans = cwd_spans;
        if let Some(viewers) = self.hub_viewers {
            cwd_spans.push(Span::from(" · hub").fg(Color::DarkGray));
            cwd_spans.push(
                Span::from(match viewers {
                    0 => " ●".to_string(),
                    n => format!(" ● {n} viewing"),
                })
                .fg(if viewers > 0 {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
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
        } else if self.backend_picker.is_some() {
            "↑/↓ move · enter select · d remove · esc cancel"
        } else if self.help_scroll.is_some() {
            "↑/↓ j/k scroll · pgup/pgdn page · esc close"
        } else if let Some(tab) = self.tab_focus {
            match tab {
                FooterTab::Remote if self.hub_viewers.is_some() => {
                    "enter/↓ show the QR code · ←/→ choose · ↑ back"
                }
                FooterTab::Remote => "enter/↓ connect to the hub · ←/→ choose · ↑ back",
                FooterTab::Web => "enter/↓ open the web UI · ←/→ choose · ↑ back",
                FooterTab::Diff => "enter/↓ open the diff · ←/→ choose · ↑ back",
                FooterTab::Sessions => "enter/↓ see other sessions · ←/→ choose · ↑ back",
                FooterTab::Chat => "←/→ choose a tab · enter/↑ back to the prompt",
            }
        } else if self.is_running() && self.detachable {
            "esc to interrupt · ctrl+c to detach"
        } else if self.is_running() {
            "esc to interrupt"
        } else {
            "enter send · shift+tab mode · ↓ tabs · /help"
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
            Span::from(format!(
                "· {} ",
                self.backend.model_label(&self.settings.model.name)
            ))
            .fg(Color::Magenta),
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
        frame.render_widget(Line::from(tabs).right_aligned(), area);
        // Clickable where it was drawn, right-aligned with the rest of the tabs.
        let start = (area.x + area.width).saturating_sub(tabs_width as u16) + remote_offset;
        let remote_width = remote_tab.width() as u16;
        self.remote_tab = Some(Rect::new(start, area.y, remote_width, 1).intersection(area));
        self.web_tab = Some(
            Rect::new(start + remote_width, area.y, WEB_TAB.width() as u16, 1).intersection(area),
        );
        // At the right end, before its arrow and the margin.
        let sessions_width = SESSIONS_TAB.width() as u16;
        let sessions_start = (area.x + area.width).saturating_sub(sessions_width + 2);
        self.sessions_tab =
            Some(Rect::new(sessions_start, area.y, sessions_width, 1).intersection(area));
    }
}

const REMOTE_ON_TAB: &str = " ⇄ Remote on ";
const REMOTE_OFF_TAB: &str = " ⇄ Remote off ";
const WEB_TAB: &str = " ◉ Web ";
const SESSIONS_TAB: &str = " ≡ Sessions ";

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
pub(super) fn render_quiet<'a>(entries: &'a [Entry], width: u16, lines: &mut Vec<Line<'a>>) {
    let mut i = 0;
    while i < entries.len() {
        if !matches!(entries[i], Entry::Tool { .. }) {
            render_entry(&entries[i], false, width, lines);
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
            render_entry(entry, false, width, lines);
        }
        lines.push(Line::default());
        i = end;
    }
}

/// A path with the home directory shown as `~`.
pub(super) fn tilde(path: &std::path::Path) -> String {
    match std::env::var_os("HOME")
        .and_then(|home| path.strip_prefix(home).ok().map(|p| p.to_path_buf()))
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Keeps the end of `s` within `max` columns, e.g. "…/dev/tamale".
fn truncate_start(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut width = 1;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        width += c.width().unwrap_or(0);
        if width > max {
            break;
        }
        start = i;
    }
    format!("…{}", &s[start..])
}

pub(super) fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

pub(super) fn render_entry<'a>(
    entry: &'a Entry,
    verbose: bool,
    width: u16,
    lines: &mut Vec<Line<'a>>,
) {
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
        Entry::Assistant(text) | Entry::Note(text) => {
            markdown::render(text, width, false, lines);
        }
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

impl App {
    /// The `/goal` being worked toward, above the input.
    fn draw_goal(&self, frame: &mut Frame, area: Rect) {
        let Some(goal) = &self.goal else {
            return;
        };
        let (label, color) = if goal.checking {
            (
                format!(
                    " ◎ goal checking {}/{} ",
                    goal.turns,
                    super::goal::MAX_TURNS
                ),
                Color::Magenta,
            )
        } else if goal.paused {
            (
                format!(" ◎ goal paused {}/{} ", goal.turns, super::goal::MAX_TURNS),
                Color::DarkGray,
            )
        } else {
            (
                format!(" ◎ goal {}/{} ", goal.turns, super::goal::MAX_TURNS),
                Color::Magenta,
            )
        };
        let width = (area.width as usize).saturating_sub(label.width()).max(10);
        let first = goal.condition.lines().next().unwrap_or("");
        let line = Line::from(vec![
            Span::from(label).fg(color),
            Span::from(truncate_chars(first, width)).fg(Color::DarkGray),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}

/// Messages typed during a turn are listed above the input until the model gets them.
const MAX_QUEUED_LINES: usize = 3;

fn draw_queued(frame: &mut Frame, area: Rect, queued: &[String]) {
    if queued.is_empty() {
        return;
    }
    let width = (area.width as usize).saturating_sub(12).max(10);
    let mut lines: Vec<Line> = queued
        .iter()
        .take(MAX_QUEUED_LINES)
        .map(|text| {
            let first = text.lines().next().unwrap_or("");
            Line::from(vec![
                Span::from(" ⧗ queued ").fg(Color::Yellow),
                Span::from(truncate_chars(first, width)).fg(Color::DarkGray),
            ])
        })
        .collect();
    if queued.len() > MAX_QUEUED_LINES
        && let Some(last) = lines.last_mut()
    {
        *last = Line::from(
            format!(" ⧗ … and {} more", queued.len() - MAX_QUEUED_LINES + 1).fg(Color::Yellow),
        );
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Wraps input text to `width` columns, returning visual rows and the
/// (row, col) of the byte offset `cursor`.
pub(super) fn wrap_input(input: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
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
