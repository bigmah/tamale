//! End-to-end encryption between tamale and a browser, across a tamale-hub that only
//! moves opaque bytes. The design and threat model are in `e2e.md`.
//!
//! A machine has one pairing key (PSK), which reaches browsers through a URL fragment and
//! never reaches the hub. From it come the machine id the hub groups sessions by and the
//! key that seals the session list's labels. Each browser channel then runs a Noise
//! NNpsk0 handshake for keys of its own; `src/web/noise.js` is the other end.

use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use sha2::Sha256;
use snow::{Builder, TransportState};

const NOISE: &str = "Noise_NNpsk0_25519_AESGCM_SHA256";
const PROLOGUE: &[u8] = b"tamale-hub e2e v1";
/// The largest Noise message there is; longer `/web` messages go in several.
const MAX_NOISE: usize = 65535;
const TAG: usize = 16;
/// Each transport message's plaintext is this flag, then a piece of a `/web` message.
/// It's inside the encryption, so a hub can't cut a message short or join two.
const MORE: u8 = 0;
const LAST: u8 = 1;
const MAX_PIECE: usize = MAX_NOISE - TAG - 1;

/// The file a machine's pairing key is kept in.
pub fn key_path() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("tamale").join("hub-pairing.key"))
}

/// A machine's pairing key and what's derived from it.
#[derive(Clone)]
pub struct Keys {
    psk: [u8; 32],
    meta: [u8; 32],
    /// The machine id: tells the hub and browsers which key a session uses, and reveals
    /// nothing about it.
    pub mid: String,
}

impl Keys {
    pub fn from_psk(psk: [u8; 32]) -> Self {
        let kdf = Hkdf::<Sha256>::new(None, &psk);
        let mut mid = [0u8; 16];
        let mut meta = [0u8; 32];
        // Both lengths are far below HKDF's limit.
        kdf.expand(b"tamale-hub mid", &mut mid)
            .expect("hkdf length");
        kdf.expand(b"tamale-hub meta", &mut meta)
            .expect("hkdf length");
        Self {
            psk,
            meta,
            mid: mid.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    /// The key in `path`, made first if there isn't one.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::create(path, false),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Replaces the key in `path`, so every browser has to pair again.
    pub fn rekey(path: &Path) -> Result<Self> {
        Self::create(path, true)
    }

    fn parse(text: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(text.trim())
            .context("not base64url")?;
        let psk = <[u8; 32]>::try_from(bytes).map_err(|_| anyhow!("not a 32-byte key"))?;
        Ok(Self::from_psk(psk))
    }

    /// Writes a new key to a private temporary file and moves it into place, so another
    /// tamale starting now finds either no key or a whole one.
    fn create(path: &Path, replace: bool) -> Result<Self> {
        let mut psk = [0u8; 32];
        getrandom::fill(&mut psk).map_err(|e| anyhow!("no randomness: {e}"))?;
        let dir = path.parent().context("the key file has no directory")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let temp = dir.join(format!("hub-pairing.key.{}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let result = options
            .open(&temp)
            .and_then(|mut file| file.write_all(URL_SAFE_NO_PAD.encode(psk).as_bytes()))
            .and_then(|()| {
                if replace {
                    std::fs::rename(&temp, path)
                } else {
                    std::fs::hard_link(&temp, path)
                }
            });
        let _ = std::fs::remove_file(&temp);
        match result {
            Ok(()) => Ok(Self::from_psk(psk)),
            // Another tamale got there first.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && !replace => {
                let text = std::fs::read_to_string(path)?;
                Self::parse(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) => Err(e).with_context(|| format!("writing {}", path.display())),
        }
    }

    /// What goes after the `#` of a link that pairs a browser.
    pub fn fragment(&self) -> String {
        format!("k={}", URL_SAFE_NO_PAD.encode(self.psk))
    }

    /// Seals a label for the session list: AES-256-GCM with a random nonce, bound to the
    /// session and to what kind of label it is. Base64 of nonce and ciphertext.
    pub fn seal(&self, sid: &str, kind: &str, plaintext: &str) -> Result<String> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| anyhow!("no randomness: {e}"))?;
        let cipher = Aes256Gcm::new_from_slice(&self.meta).map_err(|e| anyhow!("{e}"))?;
        let aad = format!("{sid}{kind}");
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| anyhow!("{e}"))?;
        Ok(STANDARD.encode([nonce.as_slice(), &sealed].concat()))
    }

    /// The handshake's first step for a responder: reads message 1 and writes message 2.
    /// Fails unless the sender holds this key and is answering `nonce` on session `sid`.
    pub fn respond(&self, sid: &str, nonce: &[u8], hs1: &str) -> Result<(Channel, String)> {
        let prologue = prologue(sid, nonce);
        let mut hs = builder(&self.psk, &prologue)?.build_responder()?;
        let message = decode(hs1)?;
        let mut payload = vec![0u8; MAX_NOISE];
        hs.read_message(&message, &mut payload)?;
        let mut out = vec![0u8; MAX_NOISE];
        let n = hs.write_message(br#"{"ok":true}"#, &mut out)?;
        Ok((
            Channel {
                noise: hs.into_transport_mode()?,
                partial: Vec::new(),
            },
            STANDARD.encode(&out[..n]),
        ))
    }
}

fn builder<'a>(psk: &'a [u8; 32], prologue: &'a [u8]) -> Result<Builder<'a>> {
    Ok(Builder::new(NOISE.parse()?)
        .psk(0, psk)?
        .prologue(prologue)?)
}

/// Binds a handshake to its session and to a nonce tamale made for that channel, so one
/// recorded earlier can't be replayed, here or on another session of this machine.
fn prologue(sid: &str, nonce: &[u8]) -> Vec<u8> {
    [PROLOGUE, sid.as_bytes(), nonce].concat()
}

fn decode(text: &str) -> Result<Vec<u8>> {
    let bytes = STANDARD.decode(text).context("not base64")?;
    if bytes.len() > MAX_NOISE {
        bail!("too long for a Noise message");
    }
    Ok(bytes)
}

/// One browser channel's keys, after its handshake. Noise's counters mean a message that
/// was dropped, repeated, reordered or reflected fails to decrypt; a failure is final.
pub struct Channel {
    noise: TransportState,
    partial: Vec<u8>,
}

impl Channel {
    /// Seals one `/web` message as one or more transport messages, in base64.
    pub fn seal(&mut self, message: &str) -> Result<Vec<String>> {
        let bytes = message.as_bytes();
        let mut pieces: Vec<&[u8]> = bytes.chunks(MAX_PIECE).collect();
        if pieces.is_empty() {
            pieces.push(&[]);
        }
        let count = pieces.len();
        let mut out = vec![0u8; MAX_NOISE];
        let mut sealed = Vec::with_capacity(count);
        for (i, piece) in pieces.into_iter().enumerate() {
            let flag = if i + 1 == count { LAST } else { MORE };
            let plain = [&[flag], piece].concat();
            let n = self.noise.write_message(&plain, &mut out)?;
            sealed.push(STANDARD.encode(&out[..n]));
        }
        Ok(sealed)
    }

    /// Opens one transport message. `None` until a message's last piece has arrived.
    /// `max` caps the whole message.
    pub fn open(&mut self, sealed: &str, max: usize) -> Result<Option<String>> {
        let message = decode(sealed)?;
        let mut out = vec![0u8; MAX_NOISE];
        let n = self.noise.read_message(&message, &mut out)?;
        let (&flag, piece) = out[..n].split_first().context("an empty message")?;
        if self.partial.len() + piece.len() > max {
            bail!("the message is too long");
        }
        self.partial.extend_from_slice(piece);
        match flag {
            MORE => Ok(None),
            LAST => Ok(Some(String::from_utf8(std::mem::take(&mut self.partial))?)),
            _ => bail!("a bad fragment flag"),
        }
    }
}

#[cfg(test)]
pub mod testing {
    //! The browser's side, for tests of tamale's. `noise.js` is the real one.
    use super::*;

    pub struct Initiator {
        hs: snow::HandshakeState,
    }

    impl Initiator {
        pub fn new(psk: &[u8; 32], sid: &str, nonce: &[u8], fixed: Option<&[u8; 32]>) -> Self {
            let prologue = prologue(sid, nonce);
            let mut builder = builder(psk, &prologue).unwrap();
            if let Some(key) = fixed {
                builder = builder.fixed_ephemeral_key_for_testing_only(key);
            }
            Self {
                hs: builder.build_initiator().unwrap(),
            }
        }

        pub fn hs1(&mut self, payload: &[u8]) -> String {
            let mut out = vec![0u8; MAX_NOISE];
            let n = self.hs.write_message(payload, &mut out).unwrap();
            STANDARD.encode(&out[..n])
        }

        pub fn finish(mut self, hs2: &str) -> Result<Channel> {
            let mut out = vec![0u8; MAX_NOISE];
            self.hs.read_message(&decode(hs2)?, &mut out)?;
            Ok(Channel {
                noise: self.hs.into_transport_mode()?,
                partial: Vec::new(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Initiator;
    use super::*;

    const SID: &str = "0123456789abcdef0123456789abcdef";
    const NONCE: &[u8; 16] = b"channel-nonce-16";
    const BIG: usize = 64 << 20;

    fn pair() -> (Channel, Channel) {
        let psk = [7u8; 32];
        let mut browser = Initiator::new(&psk, SID, NONCE, None);
        let hs1 = browser.hs1(b"{}");
        let (tamale, hs2) = Keys::from_psk(psk).respond(SID, NONCE, &hs1).unwrap();
        (browser.finish(&hs2).unwrap(), tamale)
    }

    fn flip(sealed: &str) -> String {
        let mut bytes = STANDARD.decode(sealed).unwrap();
        bytes[0] ^= 1;
        STANDARD.encode(bytes)
    }

    #[test]
    fn a_round_trip_both_ways() {
        let (mut browser, mut tamale) = pair();
        let up = browser.seal(r#"{"t":"send"}"#).unwrap();
        assert_eq!(up.len(), 1);
        assert_eq!(
            tamale.open(&up[0], BIG).unwrap().unwrap(),
            r#"{"t":"send"}"#
        );
        let down = tamale.seal("hello").unwrap();
        assert_eq!(browser.open(&down[0], BIG).unwrap().unwrap(), "hello");
        let empty = tamale.seal("").unwrap();
        assert_eq!(browser.open(&empty[0], BIG).unwrap().unwrap(), "");
    }

    #[test]
    fn long_messages_cross_in_pieces() {
        let (mut browser, mut tamale) = pair();
        let message = "é".repeat(MAX_PIECE); // two pieces, split inside a character
        let sealed = tamale.seal(&message).unwrap();
        assert_eq!(sealed.len(), 2);
        assert_eq!(browser.open(&sealed[0], BIG).unwrap(), None);
        assert_eq!(browser.open(&sealed[1], BIG).unwrap().unwrap(), message);

        // Exactly one piece's worth stays in one message.
        let sealed = tamale.seal(&"x".repeat(MAX_PIECE)).unwrap();
        assert_eq!(sealed.len(), 1);
        assert_eq!(
            browser.open(&sealed[0], BIG).unwrap().unwrap().len(),
            MAX_PIECE
        );

        // The receiver's cap holds across pieces.
        let sealed = tamale.seal(&"x".repeat(MAX_PIECE * 2)).unwrap();
        assert_eq!(browser.open(&sealed[0], MAX_PIECE + 1).unwrap(), None);
        assert!(browser.open(&sealed[1], MAX_PIECE + 1).is_err());
    }

    #[test]
    fn tampering_replay_reordering_and_reflection_fail() {
        let (mut browser, mut tamale) = pair();
        let one = browser.seal("one").unwrap().remove(0);
        assert!(tamale.open(&flip(&one), BIG).is_err(), "a flipped bit");

        let (mut browser, mut tamale) = pair();
        let _first = browser.seal("one").unwrap().remove(0);
        let second = browser.seal("two").unwrap().remove(0);
        assert!(tamale.open(&second, BIG).is_err(), "out of order");

        let (mut browser, mut tamale) = pair();
        let again = browser.seal("one").unwrap().remove(0);
        tamale.open(&again, BIG).unwrap();
        assert!(tamale.open(&again, BIG).is_err(), "a replay");

        let (mut browser, _tamale) = pair();
        let own = browser.seal("one").unwrap().remove(0);
        assert!(browser.open(&own, BIG).is_err(), "sent back to its sender");
    }

    #[test]
    fn a_handshake_needs_the_key_session_and_nonce() {
        let psk = [7u8; 32];
        let keys = Keys::from_psk(psk);
        let hs1 = Initiator::new(&psk, SID, NONCE, None).hs1(b"{}");
        assert!(keys.respond(SID, NONCE, &hs1).is_ok());
        assert!(
            keys.respond(SID, b"another-nonce-16", &hs1).is_err(),
            "replayed on another channel"
        );
        assert!(
            keys.respond(&"f".repeat(32), NONCE, &hs1).is_err(),
            "another session"
        );
        assert!(
            Keys::from_psk([8u8; 32]).respond(SID, NONCE, &hs1).is_err(),
            "another key"
        );
        assert!(keys.respond(SID, NONCE, &flip(&hs1)).is_err(), "tampered");
        assert!(keys.respond(SID, NONCE, "not base64!").is_err());

        // A tampered answer doesn't complete the browser's side.
        let mut browser = Initiator::new(&psk, SID, NONCE, None);
        let hs1 = browser.hs1(b"{}");
        let (_, hs2) = keys.respond(SID, NONCE, &hs1).unwrap();
        assert!(browser.finish(&flip(&hs2)).is_err());
    }

    #[test]
    fn the_key_file_is_private_stable_and_replaceable() {
        let dir =
            std::env::temp_dir().join(format!("tamale-e2e-test-{}", crate::session::new_id()));
        let path = dir.join("nested").join("hub-pairing.key");
        let made = Keys::load_or_create(&path).unwrap();
        let again = Keys::load_or_create(&path).unwrap();
        assert_eq!(made.mid, again.mid);
        assert_eq!(made.mid.len(), 32);
        assert_eq!(made.psk, again.psk);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temp left"
        );
        let new = Keys::rekey(&path).unwrap();
        assert_ne!(new.mid, made.mid);
        assert_eq!(Keys::load_or_create(&path).unwrap().mid, new.mid);

        std::fs::write(&path, "short").unwrap();
        assert!(
            Keys::load_or_create(&path).is_err(),
            "a damaged key is not replaced quietly"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sealed_labels_differ_each_time_and_hide_the_text() {
        let keys = Keys::from_psk([7u8; 32]);
        let a = keys.seal(SID, "label", "secret-cwd").unwrap();
        let b = keys.seal(SID, "label", "secret-cwd").unwrap();
        assert_ne!(a, b);
        assert!(!String::from_utf8_lossy(&STANDARD.decode(&a).unwrap()).contains("secret-cwd"));
    }

    /// `noise.js` and the hub page's `seal.js` under Node against this module, byte for
    /// byte. Skipped without Node 22+, unless `CI` is set.
    #[test]
    fn the_browser_code_agrees_with_this_code() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let version = std::process::Command::new("node").arg("--version").output();
        let major = version
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|v| {
                v.trim()
                    .trim_start_matches('v')
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            });
        if major.is_none_or(|m| m < 22) {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI needs Node 22 or later"
            );
            eprintln!("skipped: needs Node 22 or later");
            return;
        }

        let psk = [0x42u8; 32];
        let keys = Keys::from_psk(psk);
        let ephemeral = [0x11u8; 32];
        let mut browser = Initiator::new(&psk, SID, NONCE, Some(&ephemeral));
        let hs1 = browser.hs1(br#"{"v":1}"#);
        let (mut tamale, hs2) = keys.respond(SID, NONCE, &hs1).unwrap();
        let long = "λ".repeat(MAX_PIECE); // two pieces
        let down: Vec<Vec<String>> = ["hello from tamale", "", long.as_str()]
            .iter()
            .map(|m| tamale.seal(m).unwrap())
            .collect();
        let vectors = serde_json::json!({
            "psk": URL_SAFE_NO_PAD.encode(psk),
            "fragment": keys.fragment(),
            "mid": keys.mid,
            "sid": SID,
            "nonce": STANDARD.encode(NONCE),
            "ephemeral": STANDARD.encode(ephemeral),
            "hs1": hs1,
            "hs2": hs2,
            "down": down,
            "long": long,
            "label": keys.seal(SID, "label", r#"{"host":"h","cwd":"~/x"}"#).unwrap(),
        });
        let output = std::process::Command::new("node")
            .arg(root.join("src/web/noise.test.mjs"))
            .arg(vectors.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // What noise.js sealed, opened here.
        let up: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        let mut opened = Vec::new();
        for sealed in &up {
            if let Some(message) = tamale.open(sealed, BIG).unwrap() {
                opened.push(message);
            }
        }
        assert_eq!(
            opened,
            ["hello from the browser".to_string(), "".into(), long]
        );
    }
}
