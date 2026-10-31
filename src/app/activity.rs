//! Animated activity indicator shown while a task runs: a tamale hopping back
//! and forth, a light band that sweeps across the label, and a dim
//! elapsed-time counter.

use std::f32::consts::TAU;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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

/// Builds the indicator line for `elapsed` time since the activity began.
pub(super) fn line(
    label: &str,
    palette: Palette,
    elapsed: Duration,
    interruptible: bool,
) -> Line<'static> {
    let ms = elapsed.as_millis();
    let mut spans = vec![Span::raw(tamale(elapsed)), Span::raw(" ")];

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

    spans.push(Span::styled(
        format!("  {}", format_elapsed(elapsed)),
        Style::new().fg(Color::DarkGray),
    ));
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
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
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

    #[test]
    fn line_keeps_label_text() {
        let line = line("Thinking…", THINKING, Duration::from_millis(500), true);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Thinking…"));
        assert!(text.ends_with("esc to interrupt"));
    }
}
