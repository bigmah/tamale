//! The artifacts a reply may end in: OpenUI Lang documents (see the `openui` crate) that
//! the page draws with its own components. Here: what asks the model for them, the store
//! that gives each finished one the id it's kept by, the page that shows one in a tab of
//! its own, and the files a session saves them as.

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
or options, or tables, diagrams, charts or long code), write a summary of at most three \
sentences and put the full answer in one OpenUI Lang document in a fenced block whose info \
string is exactly `openui`, using only the components in the artifact guide in your \
instructions. To revise an earlier artifact, send the whole updated document.</web-ui>";

/// The built-in artifact guide; `~/.config/tamale/artifacts.md` replaces its prose. The
/// component reference is always tamale's own (see [`openui::library::reference`]).
const ARTIFACT_GUIDE: &str = include_str!("artifact.md");

/// Where the guide's component reference goes.
const COMPONENTS_MARK: &str = "{{components}}";

/// The artifact guide for the system prompt: the user's `~/.config/tamale/artifacts.md` if
/// it has any text, else the built-in one, with the component library in it (at
/// `{{components}}`, or at the end). With a Gemini key, it ends with how to narrate a video
/// (see `gemini::narration_guide`).
pub fn artifact_guide() -> String {
    let guide = crate::config::user_config_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("artifacts.md")).ok())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| ARTIFACT_GUIDE.to_string());
    let reference = openui::library::reference();
    let guide = if guide.contains(COMPONENTS_MARK) {
        guide.replace(COMPONENTS_MARK, reference.trim_end())
    } else {
        format!(
            "{}\n\n## Components\n\n{}",
            guide.trim_end(),
            reference.trim_end()
        )
    };
    match super::gemini::narration_guide() {
        Some(narration) => format!("{}\n\n{}", guide.trim_end(), narration.trim_end()),
        None => guide,
    }
}

/// Where a session's artifacts are saved: `<sessions>/<id>/artifacts/`.
pub fn artifact_dir(session_id: &str) -> Option<PathBuf> {
    Some(crate::session::dir()?.join(session_id).join("artifacts"))
}

/// The extension artifacts are saved with.
pub const EXTENSION: &str = "openui";

/// The file an artifact is saved as: its title as a slug, then a short content hash so
/// revisions sit side by side and re-rendering the same document finds the same file.
pub(super) fn artifact_file_name(source: &str) -> String {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    let mut slug = String::new();
    for c in render::artifact_title(source).unwrap_or_default().chars() {
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
    format!("{slug}-{:08x}.{EXTENSION}", h.finish() as u32)
}

/// Saves `source` into `dir` unless it's already there.
pub(super) fn save_artifact(dir: &std::path::Path, source: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(artifact_file_name(source));
    if !path.exists() {
        std::fs::create_dir_all(dir)?;
        crate::secure::write_private(&path, source)?;
    }
    Ok(path)
}

/// SHA-256 of `text` in lowercase hex: an artifact's stable name, the same in every
/// process and Rust version, for clients that keep a library of them (see `apple/ios.md`).
pub fn content_hash(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The viewer's script without its start (wasm-bindgen's module, see `build.rs`), and its
/// module gzipped, for [`embedded_page`].
const VIEWER_BARE: &str = include_str!(concat!(env!("OUT_DIR"), "/openui_bare.js"));
const VIEWER_MODULE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/openui_bg.wasm.gz"));

/// What the viewer's page is styled with: the web UI's colour and type tokens (app.css's
/// `:root`), the page's own rules and the components'.
static VIEWER_STYLE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let app = include_str!("app.css");
    let tokens = app
        .find(":root {")
        .and_then(|start| Some(&app[start..start + app[start..].find("\n}\n")? + 2]))
        .unwrap_or_default();
    format!(
        "{tokens}\n{}\n{}",
        include_str!("../../openui/dioxus/viewer.css"),
        include_str!("../../openui/dioxus/openui.css")
    )
});

/// A page that draws one artifact with the viewer (`openui/dioxus`). The document is data in
/// a JSON script element, so nothing in it runs. `script` starts the viewer.
fn page(source: &str, class: &str, script: &str) -> String {
    let title = render::artifact_title(source).unwrap_or_else(|| "Artifact".into());
    let data = json!({ "title": title, "source": source })
        .to_string()
        // Nothing in the data may end or confuse its script element.
        .replace('<', "\\u003c");
    let mut escaped = String::new();
    for c in title.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            c => escaped.push(c),
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n\
         <meta name=\"color-scheme\" content=\"dark\">\n<title>{escaped}</title>\n\
         <style>\n{style}\n</style>\n\
         <script type=\"application/json\" id=\"openui-artifact\">{data}</script>\n\
         {script}\n</head>\n<body class=\"{class}\">\n<div id=\"main\"></div>\n</body>\n</html>\n",
        style = *VIEWER_STYLE,
    )
}

/// An artifact in a tab of its own (`/artifact/<id>`): the viewer's page, with the viewer
/// loaded from this server (`/openui.js`).
pub(super) fn viewer_page(source: &str) -> String {
    page(
        source,
        "",
        "<script type=\"module\" src=\"/openui.js\"></script>",
    )
}

/// An artifact as one self-contained page, for clients that show artifacts in a web view of
/// their own and keep them offline (the iOS app): the viewer's script inline, and its
/// module in it gzipped, so the page needs nothing from anywhere. It runs under a policy
/// that allows inline scripts and `unsafe-eval` (which WebAssembly needs), as the app's does.
pub fn embedded_page(source: &str) -> String {
    use base64::Engine;
    let module = base64::engine::general_purpose::STANDARD.encode(VIEWER_MODULE);
    let script = format!(
        "<script type=\"application/octet-stream\" id=\"openui-module\">{module}</script>\n\
         <script type=\"module\">\n{VIEWER_BARE}\n\
         const gz = Uint8Array.from(atob(document.getElementById('openui-module').textContent), (c) => c.charCodeAt(0));\n\
         const bytes = await new Response(new Blob([gz]).stream().pipeThrough(new DecompressionStream('gzip'))).arrayBuffer();\n\
         __wbg_init({{ module_or_path: bytes }}).then((wasm) => {{ globalThis.__dx_mainWasm = wasm; }});\n\
         </script>",
    );
    page(source, "embedded", &script)
}

/// Artifacts by id, and the id given to each document, so a re-rendered reply keeps its URLs.
#[derive(Default)]
pub(super) struct Artifacts {
    ids: HashMap<u64, String>,
    /// Each id's document.
    pub(super) sources: HashMap<String, Arc<str>>,
    /// Each id's [`content_hash`].
    pub(super) hashes: HashMap<String, String>,
}

impl Artifacts {
    /// The URL `source` is kept at, storing it if new and saving it into `dir`.
    pub(super) fn register(&mut self, source: &str, dir: Option<&std::path::Path>) -> String {
        let mut h = DefaultHasher::new();
        source.hash(&mut h);
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
            .or_insert_with(|| content_hash(source));
        self.sources.entry(id.clone()).or_insert_with(|| {
            if let Some(dir) = dir {
                let _ = save_artifact(dir, source);
            }
            source.into()
        });
        format!("/artifact/{id}")
    }
}

/// The answer to an `artifact` request: the document, its title and [`content_hash`], and
/// with `page`, for clients that show artifacts in a web view (the iOS app), a
/// self-contained page that draws it ([`embedded_page`]). The web UI draws the document
/// itself, and asks without.
pub(super) fn artifact_reply(
    id: &str,
    source: Option<&str>,
    hash: Option<&str>,
    page: bool,
) -> Value {
    json!({
        "t": "artifact",
        "id": id,
        "source": source,
        "title": source.and_then(render::artifact_title),
        "html": source.filter(|_| page).map(embedded_page),
        "hash": hash,
    })
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
            .filter(|e| e.path().extension().is_some_and(|x| x == EXTENSION))
            .filter_map(|e| {
                let source = std::fs::read_to_string(e.path()).ok()?;
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                let item = json!({
                    "title": render::artifact_title(&source).unwrap_or_else(|| "Artifact".into()),
                    "file": e.path().display().to_string(),
                    "modified": modified,
                    "hash": content_hash(&source),
                    "url": artifacts.lock().unwrap().register(&source, None),
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
