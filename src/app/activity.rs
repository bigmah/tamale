//! What a running task is doing, and the animated indicator that shows it: a tamale
//! hopping back and forth, a light band that sweeps across the label, the time on the
//! current step and the run's totals, and a note when one step takes long.
//!
//! [`step`] is shared by every client: the terminal views use it directly, and the web
//! page and the iOS app get its result in the session's status.

use std::f32::consts::TAU;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::session::Entry;

/// The tamale that hops across its little track.
const TAMALE: &str = "🫔";
/// Extra cells the tamale can travel across (the emoji itself is two wide).
const TRACK: usize = 4;
/// Time for one hop from one end of the track to the other.
const HOP: Duration = Duration::from_millis(700);
/// Time between discrete hop frames.
const HOP_STEP: Duration = Duration::from_millis(80);
/// One full shimmer sweep across the label, including the pause off-screen.
const SWEEP: Duration = Duration::from_millis(1800);
/// Half-width of the shimmer band, in characters.
const BAND: f32 = 3.5;

/// Colour scheme for a kind of activity.
#[derive(Clone, Copy)]
pub(super) struct Palette {
    base: (u8, u8, u8),
    peak: (u8, u8, u8),
}

pub(super) const THINKING: Palette = Palette {
    base: (118, 92, 196),
    peak: (214, 236, 255),
};
pub(super) const COMPACTING: Palette = Palette {
    base: (54, 120, 180),
    peak: (200, 248, 255),
};
pub(super) const WAITING: Palette = Palette {
    base: (176, 128, 40),
    peak: (255, 240, 196),
};

/// After this long on one step, the indicator says what it's waiting on.
pub(crate) const LONG_STEP: Duration = Duration::from_secs(180);

/// The kind of thing a run is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    /// Waiting on the model, which hasn't started writing.
    Think,
    /// The model is writing its reply.
    Write,
    Tool,
    /// A subagent (the `agent` tool) is between steps of its own.
    Subagent,
    /// Waiting on the user: an approval or a question.
    Wait,
    Compact,
}

impl StepKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StepKind::Think => "think",
            StepKind::Write => "write",
            StepKind::Tool => "tool",
            StepKind::Subagent => "subagent",
            StepKind::Wait => "wait",
            StepKind::Compact => "compact",
        }
    }

    pub(crate) fn parse(s: &str) -> StepKind {
        match s {
            "write" => StepKind::Write,
            "tool" => StepKind::Tool,
            "subagent" => StepKind::Subagent,
            "wait" => StepKind::Wait,
            "compact" => StepKind::Compact,
            _ => StepKind::Think,
        }
    }

    fn palette(self) -> Palette {
        match self {
            StepKind::Wait => WAITING,
            StepKind::Compact => COMPACTING,
            _ => THINKING,
        }
    }
}

/// What a run is doing now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    pub label: String,
    pub kind: StepKind,
    /// Tool calls since the user's last message: the run's progress so far.
    pub steps: usize,
    /// The running subagent's steps so far.
    pub sub_steps: usize,
}

/// What a run with these `entries` is doing. `waiting` is what it waits on the user for, if
/// anything (an approval or a question).
pub(crate) fn step(entries: &[Entry], waiting: Option<&str>, compacting: bool) -> Step {
    // This run's entries: everything after the user's last message.
    let run = entries
        .iter()
        .rposition(|e| matches!(e, Entry::User(_)))
        .map_or(entries, |i| &entries[i + 1..]);
    let is_tool = |e: &Entry| matches!(e, Entry::Tool { .. });
    let steps = run.iter().filter(|e| is_tool(e)).count();
    let at = |label: &str, kind| Step {
        label: label.into(),
        kind,
        steps,
        sub_steps: 0,
    };
    if let Some(waiting) = waiting {
        return at(waiting, StepKind::Wait);
    }
    if compacting {
        return at("Compacting the conversation", StepKind::Compact);
    }
    // The newest unfinished tool. A subagent's finished steps (↳) come after its `agent`
    // call, which is still running while the subagent thinks.
    let running = run
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| matches!(e, Entry::Tool { output: None, .. }));
    if let Some((i, Entry::Tool { name, description, .. })) = running {
        if i + 1 == run.len() {
            return at(description, StepKind::Tool);
        }
        if name == crate::tools::AGENT {
            return Step {
                sub_steps: run[i + 1..].iter().filter(|e| is_tool(e)).count(),
                ..at("Subagent thinking", StepKind::Subagent)
            };
        }
    }
    match run.last() {
        Some(Entry::Assistant(_)) => at("Writing", StepKind::Write),
        _ => at("Thinking", StepKind::Think),
    }
}

/// Why a step that has taken `elapsed` is still going, once that's long (see [`LONG_STEP`]).
pub(crate) fn long_note(kind: StepKind, sub_steps: usize, elapsed: Duration) -> Option<String> {
    if elapsed < LONG_STEP {
        return None;
    }
    let t = format_elapsed(elapsed);
    Some(match kind {
        StepKind::Think => format!(
            "No output from the model for {t}. It may be reasoning through a big step; \
interrupt it if it looks stuck."
        ),
        StepKind::Subagent => {
            let so_far = match sub_steps {
                0 => String::new(),
                1 => " (1 step so far)".into(),
                n => format!(" ({n} steps so far)"),
            };
            format!(
                "The subagent has been on its own for {t}{so_far}. Only its final report goes \
back to the agent."
            )
        }
        StepKind::Tool => {
            format!("This step has been running for {t}. Its output shows up when it finishes.")
        }
        StepKind::Wait => format!("Waiting on you for {t}. The run is paused until you answer."),
        StepKind::Write | StepKind::Compact => return None,
    })
}

/// The run's totals besides the current step: its tool calls, and its time once that's more
/// than the step's. Empty if there's nothing to add.
pub(crate) fn totals(steps: usize, step: Duration, total: Duration) -> String {
    let mut parts = Vec::new();
    match steps {
        0 => {}
        1 => parts.push("1 step".to_string()),
        n => parts.push(format!("{n} steps")),
    }
    if total.saturating_sub(step) >= Duration::from_secs(5) {
        parts.push(format!("{} total", format_elapsed(total)));
    }
    parts.join(" · ")
}

/// The indicator for `step`, `elapsed` into it and `total` into the run: its line, and a note
/// below it when the step is taking long.
pub(super) fn lines(
    step: &Step,
    elapsed: Duration,
    total: Duration,
    interruptible: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![line(
        &format!("{}…", step.label),
        step.kind.palette(),
        elapsed,
        total,
        &totals(step.steps, elapsed, total),
        interruptible,
    )];
    if let Some(note) = long_note(step.kind, step.sub_steps, elapsed) {
        lines.push(Line::from(Span::styled(
            format!("{}{note}", " ".repeat(TRACK + 3)),
            Style::new().fg(Color::DarkGray),
        )));
    }
    lines
}

/// Builds the indicator line `elapsed` into the step (and `total` into the run, which
/// drives the animation so it doesn't jump between steps).
fn line(
    label: &str,
    palette: Palette,
    elapsed: Duration,
    total: Duration,
    totals: &str,
    interruptible: bool,
) -> Line<'static> {
    let ms = total.as_millis();
    let mut spans = vec![Span::raw(tamale(total)), Span::raw(" ")];

    let chars: Vec<char> = label.chars().collect();
    // Sweep from just before the label to just past it, so each pass starts
    // and ends with the band fully off the text.
    let travel = chars.len() as f32 + 4.0 * BAND;
    let phase = (ms % SWEEP.as_millis()) as f32 / SWEEP.as_millis() as f32;
    let center = phase * travel - 2.0 * BAND;
    for (i, c) in chars.into_iter().enumerate() {
        let distance = (i as f32 - center).abs();
        let glow = if distance < BAND {
            // Cosine falloff gives a soft-edged highlight.
            0.5 + 0.5 * (distance / BAND * std::f32::consts::PI).cos()
        } else {
            0.0
        };
        spans.push(Span::styled(
            c.to_string(),
            Style::new().fg(mix(palette.base, palette.peak, glow)),
        ));
    }

    let long = elapsed >= LONG_STEP;
    spans.push(Span::styled(
        format!("  {}", format_elapsed(elapsed)),
        Style::new().fg(if long { Color::Yellow } else { Color::DarkGray }),
    ));
    if !totals.is_empty() {
        spans.push(Span::styled(
            format!(" · {totals}"),
            Style::new().fg(Color::DarkGray),
        ));
    }
    if interruptible {
        spans.push(Span::styled(
            " · esc to interrupt",
            Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// The tamale frame for `elapsed`: the tamale eases back and forth across a
/// fixed-width track, so the label never shifts.
fn tamale(elapsed: Duration) -> String {
    // Quantise time so the motion reads as hops rather than a smear.
    let step = elapsed.as_millis() / HOP_STEP.as_millis() * HOP_STEP.as_millis();
    let t = step as f32 / HOP.as_millis() as f32;
    // Cosine easing: slows down at each end like it's landing and pushing off.
    let swing = 0.5 - 0.5 * (t * TAU / 2.0).cos();
    let pos = (swing * TRACK as f32).round() as usize;
    format!(
        "{}{TAMALE}{}",
        " ".repeat(pos),
        " ".repeat(TRACK - pos.min(TRACK))
    )
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, secs / 60 % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_elapsed_time() {
        assert_eq!(format_elapsed(Duration::from_secs(7)), "7s");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1m 05s");
    }

    #[test]
    fn tamale_frames_have_constant_width() {
        for ms in (0..3000).step_by(40) {
            let frame = tamale(Duration::from_millis(ms));
            assert_eq!(frame.chars().filter(|&c| c == ' ').count(), TRACK);
            assert!(frame.contains(TAMALE));
        }
        assert!(tamale(Duration::ZERO).starts_with(TAMALE));
        assert!(tamale(HOP).ends_with(TAMALE));
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn tool(name: &str, description: &str, done: bool) -> Entry {
        Entry::Tool {
            name: name.into(),
            summary: String::new(),
            description: description.into(),
            output: done.then(|| (String::new(), false)),
        }
    }

    #[test]
    fn formats_hours() {
        assert_eq!(
            format_elapsed(Duration::from_secs(3 * 3600 + 7 * 60)),
            "3h 07m"
        );
    }

    #[test]
    fn line_keeps_label_text() {
        let step = step(&[], None, false);
        let lines = lines(
            &step,
            Duration::from_millis(500),
            Duration::from_millis(500),
            true,
        );
        let text = text(&lines[0]);
        assert!(text.contains("Thinking…"));
        assert!(text.ends_with("esc to interrupt"));
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn steps_name_what_the_run_is_doing() {
        let user = Entry::User("go".into());
        assert_eq!(step(std::slice::from_ref(&user), None, false).kind, StepKind::Think);
        let reading = [user.clone(), tool("read_file", "Read the config", false)];
        let s = step(&reading, None, false);
        assert_eq!(
            (s.label.as_str(), s.kind, s.steps),
            ("Read the config", StepKind::Tool, 1)
        );
        let writing = [
            user.clone(),
            tool("read_file", "Read", true),
            Entry::Assistant("Hi".into()),
        ];
        assert_eq!(step(&writing, None, false).kind, StepKind::Write);
        assert_eq!(
            step(&writing, Some("Waiting for your approval"), false).kind,
            StepKind::Wait
        );
        assert_eq!(step(&writing, None, true).kind, StepKind::Compact);
        // Only this run's tool calls count.
        let later = [tool("bash", "Old", true), user, tool("bash", "New", true)];
        assert_eq!(step(&later, None, false).steps, 1);
    }

    #[test]
    fn a_subagent_between_steps_is_still_working() {
        let entries = [
            Entry::User("go".into()),
            tool(crate::tools::AGENT, "Hand a task to a subagent", false),
            tool("grep", "↳ Find callers", true),
            tool("read_file", "↳ Read main.rs", true),
        ];
        let s = step(&entries, None, false);
        assert_eq!((s.kind, s.sub_steps, s.steps), (StepKind::Subagent, 2, 3));
    }

    #[test]
    fn long_steps_get_a_note_and_totals() {
        let s = step(&[Entry::User("go".into())], None, false);
        let short = lines(&s, Duration::from_secs(30), Duration::from_secs(30), true);
        assert_eq!(short.len(), 1);
        let long = lines(
            &s,
            Duration::from_secs(600),
            Duration::from_secs(3900),
            true,
        );
        assert_eq!(long.len(), 2);
        assert!(text(&long[0]).contains("10m 00s · 1h 05m total"));
        assert!(text(&long[1]).contains("No output from the model for 10m 00s"));
        assert_eq!(
            totals(3, Duration::from_secs(2), Duration::from_secs(4)),
            "3 steps"
        );
    }
}
