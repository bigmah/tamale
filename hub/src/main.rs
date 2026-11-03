//! tamale-hub: a self-hosted relay that makes tamale sessions reachable from a phone or
//! another computer. tamales connect out to it with a key; browsers holding the same key
//! list and drive them. See single_key.md and tunnel_plan.md in the tamale repo.

mod serve;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::state::{Key, Store};

const USAGE: &str = "\
Usage:
  tamale-hub serve --public-url https://tamale-hub.com [--listen 127.0.0.1:8787] [--data-dir DIR]
                   [--signup open|closed] [--max-keys N] [--insecure]
                   [--apple-app-id TEAMID.bundle.id]
  tamale-hub key new <name> [--public-url URL]   make a key; prints it once
  tamale-hub key list
  tamale-hub key revoke <name>                   drop the key and every socket using it

--data-dir defaults to $TAMALE_HUB_DATA, else ~/.local/share/tamale-hub.
--signup open lets anyone make a key on the hub's page (default closed: only this CLI
makes them); --max-keys (default 500) caps how many the hub holds.
--public-url defaults to $TAMALE_HUB_PUBLIC_URL for the key commands' links.
--apple-app-id (or $TAMALE_HUB_APPLE_APP_ID) serves apple-app-site-association, so the
iOS app opens the terminal's /open/<sid> links.";

fn main() {
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("tamale-hub: {e:#}");
        std::process::exit(1);
    }
}

struct Args {
    positional: Vec<String>,
    listen: Option<String>,
    public_url: Option<String>,
    data_dir: Option<PathBuf>,
    insecure: bool,
    signup: bool,
    max_keys: usize,
    apple_app_id: Option<String>,
}

fn parse(args: Vec<String>) -> Result<Args> {
    let mut out = Args {
        positional: Vec::new(),
        listen: None,
        public_url: None,
        data_dir: None,
        insecure: false,
        signup: false,
        max_keys: 500,
        apple_app_id: None,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--listen" => out.listen = Some(value("--listen")?),
            "--public-url" => out.public_url = Some(value("--public-url")?),
            "--data-dir" => out.data_dir = Some(value("--data-dir")?.into()),
            "--insecure" => out.insecure = true,
            "--apple-app-id" => out.apple_app_id = Some(value("--apple-app-id")?),
            "--signup" => {
                out.signup = match value("--signup")?.as_str() {
                    "open" => true,
                    "closed" => false,
                    other => bail!("--signup is open or closed, not {other}"),
                }
            }
            "--max-keys" => {
                out.max_keys = value("--max-keys")?
                    .parse()
                    .context("--max-keys must be a number")?
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            flag if flag.starts_with("--") => bail!("unknown option {flag}\n\n{USAGE}"),
            _ => out.positional.push(arg),
        }
    }
    Ok(out)
}

fn run(args: Vec<String>) -> Result<()> {
    let args = parse(args)?;
    let words: Vec<&str> = args.positional.iter().map(String::as_str).collect();
    let public_url = args
        .public_url
        .clone()
        .or_else(|| std::env::var("TAMALE_HUB_PUBLIC_URL").ok());
    match words.as_slice() {
        ["serve"] => {
            let listen: SocketAddr = args
                .listen
                .as_deref()
                .unwrap_or("127.0.0.1:8787")
                .parse()
                .context("--listen must be an address like 127.0.0.1:8787")?;
            let public_url =
                public_url.context("serve needs --public-url, e.g. https://tamale-hub.com")?;
            let store = Store::open(args.data_dir)?;
            tokio::runtime::Runtime::new()?.block_on(serve::serve(
                serve::Options {
                    listen,
                    public_url,
                    insecure: args.insecure,
                    signup: serve::Signup {
                        open: args.signup,
                        max_keys: args.max_keys,
                    },
                    apple_app_id: args
                        .apple_app_id
                        .clone()
                        .or_else(|| std::env::var("TAMALE_HUB_APPLE_APP_ID").ok())
                        .filter(|id| !id.is_empty()),
                },
                store,
            ))
        }
        ["key", "new", name] => {
            let store = Store::open(args.data_dir)?;
            let key = state::new_key()?;
            let bytes = tamale_hub_protocol::decode_key(&key).context("a key we just made")?;
            let auth = tamale_hub_protocol::auth_value(&bytes);
            store.edit(|s| {
                if s.by_name(name).is_some() {
                    bail!("a key named {name} already exists (revoke it first)");
                }
                s.keys.push(Key::new(name, &auth, false));
                Ok(())
            })?;
            println!("Made key {name}. It isn't shown again.\n");
            print_key(&key, public_url.as_deref());
            Ok(())
        }
        ["key", "list"] => {
            let state = Store::open(args.data_dir)?.load()?;
            if state.keys.is_empty() {
                println!("No keys. Make one with: tamale-hub key new <name>");
            }
            for key in &state.keys {
                let used = key.last_used.map_or("never used".to_string(), |t| {
                    format!("last used {}", ago(t))
                });
                println!("{:<20} made {}, {used}", key.name, ago(key.created));
            }
            Ok(())
        }
        ["key", "revoke", name] => {
            Store::open(args.data_dir)?.edit(|s| {
                let before = s.keys.len();
                s.keys.retain(|k| k.name != *name);
                if s.keys.len() == before {
                    bail!("no key named {name}");
                }
                Ok(())
            })?;
            println!("Revoked {name}. A running hub drops its sessions and tabs within seconds.");
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

fn print_key(key: &str, public_url: Option<&str>) {
    let base = public_url
        .unwrap_or("https://tamale-hub.com")
        .trim_end_matches('/');
    println!("It gives access to every machine that uses it: treat it like an SSH key.\n");
    println!("On each machine that runs tamale, then /remote in tamale:");
    if base != "https://tamale-hub.com" {
        let agent = base
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        println!("  export TAMALE_HUB_URL={agent}");
    }
    println!("  export TAMALE_HUB_KEY={key}\n");
    let link = format!("{base}/#{key}");
    println!("On your phone and browsers, scan this rather than sending it:\n");
    println!("  {link}\n");
    print_qr(&link);
}

fn print_qr(text: &str) {
    let Ok(code) = qrcode::QrCode::new(text) else {
        return;
    };
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2;
    let dark = |x: usize, y: usize| {
        x >= quiet
            && y >= quiet
            && x - quiet < width
            && y - quiet < width
            && colors[(y - quiet) * width + x - quiet] == qrcode::Color::Dark
    };
    let size = width + 2 * quiet;
    for y in (0..size).step_by(2) {
        let row: String = (0..size)
            .map(|x| match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            })
            .collect();
        // Dark on light, so it scans on any terminal theme.
        println!("  \x1b[30;47m{row}\x1b[0m");
    }
    println!();
}

fn ago(t: u64) -> String {
    let secs = state::now().saturating_sub(t);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}
