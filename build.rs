//! Builds the two apps tamale serves to a browser, so `cargo build` is all it takes:
//!
//! - `web/`, the `/web` page (tamale's Dioxus app, with pullspace in it), served as
//!   `/app.js` and `/app_bg.wasm` (gzipped);
//! - `browser/client`, the `/browser` terminal client, served as `/client.js` and
//!   `/client_bg.wasm`.
//!
//! Each is its own crate, compiled for `wasm32-unknown-unknown` by a cargo of its own (in
//! `<target>/ui`, always in release: a debug module is tens of megabytes and slow), then run
//! through wasm-bindgen as a library. The JavaScript modules a crate brings with it (Dioxus'
//! interpreter, pullspace's bridge) are folded into the one script, so each app is still
//! exactly two files. Nothing built is checked in; `src/web.rs` and `src/browser.rs` include
//! these from `OUT_DIR`.
//!
//! It needs the `wasm32-unknown-unknown` target, which `rust-toolchain.toml` has rustup add.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const WASM: &str = "wasm32-unknown-unknown";

fn main() {
    let root = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env("OUT_DIR"));
    for input in [
        "web/src",
        "web/Cargo.toml",
        "web/Cargo.lock",
        "web/.cargo/config.toml",
        "browser/client/src",
        "browser/client/Cargo.toml",
        "browser/client/Cargo.lock",
        "browser/protocol/src",
        "browser/protocol/Cargo.toml",
        "openui/src",
        "openui/Cargo.toml",
        "openui/dioxus/src",
        "openui/dioxus/Cargo.toml",
        "openui/dioxus/Cargo.lock",
        "rust-toolchain.toml",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }
    check_target();
    let ui_target = ui_target_dir(&out);

    // The /web page: the module gzipped, and the script pointed at it by a URL with its
    // version on, so a browser may cache both for good.
    let module = cargo_wasm(&root.join("web"), &ui_target, "tamale-web");
    let (script, wasm) = bindgen(&module, "app");
    let version = hex(&Sha256::digest([script.as_bytes(), &wasm].concat()))[..12].to_string();
    let script = format!(
        "{script}\n// Started here rather than by the page, which only loads this script.\n\
         __wbg_init({{ module_or_path: \"/app_bg.wasm?v={version}\" }})\
         .then((wasm) => {{ globalThis.__dx_mainWasm = wasm; }});\n"
    );
    write(&out.join("app.js"), script.as_bytes());
    write(&out.join("app_bg.wasm.gz"), &gzip(&wasm));

    // The artifact viewer (openui/dioxus): served for artifacts opened in a tab of their own,
    // and folded into the page the iOS app is sent for one (see src/web/artifacts.rs).
    let module = cargo_wasm_with(
        &root.join("openui/dioxus"),
        &ui_target,
        "openui-viewer",
        &["--features", "viewer"],
    );
    let (script, wasm) = bindgen(&module, "openui");
    let version = hex(&Sha256::digest([script.as_bytes(), &wasm].concat()))[..12].to_string();
    write(&out.join("openui_bare.js"), script.as_bytes());
    let script = format!(
        "{script}\n// Started here rather than by the page, which only loads this script.\n\
         __wbg_init({{ module_or_path: \"/openui_bg.wasm?v={version}\" }})\
         .then((wasm) => {{ globalThis.__dx_mainWasm = wasm; }});\n"
    );
    write(&out.join("openui.js"), script.as_bytes());
    write(&out.join("openui_bg.wasm.gz"), &gzip(&wasm));

    // The /browser client: its page (src/browser/index.html) starts it.
    let module = cargo_wasm(
        &root.join("browser/client"),
        &ui_target,
        "tamale_browser_client",
    );
    let (script, wasm) = bindgen(&module, "client");
    write(&out.join("client.js"), script.as_bytes());
    write(&out.join("client_bg.wasm"), &wasm);
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} isn't set"))
}

/// Fails with what to do when the toolchain can't build for the browser.
fn check_target() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let sysroot = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if !Path::new(&sysroot).join("lib/rustlib").join(WASM).exists() {
        panic!(
            "\n\ntamale builds its web UI for WebAssembly, and this toolchain ({sysroot}) has no \
             {WASM} target.\nrust-toolchain.toml asks rustup for it; without rustup, add it \
             yourself:\n\n    rustup target add {WASM}\n\n"
        );
    }
}

/// Where the browser apps are built: beside tamale's own build, shared by every profile, so
/// switching between debug and release doesn't build them again.
fn ui_target_dir(out: &Path) -> PathBuf {
    // OUT_DIR is <target>[/<triple>]/<profile>/build/tamale-<hash>/out.
    let profile_dir = out
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is inside a target directory");
    profile_dir
        .parent()
        .expect("a profile directory is inside a target directory")
        .join("ui")
}

/// Builds the crate at `dir` for the browser and returns its module.
fn cargo_wasm(dir: &Path, target_dir: &Path, artifact: &str) -> PathBuf {
    cargo_wasm_with(dir, target_dir, artifact, &[])
}

/// [`cargo_wasm`], with more arguments for cargo (features, say).
fn cargo_wasm_with(dir: &Path, target_dir: &Path, artifact: &str, args: &[&str]) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(dir)
        .args(["build", "--release", "--target", WASM])
        .args(args)
        .arg("--target-dir")
        .arg(target_dir);
    // What cargo set for this build is about the host; the crate's own `.cargo/config.toml`
    // (web/'s getrandom backend) has to win, which no RUSTFLAGS does.
    for name in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_RUSTFLAGS",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        command.env_remove(name);
    }
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("couldn't run cargo for {}: {e}", dir.display()));
    if !status.success() {
        panic!("building {} for the browser failed", dir.display());
    }
    target_dir
        .join(WASM)
        .join("release")
        .join(format!("{artifact}.wasm"))
}

/// The module's script and the module itself, from wasm-bindgen, as one script with every
/// JavaScript module it imports folded in.
fn bindgen(module: &Path, name: &str) -> (String, Vec<u8>) {
    let mut output = wasm_bindgen_cli_support::Bindgen::new()
        .input_path(module)
        .web(true)
        .and_then(|b| b.typescript(false).out_name(name).generate_output())
        .unwrap_or_else(|e| panic!("wasm-bindgen on {}: {e:#}", module.display()));
    let mut modules: Vec<(String, String)> = Vec::new();
    for (crate_id, snippets) in output.snippets() {
        for (i, source) in snippets.iter().enumerate() {
            modules.push((
                format!("./snippets/{crate_id}/inline{i}.js"),
                source.clone(),
            ));
        }
    }
    for (path, source) in output.local_modules() {
        modules.push((format!("./snippets/{path}"), source.clone()));
    }
    modules.sort();
    let script = inline_modules(output.js(), &modules)
        .unwrap_or_else(|e| panic!("folding {}'s JavaScript in: {e}", module.display()));
    (script, output.wasm_mut().emit_wasm())
}

/// `script` with each of its imports of `modules` (path, source) replaced by the module
/// itself, run once in a function of its own. They're the plain modules crates bring for
/// wasm-bindgen (`export function`, `export class`, `export { … }`, no imports of their own);
/// anything else is an error here rather than a page that doesn't start.
fn inline_modules(script: &str, modules: &[(String, String)]) -> Result<String, String> {
    let mut defined = String::new();
    let mut names: HashMap<&str, String> = HashMap::new();
    for (i, (path, source)) in modules.iter().enumerate() {
        let ident = format!("__tamale_module_{i}");
        let (body, exports) = strip_exports(source).map_err(|e| format!("{path}: {e}"))?;
        let fields: Vec<String> = exports
            .iter()
            .map(|(local, exported)| format!("{exported}: {local}"))
            .collect();
        defined.push_str(&format!(
            "const {ident} = (() => {{\n{body}\nreturn {{ {} }};\n}})();\n",
            fields.join(", ")
        ));
        names.insert(path.as_str(), ident);
    }
    let mut out = String::with_capacity(script.len() + defined.len());
    out.push_str(&defined);
    for line in script.lines() {
        let trimmed = line.trim();
        let Some(from) = trimmed
            .strip_prefix("import ")
            .and_then(|rest| rest.rsplit_once(" from "))
        else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let (what, path) = from;
        let path = path
            .trim_end_matches(';')
            .trim_matches(|c| c == '\'' || c == '"');
        let ident = names
            .get(path)
            .ok_or_else(|| format!("the script imports {path}, which wasm-bindgen didn't give"))?;
        let what = what.trim();
        if let Some(alias) = what.strip_prefix("* as ") {
            out.push_str(&format!("const {} = {ident};\n", alias.trim()));
        } else if let Some(list) = what.strip_prefix('{').and_then(|w| w.strip_suffix('}')) {
            let list: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| match n.split_once(" as ") {
                    Some((name, alias)) => format!("{}: {}", name.trim(), alias.trim()),
                    None => n.to_string(),
                })
                .collect();
            out.push_str(&format!("const {{ {} }} = {ident};\n", list.join(", ")));
        } else {
            return Err(format!("an import this doesn't read: {trimmed}"));
        }
    }
    Ok(out)
}

/// A module's source without its `export` keywords, and what it exported: (local name,
/// exported name).
fn strip_exports(source: &str) -> Result<(String, Vec<(String, String)>), String> {
    let mut exports = Vec::new();
    let mut body = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = find_keyword(rest, "export") {
        body.push_str(&rest[..at]);
        let after = rest[at + "export".len()..].trim_start();
        if let Some(list) = after.strip_prefix('{') {
            // `export { a, b as c };`
            let end = list.find('}').ok_or("an `export {` that doesn't close")?;
            for item in list[..end]
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                match item.split_once(" as ") {
                    Some((local, exported)) => {
                        exports.push((local.trim().to_string(), exported.trim().to_string()))
                    }
                    None => exports.push((item.to_string(), item.to_string())),
                }
            }
            rest = list[end + 1..].trim_start_matches(|c: char| c == ';' || c.is_whitespace());
            continue;
        }
        // `export function f`, `export async function f`, `export class C`, `export const x`.
        let mut words = after.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'));
        let kind = words.next().unwrap_or_default();
        let name = match kind {
            "function" | "class" | "const" | "let" | "var" => words.find(|w| !w.is_empty()),
            "async" => words.filter(|w| !w.is_empty()).nth(1),
            _ => None,
        }
        .ok_or_else(|| format!("an export this doesn't read: export {}", head(after)))?;
        exports.push((name.to_string(), name.to_string()));
        rest = after;
    }
    body.push_str(rest);
    if find_keyword(&body, "import").is_some_and(|at| {
        let after = body[at + "import".len()..].trim_start();
        after.starts_with('{')
            || after.starts_with('*')
            || after.starts_with('"')
            || after.starts_with('\'')
    }) {
        return Err("it imports something itself".into());
    }
    Ok((body, exports))
}

/// Where `word` starts as a statement keyword in `s`: at the start or after whitespace, `;`,
/// `{` or `}`, and not followed by more of an identifier.
fn find_keyword(s: &str, word: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut from = 0;
    while let Some(i) = s[from..].find(word).map(|i| i + from) {
        let before = i.checked_sub(1).map(|j| bytes[j]);
        let after = bytes.get(i + word.len()).copied();
        let starts = before.is_none_or(|b| b.is_ascii_whitespace() || b"};{".contains(&b));
        let ends = after.is_none_or(|b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'$'));
        if starts && ends {
            return Some(i);
        }
        from = i + word.len();
    }
    None
}

fn head(s: &str) -> &str {
    &s[..s.len().min(40)]
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(bytes).expect("gzip in memory");
    gz.finish().expect("gzip in memory")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes only what changed, so a rebuild that changed nothing leaves tamale alone.
fn write(path: &Path, bytes: &[u8]) {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return;
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}
