//! Cache styled logical lines and their wrapped row offsets. Only the visible
//! slice is handed to Paragraph, so scrolling never rewraps the entire history.
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::markdown;
use super::view::{render_entry, render_quiet};
use crate::session::Entry;

#[derive(Default)]
pub(super) struct Transcript {
    width: u16,
    verbose: bool,
    entries: usize,
    dirty_from: Option<usize>,
    groups: Vec<Group>,
    lines: Vec<Line<'static>>,
    /// Exclusive cumulative row offsets, one per logical line.
    ends: Vec<usize>,
    appended: Option<usize>,
    tail: Option<AssistantTail>,
}

struct AssistantTail {
    entry: usize,
    byte: usize,
    line: usize,
    gap: bool,
}

struct Group {
    entry: usize,
    line: usize,
}

impl Transcript {
    pub fn invalidate_from(&mut self, entry: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(entry, |old| old.min(entry)));
        self.appended = None;
    }

    pub fn append_to(&mut self, entry: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(entry, |old| old.min(entry)));
        self.appended = Some(entry);
    }

    pub fn sync(&mut self, entries: &[Entry], width: u16, verbose: bool) {
        if width != self.width || verbose != self.verbose || entries.len() < self.entries {
            self.invalidate_from(0);
        }
        self.width = width;
        self.verbose = verbose;
        if self.dirty_from.is_none() && self.entries == entries.len() {
            return;
        }
        if let Some(tail) = self.tail.take()
            && self.appended.take() == Some(tail.entry)
            && self.dirty_from == Some(tail.entry)
            && self.entries == entries.len()
            && let Some(Entry::Assistant(text)) = entries.get(tail.entry)
        {
            self.dirty_from = None;
            self.lines.truncate(tail.line);
            self.ends.truncate(tail.line);
            self.append_assistant(tail.entry, text, tail.byte, tail.gap);
            return;
        }
        self.appended = None;
        let mut from = self.dirty_from.take().unwrap_or(self.entries);
        if from >= entries.len() && entries.len() == self.entries {
            return;
        }
        from = from.min(self.entries).min(entries.len());
        // An appended tool may extend the final quiet group and hide an earlier step.
        if !verbose && from > 0 && matches!(entries.get(from - 1), Some(Entry::Tool { .. })) {
            from = self.groups.last().map_or(0, |group| group.entry).min(from);
        }
        let keep = self.groups.partition_point(|group| group.entry < from);
        let keep = if keep > 0
            && (keep == self.groups.len() || self.groups[keep].entry != from)
            && from < self.entries
        {
            keep - 1
        } else {
            keep
        };
        if let Some(group) = self.groups.get(keep) {
            from = group.entry;
            self.lines.truncate(group.line);
            self.ends.truncate(group.line);
        }
        self.groups.truncate(keep);

        while from < entries.len() {
            self.groups.push(Group {
                entry: from,
                line: self.lines.len(),
            });
            if from + 1 == entries.len()
                && let Entry::Assistant(text) = &entries[from]
            {
                self.append_assistant(from, text, 0, false);
                break;
            }
            let mut lines = Vec::new();
            let end = if !verbose && matches!(entries[from], Entry::Tool { .. }) {
                let end = entries[from..]
                    .iter()
                    .position(|entry| !matches!(entry, Entry::Tool { .. }))
                    .map_or(entries.len(), |count| from + count);
                render_quiet(&entries[from..end], width, &mut lines);
                end
            } else {
                render_entry(&entries[from], verbose, width, &mut lines);
                lines.push(Line::default());
                from + 1
            };
            self.append_lines(lines);
            from = end;
        }
        self.entries = entries.len();
    }

    fn append_assistant(&mut self, entry: usize, text: &str, byte: usize, gap: bool) {
        let mut lines = Vec::new();
        let tail = markdown::render(&text[byte..], self.width, gap, &mut lines);
        self.tail = Some(AssistantTail {
            entry,
            byte: byte + tail.byte,
            line: self.lines.len() + tail.line,
            gap: tail.gap,
        });
        lines.push(Line::default());
        self.append_lines(lines);
    }

    fn append_lines(&mut self, lines: Vec<Line<'_>>) {
        for line in lines {
            let height = Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(self.width);
            self.ends.push(self.rows() + height);
            self.lines.push(Line {
                style: line.style,
                alignment: line.alignment,
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| ratatui::text::Span::styled(span.content.into_owned(), span.style))
                    .collect(),
            });
        }
    }

    pub fn rows(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    pub fn visible(&self, offset: usize, height: usize) -> (Vec<Line<'static>>, u16) {
        let first = self.ends.partition_point(|&end| end <= offset);
        let end = self
            .ends
            .partition_point(|&end| end < offset.saturating_add(height))
            .saturating_add(1)
            .min(self.lines.len());
        let start_row = first.checked_sub(1).map_or(0, |i| self.ends[i]);
        (
            self.lines[first..end].to_vec(),
            offset.saturating_sub(start_row).min(u16::MAX as usize) as u16,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    fn check(cache: &mut Transcript, entries: &[Entry], width: u16, verbose: bool) {
        cache.sync(entries, width, verbose);
        let mut lines = Vec::new();
        if verbose {
            for entry in entries {
                render_entry(entry, true, width, &mut lines);
                lines.push(Line::default());
            }
        } else {
            render_quiet(entries, width, &mut lines);
        }
        let reference = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = reference.line_count(width);
        assert_eq!(cache.rows(), total);
        for offset in [0, 1.min(total), total / 2, total.saturating_sub(4), total] {
            let area = Rect::new(0, 0, width, 6);
            let mut expected = Buffer::empty(area);
            reference
                .clone()
                .scroll((offset as u16, 0))
                .render(area, &mut expected);
            let (lines, local) = cache.visible(offset, area.height as usize);
            let mut actual = Buffer::empty(area);
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((local, 0))
                .render(area, &mut actual);
            assert_eq!(
                actual, expected,
                "width {width}, verbose {verbose}, offset {offset}"
            );
        }
    }

    #[test]
    fn cached_view_matches_full_render_after_edits_collapsing_resize_and_clear() {
        let mut entries = vec![
            Entry::Info("Welcome".into()),
            Entry::User("café 世界 👩‍💻 e\u{301}\nsecond line\n".into()),
            Entry::Assistant("# Title\n**bold** and `code` with long words abcdefghijklmnopqrstuvwxyz\n```rust\nlet x = 1;\n```\n\nlast".into()),
        ];
        let mut cache = Transcript::default();
        for width in [1, 3, 19, 80] {
            for verbose in [true, false] {
                check(&mut cache, &entries, width, verbose);
                check(&mut cache, &entries, width, verbose);
                for i in 0..8 {
                    entries.push(Entry::Tool {
                        name: "bash".into(),
                        summary: format!("step {i}"),
                        description: format!("Run step {i}"),
                        output: None,
                    });
                    check(&mut cache, &entries, width, verbose);
                }
                let tool = entries.len() - 3;
                if let Entry::Tool { output, .. } = &mut entries[tool] {
                    *output = Some(("output\n".repeat(10), true));
                }
                cache.invalidate_from(tool);
                check(&mut cache, &entries, width, verbose);
                entries.push(Entry::Assistant("partial".into()));
                check(&mut cache, &entries, width, verbose);
                if let Some(Entry::Assistant(text)) = entries.last_mut() {
                    text.push_str(" response\n```\ncode");
                }
                cache.invalidate_from(entries.len() - 1);
                check(&mut cache, &entries, width, verbose);
                entries.pop();
                entries.push(Entry::Error("retrying".into()));
                cache.invalidate_from(entries.len() - 1);
                check(&mut cache, &entries, width, verbose);
            }
        }
        entries.clear();
        cache.invalidate_from(0);
        check(&mut cache, &entries, 80, false);
        entries.push(Entry::Assistant("replaced session".into()));
        check(&mut cache, &entries, 80, false);
    }

    #[test]
    fn scrolls_beyond_u16_rows() {
        let entries = vec![Entry::Assistant(format!(
            "{}THE END",
            "line\n".repeat(70_000)
        ))];
        let mut cache = Transcript::default();
        cache.sync(&entries, 20, false);
        let (lines, offset) = cache.visible(cache.rows() - 2, 2);
        assert_eq!(offset, 0);
        assert_eq!(lines[0].to_string(), "THE END");
    }

    #[test]
    fn streamed_markdown_matches_full_render_across_arbitrary_chunk_boundaries() {
        for width in [1, 7, 25, 80] {
            let mut entries = vec![Entry::Assistant(String::new())];
            let mut cache = Transcript::default();
            check(&mut cache, &entries, width, false);
            let text = "# Title\r\n```rust\nlet café = \"👩‍💻\";\n```\n**bold** and `code`\n\nnext\r\n\
                        - one\n  - two\n- [x] three\n\n> [!NOTE]\n> quoted *text*\n\n\
                        | a | b |\n|---|:-:|\n| 1 | 2 |\n\nSetext\n---\n1. x\n\n   y\n";
            for ch in text.chars() {
                if let Entry::Assistant(text) = &mut entries[0] {
                    text.push(ch);
                }
                cache.append_to(0);
                check(&mut cache, &entries, width, false);
                // A spinner or input-only frame between chunks must retain the tail state.
                check(&mut cache, &entries, width, false);
            }
        }
    }
}
