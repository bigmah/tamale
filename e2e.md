# End-to-end encryption between tamale and the browser

> **Keys changed: see [`single_key.md`](single_key.md).** The per-machine pairing key, scanning a
> code from the terminal, `/web remote pair` and `rekey` are gone. The PSK now comes from the one
> key every machine and browser share. The handshake, sealed labels and artifacts below are as
> built.

**Status:** phases 2a to 2d are built; 2e is still to come. Two things differ from the first
version of this plan, both forced by the code: messages over 64 KB go in pieces (see
*Long messages*), and the prologue binds a nonce tamale issues rather than the channel id,
which the browser never learns (see *Handshake*). Still to do by hand: *Manual* under
Testing, on a real phone.

Phase 2 of `tunnel_plan.md`. Today the hub at tamale-hub.com sees every session in
plaintext (TLS ends at Caddy). Since the landing page lets anyone make tokens, that means
strangers' prompts, code and tool output pass through a box they don't control, and the
hub (or whoever breaks into it) could **send commands** to their machines. This plan
seals the session between tamale and the browser so the hub only moves opaque bytes.

## Goals and limits

**Protects against**

1. **Reading**: the hub operator, a compromised VPS, logs, backups and memory dumps see
   no prompts, replies, tool output, files, artifacts, titles or working directories.
2. **Injection**: the hub can't forge, replay, reorder or reflect messages. A tamale with
   E2E on ignores anything unsealed from a relayed channel, so a hub without the key
   can't type a prompt or approve a tool call.
3. **Downgrade**: neither side falls back to plaintext once a key exists for the machine.

**Does not protect against** (stated on the landing page and in the README)

- **A hub that serves malicious page code.** The session page and `app.js` come through
  the hub, so an active attacker on it can change the code to steal the key the next time
  the page loads. E2E turns a silent, passive compromise into one that must tamper with
  code. The fix is a page from somewhere the hub doesn't control (*Phase 2e*).
- **Metadata**: the hub still sees which sessions exist, when, from which IPs, how many
  tabs, and message sizes and timing.
- **The endpoints**: a compromised laptop or phone.

Local `/web` (same machine or LAN) stays as it is; this applies to relayed channels only.

## Design

### Keys

- **Pairing key (PSK)**: 32 random bytes per **machine**, made on first `/web remote` and
  kept in tamale's data dir (`hub-pairing.key`, mode 600). Shared by every session on that
  machine, so a phone scans once per machine, not once per session. `/web remote rekey`
  replaces it (every browser must scan again).
- **Machine id (`mid`)**: `HKDF(PSK, info="tamale-hub mid")[..16]`, hex. Sent in `hello`
  so the hub can group sessions and the browser can find the right key. It reveals nothing
  about the PSK.
- **Session keys**: fresh for every browser channel, from a Noise handshake (below), so
  each tab's traffic has its own keys and forward secrecy: a PSK leaked later can't
  decrypt recorded traffic.
- **Metadata key**: `HKDF(PSK, info="tamale-hub meta")`, for the session list's labels
  (below).

The view token stays what it is: the hub's access control. It's separate from the PSK on
purpose: the hub must check the view token, so it can't also be the secret the hub never
sees.

### Getting the PSK to the browser

The PSK never reaches the hub, so it goes through the **URL fragment**, which browsers
don't send to servers:

1. `/web remote` shows `https://tamale-hub.com/open/<sid>#k=<base64url PSK>` and its QR
   code in the terminal (today it shows the same link without `#k=`).
2. The dashboard page (`tamale-hub.com`) stores the key in IndexedDB under its `mid`, and
   clears the fragment with `history.replaceState`.
3. On `/open/<sid>`, the dashboard looks up the session's `mid`, and appends the key to the
   ticket redirect as a fragment:
   `https://<sid>.tamale-hub.com/__hub/auth?ticket=…#k=…`. Fragments survive the `303` to
   `/`, so the session page reads it, keeps it in `sessionStorage` (for reloads in that
   tab) and clears the address bar. The dashboard only does this if the hub's ticket says
   the session is under that same key: a `mid` is only what a session says of itself.

Why the dashboard holds the keys: every session is its own origin, so a key stored on one
session's origin is invisible to the next session from the same machine (and to the same
machine after tamale restarts, which changes the sid). The dashboard origin is the one
place that sees them all.

Consequence: the key is held as raw bytes on the dashboard origin (it has to be passed
on), not as a non-extractable `CryptoKey`. That's within the threat model: page code from
the hub is already trusted until Phase 2e.

### Handshake: Noise `NNpsk0_25519_AESGCM_SHA256`

A standard, analysed pattern, so we implement a spec rather than invent a protocol:

- **NN**: both sides send an ephemeral X25519 key; **psk0**: the PSK is mixed in from the
  first message, so only holders of the PSK complete the handshake. One round trip.
- **AESGCM** because WebCrypto has AES-GCM but not ChaCha20-Poly1305; **SHA256** for the
  same reason.
- Prologue: `"tamale-hub e2e v1" || sid || n`, binding the handshake to this session and
  to a fresh 16-byte nonce `n` tamale issues for this channel, so a recorded handshake can't
  be replayed onto another channel or session. (Not the channel id `c`: the hub assigns it
  and the browser never sees it. A nonce is stronger anyway: a replayed `hs1` is refused
  outright, rather than making a tab whose traffic nobody can read.)

Flow on a relayed channel `c`, inside the existing `msg` frames (the hub forwards them
unchanged and needs no new frame types):

```
tamale  → browser  {"t":"hs0","n":"<base64 nonce>"}                on channel open
browser → tamale   {"t":"hs1","m":"<base64 Noise message 1>"}     psk, e; payload: page version
tamale  → browser  {"t":"hs2","m":"<base64 Noise message 2>"}     e, ee; payload: {ok:true}
then both ways     {"t":"box","m":"<base64 transport message>"}    one or more per /web message
```

- After `hs2`, each side has a send and a receive cipher. Noise's transport nonces are
  64-bit counters per direction, so a dropped, repeated, reordered or reflected message
  fails to decrypt. WebSocket order is reliable, so any failure closes the channel.
- tamale accepts nothing but `hs1` on a new relayed channel, and nothing but `box` after
  it. A channel only becomes a tab once its handshake succeeds. A bad `hs1` (wrong PSK, a
  hub guessing) closes the channel; after 5 failures in a row tamale turns new handshakes
  away for a doubling time, up to about 4 minutes, and reports it once (256-bit keys make
  guessing hopeless anyway, but the counter makes abuse visible).
- Payloads are the existing `/web` JSON messages, unchanged, so `handle()`, `Mirror` and
  `app.js`'s `onMessage` don't change; encryption is a layer around `send()` and
  `ws.onmessage`.

**Long messages.** A Noise message is at most 65535 bytes, and a `welcome` or an artifact
can be many megabytes. So each transport message's plaintext is a flag byte (`0` more to
come, `1` last) and a piece of the `/web` message. The flag is inside the encryption, so a
hub can't cut a message short or join two; the counters keep the pieces in order. tamale
holds what it reassembles from a browser to the 4 MiB a browser may send.

**Libraries**

- **Rust**: `snow` (the reference Rust Noise implementation), with its default resolver
  (x25519-dalek, aes-gcm, sha2), plus `aes-gcm` and `hkdf` directly for the sealed labels and
  key derivation (HKDF-SHA256, empty salt) and `base64`. In tamale only; the hub doesn't
  link any of this.
- **Browser**: a small Noise NNpsk0 implementation over WebCrypto (`X25519` deriveBits,
  `HMAC`/`HKDF` via `importKey`, `AES-GCM` with Noise's nonce layout: 4 zero bytes and a
  big-endian 64-bit counter). About 250 lines in `src/web/noise.js`, served with
  `app.js`. Written against the Noise spec and checked byte for byte against `snow` (see
  Testing). X25519 in WebCrypto is in current Chrome, Firefox and Safari; older browsers
  get "This browser is too old for encrypted sessions" rather than a fallback.

### Sealed metadata

The session list shows title, host, cwd and model, which the hub sees today in `hello`
and `status`. With E2E:

- `hello.label` and `status.title` are replaced by `sealed`: AES-256-GCM under the
  metadata key, random 96-bit nonce, `aad = sid || "label"/"status"`. Busy and approval
  stay plaintext booleans, so the list can sort and badge without the key; hiding even
  those isn't worth a sealed list. So does `hello.chef`, whether the session is a tamale
  chef: a key may have only one online, and the hub refuses a second (close code 4411).
  Browsers and tamale still go by the sealed label's `chef`, which the hub can't forge.
- The dashboard decrypts with the keys it holds by `mid`. A session whose key the browser
  doesn't have shows as "Encrypted session on an unknown machine: scan its code".
- The hub log keeps pair names and short sids only, as now.

### Artifacts

Today artifact pages are fetched by URL through the hub's tunnel (`/artifact/<id>`), in
plaintext. With E2E:

- tamale refuses `get` for `/artifact/…` on a relayed connection with E2E on, and the hub
  never asks for it.
- The page requests an artifact over the sealed socket (`{"t":"artifact","id":…}` →
  `{"t":"artifact","id":…,"html":…}`), like the code browser's `open` already does.
- Rendering: `srcdoc` and `blob:` documents inherit the parent page's CSP
  (`script-src 'self'`), which would block artifact scripts. Instead, the session serves
  one static, public frame page, `/artifact-frame`, through the tunnel with the artifact
  CSP header (`sandbox allow-scripts …`, no `allow-same-origin`). Its only script waits
  for one `postMessage` from the parent with the HTML and `document.write`s it. The
  parent keeps a reference to the iframe and posts to it (`targetOrigin` `*` is needed
  for an opaque origin; the content is the user's own artifact going to the frame the
  parent just made). The resize messages work as now.
- Saved-artifact gallery entries go the same way.

### Turning it on and preventing downgrade

- tamale: `[hub] e2e = true` by default once this ships. With it, `hello` carries
  `mid` and `e2e: true`, relayed channels require the handshake, and artifact GETs are
  refused. `e2e = false` keeps today's behaviour, for debugging.
- Hub: records `e2e` per session and refuses to tunnel `/artifact/…` for such sessions
  (defence in depth; tamale refuses too). It shows a lock badge in the list.
- Browser: if it holds a key for the session's `mid`, it insists on the handshake and
  refuses plaintext. If it has no key, it shows the "scan the code from this machine's
  terminal" screen instead of the session (with `/web remote pair` to show the code again).
- Locally served `/web` pages skip all of this. The page knows it's on an encrypted
  session because the hub's `/__hub/auth` redirect lands on `/?e2e=1` (and the key
  fragment) instead of `/`; without it, `app.js` talks plaintext as today. A hub that
  drops `e2e=1` doesn't downgrade anything: tamale still refuses unsealed messages, so
  the page just fails to connect.

## What changes where

| Where | Change |
|---|---|
| `src/web/relay.rs` | Per-channel Noise state; `hs1`/`hs2`/`box` handling; drop unsealed messages; refuse artifact GETs; sealed `hello`/`status`; `mid` |
| new `src/web/e2e.rs` | PSK file, `mid`/meta key derivation, Noise wrappers around `snow`, metadata sealing; unit tests |
| `src/web.rs` | `artifact` socket request; `/artifact-frame` asset |
| `src/web/app.js` | Encryption layer around `send()`/`onmessage` when `e2e=1`; key from fragment/sessionStorage; artifacts via the frame page |
| new `src/web/noise.js` | Noise NNpsk0 over WebCrypto |
| `src/web/artifact-frame.html` | The static frame page |
| `src/app/*` | `/web remote` link with `#k=`, `/web remote pair`, `/web remote rekey`, `[hub] e2e` |
| `hub/protocol` | `Hello { mid, e2e, sealed }`, `Status { sealed }`; bump `PROTO` to 2, hub accepts 1 and 2 during the transition |
| `hub/src/serve.rs` | Store `mid`/`e2e`/sealed blobs; no artifact tunnel for E2E sessions; pass `mid` to the dashboard |
| new `hub/static/seal.js` | `mid` derivation and opening labels, apart from the page so the tests can run it |
| `hub/static/hub.js` | Key store in IndexedDB by `mid`; take `#k=` on any dashboard URL; decrypt labels; append the key to ticket redirects; lock badge; "unknown machine" rows |
| landing page / README | Say what E2E covers and what it doesn't |

## Phases

**2a: crypto core.** `e2e.rs` and `noise.js` with the handshake, transport and metadata
sealing; Rust ↔ JS interop tests. No wiring yet.

**2b: sealed channels.** Relay and page use the handshake and `box` frames; tamale drops
unsealed relayed messages; keys flow through `/web remote`'s link and the dashboard.
*Done when* a session works from a phone with E2E on, and a hub patched to log
`msg.data` logs only ciphertext.

**2c: sealed metadata and artifacts.** Labels and titles sealed; artifacts over the socket
via the frame page; the hub refuses artifact GETs for E2E sessions.
*Done when* nothing in the hub's memory or logs for an E2E session contains a prompt,
title, cwd, file content or artifact.

**2d: UX.** Lock badge, "unknown machine" rows and scan screen, `/web remote pair` and
`rekey`, the landing page's trust section rewritten, README.

**2e (later): page code the hub can't change.** Closes the remaining hole. Options, best
first:
1. A desktop `tamale remote` and native apps that ship the UI themselves (already in
   `tunnel_plan.md`'s Later list); the hub is then only a pipe.
2. The web UI served from a static origin the hub doesn't run (for example GitHub Pages
   built by CI from tagged releases), talking to the hub's WebSocket only. Moves the trust
   from the hub operator to the release pipeline.
3. Not an option: Subresource Integrity or service workers on the hub's origin. The
   hub serves the bootstrap, so it can serve a different one.

## Testing

- **Interop**: a fixed PSK, ephemeral keys and prologue; `snow` produces `hs1`/`hs2` and
  transport messages, and `noise.js` (run under Node 22+, which has WebCrypto X25519)
  must produce and accept the same bytes, and vice versa. Run in `cargo test` via
  `src/web/noise.test.mjs` when Node 22+ is available; with `CI` set, a missing Node fails.
- **Tampering**: flip a bit in `hs1`, `hs2` and a `box` → the channel closes; replay a
  `box`, reorder two, send one back to its sender (reflection) → closes; a handshake for
  channel 7 replayed on channel 8 → fails (prologue).
- **Injection**: with E2E on, a stand-in hub sends plaintext `{"t":"send"}` and
  `{"t":"approve"}` on a relayed channel → tamale emits no `WebEvent`.
- **Wrong key**: a browser with another machine's PSK → handshake fails, page shows the
  scan screen.
- **Downgrade**: a hub that strips `e2e` from `hello` → the browser (which has the key)
  still insists on the handshake; tamale still refuses plaintext.
- **Metadata**: the hub's `/api/sessions` for an E2E session contains no title or cwd in
  plaintext; the dashboard shows them after decrypting.
- **Artifacts**: the hub returns 404 for `/artifact/…` on an E2E session; an artifact with
  scripts renders and runs in the frame page; the frame has an opaque origin
  (`allow-same-origin` absent) and can't open the session socket.
- **Browser**: checked in Chrome against a real local hub: everything on the wire is
  `hs0`/`hs1`/`hs2`/`box`; scan, reload and reopen from the list; a 200 KB artifact in the
  frame, a new tab and a download; wrong key and stripped `e2e=1` both end at the scan screen.
- **Manual**: phone on mobile data; scan once, open two sessions from the same machine
  without rescanning; restart tamale and reopen; `rekey` and check the old phone gets the
  scan screen.


