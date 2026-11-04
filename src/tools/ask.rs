//! The `ask_user` tool: multiple-choice questions for the user. It's run by the turn (the UI
//! shows the questions and waits), not by [`super::execute`].

use anyhow::{Result, bail};
use rig_core::completion::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const ASK_USER: &str = "ask_user";

const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 6;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Question {
    pub question: String,
    /// A very short label for the question, shown in the question tabs.
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<Choice>,
    #[serde(default, alias = "multiSelect")]
    pub multi_select: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Choice {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// The user's answer to one question: the labels they picked, and what they typed instead or
/// as well.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Answer {
    #[serde(default)]
    pub chosen: Vec<String>,
    #[serde(default)]
    pub other: Option<String>,
}

impl Answer {
    pub fn is_empty(&self) -> bool {
        self.chosen.is_empty() && self.other.as_deref().is_none_or(|o| o.trim().is_empty())
    }
}

/// How the user responded.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// One answer per question.
    Answers(Vec<Answer>),
    /// A message in their own words instead (e.g. typed in the web UI, which has no picker).
    Text(String),
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_USER.into(),
        description: "Ask the user 1-4 multiple-choice questions and wait for their answers. \
            Use it when you need a decision only the user can make (a preference, a choice \
            between approaches, missing requirements) before you can continue well. Don't \
            use it for things you can find out yourself or to ask permission for a tool call. \
            Give 2-6 distinct, concise options per question; the user can always type their \
            own answer instead, so don't add an \"Other\" option. Put a recommended option \
            first and add \"(Recommended)\" to its label. Set multi_select when more than one \
            option can apply."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "description": "The questions, asked in order",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The complete question, ending with a question mark"
                            },
                            "header": {
                                "type": "string",
                                "description": "Very short label (max 12 characters), e.g. \"Database\" or \"Auth\""
                            },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": MAX_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "The choice, in 1-5 words"
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "What this choice means or implies"
                                        }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "Let the user pick several options (default false)"
                            }
                        },
                        "required": ["question", "options"]
                    }
                }
            },
            "required": ["questions"]
        }),
    }
}

/// The questions in a call, checked so the UI can show them.
pub fn parse(args: &Value) -> Result<Vec<Question>> {
    let Some(questions) = args.get("questions") else {
        bail!("missing `questions`");
    };
    // Some models send nested arrays as a JSON string.
    let questions: Vec<Question> = match questions {
        Value::String(s) => serde_json::from_str(s)?,
        v => serde_json::from_value(v.clone())?,
    };
    if questions.is_empty() {
        bail!("ask at least one question");
    }
    if questions.len() > MAX_QUESTIONS {
        bail!("ask at most {MAX_QUESTIONS} questions at a time");
    }
    let mut questions = questions;
    for q in &mut questions {
        q.question = q.question.trim().to_string();
        q.header = q.header.trim().to_string();
        if q.question.is_empty() {
            bail!("every question needs text");
        }
        q.options.retain(|o| !o.label.trim().is_empty());
        if q.options.len() > MAX_OPTIONS {
            bail!("give at most {MAX_OPTIONS} options per question");
        }
        for o in &mut q.options {
            o.label = o.label.trim().to_string();
            o.description = o.description.trim().to_string();
        }
    }
    Ok(questions)
}

/// One line per question, for the transcript and the approval-free tool step.
pub fn summarize(args: &Value) -> String {
    match parse(args) {
        Ok(questions) => questions
            .iter()
            .map(|q| {
                let options: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
                if options.is_empty() {
                    q.question.clone()
                } else {
                    format!("{} [{}]", q.question, options.join(" / "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => args.to_string(),
    }
}

/// The tool result the model gets.
pub fn format_reply(questions: &[Question], reply: &Reply) -> String {
    match reply {
        Reply::Text(text) => format!(
            "The user didn't pick from the options and replied instead:\n{}",
            text.trim()
        ),
        Reply::Answers(answers) => {
            let mut out = String::from("The user answered:\n");
            for (q, a) in questions.iter().zip(answers) {
                let mut parts: Vec<String> = a.chosen.clone();
                if let Some(other) = a.other.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
                    parts.push(format!("(in their own words) {other}"));
                }
                let answer = if parts.is_empty() {
                    "(no answer)".to_string()
                } else {
                    parts.join(", ")
                };
                out.push_str(&format!("- {} → {answer}\n", q.question));
            }
            out.trim_end().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Value {
        json!({
            "description": "Ask about the setup",
            "questions": [
                {
                    "question": "Which database?",
                    "header": "DB",
                    "options": [
                        { "label": "Postgres (Recommended)", "description": "Robust" },
                        { "label": "SQLite" }
                    ]
                },
                {
                    "question": "Which features?",
                    "options": [{ "label": "Auth" }, { "label": "Logs" }, { "label": " " }],
                    "multiSelect": true
                }
            ]
        })
    }

    #[test]
    fn parses_and_checks_questions() {
        let qs = parse(&args()).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header, "DB");
        assert!(!qs[0].multi_select && qs[1].multi_select);
        // Blank options are dropped.
        assert_eq!(qs[1].options.len(), 2);

        let as_string = json!({ "questions": args()["questions"].to_string() });
        assert_eq!(parse(&as_string).unwrap(), qs);

        assert!(parse(&json!({})).is_err());
        assert!(parse(&json!({ "questions": [] })).is_err());
        assert!(parse(&json!({ "questions": [{ "question": " ", "options": [] }] })).is_err());
        let many = vec![json!({ "question": "?", "options": [] }); 5];
        assert!(parse(&json!({ "questions": many })).is_err());
    }

    #[test]
    fn summarizes_and_formats_answers() {
        assert_eq!(
            summarize(&args()),
            "Which database? [Postgres (Recommended) / SQLite]\nWhich features? [Auth / Logs]"
        );
        let qs = parse(&args()).unwrap();
        let reply = Reply::Answers(vec![
            Answer {
                chosen: vec!["SQLite".into()],
                other: None,
            },
            Answer {
                chosen: vec!["Auth".into(), "Logs".into()],
                other: Some(" metrics ".into()),
            },
        ]);
        assert_eq!(
            format_reply(&qs, &reply),
            "The user answered:\n- Which database? → SQLite\n\
             - Which features? → Auth, Logs, (in their own words) metrics"
        );
        assert!(format_reply(&qs, &Reply::Text("use mysql".into())).ends_with("\nuse mysql"));
    }
}
