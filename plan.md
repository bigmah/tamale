# Plan: tamale-hub (remote access to tamale sessions)

## Goal

Drive any running tamale session from a phone or another computer, wherever that session
runs (a laptop, or one of many short-lived VPSs), without Tailscale, port forwarding or a
domain per machine.

A small, self-hostable relay, **tamale-hub**, runs on one public box with TLS. Every tamale
that should be reachable makes an **outbound** WebSocket connection to it using a token the
hub issued. You open the hub in a browser that holds a matching view token (no accounts,
no passwords, no sign-in step), see the live sessions and open one in the same `/web` UI
tamale already serves locally.

```
 vps-a: tamale ──┐ outbound wss,             ┌── phone browser
 vps-b: tamale ──┼─ agent token ──► hub ◄─── ┤   (view token)
 laptop: tamale ─┘                           └── laptop browser
```

### Non-goals (for now)

- Paid or hosted plans and multi-tenant accounts. The hub serves one owner; anyone can run
  their own.
- Native apps (iOS/Android) and a `tamale remote` desktop client.
- Built-in TLS. Run the hub behind Caddy, nginx or similar.
- Accounts, sign-in, passwords or passkeys, now or later. **Tokens are the only
  credential.**
- Sharing sessions between people or roles. Anyone holding a view token controls every
  session under that token.

## Why this fits the current code

`/web` (`src/web.rs`) already has most of what a relay needs:

- The whole conversation goes over **one WebSocket as JSON**, and the server code doesn't
  depend on where a tab connects from. `handle()` takes a parsed `Value` (`send`,
  `interrupt`, `approve`, `tree`, `open`, `grep`, `artifacts`) and emits `WebEvent`s that
  `src/main.rs` hands to the app.
- `Server::welcome` / `Mirror::full` send a newly connected tab the whole session, so a phone
  that joins halfway through (or reconnects) catches up, and **the hub stores no history**.
- The page (`src/web/index.html`, `app.css`, `app.js`) is compiled in with `include_str!`,
  and the page already reconnects with backoff (`ws.onclose` in `app.js`; close code
  `4000` means "stopped serving", and the page stops retrying).
- The header already shows the permission mode (the `#mode` pill), and clicking it cycles
  the mode, so a remote viewer can see and change it.
- Artifacts are served from `/artifact/<id>` with a sandboxing CSP.

The hub serves its own copy of that page, forwards the socket messages and tunnels
artifact pages. The tamale side adds a second **transport** for the same server, next to
its local TCP listener.

## Components

### 1. `tamale-hub` crate (new workspace member, `hub/`)

A binary built on axum and tokio, like tamale.

```
tamale-hub serve   --listen 127.0.0.1:8787 --public-url https://hub.example.com [--data-dir DIR]
tamale-hub token   new <name>     # prints an agent token, a view link and a QR code
tamale-hub token   list
tamale-hub token   revoke <name>  # kills both halves and drops their sockets
tamale-hub token   rotate <name> [--agent | --view]
```

- **Data dir**: `--data-dir`, else `$TAMALE_HUB_DATA`, else `~/.local/share/tamale-hub`.
  One `state.json` written atomically (temp file + rename) and locked with an advisory
  lock while the CLI edits it. The CLI subcommands edit that file, and `serve` reloads it
  when it changes (watch mtime, or reload on `SIGHUP`). No database.
- **`--public-url` is required** unless `--listen` is a loopback address. It sets the
  expected `Origin` for the browser socket and `POST /api/tokens`, and the links `token new` prints. Refuse an
  `http://` public URL unless `--insecure` is passed, for testing.

#### Routes

| Route | Who | What |
|---|---|---|
| `GET /agent` (WebSocket) | tamale | `Authorization: Bearer tha_…`; carries the relay frames below |
| `GET /` | anyone | The hub's own static page (no secrets). It takes view tokens from the URL fragment or a paste box, then lists sessions |
| `POST /api/tokens` | browser (`Origin`) | Body `{"add":"thv_…"}` or `{"forget":true}`. Checks the token and sets or clears the `HttpOnly` cookie (below) |
| `GET /api/sessions` | browser (view token cookie) | JSON list of sessions under the tokens the browser holds |
| `GET /s/<sid>/` | browser (view token cookie) | The hub's **embedded** copy of tamale's `index.html` |
| `GET /s/<sid>/app.js`, `app.css` | browser (view token cookie) | The hub's embedded copies |
| `GET /s/<sid>/artifact/<id>` | browser (view token cookie) | Artifact page through the tunnel; the hub forces `text/html` and the sandbox CSP |
| `GET /s/<sid>/ws` (WebSocket) | browser (view token cookie + `Origin`) | One client channel of that session |

Every `/s/<sid>/…` route checks that the session's agent token belongs to the same token
pair as one of the view tokens the browser presents. Otherwise it returns 404, so you
can't even confirm that someone else's session exists.

The page assets come **from the hub**, never from the session's tamale. Every session
shares the hub's origin, so any script a session supplied would run with the cookie and
could open the socket of every other session under the same pair. Since agent tokens are
expected to leak (see Tokens), a leaked agent token could then register a fake session and
use its `app.js` to take over the real ones. With the page served by the hub, the only
content a session supplies is socket messages (rendered by the hub's `app.js`, which must
keep escaping them) and artifacts (sandboxed in an opaque origin).

The price is that the hub embeds the UI (`include_str!("../../src/web/…")`, the same
files tamale embeds), so it has to be upgraded along with tamale when the `/web` message
protocol changes. `hello` carries a `ui` version (a constant in the protocol crate, bumped
on incompatible `/web` message changes). The hub refuses a `hello` whose `ui` it doesn't
serve, with the reason "upgrade tamale-hub" or "upgrade tamale", which tamale shows as an
error. Both binaries are built from this workspace and released together.

### 2. Tokens (the only credential)

There are no accounts, logins or passwords. `tamale-hub token new <name>` creates a
**token pair**, the unit of access:

| Half | Prefix | Lives on | Can |
|---|---|---|---|
| Agent token | `tha_` | each VPS/laptop running tamale (`TAMALE_HUB_TOKEN`) | Register sessions under this pair. **Cannot** list or drive anything |
| View token | `thv_` | your phone / browsers | List and drive sessions registered with this pair's agent token |

Splitting the pair is what keeps "just tokens" safe on many throwaway VPSs. Every VPS
holds the agent token, so it *will* leak eventually. With the split, a leaked agent token
only lets someone add fake entries to your session list; it can't read or control your
real sessions (because the hub, not the session, serves the page; see Routes). The view token never touches a VPS.

- Both halves are random 32 bytes (base32). The hub stores **only SHA-256 hashes** and
  compares them in constant time. `state.json` holds
  `{ name, agent_hash, view_hash, created, last_used }` per pair.
- `token new mine` prints the agent token once (plus an `export TAMALE_HUB_TOKEN=…` line
  and a config snippet) and the **view link** `https://hub.example.com/#thv_…` with a
  terminal QR code (`qrcode` crate, as `/web` uses).
- One pair for everything is fine: put the same agent token on every VPS and scan the view
  QR code once. Use more pairs to separate groups of machines; a browser can hold several
  view tokens, and the list merges them.
- `revoke` deletes the pair and drops every socket using either half. `rotate --agent`
  replaces only the agent half (re-export on your VPSs, phone unaffected), and
  `rotate --view` replaces only the view half (rescan on your phone, VPSs unaffected).

**How a browser holds a view token**

1. You open the view link (or paste `thv_…` into the box on `/`). The token is in the
   URL **fragment**, so it's never sent to the server and never appears in Caddy or nginx
   access logs.
2. The hub's static page clears the address bar with `history.replaceState` and POSTs
   the token to `/api/tokens`. The hub checks that it's a current view token (so a typo
   gets an error rather than an empty list), adds it to the ones already in the cookie and
   replies with
   `Set-Cookie: th=<thv_a>,<thv_b>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000`
   (400 days, the browser maximum). Revoked or rotated tokens are dropped from the cookie
   the next time it's set.
3. From then on the browser sends the cookie automatically on every request: page assets,
   the WebSocket and artifact iframes alike. No tamale or `app.js` code has to know about
   it. A "Forget tokens" button on `/` clears the cookie.

The cookie is `HttpOnly`, so no script can read the token itself, not even if rendered
chat content ever got past the page's escaping. Artifacts run in opaque-origin sandboxes,
so their requests are cross-site and carry no `SameSite=Strict` cookie at all. With
`--insecure` (plain HTTP, for testing) the hub leaves out `Secure`, or browsers would
drop the cookie.

**On the tamale side**, `/web remote` prints the session's URL `https://hub…/s/<sid>/`
(no token in it). A phone that already holds the view token just opens it. tamale never
sees the view token.

### 3. Relay protocol (tamale ↔ hub)

One WebSocket per tamale session, text frames, JSON. Shared types live in a small crate
(`hub/protocol`, `tamale-hub-protocol`) used by both binaries, like `web/protocol`.

```jsonc
// tamale → hub, first frame
{"t":"hello","proto":1,"ui":1,"sid":"<random per tamale run>","label":{"host":"vps-b","cwd":"~/api","model":"claude-opus-5-5"}}
// hub → tamale
{"t":"welcome","proto":1}                         // or a close frame with a reason
// hub → tamale: a browser opened /s/<sid>/ws
{"t":"open","c":7}
// both ways: one /web message for client 7, the payload passed through unchanged
{"t":"msg","c":7,"data":"{\"t\":\"send\",\"text\":\"…\"}"}
{"t":"close","c":7}
// hub → tamale: tunnelled GET (only "artifact/<id>"); the hub sets the headers itself
{"t":"get","r":3,"path":"artifact/9f2c…"}
{"t":"got","r":3,"status":200,"body":"…"}
// tamale → hub, whenever it changes: shown in the session list
{"t":"status","busy":true,"approval":false,"title":"Fix flaky test"}
```

- `sid` is 16 random bytes (hex), made once per tamale process and kept across `/clear`
  and `/resume`, so `/s/<sid>/` stays valid across hub reconnects. It names "this
  tamale", not a transcript, and isn't a secret (every route also needs the view token).
- If a `sid` is already connected with the **same** pair, the new connection replaces the
  old one (a reconnect after a network drop shouldn't wait for the ping timeout). With a
  different pair, reject it.
- The hub assigns `c` ids and enforces limits: 4 MiB per browser → tamale message (as
  `/web` does today), 64 MiB per agent frame (a `welcome` carries the whole session), and
  caps on clients per session (16) and sessions per pair (64).
- Ping every 30 s in both directions; drop a peer after 2 missed pongs.
- When the agent drops, the hub closes that session's browser sockets with a normal code
  (not `4000`), keeps the session listed as **offline** for 10 minutes, and answers
  `/s/<sid>/ws` with `503` meanwhile, so `app.js` keeps retrying and picks up again when
  that tamale reconnects. After 10 minutes the sid is forgotten, and a browser socket to
  it is accepted and closed with `4000` "This session ended. Open the hub for the list.",
  so the page stops retrying and says why. A tamale that restarts gets a new sid and
  shows up in the list as a new session.

### 4. tamale side

**Config** (`config/default.toml`, new section):

```toml
[hub]
# url = "wss://hub.example.com"  # a tamale-hub to make this session reachable through
# token: set TAMALE_HUB_TOKEN rather than writing it here
connect = "manual"               # manual: only with /web remote; auto: on startup when url is set
```

Add `TAMALE_HUB_URL` and `TAMALE_HUB_TOKEN` env overrides next to the existing `TAMALE_*`
ones in `src/config.rs`.

**Command**: `/web remote` connects to the hub (leaving any local or LAN server as it is)
and prints the session's URL (`https://hub…/s/<sid>/`) with a QR code. `/web remote off`
disconnects. `/web off` stops everything. Update the `/web [local|off]` entry in `/help`
(`src/app/help.rs`) and the usage string in `src/app/commands.rs` to
`/web [local|remote|off]`. The footer shows a "hub" indicator with the number of remote
viewers (a `remote` flag on `Client`, next to `tabs()`). Connection failures (bad token,
`ui` mismatch, unreachable hub) show as an error entry, and the relay keeps retrying
except after a rejected token or `ui`.

**Code changes**

1. `src/web.rs`: split transport from session.
   - Extract `client()`'s per-client logic so a client is just "an id and an
     `UnboundedSender<Out>`". Local axum sockets and relayed channels both register
     through `Shared::add_client(tx) -> id` / `remove_client(id)`, and both feed
     `handle()`.
   - Extract the body of `artifact` into `fn artifact_page(&Shared, id) -> Option<String>`.
     The axum handler keeps its `own_host` check and headers, and the relay answers `get`
     with it. Move the page and artifact CSP strings into `tamale-hub-protocol` (or a
     shared module) so the hub sends exactly the same headers.
   - `Server` can then run with or without the local listener (remote-only mode), since
     `WebRequest::Open { lan }` currently always binds one.
2. `src/web/relay.rs` (new): the outbound connection.
   - `tokio-tungstenite` with rustls (a new direct dependency; 0.29 is already in
     `Cargo.lock` through axum, and reqwest has no WebSocket client).
   - Connect, send `hello`, then turn `open` / `msg` / `close` into
     `add_client` / `handle()` / `remove_client`, and send each client's `Out` back as
     `msg` frames (`Out::Close` as `close`). Answer `get` frames with `artifact_page()`.
   - Reconnect with backoff and jitter (1 s → 30 s). On reconnect, every remote client
     counts as new and gets `welcome`d again.
   - Send `status` when the busy, approval or title state changes (from `View` in
     `Server::sync`).
3. `src/web/app.js` and `index.html`: make URLs relative so the page works under `/s/<sid>/`.
   - `href="app.css"`, `src="app.js"` (no leading `/`).
   - Socket: `new URL('ws' + location.search, location.href)` with `http` → `ws` and
     `https` → `wss`, instead of the hard-coded `ws://${location.host}/ws`.
   - `render.rs` / `Artifacts::register`: `artifact/<id>` instead of `/artifact/<id>`
     (and update the tests in `render.rs` and `web.rs` that assert the old form, plus the
     `web.rs` module doc).
   - Locally nothing changes, because the page is served at `/`. Under the hub the page
     has no `?t=`, which is fine: the hub ignores it.
4. `src/main.rs`: handle `WebRequest::Remote { on }`, and connect on startup when
   `hub.connect = "auto"`.

## Security

Anyone who can send a `send` or `approve` message can run commands on that machine.
Treat a view token as an SSH key to every connected box.

- **A view token is a bearer key to your machines.** Anyone who gets the view link
  controls every session under that pair, so share it by QR code rather than chat.
  `rotate --view` if it leaks.
- **TLS is required** in front of the hub (Caddyfile example in `hub/README.md`). Tokens
  never cross plain HTTP except with `--insecure`.
- **Browser socket**: valid view token cookie **and** `Origin == public-url`. Cookie is
  `SameSite=Strict`. All browser routes send `X-Frame-Options: DENY` /
  `frame-ancestors 'none'`, except artifacts, which keep tamale's sandbox CSP and
  `frame-ancestors 'self'`.
- **Artifacts** stay in an opaque origin (`sandbox` CSP without `allow-same-origin`), so
  even though they're under the hub's domain they can't read the cookie or open the
  socket.
- **Page code comes only from the hub** (see Routes): a session, even one registered with
  a leaked agent token, supplies data, never script on the hub's origin. A fake session
  can still show misleading content when opened, so the session list shows each
  session's host and pair name, and the hub page renders labels with `textContent`.
- **Permission mode**: the default `[permissions] mode = "auto"` means remote prompts run
  tools without asking. `/web remote` prints a clear warning when the mode is `auto`
  (it doesn't change it; see Decisions). The header's mode pill already shows it.
- **Workspace exposure**: the code browser (`tree`, `open`, `grep`) and the artifacts
  gallery work remotely too. That's intended, but say so in the docs.
- **Rate limits**: per IP on token failures (`/agent`, `/api/tokens`, `/api/sessions`,
  `/s/*`); caps on sessions, clients and message size as above. The client IP is the TCP
  peer, or the last `X-Forwarded-For` entry when the peer is loopback (the reverse proxy).
- **CSRF**: `POST /api/tokens` requires `Origin == public-url`, on top of `SameSite=Strict`.
- **Logging**: the hub logs connects, disconnects, token pair names and IPs, never
  tokens or message payloads.

## Phases

### Phase 1: Relay working end to end

1. Refactor `src/web.rs` (transport split, `artifact_page()`), with no behaviour change; existing
   `web.rs` tests pass.
2. Relative URLs in `index.html`, `app.js` and `render.rs`.
3. `hub/protocol` crate with frame types and round-trip tests.
4. `tamale-hub`: state file, `token new|list|revoke|rotate`, `serve` with the static `/`
   page (fragment → `/api/tokens` → cookie, paste box, session list polled every 5 s)
   and all routes, serving the embedded tamale UI under `/s/<sid>/`.
5. `src/web/relay.rs`, `[hub]` config, `/web remote`.
6. `hub/README.md`: install, Caddy config, systemd unit, first token pair, scanning the
   view QR code.

**Done when:** two tamales on different machines show up in the hub's session list on a
phone, either can be driven from it (prompt, interrupt, approve, code browser, artifacts),
restarting the hub or cutting a tamale's network recovers an open session page without a
reload, and a restarted tamale reappears in the session list without a reload.

### Phase 2: End-to-end encryption between tamale and browser

The hub currently sees plaintext. Self-hosters control the hub themselves, but E2E also
protects against someone who gets into the hub box, and it's needed before anyone else
could run a hub for you.

- tamale generates a 256-bit pairing key once per machine (see Decisions) and `/web remote`
  shows the URL as `https://hub…/s/<sid>/#k=<key>`. Browsers don't send the fragment to the server.
  This key is separate from the view token: the hub must check the view token, so it
  can't also be the secret the hub never sees.
  `app.js` moves it to IndexedDB (non-extractable `CryptoKey`) and clears it from the
  address bar.
- `msg.data` becomes AES-256-GCM sealed (random 96-bit nonce, direction byte and
  counter in the associated data to prevent replays and reflection). WebCrypto in the
  page, the `aes-gcm` crate in tamale.
- Artifacts then **must not** go through `get` in plaintext. Send them over the sealed
  socket and render them with `<iframe sandbox="allow-scripts allow-popups allow-modals allow-forms" srcdoc=…>`
  plus a `<meta>` CSP. `status.title` and `label.cwd` are sealed too, and the session list
  decrypts them in the page using the keys it holds.
- Known limit, to state in the docs: the hub serves `app.js`, so a compromised hub
  could serve code that steals the key. E2E prevents passive reading, not an active
  attacker on the hub. Native apps (later) fix this.

### Later

- `tamale remote`: a desktop client that serves the embedded UI on `127.0.0.1` and uses
  the hub only as a pipe (removes the hub-served-JS caveat on desktops).
- Signed GitHub releases, then native apps (Tauri 2 wrapping `src/web/*`; APK on GitHub,
  iOS via TestFlight).
- Web Push notifications when a session is waiting for approval.
- Built-in ACME TLS.

## Testing

- **Hub, in-process**: start `serve` on `127.0.0.1:0` with a temp data dir, connect a fake
  agent with `tokio-tungstenite`, open a browser socket with the view token cookie and
  `Origin`, and check `open` / `msg` / `close` forwarding and `get` tunnelling. Bad or
  revoked tokens, an agent token used as a view token (and vice versa), a view token from
  another pair (404), wrong `Origin`, an oversize frame, a `ui` the hub doesn't serve and a
  duplicate `sid` from another pair are all rejected. A duplicate `sid` from the same pair
  replaces the old connection. `/s/<sid>/app.js` is the embedded copy whatever the agent
  does, artifact responses always carry the sandbox CSP and `text/html` whatever `got`
  says, and an agent drop gives `503` during the grace period and `4000` after it.
- **Cookie**: `/api/tokens` sets an `HttpOnly; SameSite=Strict` cookie (no `Secure` with
  `--insecure`), merges tokens, rejects unknown ones and drops revoked ones.
- **State**: token pair new/list/revoke/rotate, hashes only on disk, concurrent CLI
  edits.
- **tamale**: extend the existing `web.rs` tests (which start `Server::start(false)`) so
  a relayed client gets the same `welcome` and updates as a local tab, and
  `artifact_page()` returns the same body as the HTTP route.
- **Manual**: phone on mobile data, hub on a VPS, tamale on two other VPSs. Restart each
  piece mid-turn.

## Decisions

Settled while finalizing this plan; revisit only with a reason.

- **`/web remote` in `auto` mode only warns.** Switching modes silently would change how
  the terminal session behaves too, and the remote page shows the mode and can change it.
- **Session list: one flat list** across all view tokens, sessions waiting for approval
  first, then busy, then by last activity. Each row shows host, cwd, title and, when the
  browser holds more than one token, the pair name.
- **Idle sessions stay listed while connected**, with "idle 3d" and the like. The hub keeps
  no sessions of its own: one disappears 10 minutes after its tamale disconnects.
- **Phase 2 key scope: one pairing key per machine**, saved in tamale's data dir and shared
  by every session on it, so you scan once per machine. `/web remote rekey` replaces it.
