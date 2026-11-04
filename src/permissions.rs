//! Which tool calls run without asking: a mode, plus allow/deny rules from settings and
//! rules approved for the current session.
//!
//! Rules are `tool` or `tool:glob`. The glob is matched against the command for `bash`,
//! the path for file tools (as given, relative to the working directory, and its file
//! name), the URL for `web_fetch` and the query for `web_search`. `*` also matches `/`.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[cfg(test)]
mod tests;

const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "grep",
    "glob",
    "web_search",
    "web_fetch",
    // Starting a subagent needs no approval; its own calls are checked one by one.
    "agent",
];
const EDIT_TOOLS: &[&str] = &["write_file", "edit_file"];
const PATH_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "grep",
    "glob",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ask before edits and shell commands.
    Ask,
    /// Apply edits without asking; ask before shell commands.
    AcceptEdits,
    /// Never ask. Deny rules still apply.
    Auto,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "accept-edits" | "accept_edits" | "edits" => Some(Self::AcceptEdits),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AcceptEdits => "accept-edits",
            Self::Auto => "auto",
        }
    }

    /// Whether this asks about less than `other`.
    pub fn looser_than(self, other: Self) -> bool {
        let leeway = |mode| match mode {
            Self::Ask => 0,
            Self::AcceptEdits => 1,
            Self::Auto => 2,
        };
        leeway(self) > leeway(other)
    }

    /// The Shift+Tab cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Ask => Self::AcceptEdits,
            Self::AcceptEdits => Self::Auto,
            Self::Auto => Self::Ask,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Ask => "ask before edits and shell commands",
            Self::AcceptEdits => "apply edits without asking; ask before shell commands",
            Self::Auto => "run every tool without asking (deny rules still apply)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    text: String,
    tool: String,
    /// `None` matches every call to the tool.
    glob: Option<GlobMatcher>,
}

impl Rule {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let (tool, pattern) = match text.split_once(':') {
            Some((tool, pattern)) => (tool.trim(), Some(pattern)),
            None => (text, None),
        };
        if !READ_ONLY_TOOLS.contains(&tool) && !EDIT_TOOLS.contains(&tool) && tool != "bash" {
            bail!("unknown tool {tool:?} in permission rule {text:?}");
        }
        let glob = pattern
            .map(|pattern| {
                GlobBuilder::new(pattern.trim())
                    .literal_separator(false)
                    .build()
                    .map(|g| g.compile_matcher())
                    .with_context(|| format!("invalid pattern in permission rule {text:?}"))
            })
            .transpose()?;
        Ok(Self {
            text: text.into(),
            tool: tool.into(),
            glob,
        })
    }

    fn matches(&self, value: &str) -> bool {
        self.glob.as_ref().is_none_or(|g| g.is_match(value))
    }
}

impl<'de> Deserialize<'de> for Rule {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Rule::parse(&text).map_err(|e| serde::de::Error::custom(format!("{e:#}")))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionSettings {
    #[serde(deserialize_with = "deserialize_mode")]
    pub mode: Mode,
    pub allow: Vec<Rule>,
    pub deny: Vec<Rule>,
}

fn deserialize_mode<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Mode, D::Error> {
    let value = String::deserialize(deserializer)?;
    Mode::parse(&value).ok_or_else(|| {
        serde::de::Error::custom(format!(
            "unknown permission mode {value:?} (use ask, accept-edits, auto)"
        ))
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Blocked by the named deny rule.
    Deny(String),
    /// Needs approval. `always` holds the session rules offered by "always allow"; it is
    /// empty when the call can't be covered by a rule (e.g. command substitution).
    Ask {
        always: Vec<String>,
    },
}

/// What the user answered to an approval prompt.
#[derive(Debug)]
pub enum Approval {
    Allow,
    Deny { reason: Option<String> },
}

pub type Shared = Arc<Mutex<Policy>>;

#[derive(Clone, Debug)]
pub struct Policy {
    pub mode: Mode,
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    session: Vec<Rule>,
    cwd: PathBuf,
}

/// The part of a call that rules are matched against.
enum Subject {
    Bash {
        whole: String,
        /// Commands joined by `;`, `&&`, `||`, `|`, `&` or newlines.
        parts: Vec<String>,
        /// False if the command substitutes or redirects in ways a prefix rule can't vet.
        allowlistable: bool,
    },
    Path {
        /// The path relative to cwd where possible, then as given.
        candidates: Vec<String>,
        /// The file name. Only deny rules match it, so `.env*` blocks `config/.env`.
        name: Option<String>,
        /// Contains `..`, so a prefix like `src/*` says nothing about where it points.
        escapes: bool,
    },
    Text(String),
}

impl Policy {
    pub fn new(settings: &PermissionSettings, cwd: PathBuf) -> Self {
        Self {
            mode: settings.mode,
            allow: settings.allow.clone(),
            deny: settings.deny.clone(),
            session: Vec::new(),
            cwd,
        }
    }

    pub fn shared(self) -> Shared {
        Arc::new(Mutex::new(self))
    }

    pub fn check(&self, tool: &str, args: &Value) -> Decision {
        let subject = self.subject(tool, args);
        if let Some(rule) = self
            .deny
            .iter()
            .find(|r| r.tool == tool && denies(r, &subject))
        {
            return Decision::Deny(rule.text.clone());
        }
        if self.allowed_by_rules(tool, &subject) {
            return Decision::Allow;
        }
        let needs_approval = match self.mode {
            Mode::Auto => false,
            Mode::AcceptEdits => tool == "bash",
            Mode::Ask => tool == "bash" || EDIT_TOOLS.contains(&tool),
        } && !READ_ONLY_TOOLS.contains(&tool);
        if needs_approval {
            Decision::Ask {
                always: self.suggest(tool, &subject),
            }
        } else {
            Decision::Allow
        }
    }

    /// Adds "always allow" rules for the rest of the session. Invalid rules are skipped.
    pub fn allow_for_session(&mut self, rules: &[String]) {
        for text in rules {
            if let Ok(rule) = Rule::parse(text)
                && !self.session.iter().any(|r| r.text == rule.text)
            {
                self.session.push(rule);
            }
        }
    }

    /// A copy that also denies every file edit, for agents that should only look.
    pub fn read_only(&self) -> Self {
        let mut policy = self.clone();
        policy
            .deny
            .extend(EDIT_TOOLS.iter().map(|tool| Rule::parse(tool).unwrap()));
        policy
    }

    pub fn clear_session(&mut self) {
        self.session.clear();
    }

    /// A readable summary for `/permissions`.
    pub fn describe(&self) -> String {
        let mut out = format!(
            "Permission mode: {} ({}). Shift+Tab cycles modes.",
            self.mode.as_str(),
            self.mode.describe()
        );
        for (label, rules) in [
            ("Allow", &self.allow),
            ("Deny", &self.deny),
            ("Allowed this session", &self.session),
        ] {
            if rules.is_empty() {
                continue;
            }
            out.push_str(&format!("\n{label}:"));
            for rule in rules {
                out.push_str(&format!("\n  {}", rule.text));
            }
        }
        out
    }

    fn rules(&self, tool: &str) -> impl Iterator<Item = &Rule> {
        self.allow
            .iter()
            .chain(&self.session)
            .filter(move |r| r.tool == tool)
    }

    fn allowed_by_rules(&self, tool: &str, subject: &Subject) -> bool {
        // A bare `tool` rule allows everything, including compound commands.
        if self.rules(tool).any(|r| r.glob.is_none()) {
            return true;
        }
        match subject {
            Subject::Bash {
                parts,
                allowlistable,
                ..
            } => {
                *allowlistable
                    && !parts.is_empty()
                    && parts.iter().all(|p| self.rules(tool).any(|r| r.matches(p)))
            }
            Subject::Path {
                candidates,
                escapes,
                ..
            } => {
                !escapes
                    && self
                        .rules(tool)
                        .any(|r| candidates.iter().any(|c| r.matches(c)))
            }
            Subject::Text(text) => self.rules(tool).any(|r| r.matches(text)),
        }
    }

    fn suggest(&self, tool: &str, subject: &Subject) -> Vec<String> {
        match subject {
            Subject::Bash {
                parts,
                allowlistable: true,
                ..
            } => {
                let mut out: Vec<String> = Vec::new();
                for part in parts {
                    if self.rules(tool).any(|r| r.matches(part)) {
                        continue;
                    }
                    let rule = format!("bash:{}*", globset::escape(&command_prefix(part)));
                    if !out.contains(&rule) {
                        out.push(rule);
                    }
                }
                out
            }
            Subject::Path {
                candidates,
                escapes: false,
                ..
            } => {
                let path = Path::new(&candidates[0]);
                let pattern = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    Some(parent) => format!("{}/*", globset::escape(&parent.to_string_lossy())),
                    None => globset::escape(&candidates[0]),
                };
                vec![format!("{tool}:{pattern}")]
            }
            Subject::Text(_) => vec![tool.to_string()],
            _ => Vec::new(),
        }
    }

    fn subject(&self, tool: &str, args: &Value) -> Subject {
        let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
        if tool == "bash" {
            let whole = arg("command").trim().to_string();
            let (parts, allowlistable) = split_command(&whole);
            return Subject::Bash {
                whole,
                parts,
                allowlistable,
            };
        }
        if PATH_TOOLS.contains(&tool) {
            let raw = match arg("path") {
                "" => ".",
                p => p,
            };
            let path = Path::new(raw);
            let escapes = path.components().any(|c| c == Component::ParentDir);
            let relative = path.strip_prefix(&self.cwd).unwrap_or(path);
            let relative = relative.strip_prefix(".").unwrap_or(relative);
            let mut candidates = Vec::new();
            for candidate in [relative.to_string_lossy(), raw.into()] {
                if !candidate.is_empty() && !candidates.iter().any(|c| *c == candidate) {
                    candidates.push(candidate.into_owned());
                }
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            return Subject::Path {
                candidates,
                name,
                escapes,
            };
        }
        Subject::Text(
            match tool {
                "web_fetch" => arg("url"),
                "web_search" => arg("query"),
                "agent" => arg("prompt"),
                _ => "",
            }
            .to_string(),
        )
    }
}

fn denies(rule: &Rule, subject: &Subject) -> bool {
    match subject {
        Subject::Bash { whole, parts, .. } => {
            rule.matches(whole) || parts.iter().any(|p| rule.matches(p))
        }
        Subject::Path {
            candidates, name, ..
        } => candidates.iter().chain(name).any(|c| rule.matches(c)),
        Subject::Text(text) => rule.matches(text),
    }
}

/// Splits a shell command on control operators outside quotes. The second value is false if
/// any part uses substitution or file redirection, which prefix rules can't vet.
fn split_command(command: &str) -> (Vec<String>, bool) {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => {
                quote = None;
                current.push(c);
            }
            (Some('"'), '\\') => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), _) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                current.push(c);
            }
            (None, '\\') => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            // `2>&1` and `>&2` duplicate descriptors; they aren't separators.
            (None, '&') if current.ends_with('>') => current.push(c),
            (None, ';' | '|' | '&' | '\n') => parts.push(std::mem::take(&mut current)),
            (None, _) => current.push(c),
        }
    }
    parts.push(current);
    let parts: Vec<String> = parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let allowlistable = quote.is_none() && parts.iter().all(|p| plain_command(p));
    (parts, allowlistable)
}

/// No substitution, and no redirection except descriptor duplication and `/dev/null`.
fn plain_command(part: &str) -> bool {
    if part.contains('`') || part.contains("$(") || part.contains("<(") || part.contains(">(") {
        return false;
    }
    let mut rest = part.to_string();
    for harmless in [
        "2>&1",
        "1>&2",
        ">&2",
        "2>/dev/null",
        ">/dev/null",
        "</dev/null",
    ] {
        rest = rest.replace(harmless, " ");
    }
    !rest.contains('>') && !rest.contains('<')
}

/// The command name plus its subcommand, if any: `cargo test --all` → `cargo test`.
fn command_prefix(part: &str) -> String {
    let mut words = part.split_whitespace();
    let first = words.next().unwrap_or("");
    match words.next() {
        Some(second)
            if second
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !second.starts_with('-') =>
        {
            format!("{first} {second}")
        }
        _ => first.to_string(),
    }
}
