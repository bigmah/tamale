//! Gemini in the page, and the videos a reply shows.
//!
//! The page talks to Gemini's Live API itself (see the voice section of `app.js`); tamale only
//! keeps its key and the user's choices, so they're the same in every session and browser
//! (each session's page is an origin of its own, so the browser can't keep them). A key pasted
//! in the page goes to `~/.config/tamale/gemini.json`, readable only by this user; without one
//! the page gets `GEMINI_API_KEY` from tamale's environment. Anyone who can open the page can
//! already run commands as the user, so handing it the key gives nothing away.
//!
//! A reply shows a video it rendered (with Manim, say) as a ```` ```video ```` block holding the
//! file's path. The page asks for the file over its socket, as it does for artifacts, so it
//! plays through a hub too.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The environment variable the page's key falls back to.
pub const KEY_ENV: &str = "GEMINI_API_KEY";

/// Past this, a video isn't sent: it would hold up the session's socket for too long.
pub const MAX_VIDEO: u64 = 32 << 20;

/// What `gemini.json` keeps. Empty `model` and `voice` mean Gemini's choice.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub model: String,
    pub voice: String,
}

pub fn settings_path() -> Option<PathBuf> {
    Some(crate::config::user_config_dir()?.join("gemini.json"))
}

pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes `settings` to `path`, only this user able to read it.
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("json.{}", std::process::id()));
    crate::secure::write_private(&temp, serde_json::to_string_pretty(settings)?)?;
    std::fs::rename(&temp, path)
}

/// `GEMINI_API_KEY=AIza…`, quotes and all, as it might be pasted from a `.env` file.
pub fn clean_key(pasted: &str) -> String {
    let mut s = pasted.trim();
    if let Some((name, value)) = s.split_once('=')
        && name.to_ascii_uppercase().contains("KEY")
    {
        s = value;
    }
    s.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
        .to_string()
}

/// The `gemini` message: the key to use and where it's from (`saved`, `env` or null), and
/// the chosen model and voice.
pub fn reply(settings: &Settings, env_key: Option<String>) -> Value {
    let (key, from) = pick_key(settings, env_key);
    json!({
        "t": "gemini",
        "key": key,
        "from": from,
        "env": KEY_ENV,
        "model": settings.model,
        "voice": settings.voice,
    })
}

/// The key to use, the saved one before the environment's, and where it's from.
fn pick_key(
    settings: &Settings,
    env_key: Option<String>,
) -> (Option<String>, Option<&'static str>) {
    let saved = settings.key.clone().filter(|k| !k.trim().is_empty());
    let env_key = env_key.filter(|k| !k.trim().is_empty());
    match (saved, env_key) {
        (Some(key), _) => (Some(key), Some("saved")),
        (None, Some(key)) => (Some(key), Some("env")),
        (None, None) => (None, None),
    }
}

/// What a video's scene runs to narrate itself (see [`narration_guide`]).
const NARRATION: &str = include_str!("narration.py");

/// The key and the chosen voice outside the page: the key saved in `gemini.json`, else
/// `GEMINI_API_KEY`.
fn current() -> (Option<String>, String) {
    let settings = settings_path().map(|path| load(&path)).unwrap_or_default();
    let (key, _) = pick_key(&settings, env_key());
    (key, settings.voice)
}

/// Environment for the commands the agent runs, so a video's scene can narrate itself with
/// the key saved in the page too: `TAMALE_GEMINI_KEY` and `TAMALE_GEMINI_VOICE`. Names of
/// tamale's own, so a backend's CLI doesn't take the key for its own.
pub fn child_env() -> Vec<(&'static str, String)> {
    let (key, voice) = current();
    let mut env = Vec::new();
    if let Some(key) = key {
        env.push(("TAMALE_GEMINI_KEY", key));
        if !voice.trim().is_empty() {
            env.push(("TAMALE_GEMINI_VOICE", voice));
        }
    }
    env
}

/// The artifact guide's part on narrating videos, when there's a Gemini key to speak with.
pub fn narration_guide() -> Option<String> {
    current().0.map(|_| narration_section())
}

fn narration_section() -> String {
    format!(
        "## Narrated videos\n\n\
A Gemini key is set up, so every video you make has a spoken narration unless the user asks \
for a silent one. Paste this code, unchanged, at the top of the scene file, before \
`from manim import *`:\n\n```python\n{}```\n\n\
- Write the narration with the story: one or two short sentences for each step, in plain \
words that read well aloud (say \"x squared\", not \"x^2\"; no markdown, symbols or code).\n\
- Wrap each step in `with narrate(self, \"The sentence.\") as line:`. The line starts with \
the step, and the step is stretched to last as long as the line. `line.seconds` is its \
length, to pace animations by (`run_time=line.seconds * 0.6`). Show what is said as it's \
said, and let the narration set the video's length.\n\
- The helper reads the key from the environment itself: never print, write or ask for it.\n\
- If the render prints `NARRATION FAILED`, the video still renders, silently, at the same \
pace: show it and say in one sentence that it has no sound, and why.\n",
        NARRATION
    )
}

/// Applies a `gemini_save` request's fields to `settings`: a `key` of "" forgets the saved one.
pub fn update(settings: &mut Settings, request: &Value) {
    if let Some(key) = request.get("key").and_then(Value::as_str) {
        let key = clean_key(key);
        settings.key = (!key.is_empty()).then_some(key);
    }
    if let Some(model) = request.get("model").and_then(Value::as_str) {
        settings.model = model.trim().to_string();
    }
    if let Some(voice) = request.get("voice").and_then(Value::as_str) {
        settings.voice = voice.trim().to_string();
    }
}

/// The key tamale's environment has for Gemini, if any.
pub fn env_key() -> Option<String> {
    std::env::var(KEY_ENV).ok()
}

/// A video's MIME type, by its extension; `None` for anything that isn't one.
fn video_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "gif" => "image/gif",
        _ => return None,
    })
}

/// The `video` message for `path`: the file's bytes, in base64, if it's a video inside one
/// of `allowed` (the workspace and the temporary directory, where a scratch render goes).
/// A relative path is taken from the first of them.
pub fn video(allowed: &[PathBuf], path: &str) -> Value {
    let error = |text: String| json!({ "t": "video", "path": path, "error": text });
    let given = PathBuf::from(path.trim());
    let full = match (given.is_absolute(), allowed.first()) {
        (true, _) => given,
        (false, Some(base)) => base.join(given),
        (false, None) => return error("This session has no working directory.".into()),
    };
    let Some(mime) = video_type(&full) else {
        return error("Only .mp4, .m4v, .mov, .webm and .gif files play here.".into());
    };
    // Resolved, so neither `..` nor a symlink leads out of the allowed directories.
    let Ok(real) = full.canonicalize() else {
        return error(format!("There's no file at {}.", full.display()));
    };
    let inside = allowed
        .iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .any(|dir| real.starts_with(dir));
    if !inside {
        return error("Only videos in the project or the temporary directory play here.".into());
    }
    match std::fs::metadata(&real) {
        Ok(meta) if !meta.is_file() => return error(format!("{} isn't a file.", real.display())),
        Ok(meta) if meta.len() > MAX_VIDEO => {
            return error(format!(
                "The video is {} MB; at most {} MB play here. Render it smaller (`-ql` or `-qm`).",
                meta.len() >> 20,
                MAX_VIDEO >> 20
            ));
        }
        Err(e) => return error(format!("Couldn't read {}: {e}", real.display())),
        Ok(_) => {}
    }
    match std::fs::read(&real) {
        Ok(bytes) => json!({
            "t": "video",
            "path": path,
            "mime": mime,
            "size": bytes.len(),
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }),
        Err(e) => error(format!("Couldn't read {}: {e}", real.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tamale-gemini-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_pasted_key_loses_its_name_and_quotes() {
        assert_eq!(clean_key("  GEMINI_API_KEY=\"AIza123\"\n"), "AIza123");
        assert_eq!(clean_key("AIza123"), "AIza123");
        assert_eq!(clean_key("'AIza123'"), "AIza123");
    }

    #[test]
    fn a_saved_key_wins_over_the_environment() {
        let mut settings = Settings::default();
        assert_eq!(reply(&settings, None)["key"], Value::Null);
        let from_env = reply(&settings, Some("env-key".into()));
        assert_eq!(
            (from_env["key"].as_str(), from_env["from"].as_str()),
            (Some("env-key"), Some("env"))
        );

        update(
            &mut settings,
            &json!({ "key": "GEMINI_API_KEY=saved", "voice": " Puck " }),
        );
        let saved = reply(&settings, Some("env-key".into()));
        assert_eq!(
            (saved["key"].as_str(), saved["from"].as_str()),
            (Some("saved"), Some("saved"))
        );
        assert_eq!(saved["voice"], "Puck");

        // Forgetting it goes back to the environment's; fields not sent stay as they were.
        update(&mut settings, &json!({ "key": "" }));
        assert_eq!(reply(&settings, Some("env-key".into()))["from"], "env");
        assert_eq!(settings.voice, "Puck");
    }

    #[test]
    fn the_narration_guide_carries_the_whole_helper() {
        let guide = narration_section();
        assert!(
            guide.contains(NARRATION),
            "the helper is pasted in unchanged"
        );
        assert!(guide.contains("with narrate(self, "));
        assert!(guide.contains("NARRATION FAILED") && NARRATION.contains("NARRATION FAILED"));
        // It reads the key tamale gives the agent's commands.
        assert!(
            NARRATION.contains("TAMALE_GEMINI_KEY") && NARRATION.contains("TAMALE_GEMINI_VOICE")
        );
    }

    #[test]
    fn settings_are_saved_privately_and_read_back() {
        let dir = scratch("settings");
        let path = dir.join("nested").join("gemini.json");
        assert_eq!(load(&path), Settings::default());
        let settings = Settings {
            key: Some("k".into()),
            model: "m".into(),
            voice: "Kore".into(),
        };
        save(&path, &settings).unwrap();
        assert_eq!(load(&path), settings);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn videos_are_read_only_from_allowed_places() {
        let root = scratch("video-root");
        let outside = scratch("video-outside");
        std::fs::create_dir_all(root.join("media")).unwrap();
        std::fs::write(root.join("media/scene.mp4"), b"not really a video").unwrap();
        std::fs::write(root.join("notes.txt"), b"secret").unwrap();
        std::fs::write(outside.join("other.mp4"), b"elsewhere").unwrap();
        let allowed = [root.clone()];

        let ok = video(&allowed, "media/scene.mp4");
        assert_eq!(ok["mime"], "video/mp4", "{ok}");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(ok["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(bytes, b"not really a video");
        let absolute = video(&allowed, root.join("media/scene.mp4").to_str().unwrap());
        assert!(absolute["data"].is_string(), "{absolute}");

        // Not a video, outside, escaping with `..`, or missing.
        assert!(video(&allowed, "notes.txt")["error"].is_string());
        assert!(video(&allowed, outside.join("other.mp4").to_str().unwrap())["error"].is_string());
        let escape = format!(
            "../{}/other.mp4",
            outside.file_name().unwrap().to_str().unwrap()
        );
        assert!(video(&allowed, &escape)["error"].is_string());
        assert!(video(&allowed, "media/missing.mp4")["error"].is_string());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("other.mp4"), root.join("link.mp4")).unwrap();
            assert!(video(&allowed, "link.mp4")["error"].is_string());
        }
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}
