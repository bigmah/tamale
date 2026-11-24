//! The page's end of tamale's end-to-end encryption (see e2e.md and tamale's
//! `src/web/e2e.rs`, the other end): Noise_NNpsk0_25519_AESGCM_SHA256 as an initiator, with
//! long messages cut into pieces whose first byte says whether more follow.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

const NOISE: &str = "Noise_NNpsk0_25519_AESGCM_SHA256";
const PROLOGUE: &[u8] = b"tamale-hub e2e v1";
const MAX_NOISE: usize = 65535;
const TAG: usize = 16;
const MORE: u8 = 0;
const LAST: u8 = 1;
const MAX_PIECE: usize = MAX_NOISE - TAG - 1;
/// The most one message may grow to, pieces and all: a long session's history is big.
const MAX_MESSAGE: usize = 256 << 20;

/// Base64 in either alphabet, padded or not: the hub's nonce and the key in the address
/// are URL-safe, the sealed messages standard.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let clean: String = text
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    URL_SAFE_NO_PAD.decode(clean).ok()
}

pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// The handshake, as the page runs it: `start` with the key and the session's nonce gives
/// the first message; `finish` with tamale's answer gives the channel, or fails if that
/// answer isn't genuine.
pub struct Handshake {
    hs: snow::HandshakeState,
}

impl Handshake {
    pub fn start(
        psk: &[u8],
        sid: &str,
        nonce: &[u8],
        payload: &[u8],
    ) -> Result<(Self, String), String> {
        Self::start_with(psk, sid, nonce, payload, None)
    }

    /// `start`, with a fixed ephemeral key for tests.
    fn start_with(
        psk: &[u8],
        sid: &str,
        nonce: &[u8],
        payload: &[u8],
        ephemeral: Option<&[u8]>,
    ) -> Result<(Self, String), String> {
        let psk: [u8; 32] = psk
            .try_into()
            .map_err(|_| "The key isn't 32 bytes.".to_string())?;
        let prologue = [PROLOGUE, sid.as_bytes(), nonce].concat();
        let mut builder = snow::Builder::new(NOISE.parse().map_err(err)?)
            .psk(0, &psk)
            .map_err(err)?
            .prologue(&prologue)
            .map_err(err)?;
        if let Some(key) = ephemeral {
            builder = builder.fixed_ephemeral_key_for_testing_only(key);
        }
        let mut hs = builder.build_initiator().map_err(err)?;
        let mut out = vec![0u8; MAX_NOISE];
        let n = hs.write_message(payload, &mut out).map_err(err)?;
        Ok((Self { hs }, encode(&out[..n])))
    }

    pub fn finish(mut self, hs2: &str) -> Result<Channel, String> {
        let message = decode(hs2).ok_or("The handshake's answer isn't base64.")?;
        let mut out = vec![0u8; MAX_NOISE];
        self.hs.read_message(&message, &mut out).map_err(err)?;
        Ok(Channel {
            noise: self.hs.into_transport_mode().map_err(err)?,
            partial: Vec::new(),
        })
    }
}

/// One channel after its handshake. A message dropped, repeated or reordered fails to
/// open; a failure is final, and the connection with it.
pub struct Channel {
    noise: snow::TransportState,
    partial: Vec<u8>,
}

impl Channel {
    /// Seals one message as its transport messages, in base64.
    pub fn seal(&mut self, message: &str) -> Result<Vec<String>, String> {
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
            let n = self.noise.write_message(&plain, &mut out).map_err(err)?;
            sealed.push(encode(&out[..n]));
        }
        Ok(sealed)
    }

    /// Opens one transport message: the whole message once its last piece is in.
    pub fn open(&mut self, sealed: &str) -> Result<Option<String>, String> {
        let message = decode(sealed).ok_or("A sealed message isn't base64.")?;
        let mut out = vec![0u8; MAX_NOISE];
        let n = self.noise.read_message(&message, &mut out).map_err(err)?;
        let (&flag, piece) = out[..n].split_first().ok_or("An empty message.")?;
        if self.partial.len() + piece.len() > MAX_MESSAGE {
            return Err("A message too long to take.".into());
        }
        self.partial.extend_from_slice(piece);
        match flag {
            MORE => Ok(None),
            LAST => String::from_utf8(std::mem::take(&mut self.partial))
                .map(Some)
                .map_err(|_| "A message that isn't UTF-8.".into()),
            _ => Err("A bad fragment flag.".into()),
        }
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_carries_long_messages_both_ways() {
        let psk = [7u8; 32];
        let sid = "0123456789abcdef";
        let nonce = b"channel-nonce-16";
        let (hs, hs1) = Handshake::start(&psk, sid, nonce, b"{\"v\":1}").unwrap();
        let prologue = [PROLOGUE, sid.as_bytes(), nonce].concat();
        let mut responder = snow::Builder::new(NOISE.parse().unwrap())
            .psk(0, &psk)
            .unwrap()
            .prologue(&prologue)
            .unwrap()
            .build_responder()
            .unwrap();
        let mut buf = vec![0u8; MAX_NOISE];
        let n = responder
            .read_message(&decode(&hs1).unwrap(), &mut buf)
            .unwrap();
        assert_eq!(&buf[..n], b"{\"v\":1}");
        let n = responder.write_message(&[], &mut buf).unwrap();
        let mut page = hs.finish(&encode(&buf[..n])).unwrap();
        let mut tamale = Channel {
            noise: responder.into_transport_mode().unwrap(),
            partial: Vec::new(),
        };
        let long = "x".repeat(MAX_PIECE * 2 + 5);
        let boxes = page.seal(&long).unwrap();
        assert_eq!(boxes.len(), 3);
        let mut got = None;
        for b in &boxes {
            got = tamale.open(b).unwrap();
        }
        assert_eq!(got.as_deref(), Some(long.as_str()));
        let back = tamale.seal("hi").unwrap();
        assert_eq!(page.open(&back[0]).unwrap().as_deref(), Some("hi"));
    }

    /// The vectors tamale's own tests keep current (apple/Fixtures/e2e-vectors.json): the
    /// same handshake bytes as tamale, its messages opened here, and ours as it seals them.
    #[test]
    fn agrees_with_tamale_byte_for_byte() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../apple/Fixtures/e2e-vectors.json")).unwrap();
        let hex = |s: &str| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect::<Vec<u8>>()
        };
        let text = |k: &str| v[k].as_str().unwrap().to_string();
        let long = v["long"]["repeat"]
            .as_str()
            .unwrap()
            .repeat(v["long"]["count"].as_u64().unwrap() as usize);
        let plain = |m: &serde_json::Value| match m["plain"].as_str().unwrap() {
            "<long>" => long.clone(),
            p => p.to_string(),
        };
        let (hs, hs1) = Handshake::start_with(
            &hex(&text("psk")),
            &text("sid"),
            &decode(&text("nonce")).unwrap(),
            text("hs1_payload").as_bytes(),
            Some(&hex(&text("initiator_ephemeral"))),
        )
        .unwrap();
        assert_eq!(hs1, text("hs1"));
        let mut channel = hs.finish(&text("hs2")).unwrap();
        for m in v["down"].as_array().unwrap() {
            let mut got = None;
            for sealed in m["sealed"].as_array().unwrap() {
                got = channel.open(sealed.as_str().unwrap()).unwrap();
            }
            assert_eq!(got, Some(plain(m)));
        }
        for m in v["up"].as_array().unwrap() {
            let sealed: Vec<String> = m["sealed"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect();
            assert_eq!(channel.seal(&plain(m)).unwrap(), sealed);
        }
    }
}
