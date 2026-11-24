//! The interactive HTML artifacts a reply may end in: what asks the model for them, the
//! store that gives each finished one the id it's served by, and the files a session
//! saves them as.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::render;

/// Added to every prompt during a web session: a reminder of the reply's shape. The design
/// guide itself is in the system prompt (see [`artifact_guide`]).
pub const ARTIFACT_PROMPT: &str = "\n\n<web-ui>Reply for tamale's web UI: do the work with \
tools as usual, then keep the reply brief: a few sentences, or a short list of at most 10 \
items. If the full answer is longer than that (more than 10 findings, steps, files, changes \
or options, or tables, diagrams or long code), write a summary of at most three sentences \
and put the full answer in one complete, self-contained HTML document in a fenced block \
whose info string is exactly `html artifact`, following the artifact design guide in your \
instructions. To revise an earlier artifact, send the whole updated document.</web-ui>";

/// The built-in artifact design guide; `~/.config/tamale/artifacts.md` replaces it.
const ARTIFACT_GUIDE: &str = include_str!("artifact.md");

/// The artifact design guide for the system prompt: the user's
/// `~/.config/tamale/artifacts.md` if it has any text, else the built-in one. With a Gemini
/// key, it ends with how to narrate a video (see `gemini::narration_guide`).
pub fn artifact_guide() -> String {
    let guide = crate::config::user_config_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("artifacts.md")).ok())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| ARTIFACT_GUIDE.to_string());
    match super::gemini::narration_guide() {
        Some(narration) => format!("{}\n\n{}", guide.trim_end(), narration.trim_end()),
        None => guide,
    }
}

/// Where a session's artifacts are saved: `<sessions>/<id>/artifacts/`.
pub fn artifact_dir(session_id: &str) -> Option<PathBuf> {
    Some(crate::session::dir()?.join(session_id).join("artifacts"))
}

/// The file an artifact is saved as: its title as a slug, then a short content hash so
/// revisions sit side by side and re-rendering the same document finds the same file.
pub(super) fn artifact_file_name(html: &str) -> String {
    let mut h = DefaultHasher::new();
    html.hash(&mut h);
    let mut slug = String::new();
    for c in render::artifact_title(html).unwrap_or_default().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "artifact" } else { slug };
    format!("{slug}-{:08x}.html", h.finish() as u32)
}

/// Saves `html` into `dir` unless it's already there.
pub(super) fn save_artifact(dir: &std::path::Path, html: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(artifact_file_name(html));
    if !path.exists() {
        std::fs::create_dir_all(dir)?;
        crate::secure::write_private(&path, html)?;
    }
    Ok(path)
}

/// Script added to each artifact so the page can size its iframe to fit.
const ARTIFACT_RESIZER: &str = "<script>(()=>{const post=()=>parent.postMessage(\
{tamaleArtifactHeight:Math.ceil(document.documentElement.scrollHeight)},'*');\
addEventListener('load',post);new ResizeObserver(post).observe(document.documentElement);\
post();})();</script>";

/// Script added to each artifact so nothing needs sideways scrolling on a narrow screen.
const ARTIFACT_FIT: &str = concat!("<script>", include_str!("artifact-fit.js"), "</script>");

/// SHA-256 of `text` in lowercase hex: an artifact's stable name, the same in every
/// process and Rust version, for clients that keep a library of them (see `apple/ios.md`).
pub fn content_hash(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Artifacts by id, and the id given to each document, so a re-rendered reply keeps its URLs.
#[derive(Default)]
pub(super) struct Artifacts {
    ids: HashMap<u64, String>,
    pub(super) pages: HashMap<String, Arc<str>>,
    /// Each id's [`content_hash`] of the document as the model wrote it.
    pub(super) hashes: HashMap<String, String>,
}

impl Artifacts {
    /// The URL `html` is served from, storing it if new and saving it into `dir`.
    pub(super) fn register(&mut self, html: &str, dir: Option<&std::path::Path>) -> String {
        let mut h = DefaultHasher::new();
        html.hash(&mut h);
        let id = self
            .ids
            .entry(h.finish())
            .or_insert_with(|| {
                let mut bytes = [0u8; 16];
                // Without randomness, fall back to the hash: still served only by this host.
                if getrandom::fill(&mut bytes).is_err() {
                    bytes[..8].copy_from_slice(&h.finish().to_le_bytes());
                }
                bytes.iter().map(|b| format!("{b:02x}")).collect()
            })
            .clone();
        self.hashes
            .entry(id.clone())
            .or_insert_with(|| content_hash(html));
        self.pages.entry(id.clone()).or_insert_with(|| {
            if let Some(dir) = dir {
                let _ = save_artifact(dir, html);
            }
            format!("{html}\n{ARTIFACT_FIT}\n{ARTIFACT_RESIZER}").into()
        });
        format!("/artifact/{id}")
    }
}

/// The answer to an `artifact` request: the page as served (with tamale's scripts), and
/// the [`content_hash`] of the document as written.
pub(super) fn artifact_reply(id: &str, html: Option<&str>, hash: Option<&str>) -> Value {
    json!({ "t": "artifact", "id": id, "html": html, "hash": hash })
}

/// The artifacts saved by the current session (its directory is `current`) and by
/// `sessions`, newest first, each with a URL to view it at. Sessions without any are left
/// out.
pub(super) fn saved_artifacts(
    current: Option<PathBuf>,
    sessions: impl Iterator<Item = (PathBuf, Option<crate::session::Meta>)>,
    artifacts: &Mutex<Artifacts>,
) -> Value {
    let mut dirs: Vec<(PathBuf, Option<crate::session::Meta>)> =
        current.clone().map(|dir| (dir, None)).into_iter().collect();
    for (dir, meta) in sessions {
        match dirs.iter_mut().find(|(d, _)| *d == dir) {
            Some(slot) => slot.1 = meta,
            None => dirs.push((dir, meta)),
        }
    }
    let mut out = Vec::new();
    for (dir, meta) in dirs {
        let mut items: Vec<(u64, Value)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "html"))
            .filter_map(|e| {
                let html = std::fs::read_to_string(e.path()).ok()?;
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                let item = json!({
                    "title": render::artifact_title(&html).unwrap_or_else(|| "Artifact".into()),
                    "file": e.path().display().to_string(),
                    "modified": modified,
                    "hash": content_hash(&html),
                    "url": artifacts.lock().unwrap().register(&html, None),
                });
                Some((modified, item))
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        items.sort_by(|a, b| b.0.cmp(&a.0));
        let newest = items[0].0;
        out.push(json!({
            "current": current.as_ref() == Some(&dir),
            "title": meta.as_ref().map(|m| m.title.clone()).unwrap_or_default(),
            "updated": meta.map_or(newest, |m| m.updated.max(newest)),
            "items": items.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        }));
    }
    // The current session first, then the rest by their latest activity.
    out.sort_by_key(|s| {
        (
            !s["current"].as_bool().unwrap_or(false),
            std::cmp::Reverse(s["updated"].as_u64().unwrap_or(0)),
        )
    });
    json!({ "t": "artifacts", "sessions": out })
}
