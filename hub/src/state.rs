//! The hub's only persistent state: token pairs, as SHA-256 hashes, in one `state.json`.
//!
//! The CLI edits it under an advisory lock and replaces it atomically (temp file +
//! rename); `serve` notices the new mtime and reloads. The file also holds a random
//! secret that signs session cookies, so they survive restarts.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pair {
    pub name: String,
    pub agent_hash: String,
    pub view_hash: String,
    pub created: u64,
    #[serde(default)]
    pub last_used: Option<u64>,
    /// Made from the hub's page rather than the CLI; such pairs expire when unused.
    #[serde(default)]
    pub web: bool,
}

impl Pair {
    /// A pair for `agent` and `view`, which only keeps their hashes.
    pub fn new(name: &str, agent: &str, view: &str, web: bool) -> Self {
        Self {
            name: name.to_string(),
            agent_hash: hash(agent),
            view_hash: hash(view),
            created: now(),
            last_used: None,
            web,
        }
    }
}

/// A name for a new pair: `label` cleaned up to lowercase letters, digits and dashes, or
/// two random words and a number; made unique among `taken`.
pub fn pair_name(label: Option<&str>, taken: &[Pair]) -> Result<String> {
    const FIRST: [&str; 16] = [
        "amber", "brisk", "calm", "dusty", "eager", "fuzzy", "gold", "hazy", "ivory", "jolly",
        "keen", "lucky", "mellow", "nimble", "olive", "plucky",
    ];
    const SECOND: [&str; 16] = [
        "otter", "heron", "maple", "comet", "pepper", "falcon", "cactus", "walrus", "lantern",
        "pebble", "badger", "quail", "tamale", "salsa", "mango", "cedar",
    ];
    let cleaned: String = label
        .unwrap_or("")
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base: String = cleaned.chars().take(32).collect();
    let base = base.trim_end_matches('-').to_string();
    let free = |name: &str| !taken.iter().any(|p| p.name == name);
    if !base.is_empty() {
        if free(&base) {
            return Ok(base);
        }
        if let Some(name) = (2..100).map(|n| format!("{base}-{n}")).find(|n| free(n)) {
            return Ok(name);
        }
    }
    for _ in 0..100 {
        let [a, b, n] = random::<3>()?;
        let name = format!(
            "{}-{}-{}",
            FIRST[usize::from(a) % 16],
            SECOND[usize::from(b) % 16],
            10 + u32::from(n) % 90
        );
        if free(&name) {
            return Ok(name);
        }
    }
    bail!("couldn't find a free name")
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct State {
    #[serde(default)]
    pub pairs: Vec<Pair>,
    /// Hex; signs session cookies. Made on first use.
    #[serde(default)]
    pub secret: String,
}

impl State {
    /// The pair whose agent half is `token`.
    pub fn by_agent(&self, token: &str) -> Option<&Pair> {
        let hash = hash(token);
        self.pairs.iter().find(|p| ct_eq(&p.agent_hash, &hash))
    }

    /// The pair whose view half is `token`.
    pub fn by_view(&self, token: &str) -> Option<&Pair> {
        let hash = hash(token);
        self.pairs.iter().find(|p| ct_eq(&p.view_hash, &hash))
    }

    pub fn by_name(&self, name: &str) -> Option<&Pair> {
        self.pairs.iter().find(|p| p.name == name)
    }
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    /// `--data-dir`, else `$TAMALE_HUB_DATA`, else `~/.local/share/tamale-hub`.
    pub fn open(dir: Option<PathBuf>) -> Result<Self> {
        let dir = match dir.or_else(|| std::env::var_os("TAMALE_HUB_DATA").map(PathBuf::from)) {
            Some(dir) => dir,
            None => {
                let home = std::env::var_os("HOME").context("HOME is not set; pass --data-dir")?;
                Path::new(&home).join(".local/share/tamale-hub")
            }
        };
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        Ok(Self { dir })
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn mtime(&self) -> Option<SystemTime> {
        fs::metadata(self.path()).and_then(|m| m.modified()).ok()
    }

    pub fn load(&self) -> Result<State> {
        match fs::read_to_string(self.path()) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", self.path().display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path().display())),
        }
    }

    /// Reads the state, lets `f` change it and writes it back, all under the lock, so
    /// concurrent edits (two CLIs, or the CLI and `serve`) don't lose each other's.
    pub fn edit<T>(&self, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        if state.secret.is_empty() {
            state.secret = hex(&random::<32>()?);
        }
        let out = f(&mut state)?;
        self.write(&state)?;
        Ok(out)
    }

    fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.dir.join("state.lock"))
            .context("opening the state lock")?;
        // SAFETY: flock on a file descriptor we own; released when the file closes.
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
            bail!("couldn't lock {}", self.dir.display());
        }
        Ok(file)
    }

    fn write(&self, state: &State) -> Result<()> {
        let tmp = self.dir.join(format!("state.json.{}", std::process::id()));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, self.path()).context("replacing state.json")?;
        Ok(())
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(bytes)
}

/// A new token: `prefix` and 32 random bytes in lowercase base32.
pub fn new_token(prefix: &str) -> Result<String> {
    Ok(format!("{prefix}{}", base32(&random::<32>()?)))
}

pub fn hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Compares in time that depends only on the lengths.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// HMAC-SHA256 of `message` under `key`, as hex.
pub fn hmac(key: &[u8], message: &[u8]) -> String {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    hex(&Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Store {
        let dir =
            std::env::temp_dir().join(format!("tamale-hub-state-{}", hex(&random::<8>().unwrap())));
        Store::open(Some(dir)).unwrap()
    }

    #[test]
    fn tokens_are_prefixed_base32_and_only_hashes_are_stored() {
        let store = temp_store();
        let agent = new_token("tha_").unwrap();
        let view = new_token("thv_").unwrap();
        assert_eq!(agent.len(), 4 + 52);
        assert!(
            agent[4..]
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
        );
        store
            .edit(|s| {
                s.pairs.push(Pair::new("mine", &agent, &view, false));
                Ok(())
            })
            .unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains(&agent[4..]) && !text.contains(&view[4..]));
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let state = store.load().unwrap();
        assert_eq!(state.by_agent(&agent).unwrap().name, "mine");
        assert_eq!(state.by_view(&view).unwrap().name, "mine");
        assert!(state.by_agent(&view).is_none() && state.by_view(&agent).is_none());
        assert_eq!(state.secret.len(), 64);
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn concurrent_edits_keep_every_pair() {
        let store = std::sync::Arc::new(temp_store());
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .edit(|s| {
                            s.pairs.push(Pair::new(&format!("p{i}"), "a", "v", false));
                            Ok(())
                        })
                        .unwrap()
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(store.load().unwrap().pairs.len(), 8);
        let _ = fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn pair_names_are_cleaned_and_unique() {
        let taken = vec![Pair::new("laptop", "a", "v", false)];
        assert_eq!(pair_name(Some("My VPSs!"), &taken).unwrap(), "my-vpss");
        assert_eq!(pair_name(Some("Laptop"), &taken).unwrap(), "laptop-2");
        let random = pair_name(Some("  ***  "), &taken).unwrap();
        assert_eq!(random.split('-').count(), 3, "{random}");
        assert!(pair_name(Some(&"x".repeat(80)), &taken).unwrap().len() <= 32);
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hmac(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(ct_eq("abc", "abc") && !ct_eq("abc", "abd") && !ct_eq("ab", "abc"));
    }
}
