//! Lessons: artifacts whose root is a `Lesson` (see the library's Lessons group), which
//! teach something from the ground up in parts the reader checks their understanding of.
//!
//! What the reader writes in a lesson (an answer to a `Check`, a question, a reply to the
//! agent's follow-up) goes to the agent as an ordinary message whose first line is a
//! [`marker`] naming the lesson and the part. The agent sees which part it's about; the page
//! finds the exchanges that belong to each part by the same line ([`parse`]), and shows them
//! in the lesson.

use crate::{Eval, initial_state, parse as parse_program};

/// The component a lesson's root is.
pub const ROOT: &str = "Lesson";

/// Whether `source` is a lesson: its root is a `Lesson`. Cheap enough to ask of every
/// artifact (it only parses when the document mentions one).
pub fn is_lesson(source: &str) -> bool {
    if !source.contains("Lesson(") {
        return false;
    }
    let program = parse_program(source);
    let state = initial_state(&program);
    Eval::new(&program, &state)
        .root()
        .is_some_and(|root| root.name == ROOT)
}

/// The first line of a message about part `part` (from 1) of the lesson `title`.
pub fn marker(title: &str, part: usize, part_title: &str) -> String {
    format!(
        "[Lesson: {} · Part {part}: {}]",
        one_line(title),
        one_line(part_title)
    )
}

/// A message sent from a lesson, read back.
#[derive(Clone, Debug, PartialEq)]
pub struct Marked<'a> {
    pub title: &'a str,
    /// The part it's about, from 1.
    pub part: usize,
    pub part_title: &'a str,
    /// What the reader wrote, after the marker line.
    pub body: &'a str,
}

/// The lesson and part `text` is about, if it starts with a [`marker`].
pub fn parse(text: &str) -> Option<Marked<'_>> {
    let rest = text.strip_prefix("[Lesson: ")?;
    let (line, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let line = line.trim_end().strip_suffix(']')?;
    // The title may hold " · Part ", the part's title may not hold a newline: the last one
    // splits them.
    let at = line.rfind(" · Part ")?;
    let title = &line[..at];
    let (n, part_title) = line[at + " · Part ".len()..].split_once(": ")?;
    Some(Marked {
        title,
        part: n.parse().ok().filter(|n| *n >= 1)?,
        part_title,
        body: body.trim(),
    })
}

/// The marker's own words for what the reader sent, so the page and the agent agree.
pub mod says {
    /// An answer to a `Check`: the question, then the reader's answer.
    pub fn answer(question: &str, answer: &str) -> String {
        format!(
            "Check question: {}\nMy answer: {}",
            question.trim(),
            answer.trim()
        )
    }

    /// A question or a reply to the agent's follow-up.
    pub fn note(text: &str) -> String {
        text.trim().to_string()
    }

    /// The question a body answers and the answer, if it's an answer to a `Check`.
    pub fn answered(body: &str) -> Option<(&str, &str)> {
        let rest = body.strip_prefix("Check question: ")?;
        Some(match rest.split_once("\nMy answer:") {
            Some((q, a)) => (q.trim(), a.trim()),
            None => (rest.trim(), ""),
        })
    }
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.replace(']', ")")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_read_back() {
        let m = marker("How the cache · Part works", 3, "Adding\nretries [v2]");
        assert_eq!(
            m,
            "[Lesson: How the cache · Part works · Part 3: Adding retries [v2)]"
        );
        let text = format!("{m}\n{}", says::answer("Why?", "Because"));
        let got = parse(&text).unwrap();
        assert_eq!(got.title, "How the cache · Part works");
        assert_eq!(got.part, 3);
        assert_eq!(got.part_title, "Adding retries [v2)");
        assert_eq!(says::answered(got.body), Some(("Why?", "Because")));
        assert_eq!(parse("[Lesson: x]\nhi"), None);
        assert_eq!(parse("hello"), None);
        let q = parse("[Lesson: T · Part 1: Start]\nWhat is a pod?").unwrap();
        assert_eq!(q.body, "What is a pod?");
        assert_eq!(says::answered(q.body), None);
    }

    #[test]
    fn lessons_are_known_by_their_root() {
        let lesson = "title = \"T\"\nroot = Lesson(\"Know X\", [p1])\np1 = LessonPart(\"Start\", \"The simplest case\", [TextContent(\"x\")], [Check(\"Why?\")])";
        assert!(is_lesson(lesson));
        assert!(!is_lesson("root = Card([TextContent(\"A Lesson(\")])"));
        assert!(
            crate::problems(lesson).is_empty(),
            "{:?}",
            crate::problems(lesson)
        );
        assert_eq!(crate::title(lesson).as_deref(), Some("T"));
    }
}
