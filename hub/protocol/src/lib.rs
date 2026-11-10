//! The frames tamale and tamale-hub exchange over the agent socket (`wss://<hub>/agent`):
//! JSON text frames, tagged by `t`. A browser's `/web` messages ride inside `msg` frames
//! unchanged, so this protocol doesn't change when the web UI does.

use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Bumped on incompatible changes to these frames.
pub const PROTO: u32 = 2;

/// The oldest protocol a hub still accepts. Version 1 is version 2 without `mid`, `e2e` and
/// `sealed`: tamales from before end-to-end encryption, which stay in plaintext.
pub const MIN_PROTO: u32 = 1;

/// The longest sealed label a hub keeps for a session.
pub const MAX_SEALED: usize = 16 << 10;

/// The largest frame either side sends or accepts: a `welcome` carries the whole
/// session, which can be large.
pub const MAX_AGENT_FRAME: usize = 64 << 20;

/// The largest message a browser may send a session, as tamale's own `/web` allows.
pub const MAX_BROWSER_MESSAGE: usize = 4 << 20;

/// Close codes the hub ends an agent socket with. tamale stops retrying on the
/// permanent ones (see [`is_permanent`]).
pub mod close {
    /// The key is missing, unknown, replaced or deleted.
    pub const BAD_TOKEN: u16 = 4401;
    /// The `hello` was missing, malformed or from an unsupported protocol.
    pub const BAD_HELLO: u16 = 4400;
    /// The sid is connected under another key.
    pub const SID_TAKEN: u16 = 4409;
    /// The key has as many sessions as the hub allows.
    pub const TOO_MANY: u16 = 4429;
    /// Another connection with the same sid and key took over.
    pub const REPLACED: u16 = 4410;
    /// The key already has a tamale chef online, and it may only have one.
    pub const CHEF_TAKEN: u16 = 4411;
}

/// Whether an agent socket closed with `code` should not be retried as is.
pub fn is_permanent(code: u16) -> bool {
    matches!(
        code,
        close::BAD_TOKEN
            | close::BAD_HELLO
            | close::SID_TAKEN
            | close::REPLACED
            | close::CHEF_TAKEN
    )
}

/// What starts a key: what machines and browsers share, and the hub never sees.
pub const KEY_PREFIX: &str = "thk_";

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
/// 32 bytes in unpadded base32.
const KEY_CHARS: usize = 52;

/// `bytes` as a key: `thk_` and lowercase base32.
pub fn encode_key(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(KEY_PREFIX.len() + KEY_CHARS);
    out.push_str(KEY_PREFIX);
    let (mut buffer, mut bits) = (0u32, 0);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    out
}

/// The bytes of a key, if `text` is one.
pub fn decode_key(text: &str) -> Option<[u8; 32]> {
    let body = text.strip_prefix(KEY_PREFIX)?;
    if body.len() != KEY_CHARS {
        return None;
    }
    let mut out = [0u8; 32];
    let (mut buffer, mut bits, mut at) = (0u32, 0, 0);
    for c in body.bytes() {
        let value = BASE32.iter().position(|&b| b == c)? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(at)? = (buffer >> bits) as u8;
            at += 1;
        }
    }
    // The last character's spare bits are zero, so each key has one spelling.
    (at == 32 && buffer & ((1 << bits) - 1) == 0).then_some(out)
}

fn derive(key: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(None, key)
        .expand(info, &mut out)
        .expect("32 bytes is far below HKDF's limit");
    out
}

/// What proves access to the hub, in lowercase hex; the hub keeps only its SHA-256. It
/// can't be turned back into the key or the handshake's secret.
pub fn auth_value(key: &[u8; 32]) -> String {
    derive(key, b"tamale-hub auth")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The secret of the end-to-end handshake (see `e2e.md`), which never reaches the hub.
pub fn e2e_secret(key: &[u8; 32]) -> [u8; 32] {
    derive(key, b"tamale-hub e2e")
}

/// Whether `auth` is well formed: 64 lowercase hex characters.
pub fn valid_auth(auth: &str) -> bool {
    auth.len() == 64
        && auth
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `sid` is well formed: 32 lowercase hex characters, a valid DNS label.
pub fn valid_sid(sid: &str) -> bool {
    sid.len() == 32
        && sid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `mid` is well formed: 32 lowercase hex characters, as a sid is.
pub fn valid_mid(mid: &str) -> bool {
    valid_sid(mid)
}

/// Paths a hub may ask tamale for with [`ToAgent::Get`].
pub fn tunnelled_path(path: &str) -> bool {
    matches!(
        path,
        "/" | "/app.js" | "/app.css" | "/noise.js" | "/artifact-frame"
    ) || path
        .strip_prefix("/artifact/")
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// What the session list shows of a session.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Label {
    pub host: String,
    pub cwd: String,
    pub model: String,
    /// Whether it's a tamale chef, which looks after the key's other sessions rather than
    /// working in a project (see tamale's `docs/chef.md`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub chef: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// tamale → hub.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ToHub {
    /// The first frame.
    Hello {
        proto: u32,
        sid: String,
        /// Empty when `e2e`: the hub gets `sealed` instead.
        #[serde(default)]
        label: Label,
        /// Which key this session uses (derived, and revealing nothing about it), for
        /// browsers to find theirs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mid: Option<String>,
        /// Whether relayed channels are end-to-end encrypted (see `e2e.md`).
        #[serde(default)]
        e2e: bool,
        /// The label, for browsers holding the key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sealed: Option<String>,
        /// Whether it's a tamale chef, in the clear even when the label is sealed: a key may
        /// have only one chef online, and the hub is what holds it to that. Browsers go by
        /// the sealed label's `chef`, which the hub can't forge.
        #[serde(default, skip_serializing_if = "is_false")]
        chef: bool,
    },
    /// One `/web` message for browser client `c`.
    Msg { c: u64, data: String },
    /// tamale closed client `c` (it stopped serving, say).
    Close { c: u64, reason: String },
    /// The answer to [`ToAgent::Get`] `r`.
    Got {
        r: u64,
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    /// What the session list shows, sent whenever it changes.
    Status {
        busy: bool,
        approval: bool,
        /// Empty when `sealed` carries them.
        #[serde(default)]
        title: String,
        #[serde(default)]
        model: String,
        /// The title and model, for browsers holding the key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sealed: Option<String>,
    },
    /// The session is over (it was closed or quit), so it isn't coming back: the hub drops it
    /// from the list and ends its browsers' channels for good. The last frame. Hubs from
    /// before it ignore it, and list the session as offline for a while instead.
    End,
}

/// hub → tamale.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum ToAgent {
    Welcome {
        proto: u32,
    },
    /// A browser opened the session's socket as client `c`.
    Open {
        c: u64,
    },
    /// One `/web` message from client `c`.
    Msg {
        c: u64,
        data: String,
    },
    /// Client `c` went away.
    Close {
        c: u64,
    },
    /// A page request, for one of the [`tunnelled_path`]s.
    Get {
        r: u64,
        path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_have_one_spelling() {
        let bytes: [u8; 32] = std::array::from_fn(|i| (i * 7 + 1) as u8);
        let key = encode_key(&bytes);
        assert_eq!(key.len(), 56);
        assert!(key.starts_with("thk_"));
        assert_eq!(decode_key(&key), Some(bytes));
        assert_eq!(decode_key(&encode_key(&[0xff; 32])), Some([0xff; 32]));

        assert_eq!(decode_key(&key[4..]), None, "no prefix");
        assert_eq!(decode_key(&key[..55]), None, "too short");
        assert_eq!(decode_key(&format!("{key}a")), None, "too long");
        assert_eq!(decode_key(&key.to_uppercase()), None);
        assert_eq!(decode_key(&format!("{}1", &key[..55])), None, "not base32");
        // Only the top bit of the last character counts; the others must be zero.
        assert_eq!(decode_key(&format!("{}b", &encode_key(&[0; 32])[..55])), None);
    }

    #[test]
    fn what_a_key_derives_is_separate_and_stable() {
        let key = [0x42u8; 32];
        let auth = auth_value(&key);
        assert!(valid_auth(&auth));
        assert_eq!(auth, auth_value(&key));
        assert_ne!(auth, auth_value(&[0x43; 32]));
        let secret = e2e_secret(&key);
        assert_ne!(secret, key);
        assert_ne!(
            auth,
            secret.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert!(!valid_auth("ABC"));
        assert!(!valid_auth(&"A".repeat(64)));
        assert!(!valid_auth(&"a".repeat(63)));
    }

    #[test]
    fn frames_round_trip_with_short_tags() {
        let hello = ToHub::Hello {
            proto: PROTO,
            sid: "0".repeat(32),
            label: Label {
                host: "vps-b".into(),
                cwd: "~/api".into(),
                model: "claude-opus-5-5".into(),
                ..Default::default()
            },
            mid: Some("1".repeat(32)),
            e2e: true,
            sealed: Some("c2VhbGVk".into()),
            chef: false,
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert!(text.starts_with(r#"{"t":"hello","proto":2,"#), "{text}");
        assert!(!text.contains("chef"), "{text}");
        assert_eq!(serde_json::from_str::<ToHub>(&text).unwrap(), hello);
        let ToHub::Hello { chef, .. } = serde_json::from_str(&text.replacen(
            r#""proto":2,"#,
            r#""proto":2,"chef":true,"#,
            1,
        ))
        .unwrap() else {
            panic!("not a hello");
        };
        assert!(chef);

        // A tamale from before end-to-end encryption still parses, in plaintext.
        let old = format!(
            r#"{{"t":"hello","proto":1,"sid":"{}","label":{{"host":"h","cwd":"c","model":"m"}}}}"#,
            "0".repeat(32)
        );
        let ToHub::Hello {
            label,
            mid,
            e2e,
            sealed,
            chef,
            ..
        } = serde_json::from_str(&old).unwrap()
        else {
            panic!("not a hello");
        };
        assert_eq!(
            (label.host.as_str(), mid, e2e, sealed, chef),
            ("h", None, false, None, false)
        );
        let old = r#"{"t":"status","busy":true,"approval":false,"title":"t","model":"m"}"#;
        assert!(matches!(
            serde_json::from_str(old).unwrap(),
            ToHub::Status { sealed: None, .. }
        ));

        let got = ToHub::Got {
            r: 3,
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: "<p>".into(),
        };
        let text = serde_json::to_string(&got).unwrap();
        assert_eq!(serde_json::from_str::<ToHub>(&text).unwrap(), got);

        let end = serde_json::to_string(&ToHub::End).unwrap();
        assert_eq!(end, r#"{"t":"end"}"#);
        assert_eq!(serde_json::from_str::<ToHub>(&end).unwrap(), ToHub::End);

        for frame in [
            ToAgent::Welcome { proto: 1 },
            ToAgent::Open { c: 7 },
            ToAgent::Msg {
                c: 7,
                data: r#"{"t":"send"}"#.into(),
            },
            ToAgent::Close { c: 7 },
            ToAgent::Get {
                r: 1,
                path: "/".into(),
            },
        ] {
            let text = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<ToAgent>(&text).unwrap(), frame);
        }
        assert_eq!(
            serde_json::to_string(&ToAgent::Open { c: 7 }).unwrap(),
            r#"{"t":"open","c":7}"#
        );
    }

    #[test]
    fn sids_and_paths_are_checked() {
        assert!(valid_sid(&"a1".repeat(16)));
        assert!(!valid_sid(&"A1".repeat(16)));
        assert!(!valid_sid("www"));
        assert!(tunnelled_path("/") && tunnelled_path("/app.js"));
        assert!(tunnelled_path("/artifact/9f2c"));
        assert!(tunnelled_path("/noise.js") && tunnelled_path("/artifact-frame"));
        assert!(!tunnelled_path("/artifact/../x") && !tunnelled_path("/artifact/"));
        assert!(!tunnelled_path("/ws") && !tunnelled_path("/etc/passwd"));
    }
}
