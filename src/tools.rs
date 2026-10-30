use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rig_core::completion::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

mod search;
mod web;

/// The tool that starts a subagent. It's run by the turn, not by [`execute`].
pub const AGENT: &str = "agent";

const MAX_OUTPUT_CHARS: usize = 30_000;
const DEFAULT_READ_LIMIT: usize = 2_000;

/// Every tool takes a `description`: a short, model-written explanation of the step that the
/// UI shows instead of raw arguments, so a run of tool calls reads as a narrative.
const DESCRIPTION_SCHEMA: &str = "Short (3-10 words) description of what this step does and why, \
    shown to the user as the step's label, e.g. \"Find where tool calls are rendered\" or \
    \"Run tests to check the scroll fix\". Write it in the imperative mood.";

pub fn definitions() -> Vec<ToolDefinition> {
    let mut defs = vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a text file. Returns lines prefixed with 1-based line numbers. \
                Use offset/limit for large files."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file" },
                    "offset": { "type": "integer", "description": "1-based line to start from" },
                    "limit": { "type": "integer", "description": "Max lines to read (default 2000)" }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given content. \
                Parent directories are created automatically."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace exact text in a file. old_string must match exactly once \
                unless replace_all is true. Read the file first."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        ToolDefinition {
            name: "list_dir".into(),
            description: "List entries in a directory (directories end with '/').".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory (default '.')" }
                }
            }),
        },
        ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command with `sh -c` in the working directory. \
                Returns combined stdout/stderr and exit code. Use for building, testing, git, \
                etc. Prefer grep/glob for searching and web_search/web_fetch for the web."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "description": "Default 120" }
                },
                "required": ["command"]
            }),
        },
    ];
    defs.extend(search::definitions());
    defs.extend(web::definitions());
    defs.push(ToolDefinition {
        name: AGENT.into(),
        description: "Hand a self-contained task to a subagent: a fresh agent with the same \
            tools (except this one) and permissions, but none of this conversation. It works \
            until done and returns only its final report, so the files and output it goes \
            through don't fill your context. Use it for broad searches or investigations \
            across many files, or a side task you can describe completely. Don't use it for \
            a few tool calls you could make directly, or when you already know which file \
            to look at. The prompt must stand alone: say what to do, what you already know \
            or ruled out, where to look, and what to report back. Treat its report as a \
            lead to check, not a verified fact."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The complete task for the subagent, including context and what to report"
                }
            },
            "required": ["prompt"]
        }),
    });
    for def in &mut defs {
        def.parameters["properties"]["description"] =
            json!({ "type": "string", "description": DESCRIPTION_SCHEMA });
        let required = def.parameters["required"].take();
        let mut required = match required {
            Value::Array(r) => r,
            _ => Vec::new(),
        };
        required.insert(0, json!("description"));
        def.parameters["required"] = Value::Array(required);
    }
    defs
}

/// Human-readable label for a tool call: the model's `description` if it gave one, otherwise
/// a label derived from the arguments.
pub fn describe(name: &str, args: &Value) -> String {
    if let Some(d) = args.get("description").and_then(Value::as_str) {
        let d = d.trim();
        if !d.is_empty() {
            return d.lines().next().unwrap_or(d).to_string();
        }
    }
    let target = summarize(name, args);
    match name {
        "read_file" => format!("Read {target}"),
        "write_file" => format!("Write {target}"),
        "edit_file" => format!("Edit {target}"),
        "list_dir" => format!("List {target}"),
        "grep" => format!("Search for {target}"),
        "glob" => format!("Find files matching {target}"),
        "web_search" => format!("Search the web for {target}"),
        "web_fetch" => format!("Fetch {target}"),
        "bash" => "Run a shell command".into(),
        "agent" => "Hand a task to a subagent".into(),
        _ => format!("Use {name}"),
    }
}

/// Short human-readable summary of a tool call for the UI.
pub fn summarize(name: &str, args: &Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match name {
        "read_file" | "write_file" | "edit_file" => s("path"),
        "list_dir" => {
            let p = s("path");
            if p.is_empty() { ".".into() } else { p }
        }
        "bash" => s("command"),
        "grep" | "glob" => s("pattern"),
        "web_search" => s("query"),
        "web_fetch" => s("url"),
        "agent" => s("prompt"),
        _ => args.to_string(),
    }
}

/// The file a call will modify, if any.
pub fn write_target(name: &str, args: &Value) -> Option<PathBuf> {
    match name {
        "write_file" | "edit_file" => args.get("path").and_then(Value::as_str).map(PathBuf::from),
        _ => None,
    }
}

pub async fn execute(name: &str, args: Value) -> Result<String> {
    match name {
        "read_file" => read_file(serde_json::from_value(args)?).await,
        "write_file" => write_file(serde_json::from_value(args)?).await,
        "edit_file" => edit_file(serde_json::from_value(args)?).await,
        "list_dir" => list_dir(serde_json::from_value(args)?).await,
        "bash" => bash(serde_json::from_value(args)?).await,
        "grep" => search::grep(args).await,
        "glob" => search::glob(args).await,
        "web_search" => web::search(args).await,
        "web_fetch" => web::fetch(args).await,
        other => bail!("unknown tool: {other}"),
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: PathBuf,
    offset: Option<usize>,
    limit: Option<usize>,
}

async fn read_file(a: ReadArgs) -> Result<String> {
    let text = tokio::fs::read_to_string(&a.path)
        .await
        .with_context(|| format!("reading {}", a.path.display()))?;
    let start = a.offset.unwrap_or(1).max(1);
    let limit = a.limit.unwrap_or(DEFAULT_READ_LIMIT);
    let total = text.lines().count();
    let mut out: String = text
        .lines()
        .enumerate()
        .skip(start - 1)
        .take(limit)
        .map(|(i, l)| format!("{:>6}\t{}\n", i + 1, l))
        .collect();
    if start - 1 + limit < total {
        out.push_str(&format!("... ({total} lines total)\n"));
    }
    if out.is_empty() {
        out = "(empty file)".into();
    }
    Ok(truncate(out))
}

#[derive(Deserialize)]
struct WriteArgs {
    path: PathBuf,
    content: String,
}

async fn write_file(a: WriteArgs) -> Result<String> {
    if let Some(parent) = a.path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&a.path, &a.content).await?;
    Ok(format!(
        "Wrote {} lines to {}",
        a.content.lines().count(),
        a.path.display()
    ))
}

#[derive(Deserialize)]
struct EditArgs {
    path: PathBuf,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

async fn edit_file(a: EditArgs) -> Result<String> {
    if a.old_string.is_empty() {
        bail!("old_string must not be empty");
    }
    let text = tokio::fs::read_to_string(&a.path)
        .await
        .with_context(|| format!("reading {}", a.path.display()))?;
    let count = text.matches(&a.old_string).count();
    let updated = match (count, a.replace_all) {
        (0, _) => bail!("old_string not found in {}", a.path.display()),
        (1, _) | (_, true) => text.replace(&a.old_string, &a.new_string),
        (n, false) => bail!("old_string matches {n} times; add context or set replace_all"),
    };
    tokio::fs::write(&a.path, updated).await?;
    Ok(format!(
        "Edited {} ({count} replacement(s))",
        a.path.display()
    ))
}

#[derive(Deserialize)]
struct ListArgs {
    path: Option<PathBuf>,
}

async fn list_dir(a: ListArgs) -> Result<String> {
    let dir = a.path.unwrap_or_else(|| Path::new(".").to_path_buf());
    let mut rd = tokio::fs::read_dir(&dir)
        .await
        .with_context(|| format!("listing {}", dir.display()))?;
    let mut entries = Vec::new();
    while let Some(e) = rd.next_entry().await? {
        let mut name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().await?.is_dir() {
            name.push('/');
        }
        entries.push(name);
    }
    entries.sort();
    Ok(if entries.is_empty() {
        "(empty directory)".into()
    } else {
        truncate(entries.join("\n"))
    })
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    timeout_secs: Option<u64>,
}

async fn bash(a: BashArgs) -> Result<String> {
    let timeout = Duration::from_secs(a.timeout_secs.unwrap_or(120));
    let child = Command::new("sh")
        .arg("-c")
        .arg(&a.command)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(timeout, child)
        .await
        .map_err(|_| anyhow!("command timed out after {}s", timeout.as_secs()))??;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let code = out.status.code().map_or("signal".into(), |c| c.to_string());
    text.push_str(&format!("\n[exit code: {code}]"));
    Ok(truncate(text))
}

/// Bounds tool output for the model. Errors and summaries (compiler errors, failing tests)
/// tend to come last, so this keeps the first third and the last two thirds of the budget,
/// and saves the full output to a file the model can read or grep.
fn truncate(s: String) -> String {
    if s.len() <= MAX_OUTPUT_CHARS {
        return s;
    }
    let head = s.floor_char_boundary(MAX_OUTPUT_CHARS / 3);
    let tail = s.ceil_char_boundary(s.len() - MAX_OUTPUT_CHARS * 2 / 3);
    // Cut at line boundaries unless the nearest one is far away (very long lines).
    let slack = MAX_OUTPUT_CHARS / 10;
    let head = s[..head]
        .rfind('\n')
        .map(|i| i + 1)
        .filter(|&i| head - i <= slack)
        .unwrap_or(head);
    let tail = s[tail..]
        .find('\n')
        .map(|i| tail + i + 1)
        .filter(|&i| i - tail <= slack)
        .unwrap_or(tail);
    let omitted = &s[head..tail];
    let lines = omitted.matches('\n').count();
    let saved = match spill(&s) {
        Ok(path) => format!("full output: {}", path.display()),
        Err(e) => format!("full output could not be saved: {e:#}"),
    };
    format!(
        "{}{}… {lines} lines ({} bytes) omitted; {saved} …\n{}",
        &s[..head],
        if s[..head].ends_with('\n') { "" } else { "\n" },
        omitted.len(),
        &s[tail..]
    )
}

/// Full tool outputs live in `$XDG_CACHE_HOME/tamale/out` (default `~/.cache/tamale/out`).
fn spill_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|dir| dir.join("tamale").join("out"))
}

fn spill(s: &str) -> Result<PathBuf> {
    let dir = spill_dir().context("HOME is not set")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    static PRUNED: std::sync::Once = std::sync::Once::new();
    PRUNED.call_once(|| prune_spills(&dir, Duration::from_secs(7 * 24 * 3600)));
    let path = dir.join(format!("{}.txt", crate::session::new_id()));
    std::fs::write(&path, s).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Best effort: remove saved outputs older than `max_age`, once per process.
fn prune_spills(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_head_and_tail_and_saves_full_output() {
        let short = "fits".to_string();
        assert_eq!(truncate(short.clone()), short);

        let lines: Vec<String> = (0..20_000).map(|i| format!("line {i} é")).collect();
        let full = format!(
            "{}\nerror[E0308]: mismatched types\n[exit code: 101]",
            lines.join("\n")
        );
        let out = truncate(full.clone());
        assert!(out.len() < MAX_OUTPUT_CHARS + 200);
        assert!(out.starts_with("line 0 é\nline 1 é"));
        // Both cuts fall on line boundaries.
        let before = out
            .lines()
            .take_while(|l| !l.starts_with("… "))
            .last()
            .unwrap();
        let after = out
            .lines()
            .skip_while(|l| !l.starts_with("… "))
            .nth(1)
            .unwrap();
        assert!(
            before.starts_with("line ") && before.ends_with(" é"),
            "{before}"
        );
        assert!(
            after.starts_with("line ") && after.ends_with(" é"),
            "{after}"
        );
        assert!(out.ends_with("error[E0308]: mismatched types\n[exit code: 101]"));
        let marker = out.lines().find(|l| l.starts_with("… ")).unwrap();
        assert!(marker.contains("lines (") && marker.contains("bytes) omitted"));
        let path = marker
            .split("full output: ")
            .nth(1)
            .and_then(|rest| rest.strip_suffix(" …"))
            .expect("marker names the saved file");
        assert_eq!(std::fs::read_to_string(path).unwrap(), full);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn every_tool_requires_a_description() {
        for def in definitions() {
            assert!(def.parameters["properties"]["description"].is_object());
            assert_eq!(def.parameters["required"][0], "description", "{}", def.name);
        }
    }

    #[test]
    fn describe_prefers_model_description_with_fallback() {
        let args = json!({ "description": "  Find the render loop\nextra", "path": "src/app.rs" });
        assert_eq!(describe("read_file", &args), "Find the render loop");
        assert_eq!(
            describe("edit_file", &json!({ "path": "src/app.rs" })),
            "Edit src/app.rs"
        );
        assert_eq!(
            describe("bash", &json!({ "command": "ls", "description": "" })),
            "Run a shell command"
        );
    }
}
