//! End-to-end encryption between tamale and a browser, across a tamale-hub that only
//! moves opaque bytes. The design and threat model are in `e2e.md`.
//!
//! The handshake's secret (PSK) comes from the key every machine and browser share (see
//! `single_key.md`), and never reaches the hub. From it come the id browsers find their key
//! by and the key that seals the session list's labels. Each browser channel then runs a
//! Noise NNpsk0 handshake for keys of its own; `src/web/noise.js` is the other end.

use std::path::PathBuf;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
#[cfg(test)]
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
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

/// Deletes the pairing key tamale kept per machine before keys came from the hub's site.
/// Nothing uses it now, and it's still a secret some browsers trust until the hub upgrades.
pub fn remove_old_pairing_key() {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(data) = data {
        let _ = std::fs::remove_file(data.join("tamale").join("hub-pairing.key"));
    }
}

/// The handshake's secret and what's derived from it.
#[derive(Clone)]
pub struct Keys {
    psk: [u8; 32],
    meta: [u8; 32],
    /// Tells the hub and browsers which key a session uses, and reveals nothing about it.
    pub mid: String,
}

impl Keys {
    /// What the key `key` means for the encrypted channel.
    pub fn from_key(key: &[u8; 32]) -> Self {
        Self::from_psk(tamale_hub_protocol::e2e_secret(key))
    }

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

    /// Seals a label for the session list: AES-256-GCM with a random nonce, bound to the
    /// session and to what kind of label it is. Base64 of nonce and ciphertext.
    pub fn seal(&self, sid: &str, kind: &str, plaintext: &str) -> Result<String> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| anyhow!("no randomness: {e}"))?;
        self.seal_with(sid, kind, plaintext, nonce)
    }

    /// The meta key, for test vectors.
    #[cfg(test)]
    pub fn meta_key(&self) -> [u8; 32] {
        self.meta
    }

    fn seal_with(&self, sid: &str, kind: &str, plaintext: &str, nonce: [u8; 12]) -> Result<String> {
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

    /// Opens a label [`Keys::seal`] sealed for the same session and kind.
    #[cfg(unix)]
    pub fn open(&self, sid: &str, kind: &str, sealed: &str) -> Result<String> {
        let bytes = STANDARD.decode(sealed).context("not base64")?;
        if bytes.len() < 12 {
            bail!("too short");
        }
        let (nonce, sealed) = bytes.split_at(12);
        let cipher = Aes256Gcm::new_from_slice(&self.meta).map_err(|e| anyhow!("{e}"))?;
        let aad = format!("{sid}{kind}");
        let plain = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("sealed with another key"))?;
        Ok(String::from_utf8(plain)?)
    }

    /// The initiator's first step, answering the `n` of a session's `hs0`: message 1.
    #[cfg(unix)]
    pub fn initiate(&self, sid: &str, n: &str) -> Result<(Handshake, String)> {
        let nonce = STANDARD.decode(n).context("not base64")?;
        let prologue = prologue(sid, &nonce);
        let mut hs = builder(&self.psk, &prologue)?.build_initiator()?;
        let mut out = vec![0u8; MAX_NOISE];
        let n = hs.write_message(br#"{"v":1}"#, &mut out)?;
        Ok((Handshake { hs }, STANDARD.encode(&out[..n])))
    }

    /// The handshake's first step for a responder: reads message 1 and writes message 2.
    /// Fails unless the sender holds this key and is answering `nonce` on session `sid`.
    pub fn respond(&self, sid: &str, nonce: &[u8], hs1: &str) -> Result<(Channel, String)> {
        self.respond_with(sid, nonce, hs1, None)
    }

    /// [`Keys::seal`] with a given nonce, so test vectors come out the same every time.
    #[cfg(test)]
    pub fn seal_fixed(&self, sid: &str, kind: &str, plaintext: &str, nonce: [u8; 12]) -> Result<String> {
        self.seal_with(sid, kind, plaintext, nonce)
    }

    /// [`Keys::respond`] with a given ephemeral key, for test vectors.
    #[cfg(test)]
    pub fn respond_fixed(
        &self,
        sid: &str,
        nonce: &[u8],
        hs1: &str,
        ephemeral: &[u8; 32],
    ) -> Result<(Channel, String)> {
        self.respond_with(sid, nonce, hs1, Some(ephemeral))
    }

    fn respond_with(
        &self,
        sid: &str,
        nonce: &[u8],
        hs1: &str,
        fixed: Option<&[u8; 32]>,
    ) -> Result<(Channel, String)> {
        let prologue = prologue(sid, nonce);
        let mut builder = builder(&self.psk, &prologue)?;
        if let Some(key) = fixed {
            builder = builder.fixed_ephemeral_key_for_testing_only(key);
        }
        let mut hs = builder.build_responder()?;
        let message = decode(hs1)?;
        let mut payload = vec![0u8; MAX_NOISE];
        let n = hs.read_message(&message, &mut payload)?;
        let tail = serde_json::from_slice::<serde_json::Value>(&payload[..n])
            .ok()
            .and_then(|hello| hello.get("tail")?.as_u64())
            .map(|tail| tail as usize);
        let mut out = vec![0u8; MAX_NOISE];
        let n = hs.write_message(br#"{"ok":true}"#, &mut out)?;
        Ok((
            Channel {
                noise: hs.into_transport_mode()?,
                partial: Vec::new(),
                tail,
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

/// An initiator waiting for a session's `hs2`.
#[cfg(unix)]
pub struct Handshake {
    hs: snow::HandshakeState,
}

#[cfg(unix)]
impl Handshake {
    pub fn finish(mut self, hs2: &str) -> Result<Channel> {
        let mut out = vec![0u8; MAX_NOISE];
        self.hs.read_message(&decode(hs2)?, &mut out)?;
        Ok(Channel {
            noise: self.hs.into_transport_mode()?,
            partial: Vec::new(),
            tail: None,
        })
    }
}

/// One browser channel's keys, after its handshake. Noise's counters mean a message that
/// was dropped, repeated, reordered or reflected fails to decrypt; a failure is final.
pub struct Channel {
    noise: TransportState,
    partial: Vec<u8>,
    /// How many of the latest entries the other side wants first, if it loads the rest as it
    /// scrolls back (the handshake's first payload says). Older clients want them all.
    pub tail: Option<usize>,
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
                tail: None,
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
    fn sealed_labels_differ_each_time_and_hide_the_text() {
        let keys = Keys::from_psk([7u8; 32]);
        let a = keys.seal(SID, "label", "secret-cwd").unwrap();
        let b = keys.seal(SID, "label", "secret-cwd").unwrap();
        assert_ne!(a, b);
        assert!(!String::from_utf8_lossy(&STANDARD.decode(&a).unwrap()).contains("secret-cwd"));
    }

    fn hex_sha256(text: &str) -> String {
        use sha2::Digest;
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `noise.js` and the hub page's `seal.js` under Node against this module, byte for
    /// byte. Skipped without Node 22+, unless `CI` is set.
    #[test]
    fn the_browser_code_agrees_with_this_code() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
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

        let key = [0x37u8; 32];
        let psk = tamale_hub_protocol::e2e_secret(&key);
        let keys = Keys::from_key(&key);
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
            "key": tamale_hub_protocol::encode_key(&key),
            "auth": tamale_hub_protocol::auth_value(&key),
            "spelling": tamale_hub_protocol::encode_key(&tamale_hub_protocol::decode_key(&tamale_hub_protocol::encode_key(&key)).unwrap()),
            "id": hex_sha256(&tamale_hub_protocol::auth_value(&key))[..16],
            "psk": URL_SAFE_NO_PAD.encode(psk),
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
        let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

        // A key the page made is one tamale takes.
        assert!(tamale_hub_protocol::decode_key(answer["fresh"].as_str().unwrap()).is_some());

        // The code the page draws scans to the exact link, at a real key's length.
        let rows: Vec<Vec<bool>> = serde_json::from_value(answer["rows"].clone()).unwrap();
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(
            (rows.len() + 8) * 4,
            (rows.len() + 8) * 4,
            |x, y| {
                let dark = rows
                    .get((y / 4).wrapping_sub(4))
                    .and_then(|row| row.get((x / 4).wrapping_sub(4)))
                    .is_some_and(|d| *d);
                if dark { 0 } else { 255 }
            },
        );
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1);
        let (_, text) = grids[0].decode().unwrap();
        assert_eq!(
            text,
            format!(
                "https://tamale-hub.com/#{}",
                tamale_hub_protocol::encode_key(&key)
            )
        );

        // What noise.js sealed, opened here.
        let up: Vec<String> = serde_json::from_value(answer["up"].clone()).unwrap();
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
