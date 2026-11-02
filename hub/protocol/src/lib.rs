//! The frames tamale and tamale-hub exchange over the agent socket (`wss://<hub>/agent`):
//! JSON text frames, tagged by `t`. A browser's `/web` messages ride inside `msg` frames
//! unchanged, so this protocol doesn't change when the web UI does.

use serde::{Deserialize, Serialize};

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
    /// The agent token is missing, unknown or revoked.
    pub const BAD_TOKEN: u16 = 4401;
    /// The `hello` was missing, malformed or from an unsupported protocol.
    pub const BAD_HELLO: u16 = 4400;
    /// The sid is connected under another token pair.
    pub const SID_TAKEN: u16 = 4409;
    /// The pair has as many sessions as the hub allows.
    pub const TOO_MANY: u16 = 4429;
    /// Another connection with the same sid and pair took over.
    pub const REPLACED: u16 = 4410;
}

/// Whether an agent socket closed with `code` should not be retried as is.
pub fn is_permanent(code: u16) -> bool {
    matches!(
        code,
        close::BAD_TOKEN | close::BAD_HELLO | close::SID_TAKEN | close::REPLACED
    )
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
        /// Which machine's pairing key this session uses, for browsers to find theirs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mid: Option<String>,
        /// Whether relayed channels are end-to-end encrypted (see `e2e.md`).
        #[serde(default)]
        e2e: bool,
        /// The label, for browsers holding the key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sealed: Option<String>,
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
    fn frames_round_trip_with_short_tags() {
        let hello = ToHub::Hello {
            proto: PROTO,
            sid: "0".repeat(32),
            label: Label {
                host: "vps-b".into(),
                cwd: "~/api".into(),
                model: "claude-opus-5-5".into(),
            },
            mid: Some("1".repeat(32)),
            e2e: true,
            sealed: Some("c2VhbGVk".into()),
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert!(text.starts_with(r#"{"t":"hello","proto":2,"#), "{text}");
        assert_eq!(serde_json::from_str::<ToHub>(&text).unwrap(), hello);

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
            ..
        } = serde_json::from_str(&old).unwrap()
        else {
            panic!("not a hello");
        };
        assert_eq!(
            (label.host.as_str(), mid, e2e, sealed),
            ("h", None, false, None)
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
