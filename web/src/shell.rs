//! The desktop app: a window of its own that runs the same Dioxus app the browser does, built
//! for WebAssembly and served by tamale (or a tamale-hub), in the system's webview. Being the
//! same app from the same address, everything works as it does in a browser tab: the code
//! view, artifacts, end-to-end encryption, the hub's session list.
//!
//! Start it with the address `/web` prints, a hub's address, or nothing to be asked:
//!
//! ```text
//! tamale-web http://127.0.0.1:…/?t=…
//! tamale-web https://tamale-hub.com
//! ```
//!
//! Links and tabs the page opens go to the system's browser; downloads to ~/Downloads.

use std::path::PathBuf;

use dioxus::desktop::LogicalSize;
use dioxus::desktop::muda::{Menu, PredefinedMenuItem, Submenu};
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::tao::event_loop::{ControlFlow, EventLoopBuilder};
use dioxus::desktop::tao::window::WindowBuilder;
use dioxus::desktop::wry::{NewWindowResponse, WebViewBuilder};

/// Shown when the app is started without an address.
const ASK: &str = r#"<!doctype html><meta charset="utf-8"><meta name="color-scheme" content="dark">
<title>tamale</title>
<style>
body { font: 15px/1.5 -apple-system, system-ui, sans-serif; background: #17181d; color: #d7dae0; display: grid; place-items: center; height: 100vh; margin: 0; }
form { width: min(520px, 90vw); } h1 { font-size: 20px; margin: 0 0 6px; } p { opacity: .75; margin: 0 0 14px; }
input { width: 100%; box-sizing: border-box; padding: 10px 12px; border-radius: 10px; border: 1px solid #3a3d47; background: #1e2026; color: inherit; font: inherit; }
</style>
<form onsubmit="event.preventDefault(); const v = this.u.value.trim(); if (/^https?:\/\//.test(v)) location.href = v;">
<h1>🫔 Open a tamale session</h1>
<p>Paste the address <code>/web</code> prints in tamale, or your hub's address.</p>
<input name="u" autofocus placeholder="http://127.0.0.1:…/?t=…">
</form>"#;

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
    for sub in [app, edit, window].into_iter().flatten() {
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
  });
})()"#;

#[derive(Debug)]
enum UserEvent {
    /// The next step of `--check`.
    Step,
}

/// Runs the app. `--check` opens the code view, prints what the window shows and quits: a
/// way to see from a script that the app works.
pub fn run() {
    let check = std::env::args().any(|a| a == "--check");
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let menu = menu();
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();
    let window = WindowBuilder::new()
        .with_title("tamale")
        .with_inner_size(LogicalSize::new(1280.0, 860.0))
        .build(&event_loop)
        .expect("a window");

    let builder = WebViewBuilder::new()
        .with_initialization_script(startup_script())
        .with_devtools(cfg!(debug_assertions))
        .with_background_color((23, 24, 29, 255))
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
    let builder = match address() {
        Some(url) => builder.with_url(url),
        None => builder.with_html(ASK),
    };
    let webview = builder.build(&window).expect("a webview");

    // `--check`: after the page has loaded, open the code view; later, report and quit.
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
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Step) => {
                stage += 1;
                match stage {
                    1 => {
                        let _ = webview
                            .evaluate_script("document.getElementById('view-split')?.click()");
                        later(10);
                    }
                    2 => {
                        let done = later.clone();
                        let _ = webview.evaluate_script_with_callback(CHECK, move |result| {
                            println!("CHECK {result}");
                            done(0);
                        });
                    }
                    _ => *flow = ControlFlow::Exit,
                }
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
