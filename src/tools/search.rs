use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use rig_core::completion::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{MAX_OUTPUT_CHARS, truncate};

#[cfg(test)]
mod tests;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn definitions() -> Vec<ToolDefinition> {
    let common = json!({
        "path": { "type": "string", "description": "File or directory to search (default '.')" },
        "include_hidden": { "type": "boolean", "description": "Include hidden files (default false); .git is always excluded" },
        "respect_ignore": { "type": "boolean", "description": "Honor .gitignore, .ignore and global git ignores (default true)" },
        "max_results": { "type": "integer", "minimum": 1, "maximum": 2000, "description": "Maximum matching lines or files (default 200)" }
    });
    let mut grep = common.clone();
    grep.as_object_mut().unwrap().extend(json!({
        "pattern": { "type": "string", "description": "Rust regex (line-based; no lookaround/backreferences), or literal with fixed_strings" },
        "glob": { "type": "string", "description": "Optional file filter, e.g. '*.rs' or 'src/**/*.rs', relative to path" },
        "ignore_case": { "type": "boolean", "description": "Case insensitive matching (default false)" },
        "fixed_strings": { "type": "boolean", "description": "Treat pattern as literal text (default false)" },
        "context": { "type": "integer", "minimum": 0, "maximum": 20, "description": "Surrounding lines per match (default 0)" },
        "output_mode": { "type": "string", "enum": ["content", "files_with_matches", "count"], "description": "Numbered matching lines (default), matching paths, or matching line counts per file" }
    }).as_object().unwrap().clone());
    let mut glob = common;
    glob["pattern"] = json!({ "type": "string", "description": "Glob relative to path, e.g. '**/*.rs', 'src/*.{rs,toml}'. Patterns without '/' match basenames at any depth." });
    vec![
        ToolDefinition {
            name: "grep".into(),
            description: "Search text files recursively without a shell or external grep. Returns path:line:text, matching paths, or counts. Skips binary files and files over 2 MiB. Output and runtime are bounded; narrow path/glob if truncated.".into(),
            parameters: json!({ "type": "object", "properties": grep, "required": ["pattern"] }),
        },
        ToolDefinition {
            name: "glob".into(),
            description: "Find files recursively by glob pattern without a shell. Respects ignore files and skips hidden files by default. Returns sorted paths, with explicit notice when results are limited.".into(),
            parameters: json!({ "type": "object", "properties": glob, "required": ["pattern"] }),
        },
    ]
}

#[derive(Deserialize)]
struct Options {
    #[serde(default = "default_path")]
    path: PathBuf,
    #[serde(default)]
    include_hidden: bool,
    #[serde(default = "yes")]
    respect_ignore: bool,
    max_results: Option<usize>,
}

fn default_path() -> PathBuf {
    PathBuf::from(".")
}

fn yes() -> bool {
    true
}

impl Options {
    fn limit(&self) -> usize {
        self.max_results.unwrap_or(200).clamp(1, 2000)
    }

    fn walker(&self) -> Result<ignore::Walk> {
        let metadata = std::fs::metadata(&self.path)
            .with_context(|| format!("searching {}", self.path.display()))?;
        if !metadata.is_file() && !metadata.is_dir() {
            bail!("search path must be a regular file or directory");
        }
        Ok(WalkBuilder::new(&self.path)
            .standard_filters(self.respect_ignore)
            .hidden(!self.include_hidden)
            .require_git(false)
            .follow_links(false)
            .sort_by_file_name(|a, b| a.cmp(b))
            .filter_entry(|e| e.file_name() != ".git")
            .build())
    }
}

fn matcher(pattern: &str) -> Result<GlobMatcher> {
    if pattern.is_empty() {
        bail!("glob pattern must not be empty");
    }
    Ok(GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .context("invalid glob pattern")?
        .compile_matcher())
}

fn matches_path(matcher: &GlobMatcher, pattern: &str, path: &Path, root: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative = if relative.as_os_str().is_empty() {
        path.file_name().map(Path::new).unwrap_or(path)
    } else {
        relative
    };
    matcher.is_match(relative)
        || (!pattern.contains('/')
            && path
                .file_name()
                .is_some_and(|name| matcher.is_match(Path::new(name))))
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(flatten)]
    options: Options,
}

pub(super) async fn glob(args: Value) -> Result<String> {
    let args = serde_json::from_value::<GlobArgs>(args)?;
    tokio::task::spawn_blocking(move || glob_sync(args)).await?
}

fn glob_sync(a: GlobArgs) -> Result<String> {
    let matcher = matcher(&a.pattern)?;
    let started = Instant::now();
    let mut paths = Vec::new();
    let mut bytes = 0;
    let mut warnings = 0;
    let mut limited = false;
    for entry in a.options.walker()? {
        if started.elapsed() >= SEARCH_TIMEOUT {
            limited = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                warnings += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file())
            || !matches_path(&matcher, &a.pattern, entry.path(), &a.options.path)
        {
            continue;
        }
        let path = entry.path().display().to_string();
        if paths.len() >= a.options.limit() || bytes + path.len() + 1 > MAX_OUTPUT_CHARS {
            limited = true;
            break;
        }
        bytes += path.len() + 1;
        paths.push(path);
    }
    paths.sort();
    let mut out = if paths.is_empty() {
        "No files found.".into()
    } else {
        paths.join("\n")
    };
    finish(&mut out, limited, warnings, 0);
    Ok(out)
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
    #[default]
    Content,
    FilesWithMatches,
    Count,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    glob: Option<String>,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    fixed_strings: bool,
    #[serde(default)]
    context: usize,
    #[serde(default)]
    output_mode: OutputMode,
    #[serde(flatten)]
    options: Options,
}

pub(super) async fn grep(args: Value) -> Result<String> {
    let args = serde_json::from_value::<GrepArgs>(args)?;
    tokio::task::spawn_blocking(move || grep_sync(args)).await?
}

fn grep_sync(a: GrepArgs) -> Result<String> {
    let pattern = if a.fixed_strings {
        regex::escape(&a.pattern)
    } else {
        a.pattern
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(a.ignore_case)
        .size_limit(10 * 1024 * 1024)
        .build()
        .context("invalid search regex")?;
    let filter = a.glob.as_deref().map(matcher).transpose()?;
    let started = Instant::now();
    let mut out = String::new();
    let mut results = 0;
    let mut warnings = 0;
    let mut skipped_large = 0;
    let mut limited = false;
    for entry in a.options.walker()? {
        if started.elapsed() >= SEARCH_TIMEOUT {
            limited = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                warnings += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if let (Some(filter), Some(pattern)) = (&filter, &a.glob)
            && !matches_path(filter, pattern, path, &a.options.path)
        {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            skipped_large += 1;
            continue;
        }
        let mut bytes = Vec::new();
        let read =
            File::open(path).and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
        if read.is_err() {
            warnings += 1;
            continue;
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            skipped_large += 1;
            continue;
        }
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<_> = text.lines().collect();
        let matches: Vec<_> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| regex.is_match(line).then_some(i))
            .collect();
        if matches.is_empty() {
            continue;
        }
        if results >= a.options.limit() {
            limited = true;
            break;
        }
        match a.output_mode {
            OutputMode::FilesWithMatches => {
                out.push_str(&format!("{}\n", path.display()));
                results += 1;
            }
            OutputMode::Count => {
                out.push_str(&format!("{}:{}\n", path.display(), matches.len()));
                results += 1;
            }
            OutputMode::Content => {
                let take = matches.len().min(a.options.limit() - results);
                limited = take < matches.len();
                results += take;
                let context = a.context.min(20);
                let mut end = 0;
                for &matched in &matches[..take] {
                    let start = matched.saturating_sub(context).max(end);
                    let next_end = (matched + context + 1).min(lines.len());
                    if start > end && end > 0 {
                        out.push_str("--\n");
                    }
                    for (i, line) in lines.iter().enumerate().take(next_end).skip(start) {
                        let sep = if matches.binary_search(&i).is_ok() {
                            ':'
                        } else {
                            '-'
                        };
                        out.push_str(&format!("{}{sep}{}{sep}{line}\n", path.display(), i + 1));
                        if out.len() >= MAX_OUTPUT_CHARS {
                            break;
                        }
                    }
                    end = next_end;
                    if out.len() >= MAX_OUTPUT_CHARS {
                        break;
                    }
                }
            }
        }
        if limited || out.len() >= MAX_OUTPUT_CHARS {
            limited = true;
            break;
        }
    }
    out = truncate(out);
    if out.is_empty() {
        out.push_str("No matches found.");
    }
    finish(&mut out, limited, warnings, skipped_large);
    Ok(out)
}

fn finish(out: &mut String, limited: bool, warnings: usize, skipped_large: usize) {
    if limited {
        out.push_str("\n[Results limited by max_results, output size, or 15s timeout; narrow path/pattern to continue.]");
    }
    if warnings > 0 {
        out.push_str(&format!(
            "\n[Skipped {warnings} unreadable files/directories or ignore-file errors.]"
        ));
    }
    if skipped_large > 0 {
        out.push_str(&format!("\n[Skipped {skipped_large} files over 2 MiB.]"));
    }
}
