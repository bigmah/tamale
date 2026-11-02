//! tamale-hub: a self-hosted relay that makes tamale sessions reachable from a phone or
//! another computer. tamales connect out to it with an agent token; browsers holding the
//! matching view token list and drive them. See tunnel_plan.md in the tamale repo.

mod serve;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::state::{Pair, Store};

const USAGE: &str = "\
Usage:
  tamale-hub serve --public-url https://tamale-hub.com [--listen 127.0.0.1:8787] [--data-dir DIR]
                   [--signup open|closed] [--max-pairs N] [--insecure]
  tamale-hub token new <name> [--public-url URL]   make a token pair; prints both halves once
  tamale-hub token list
  tamale-hub token revoke <name>                   drop the pair and every socket using it
  tamale-hub token rotate <name> --agent|--view [--public-url URL]

--data-dir defaults to $TAMALE_HUB_DATA, else ~/.local/share/tamale-hub.
--signup open lets anyone make a token pair on the hub's page (default closed: only the
CLI makes them); --max-pairs (default 500) caps how many the hub holds.
--public-url defaults to $TAMALE_HUB_PUBLIC_URL for the token commands' links.";

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
    max_pairs: usize,
    agent: bool,
    view: bool,
}

fn parse(args: Vec<String>) -> Result<Args> {
    let mut out = Args {
        positional: Vec::new(),
        listen: None,
        public_url: None,
        data_dir: None,
        insecure: false,
        signup: false,
        max_pairs: 500,
        agent: false,
        view: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--listen" => out.listen = Some(value("--listen")?),
            "--public-url" => out.public_url = Some(value("--public-url")?),
            "--data-dir" => out.data_dir = Some(value("--data-dir")?.into()),
            "--insecure" => out.insecure = true,
            "--signup" => {
                out.signup = match value("--signup")?.as_str() {
                    "open" => true,
                    "closed" => false,
                    other => bail!("--signup is open or closed, not {other}"),
                }
            }
            "--max-pairs" => {
                out.max_pairs = value("--max-pairs")?
                    .parse()
                    .context("--max-pairs must be a number")?
            }
            "--agent" => out.agent = true,
            "--view" => out.view = true,
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
                        max_pairs: args.max_pairs,
                    },
                },
                store,
            ))
        }
        ["token", "new", name] => {
            let store = Store::open(args.data_dir)?;
            let agent = state::new_token("tha_")?;
            let view = state::new_token("thv_")?;
            store.edit(|s| {
                if s.by_name(name).is_some() {
                    bail!("a pair named {name} already exists (rotate or revoke it)");
                }
                s.pairs.push(Pair::new(name, &agent, &view, false));
                Ok(())
            })?;
            println!("Made token pair {name}. Neither half is shown again.\n");
            print_agent(&agent, public_url.as_deref());
            print_view(&view, public_url.as_deref());
            Ok(())
        }
        ["token", "list"] => {
            let state = Store::open(args.data_dir)?.load()?;
            if state.pairs.is_empty() {
                println!("No token pairs. Make one with: tamale-hub token new <name>");
            }
            for pair in &state.pairs {
                let used = pair.last_used.map_or("never used".to_string(), |t| {
                    format!("last used {}", ago(t))
                });
                println!("{:<20} made {}, {used}", pair.name, ago(pair.created));
            }
            Ok(())
        }
        ["token", "revoke", name] => {
            Store::open(args.data_dir)?.edit(|s| {
                let before = s.pairs.len();
                s.pairs.retain(|p| p.name != *name);
                if s.pairs.len() == before {
                    bail!("no pair named {name}");
                }
                Ok(())
            })?;
            println!("Revoked {name}. A running hub drops its sessions and tabs within seconds.");
            Ok(())
        }
        ["token", "rotate", name] => {
            if args.agent == args.view {
                bail!("rotate needs exactly one of --agent or --view");
            }
            let token = state::new_token(if args.agent { "tha_" } else { "thv_" })?;
            Store::open(args.data_dir)?.edit(|s| {
                let pair = s
                    .pairs
                    .iter_mut()
                    .find(|p| p.name == *name)
                    .with_context(|| format!("no pair named {name}"))?;
                if args.agent {
                    pair.agent_hash = state::hash(&token);
                } else {
                    pair.view_hash = state::hash(&token);
                }
                Ok(())
            })?;
            if args.agent {
                println!("New agent token for {name}; the old one stops working within seconds.\n");
                print_agent(&token, public_url.as_deref());
            } else {
                println!("New view token for {name}; browsers holding the old one lose access.\n");
                print_view(&token, public_url.as_deref());
            }
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

fn agent_url(public_url: Option<&str>) -> String {
    let base = public_url
        .unwrap_or("https://tamale-hub.com")
        .trim_end_matches('/');
    base.replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1)
}

fn print_agent(token: &str, public_url: Option<&str>) {
    println!("Agent token, for every machine that runs tamale (it can't read or drive anything):");
    println!("  export TAMALE_HUB_URL={}", agent_url(public_url));
    println!("  export TAMALE_HUB_TOKEN={token}");
    println!("Then /web remote in tamale, or [hub] connect = \"auto\" to connect on startup.\n");
}

fn print_view(token: &str, public_url: Option<&str>) {
    let base = public_url
        .unwrap_or("https://tamale-hub.com")
        .trim_end_matches('/');
    let link = format!("{base}/#{token}");
    println!("View link, for your phone and browsers. It controls every session of this pair:");
    println!("treat it like an SSH key, and scan it rather than sending it.\n");
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
