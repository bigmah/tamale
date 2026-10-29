use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rig_core::completion::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

mod search;
mod web;

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
        _ => args.to_string(),
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

fn truncate(mut s: String) -> String {
    if s.len() > MAX_OUTPUT_CHARS {
        let mut cut = MAX_OUTPUT_CHARS;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n... (output truncated)");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

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
