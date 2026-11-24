//! The desktop app: a window onto your tamale sessions. It lists the ones running in the
//! background on this machine (`tamale ls`), starts new ones in a folder you pick, and opens
//! one by asking it to serve its web page (`tamale web <id>`): the same Dioxus app the browser
//! runs, built for WebAssembly, in the system's webview. So everything works as in a browser
//! tab (the code view, artifacts, end-to-end encryption) and the sessions are tamale's own:
//! closing the window leaves them running, and `tamale attach` picks them up in a terminal.
//!
//! An address opens it at that page instead, a hub's or one `/web` printed:
//!
//! ```text
//! tamale-web https://tamale-hub.com
//! ```
//!
//! Links and tabs the page opens go to the system's browser; downloads to ~/Downloads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use dioxus::desktop::LogicalSize;
use dioxus::desktop::muda::accelerator::Accelerator;
use dioxus::desktop::muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use dioxus::desktop::tao::window::WindowBuilder;
use dioxus::desktop::wry::{NewWindowResponse, WebViewBuilder};
use serde_json::{Value, json};

/// The list of sessions, the app's first page. `__NONCE__` becomes a secret of this run that
/// its messages carry: the session pages can't know it, so they can't start or end sessions.
const HOME: &str = include_str!("home.html");

/// The address to open: the first argument, or `TAMALE_WEB_URL`.
fn address() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .or_else(|| std::env::var("TAMALE_WEB_URL").ok())
        .filter(|a| a.starts_with("http://") || a.starts_with("https://"))
}

/// Opens `url` in the system's browser.
fn open_outside(url: &str) {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("cmd", vec!["/c", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    let _ = std::process::Command::new(program).args(args).spawn();
}

/// Whether the app runs from a macOS app bundle, whose Info.plist can say what it wants the
/// microphone for. A bare executable that touches the microphone is stopped by macOS.
fn bundled() -> bool {
    !cfg!(target_os = "macos")
        || std::env::current_exe()
            .is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

/// What the page is told before it starts: that this is the desktop app, and, without a
/// bundle to ask for the microphone, that there's none (the voice card then takes typing).
fn startup_script() -> String {
    let mut js = String::from("window.tamaleDesktop = true;");
    if !bundled() {
        js.push_str(
            "try { if (window.MediaDevices) delete MediaDevices.prototype.getUserMedia; } catch {}",
        );
    }
    js
}

fn downloads() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join("Downloads"))
}

/// A free name for `file` in `dir`.
fn free_path(dir: &std::path::Path, file: &str) -> PathBuf {
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let mut path = dir.join(file);
    let mut n = 1;
    while path.exists() {
        path = dir.join(if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        });
        n += 1;
    }
    path
}

/// The menu item that goes back to the list of sessions.
const SESSIONS: &str = "sessions";

/// The app's menu: without an Edit menu, macOS gives a webview no copy and paste keys.
fn menu() -> Menu {
    let menu = Menu::new();
    let app = Submenu::with_items(
        "tamale",
        true,
        &[
            &PredefinedMenuItem::about(None, None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ],
    );
    let file = Submenu::with_items(
        "File",
        true,
        &[&MenuItem::with_id(
            SESSIONS,
            "All Sessions",
            true,
            "CmdOrCtrl+O".parse::<Accelerator>().ok(),
        )],
    );
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    );
    let window = Submenu::with_items(
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::fullscreen(None),
            &PredefinedMenuItem::close_window(None),
        ],
    );
    for sub in [app, file, edit, window].into_iter().flatten() {
        let _ = menu.append(&sub);
    }
    menu
}

/// What `--check` reports: what the page shows, once it has had time to.
const CHECK: &str = r#"(() => {
  const q = (s) => document.querySelector(s);
  const review = document.getElementById('review-frame');
  return JSON.stringify({
    url: location.origin,
    conn: q('#conn')?.className ?? null,
    thread: (q('#thread')?.innerText ?? '').slice(0, 120),
    artifact: q('.artifact iframe')?.style.height ?? null,
    review: q('#review')?.className ?? null,
    pullspace: (review?.contentDocument?.body?.innerText ?? '').slice(0, 80),
    voice: !!navigator.mediaDevices?.getUserMedia,
    // Where the main parts are drawn, and in what colours: a page that renders lays them out.
    layout: Object.fromEntries(['.topbar', '.chat', '#review', '.composer', '.artifact', '#review-frame'].map((s) => {
      const e = q(s);
      if (!e) return [s, null];
      const r = e.getBoundingClientRect();
      const st = getComputedStyle(e);
      return [s, { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height), bg: st.backgroundColor, fg: st.color, visible: st.visibility !== 'hidden' && st.display !== 'none' && +st.opacity > 0 }];
    })),
    window: [innerWidth, innerHeight],
    stylesheet: [...document.styleSheets].some((x) => (x.href || '').endsWith('/app.css') && x.cssRules.length > 100),
    app: typeof window.__dx_mainWasm,
    home: (q('#sessions')?.innerText ?? null)?.slice(0, 400) ?? null,
    problem: q('#problem')?.style.display === 'block' ? q('#problem').innerText : null,
  });
})()"#;

#[derive(Debug)]
enum UserEvent {
    /// The next step of `--check`.
    Step,
    /// Back to the list of sessions.
    Home,
    /// Open this page.
    Load(String),
    /// Run this in the list of sessions, if it's still showing.
    HomeScript(String),
    Title(String),
}

// ---------------------------------------------------------------- tamale itself

/// The `tamale` to run: `TAMALE_BIN`, else the first on the search path or in the usual
/// places to install it (an app opened from the Finder gets a short search path).
fn tamale_bin() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("TAMALE_BIN") {
        return Some(bin.into());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = shell_env()
        .iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            home.iter()
                .flat_map(|h| [h.join(".cargo/bin"), h.join(".local/bin")]),
        )
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .map(|dir| dir.join("tamale"))
        .find(|bin| bin.is_file())
}

/// The environment of the user's login shell. An app opened from the Finder doesn't get it,
/// and the sessions it starts need it: the search path that finds `claude` and the like, and
/// keys in the environment. Empty when started from a terminal, which already has it.
fn shell_env() -> &'static [(String, String)] {
    static ENV: OnceLock<Vec<(String, String)>> = OnceLock::new();
    ENV.get_or_init(|| {
        if std::env::var_os("TERM").is_some() {
            return Vec::new();
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        const MARK: &str = "__TAMALE_ENV__";
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = Command::new(shell)
                .args(["-l", "-i", "-c", &format!("printf {MARK}; env -0")])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            let _ = tx.send(out);
        });
        let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(10)) else {
            return Vec::new();
        };
        let out = String::from_utf8_lossy(&out.stdout);
        let Some((_, vars)) = out.split_once(MARK) else {
            return Vec::new();
        };
        vars.split('\0')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(k, _)| !k.is_empty() && !matches!(*k, "_" | "SHLVL" | "PWD" | "OLDPWD"))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    })
}

/// Runs `tamale args…` (in `dir`) for what it prints, or what went wrong.
fn tamale(args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let bin = tamale_bin().ok_or(
        "Couldn't find tamale. Install it (cargo install --path . in its repository), or \
         set TAMALE_BIN to where it is.",
    )?;
    let mut command = Command::new(&bin);
    command
        .args(args)
        .envs(shell_env().iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let out = command
        .output()
        .map_err(|e| format!("Couldn't run {}: {e}", bin.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(err.strip_prefix("Error: ").unwrap_or(err).to_string())
    }
}

/// The folder a person typed, with `~` for their home.
fn folder(typed: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = match (typed.strip_prefix('~'), &home) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(typed),
    };
    if path.is_dir() {
        Ok(path)
    } else {
        Err(format!("There's no folder {typed}."))
    }
}

/// Asks for a folder with the system's dialog.
fn choose_folder() -> Option<String> {
    let out = if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args([
                "-e",
                "POSIX path of (choose folder with prompt \"Folder for the new session\")",
            ])
            .output()
    } else {
        Command::new("zenity")
            .args(["--file-selection", "--directory"])
            .output()
    }
    .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let path = path.trim_end_matches('/');
    (out.status.success() && !path.is_empty()).then(|| path.to_string())
}

/// A call of the list page's script.
fn call(function: &str, args: &[Value]) -> UserEvent {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    UserEvent::HomeScript(format!("tamaleHome.{function}({})", args.join(",")))
}

/// What the list page asked for, done away from the window's thread: tamale can take a
/// moment to answer.
fn on_home_message(message: &Value, proxy: &EventLoopProxy<UserEvent>) {
    let proxy = proxy.clone();
    let message = message.clone();
    std::thread::spawn(move || {
        let id = message["id"].as_u64().unwrap_or_default().to_string();
        let event = match message["do"].as_str().unwrap_or_default() {
            "list" => list(),
            "open" => match tamale(&["web", &id], None) {
                Ok(url) => UserEvent::Load(url),
                Err(why) => call("failed", &[json!(message["id"]), json!(why)]),
            },
            "end" => match tamale(&["kill", &id], None) {
                Ok(_) => list(),
                Err(why) => call("failed", &[json!(message["id"]), json!(why)]),
            },
            "new" => {
                let started = folder(message["folder"].as_str().unwrap_or_default())
                    .and_then(|dir| tamale(&["web", "new"], Some(&dir)));
                match started {
                    Ok(url) => UserEvent::Load(url),
                    Err(why) => call("problem", &[json!(why)]),
                }
            }
            "choose" => match choose_folder() {
                Some(path) => call("chosen", &[json!(path)]),
                None => return,
            },
            "url" => match message["url"].as_str() {
                Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                    UserEvent::Load(url.to_string())
                }
                _ => return,
            },
            _ => return,
        };
        let _ = proxy.send_event(event);
    });
}

/// The running sessions, for the list page.
fn list() -> UserEvent {
    match tamale(&["ls", "--json"], None) {
        Ok(out) => {
            let sessions: Value = serde_json::from_str(&out).unwrap_or(json!([]));
            let home = std::env::var("HOME").unwrap_or_default();
            call("render", &[sessions, json!(home)])
        }
        Err(why) => call("problem", &[json!(why)]),
    }
}

/// A secret for this run's list page (see [`HOME`]).
fn nonce() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- the window

/// Runs the app. `--check` prints what the window shows after a while and quits: a way to
/// see from a script that the app works. With `--check-open=<id>` it opens that session from
/// the list first, and on a session's page it opens the code view; then it goes back to the
/// list and reports again.
pub fn run() {
    let check = std::env::args().any(|a| a == "--check");
    let check_open =
        std::env::args().find_map(|a| a.strip_prefix("--check-open=").map(String::from));
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let menu = menu();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();
    let to_home = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id() == SESSIONS {
            let _ = to_home.send_event(UserEvent::Home);
        }
    }));
    let window = WindowBuilder::new()
        .with_title("tamale")
        .with_inner_size(LogicalSize::new(1280.0, 860.0))
        .build(&event_loop)
        .expect("a window");

    let nonce = nonce();
    let home = HOME.replace("__NONCE__", &nonce);
    let ipc_proxy = proxy.clone();
    let title_proxy = proxy.clone();
    let builder = WebViewBuilder::new()
        .with_initialization_script(startup_script())
        .with_devtools(cfg!(debug_assertions))
        .with_background_color((23, 24, 29, 255))
        // From the list page, which signs what it asks with the nonce; any page may only go
        // back to the list (the session page's button).
        .with_ipc_handler(move |request| {
            let Ok(message) = serde_json::from_str::<Value>(request.body()) else {
                return;
            };
            if message["do"] == "home" {
                let _ = ipc_proxy.send_event(UserEvent::Home);
            } else if message["nonce"].as_str() == Some(nonce.as_str()) {
                on_home_message(&message, &ipc_proxy);
            }
        })
        .with_document_title_changed_handler(move |title| {
            let _ = title_proxy.send_event(UserEvent::Title(title));
        })
        // Tabs the page opens (artifacts, links) open in the system's browser.
        .with_new_window_req_handler(|url, _| {
            open_outside(&url);
            NewWindowResponse::Deny
        })
        // Links that would leave the session's own pages, likewise.
        .with_navigation_handler(move |url| {
            let ours = url.starts_with("http://127.0.0.1")
                || url.starts_with("http://localhost")
                || url.contains("tamale-hub")
                || url.starts_with("about:")
                || url.starts_with("blob:")
                || url.starts_with("data:")
                || address().is_some_and(|a| same_site(&a, &url));
            if !ours {
                open_outside(&url);
            }
            ours
        })
        .with_download_started_handler(|url, path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| url.rsplit('/').next().unwrap_or("download").to_string());
            match downloads() {
                Some(dir) => {
                    *path = free_path(&dir, &name);
                    true
                }
                None => false,
            }
        });
    let mut on_home = address().is_none();
    let builder = match address() {
        Some(url) => builder.with_url(url),
        None => builder.with_html(&home),
    };
    let webview = builder.build(&window).expect("a webview");

    // `--check`: after the page has loaded, open the session or the code view; later,
    // report and quit.
    let later = move |secs: u64| {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            let _ = proxy.send_event(UserEvent::Step);
        });
    };
    if check {
        later(5);
    }
    let mut stage = 0;
    let mut opening = false;
    let mut returned = false;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Step) => {
                stage += 1;
                match stage {
                    1 => {
                        let step = match (&check_open, on_home) {
                            (Some(id), true) => {
                                opening = true;
                                format!("document.querySelector('.row[data-id=\"{id}\"]')?.click()")
                            }
                            _ => "document.getElementById('view-split')?.click()".into(),
                        };
                        let _ = webview.evaluate_script(&step);
                        later(10);
                    }
                    // Opened from the list: now the code view, then the report.
                    2 if opening => {
                        opening = false;
                        stage = 1;
                        let _ = webview
                            .evaluate_script("document.getElementById('view-split')?.click()");
                        later(8);
                    }
                    2 => {
                        let done = later.clone();
                        let _ = webview.evaluate_script_with_callback(CHECK, move |result| {
                            println!("CHECK {result}");
                            done(0);
                        });
                    }
                    // Opened from the list: back to it with the page's own button.
                    3 if check_open.is_some() && !returned => {
                        returned = true;
                        let _ = webview
                            .evaluate_script("document.getElementById('to-sessions')?.click()");
                        stage = 1;
                        later(4);
                    }
                    _ => *flow = ControlFlow::Exit,
                }
            }
            Event::UserEvent(UserEvent::Home) => {
                if !on_home {
                    on_home = true;
                    let _ = webview.load_html(&home);
                    window.set_title("tamale");
                }
            }
            Event::UserEvent(UserEvent::Load(url)) => {
                on_home = false;
                let _ = webview.load_url(&url);
            }
            Event::UserEvent(UserEvent::HomeScript(script)) => {
                if on_home {
                    let _ = webview.evaluate_script(&script);
                }
            }
            Event::UserEvent(UserEvent::Title(title)) => {
                window.set_title(if title.is_empty() { "tamale" } else { &title });
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *flow = ControlFlow::Exit,
            _ => {}
        }
    });
}

/// Whether two addresses are of the same site: the session's own, or its hub's subdomains.
fn same_site(a: &str, b: &str) -> bool {
    let host = |u: &str| {
        u.split("://")
            .nth(1)
            .and_then(|r| r.split(['/', '?', '#']).next())
            .map(|h| h.split(':').next().unwrap_or(h).to_string())
    };
    match (host(a), host(b)) {
        (Some(a), Some(b)) => {
            a == b || b.ends_with(&format!(".{a}")) || a.ends_with(&format!(".{b}"))
        }
        _ => false,
    }
}
