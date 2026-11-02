# Plan: tamale-hub tunnels (remote access to tamale sessions)

Supersedes the routing and page-serving design in `plan.md`. Sessions get **a subdomain
each**, ngrok-style, so the hub is a plain pipe and every session is isolated by the
browser exactly as it is on localhost.

## Goal

Drive any running tamale session from a phone or another computer, wherever it runs (a
laptop, or one of many short-lived VPSs), without Tailscale or port forwarding.

```
 vps-a: tamale ──┐ outbound wss,              ┌── phone browser
 vps-b: tamale ──┼─ agent token ──► hub ◄──── ┤   (view token)
 laptop: tamale ─┘   (Hetzner VPS)            └── laptop browser

 https://tamale-hub.com            the hub's own pages: session list, tokens, /open
 https://<sid>.tamale-hub.com      one session; its page comes from that tamale
```

The hub has its own domain, **`tamale-hub.com`** (registered in Route 53, hosted zone
`Z05242741VGZGJYQ7RZLD`). The bare domain is the dashboard and every session is a
subdomain of it. `bigmah.com` and `pullspace.dev` are not touched.

## Status (2026-11-01)

**Phase 1 is built and deployed**; what's left is the manual phone check in *Done when*.

| Piece | State |
|---|---|
| DNS | `tamale-hub.com` and `*.tamale-hub.com` → `5.78.153.154` (Route 53, TTL 300) |
| IAM | user `caddy-acme`, inline policy `tamale-hub-acme` (step 3); its key is only in `/var/lib/caddy/.aws/credentials` on the VPS |
| Caddy | custom 2.10.2 + `caddy-dns/route53` v1.6.2 at `/usr/bin/caddy.custom`, selected by `update-alternatives`, stock binary diverted to `/usr/bin/caddy.default`. Wildcard and apex certificates issued. Backups of the original binary and Caddyfile in `/root/caddy-backup-20261101/` |
| Hub | `/usr/local/bin/tamale-hub`, systemd unit `tamale-hub`, data in `/var/lib/tamale-hub`, source synced to `/root/tamale` |
| Tokens | pair `mine`; both halves only in `~/.config/tamale/tamale-hub-mine.txt` on the laptop (mode 600), exports in `~/.config/tamale/hub.env` |
| Code | `hub/` (binary), `hub/protocol/` (frames), `src/web/relay.rs`, `[hub]` settings, `/web remote` |
| Tests | `cargo test --workspace` (hub in-process end to end, relay against a stand-in hub); `real_hub_relays_a_browser_end_to_end` (ignored) passed against the live hub |

**Updating the hub:** `rsync -az --delete --exclude target --exclude .git ./ root@5.78.153.154:/root/tamale/`,
then on the box `cd /root/tamale && nice cargo +1.93.0 build -p tamale-hub --release -j 2 &&
install -m 755 target/release/tamale-hub /usr/local/bin/ && systemctl restart tamale-hub`.
(There's no default rustup toolchain on the box; 1.93.0 is the one pullspace pins.)

**End-to-end encryption (Phase 2a to 2d, deployed 2026-11-01):** tamale seals relayed
channels, labels and artifacts by default; see `e2e.md`, including what's still to check by
hand on a phone. Upgrade the hub before tamales: a new hub accepts older ones, in plaintext,
but an older hub refuses new ones.

**Self-serve tokens (added 2026-11-01):** the landing page at `/` lets visitors make a
token pair (`POST /api/pairs`), and holders of a pair's view token rotate either half or
delete it (`/api/pairs/rotate`, `/api/pairs/delete`), so nobody needs SSH access. Limits:
5 pairs per IP per day, 500 in all (`--max-pairs`); page-made pairs (`web: true` in
`state.json`) expire after 7 days if never used and after 90 days idle, unless a session
is connected. The page says what end-to-end encryption does and doesn't cover. The
production unit runs with `--signup open`; the binary defaults to closed. Before
inviting people: make `github.com/bigmah/tamale` public (the page's install line points
there), and consider the separate-session-domain hardening below, since strangers' pages
then share a site with the dashboard.

**Where the build differs from this plan:**

- `/web remote` with no web server running starts the usual loopback one (random port,
  `127.0.0.1` only, token in the URL) rather than a listener-less mode. Simpler, and harmless.
- `[hub] connect = "auto"` connects on startup *without* taking over the terminal (the
  footer shows `hub ●`); `/web remote` takes over the terminal like `/web`, showing the
  hub link and its QR code.
- `serve` always needs `--public-url`, loopback or not.
- Session cookies are stateless and signed (HMAC with a secret in `state.json`), so they
  survive hub restarts; tickets stay in memory (60 s).
- After a hub restart, unknown sessions answer `503` for the first 10 minutes (their
  tamales are reconnecting) instead of `4000`.
- A same-pair reconnect closes the session's open tabs with `1012`; they reconnect and
  get `welcome`d by the new connection.
- `status` frames also carry the model; the hub's `close` frames from tamale carry a
  reason. Agent close codes are in `tamale_hub_protocol::close`.

### Non-goals (for now)

- Paid plans and accounts. Since 2026-11-01 tamale-hub.com does let anyone make a token
  pair on its page (`--signup open`; see Status), but each pair is still just tokens, and
  people who need more trust than "the operator could read and drive my sessions" should
  run their own hub until Phase 2.
- Native apps and a `tamale remote` desktop client.
- TLS in the hub itself. Caddy does TLS in front of it.
- Accounts, sign-in, passwords or passkeys. **Tokens are the only credential.**
- Sharing sessions between people or roles.

## Why subdomains

A browser isolates **origins** (scheme + host + port), not paths. On localhost every
tamale has its own port, so its page can't touch another tamale's. If the hub put every
session under one host (`tamale-hub.com/s/<sid>/`), a page from one session would share
an origin, and the cookie, with all of them. A tamale registered with a leaked agent
token could then serve script that drives every real session.

With `<sid>.tamale-hub.com`, each session is its own origin again. So:

- The hub passes tamale's own `index.html`, `app.js`, `app.css` and artifacts through
  unchanged. A tamale always gets the page that matches it, so there's **no version
  coupling** between hub and tamale.
- A malicious session's script can only reach its own session.
- The view token lives only on `tamale-hub.com` and never reaches a session origin.

This is the pattern ngrok (tunnels on their own subdomains), GitHub
(`githubusercontent.com`) and others use for content they don't control.

## Infrastructure setup (Hetzner + Route 53)

One-time setup, about 30–45 minutes. `hub/README.md` repeats this as the install guide
(written for a fresh box; the steps below are for the existing one).

### 1. The existing Hetzner VPS (`5.78.153.154`)

The hub goes on the box that already serves the other sites through Caddy. It's a relay
and uses very little CPU or memory, so it can share. Checked on 2026-11-01:

| | |
|---|---|
| OS | Ubuntu 24.04.3, x86_64, 2 vCPU, 1.9 GiB RAM + 4 GiB swap, 18 GB disk free |
| Caddy | 2.10.2 from the apt package, `/usr/bin/caddy`, stock unit (`caddy run --environ`), no drop-ins, no DNS plugins, not diverted |
| Sites | `pullspace.dev`, `www.pullspace.dev`, `bigmah.com`, `www.bigmah.com`; no wildcards, nothing for `tamale-hub.com` |
| DNS | `tamale-hub.com` is its own Route 53 hosted zone, delegated and resolving publicly |
| Ports | 22, 80, 443 (tcp + udp) and Caddy's admin API on `127.0.0.1:2019`; **8787 is free** |
| IPv6 | none configured, so **no AAAA records** |
| Firewall | `ufw`: OpenSSH and 80,443/tcp allowed (v4 and v6). Nothing to change |
| Rust | rustup toolchain in `/root/.cargo` (pullspace is built on the box) |

The hub uses `tamale-hub.com`, a separate domain, so nothing about the existing sites
conflicts with the new block, and Caddy's DNS credentials (step 3) can't touch their
zones.

### 2. Route 53 DNS records

In the `tamale-hub.com` hosted zone (`Z05242741VGZGJYQ7RZLD`, made by the registrar and
holding only NS and SOA as of 2026-11-01), add (no AAAA records; the box has no IPv6):

| Name | Type | Value | TTL |
|---|---|---|---|
| `tamale-hub.com` | A | `5.78.153.154` | 300 |
| `*.tamale-hub.com` | A | `5.78.153.154` | 300 |

Check with `dig +short tamale-hub.com` and `dig +short anything.tamale-hub.com`.

### 3. An IAM user for Caddy's certificates

Let's Encrypt issues wildcard certificates only via the DNS-01 challenge, so Caddy has to
create a TXT record at `_acme-challenge.tamale-hub.com`. Give it an IAM user that can do
only that:

- IAM → Users → create `caddy-acme` (no console access), and attach an inline policy
  (scoped to the `tamale-hub.com` zone only):

  ```json
  {
    "Version": "2012-10-17",
    "Statement": [
      {
        "Effect": "Allow",
        "Action": ["route53:ListHostedZones", "route53:ListHostedZonesByName"],
        "Resource": "*"
      },
      {
        "Effect": "Allow",
        "Action": ["route53:GetChange"],
        "Resource": "arn:aws:route53:::change/*"
      },
      {
        "Effect": "Allow",
        "Action": ["route53:ListResourceRecordSets"],
        "Resource": "arn:aws:route53:::hostedzone/Z05242741VGZGJYQ7RZLD"
      },
      {
        "Effect": "Allow",
        "Action": ["route53:ChangeResourceRecordSets"],
        "Resource": "arn:aws:route53:::hostedzone/Z05242741VGZGJYQ7RZLD",
        "Condition": {
          "ForAllValues:StringEquals": {
            "route53:ChangeResourceRecordSetsNormalizedRecordNames": ["_acme-challenge.tamale-hub.com"],
            "route53:ChangeResourceRecordSetsRecordTypes": ["TXT"]
          }
        }
      }
    ]
  }
  ```

  Check the action list against the `caddy-dns/route53` README when setting it up; if
  issuance fails with an access error, the plugin may need one more read action.
- Create an access key for the user. It goes on the VPS only, in step 4.

### 4. Caddy with the Route 53 plugin

The same Caddy serves the other two sites, so this is the step to do carefully.

- **The risk:** a later `apt upgrade` replaces a custom binary with the stock one, which
  lacks the plugin. Caddy then refuses the Caddyfile (`module not registered:
  dns.providers.route53`) and **every site on the box** goes down on the next restart.
  Avoid it with Caddy's documented diversion for custom binaries, so apt upgrades the
  stock binary aside and leaves yours in place.
- **What was done** (2026-11-01). Two shortcuts didn't work and are worth knowing about:
  caddyserver.com's download API ignored `version=v2.10.2` and returned 2.11.7, and an
  `xcaddy` build of v2.10.2 with Go 1.27 **panicked when validating the existing
  Caddyfile** (`expected ModuleMap…`). Building v2.10.2 with Go 1.25 worked. Always run
  `validate` on the existing config before switching:

  ```sh
  mkdir -p /root/caddy-backup-20261101 && cp -a /usr/bin/caddy /etc/caddy/Caddyfile /root/caddy-backup-20261101/
  # A throwaway Go in /root/caddy-build (removed afterwards):
  GOTOOLCHAIN=go1.25.5 xcaddy build v2.10.2 --with github.com/caddy-dns/route53@v1.6.2 --output /root/caddy-build/caddy125
  /root/caddy-build/caddy125 validate --config /etc/caddy/Caddyfile --adapter caddyfile   # "Valid configuration"
  dpkg-divert --divert /usr/bin/caddy.default --rename /usr/bin/caddy
  install -m 755 /root/caddy-build/caddy125 /usr/bin/caddy.custom
  update-alternatives --install /usr/bin/caddy caddy /usr/bin/caddy.default 10
  update-alternatives --install /usr/bin/caddy caddy /usr/bin/caddy.custom 50
  systemctl restart caddy        # a few seconds' blip for the other sites; checked after
  ```

- **Rolling back:** `update-alternatives --set caddy /usr/bin/caddy.default`, remove the
  hub block from the Caddyfile, `systemctl restart caddy`.
- **Upgrading Caddy later:** `apt upgrade` now only updates `/usr/bin/caddy.default`. To
  move the custom build, rebuild with `xcaddy` for the new version (with the Go version
  that release was built with), `validate` the Caddyfile with it, then replace
  `/usr/bin/caddy.custom` and restart.
- **Credentials: not in the environment.** The stock unit runs `caddy run --environ`,
  which prints every environment variable to the journal at startup, so an
  `EnvironmentFile` would put the secret key in `journalctl`. Use the AWS shared
  credentials file in the `caddy` user's home (`/var/lib/caddy`), which the AWS SDK in
  the plugin reads by default:

  ```sh
  sudo install -d -m 700 -o caddy -g caddy /var/lib/caddy/.aws
  sudo install -m 600 -o caddy -g caddy /dev/null /var/lib/caddy/.aws/credentials
  sudo -e /var/lib/caddy/.aws/credentials
  ```

  ```ini
  [default]
  aws_access_key_id = …
  aws_secret_access_key = …
  region = us-east-1
  ```

  If the plugin doesn't pick it up, set its `profile` option or add
  `Environment=AWS_SHARED_CREDENTIALS_FILE=/var/lib/caddy/.aws/credentials` in a
  `systemctl edit caddy` drop-in (a path, so harmless in the journal).
- **Append** this block to the existing `/etc/caddy/Caddyfile`; don't touch the other
  sites' blocks. The `tls { dns route53 }` applies only here, so the other sites keep
  getting their certificates as they do now:

  ```
  tamale-hub.com, *.tamale-hub.com {
      tls {
          dns route53
      }
      reverse_proxy 127.0.0.1:8787
  }
  ```

  The wildcard doesn't cover `tamale-hub.com` itself, hence both names. Caddy keeps the
  `Host` header and sets `X-Forwarded-For`, which the hub relies on. Leave access logging
  off (Caddy's default).
- Apply without dropping connections:
  `caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile && sudo systemctl reload caddy`.
  A config that fails to load is rejected and the old one keeps running.
- Check: `curl -I https://tamale-hub.com` and `curl -I https://x.tamale-hub.com` both get
  a valid certificate (a 502 is expected until the hub runs), and the two existing sites
  still load. If issuance stalls on propagation, add `resolvers 1.1.1.1` inside the `tls`
  block. Logs: `journalctl -u caddy -f`.

### 5. tamale-hub as a service

- Build on the VPS, as pullspace already is: clone the repo to `/root/tamale` and run
  `cargo build -p tamale-hub --release -j1` (`-j1` keeps the 2 vCPUs and 1.9 GiB from
  starving the sites; the 4 GiB swap absorbs peaks). Then
  `install -m 755 target/release/tamale-hub /usr/local/bin/`. Alternatively build on the
  laptop with `cargo zigbuild -p tamale-hub --release --target x86_64-unknown-linux-gnu`
  and `scp` the binary.
- A system user and data dir:
  `sudo useradd --system --home /var/lib/tamale-hub --create-home tamale-hub`.
- `/etc/systemd/system/tamale-hub.service`:

  ```ini
  [Unit]
  Description=tamale-hub
  After=network-online.target
  Wants=network-online.target

  [Service]
  User=tamale-hub
  ExecStart=/usr/local/bin/tamale-hub serve --listen 127.0.0.1:8787 --public-url https://tamale-hub.com --data-dir /var/lib/tamale-hub
  ExecReload=/bin/kill -HUP $MAINPID
  Restart=on-failure
  NoNewPrivileges=true
  ProtectSystem=strict
  ProtectHome=true
  ReadWritePaths=/var/lib/tamale-hub
  PrivateTmp=true

  [Install]
  WantedBy=multi-user.target
  ```

  then `sudo systemctl enable --now tamale-hub`.
- First token pair:
  `sudo -u tamale-hub tamale-hub token new mine --data-dir /var/lib/tamale-hub`.
  Put the printed `export TAMALE_HUB_TOKEN=…` and `TAMALE_HUB_URL=wss://tamale-hub.com`
  on each machine that runs tamale, and scan the view QR code with your phone.

## Components

### 1. `tamale-hub` crate (new workspace member, `hub/`)

A binary built on axum and tokio, like tamale.

```
tamale-hub serve   --listen 127.0.0.1:8787 --public-url https://tamale-hub.com [--data-dir DIR]
tamale-hub token   new <name>     # prints an agent token, a view link and a QR code
tamale-hub token   list
tamale-hub token   revoke <name>  # kills both halves and drops their sockets
tamale-hub token   rotate <name> [--agent | --view]
```

- **Data dir**: `--data-dir`, else `$TAMALE_HUB_DATA`, else `~/.local/share/tamale-hub`.
  One `state.json`, written atomically (temp file + rename) under an advisory lock while
  the CLI edits it. `serve` reloads it when its mtime changes or on `SIGHUP`. No database.
- **`--public-url`** is the dashboard origin; session origins are
  `<scheme>://<sid>.<host>`. It's required unless `--listen` is loopback, and an `http://`
  URL is refused unless `--insecure` is passed. For local testing use
  `--insecure --public-url http://hub.localhost:8787`: browsers resolve `*.localhost` to
  loopback and treat it as a secure context, so the cookie rules below still work.
- The hub routes on the `Host` header: the dashboard host, a session host
  (`<sid>.<host>`, `sid` = 32 lowercase hex characters), `www.<host>` (redirected to the
  dashboard, since the wildcard DNS record catches it), or 404.

### 2. Tokens (the only credential)

`tamale-hub token new <name>` creates a **token pair**, the unit of access:

| Half | Prefix | Lives on | Can |
|---|---|---|---|
| Agent token | `tha_` | each machine running tamale (`TAMALE_HUB_TOKEN`) | Register sessions under this pair. **Cannot** list or drive anything |
| View token | `thv_` | your phone / browsers | List and open sessions registered with this pair's agent token |

Every VPS holds the agent token, so it *will* leak eventually. With the split, a leaked
agent token only lets someone add fake entries to your list, and because each session is
its own origin, opening one doesn't hand it anything. The view token never touches a VPS.

- Both halves are 32 random bytes (base32). The hub stores **only SHA-256 hashes** and
  compares them in constant time. `state.json` holds
  `{ name, agent_hash, view_hash, created, last_used }` per pair.
- `token new` prints the agent token once (with an `export TAMALE_HUB_TOKEN=…` line) and
  the **view link** `https://tamale-hub.com/#thv_…` with a terminal QR code (`qrcode`
  crate, as `/web` uses).
- One pair for everything is fine. More pairs separate groups of machines; a browser can
  hold several view tokens, and the list merges them.
- `revoke` deletes the pair and drops every socket and session cookie using it.
  `rotate --agent` replaces only the agent half (re-export on your machines), and
  `rotate --view` only the view half (rescan on your phone).

### 3. Cookies and opening a session

Two kinds of cookie, both `HttpOnly; Secure; SameSite=Strict; Path=/` and both with the
**`__Host-` prefix**, which forbids a `Domain` attribute. That stops a session subdomain
from setting or overwriting a cookie for the dashboard or another session ("cookie
tossing"), since all of them share a parent domain.

**On the dashboard (`tamale-hub.com`): `__Host-th`, the view tokens.**

1. You open the view link (or paste `thv_…` on `/`). The token is in the URL
   **fragment**, so it isn't sent to the server or logged.
2. The page clears the address bar (`history.replaceState`) and POSTs the token to
   `/api/tokens`. The hub checks that it's a current view token, merges it with the ones
   already in the cookie, drops revoked ones, and sets `__Host-th=<thv_a>,<thv_b>` with
   `Max-Age=34560000` (400 days, the browser maximum). "Forget tokens" clears it.

**On a session (`<sid>.tamale-hub.com`): `__Host-ts`, a ticket for that one session.**

1. You tap a session in the list, or open the link `/web remote` printed,
   `https://tamale-hub.com/open/<sid>`. That page's script POSTs to `/api/ticket` with
   the sid. That's a same-origin request, so the `Strict` cookie is sent even when you
   came from a QR scanner or a chat app.
2. If one of the cookie's view tokens belongs to the session's pair, the hub replies with
   a one-time ticket (32 random bytes, valid 60 s, bound to the sid). The page navigates
   to `https://<sid>.tamale-hub.com/__hub/auth?ticket=…`.
3. The hub consumes the ticket, sets `__Host-ts` (32 random bytes, bound to the sid and
   pair, held only as a hash in memory) and redirects to `/`.
4. A request to a session host without a valid `__Host-ts` (a reload after a hub restart,
   a bookmark, a cross-site link) is redirected to `https://tamale-hub.com/open/<sid>`,
   which repeats steps 1–3. You don't notice any of it.

`__Host-ts` lasts as long as the session's sid is known to the hub, and dies with
`revoke` or `rotate --view`. The paths under `/__hub/` on session hosts belong to the hub
and are never forwarded to tamale.

### 4. Routes

**Dashboard host (`tamale-hub.com`)**

| Route | Who | What |
|---|---|---|
| `GET /agent` (WebSocket) | tamale | `Authorization: Bearer tha_…`; carries the relay frames below |
| `GET /` | anyone | The hub's own page (no secrets): token paste box, session list |
| `POST /api/tokens` | page (`Origin` = dashboard) | `{"add":"thv_…"}` or `{"forget":true}`; sets or clears `__Host-th` |
| `GET /api/sessions` | page (`__Host-th`) | Sessions under the browser's view tokens, as JSON |
| `GET /open/<sid>` | anyone | A small page that runs the ticket flow above (no secrets in it) |
| `POST /api/ticket` | page (`__Host-th`, `Origin` = dashboard) | `{"sid":…}` → `{"url":"https://<sid>…/__hub/auth?ticket=…"}`, or 404 |

All dashboard responses send `X-Frame-Options: DENY`, a strict CSP
(`default-src 'self'; frame-ancestors 'none'`) and no CORS headers.

**Session host (`<sid>.tamale-hub.com`)**

| Route | Who | What |
|---|---|---|
| `GET /__hub/auth?ticket=…` | browser | Sets `__Host-ts`, redirects to `/` |
| `GET /`, `/app.js`, `/app.css`, `/artifact/<id>` | browser (`__Host-ts`) | Tunnelled to tamale as `get` (below) |
| `GET /ws` (WebSocket) | browser (`__Host-ts` + `Origin` = this session host) | One client channel of that session |

Any other path is 404. A session the browser's cookie doesn't cover is 404 (or the
redirect to `/open/<sid>`, which then 404s), so you can't confirm that someone else's
session exists. The `Origin` check on `/ws` matters: sibling subdomains count as the
same *site*, so `SameSite` alone wouldn't stop `aaa.tamale-hub.com` from opening `bbb.tamale-hub.com/ws`.

### 5. Relay protocol (tamale ↔ hub)

One WebSocket per tamale session, text frames, JSON. Shared types in a small crate
(`hub/protocol`, `tamale-hub-protocol`) used by both binaries, like `web/protocol`.

```jsonc
// tamale → hub, first frame
{"t":"hello","proto":1,"sid":"<32 hex>","label":{"host":"vps-b","cwd":"~/api","model":"claude-opus-5-5"}}
// hub → tamale
{"t":"welcome","proto":1}                         // or a close frame with a reason
// hub → tamale: a browser opened /ws
{"t":"open","c":7}
// both ways: one /web message for client 7, passed through unchanged
{"t":"msg","c":7,"data":"{\"t\":\"send\",\"text\":\"…\"}"}
{"t":"close","c":7}
// hub → tamale: tunnelled GET ("/", "/app.js", "/app.css", "/artifact/<id>" only)
{"t":"get","r":3,"path":"/artifact/9f2c…"}
{"t":"got","r":3,"status":200,"headers":[["content-type","text/html; charset=utf-8"],["content-security-policy","sandbox …"]],"body":"…"}
// tamale → hub, whenever it changes: shown in the session list
{"t":"status","busy":true,"approval":false,"title":"Fix flaky test"}
```

- **Headers in `got`**: the hub forwards only `content-type`, `cache-control`,
  `content-security-policy`, `referrer-policy` and `x-frame-options`, and always adds
  `X-Content-Type-Options: nosniff`. Never `Set-Cookie`. tamale sends the same headers its
  local routes send today, so artifacts keep their sandbox CSP.
- **`sid`**: 16 random bytes as 32 lowercase hex (a valid DNS label), made once per
  tamale process and kept across `/clear` and `/resume`. It names "this tamale", not a
  transcript. It isn't secret; every route also needs a cookie.
- **Duplicate `sid`**: from the same pair, the new connection replaces the old one (so a
  reconnect after a network drop doesn't wait for the ping timeout); from another pair,
  rejected.
- **Limits**: 4 MiB per browser → tamale message (as `/web` does today), 64 MiB per agent
  frame (a `welcome` carries the whole session), 16 clients per session, 64 sessions per
  pair, 30 s timeout on a `get`.
- **Liveness**: ping every 30 s both ways; drop a peer after 2 missed pongs.
- **Agent drops**: the hub closes that session's browser sockets with a normal code (not
  `4000`), lists the session as **offline** for 10 minutes and answers `/ws` with `503`
  meanwhile, so `app.js` keeps retrying and picks up when that tamale reconnects. After
  10 minutes the sid is forgotten; a browser socket to it is accepted and closed with
  `4000` "This session ended. Open the hub for the list.", so the page stops retrying and
  says why. A restarted tamale gets a new sid and appears in the list as a new session.

### 6. tamale side

**Config** (`config/default.toml`, new section):

```toml
[hub]
# url = "wss://tamale-hub.com"  # a tamale-hub to make this session reachable through
# token: set TAMALE_HUB_TOKEN rather than writing it here
connect = "manual"               # manual: only with /web remote; auto: on startup when url is set
```

Add `TAMALE_HUB_URL` and `TAMALE_HUB_TOKEN` overrides next to the existing `TAMALE_*` ones
in `src/config.rs`.

**Command**: `/web remote` connects to the hub (leaving any local or LAN server running)
and prints `https://tamale-hub.com/open/<sid>` with a QR code, plus a warning when the
permission mode is `auto`. `/web remote off` disconnects; `/web off` stops everything.
Change `/web [local|off]` to `/web [local|remote|off]` in `/help` (`src/app/help.rs`) and
the usage string in `src/app/commands.rs`. The footer shows a "hub" indicator with the
number of remote viewers. Connection failures (bad token, unreachable hub) show as an
error entry; the relay keeps retrying except after a rejected token.

**Code changes**

1. `src/web.rs`: split transport from session.
   - Extract `client()`'s per-client logic so a client is "an id and an
     `UnboundedSender<Out>`" plus a `remote` flag. Local axum sockets and relayed
     channels both register through `Shared::add_client(tx, remote) -> id` /
     `remove_client(id)`, and both feed `handle()`. Add a `remote_tabs()` count for the
     footer.
   - Extract the bodies of `page`, `style`, `script` and `artifact` into
     `fn asset(&Shared, path) -> Option<Asset>` (status, headers, body). The axum handlers
     keep their `own_host` / `authorized` checks and call it; the relay calls it
     directly, since the hub has already checked the browser.
   - Let `Server` run without a local listener (remote-only), since
     `WebRequest::Open { lan }` currently always binds one.
2. `src/web/relay.rs` (new): the outbound connection.
   - `tokio-tungstenite` with rustls as a direct dependency (0.29 is already in
     `Cargo.lock` through axum; reqwest has no WebSocket client).
   - Connect, send `hello`, then turn `open` / `msg` / `close` into
     `add_client` / `handle()` / `remove_client`, and send each client's `Out` back as
     `msg` (or `close` for `Out::Close`). Answer `get` with `asset()`.
   - Reconnect with backoff and jitter (1 s → 30 s). On reconnect every remote client
     counts as new and gets `welcome`d again.
   - Send `status` when busy, approval or title change (from `View` in `Server::sync`).
3. `src/web/app.js`: the one page change. Line 206 hard-codes `ws://`, which browsers
   block on an `https://` page. Use `wss:` when `location.protocol` is `https:`. The page's
   other URLs are root paths (`/app.js`, `/artifact/<id>`), which work unchanged on a
   session subdomain. The missing `?t=` is fine; the hub ignores it.
4. `src/main.rs`: handle `WebRequest::Remote { on }`, and connect on startup when
   `hub.connect = "auto"`.

## Security

Anyone who can send a `send` or `approve` message can run commands on that machine.
Treat a view token as an SSH key to every connected box.

- **View token**: a bearer key to every session in its pair. Share the view link by QR
  code, not chat; `rotate --view` if it leaks.
- **Agent token**: expected to leak. It can add fake sessions to your list. Opening one
  runs its script only on its own origin, with only its own `__Host-ts`. It can still show
  misleading content, so the list shows each session's host and pair name, rendered with
  `textContent`.
- **Origins**: dashboard and each session are separate origins. `__Host-` cookies prevent
  cookie tossing between them. Every state-changing dashboard endpoint and every `/ws`
  checks `Origin` exactly, because sibling subdomains are same-site.
- **Artifacts** keep tamale's sandbox CSP (no `allow-same-origin`), so they run in an
  opaque origin, send no cookies and fail the `/ws` `Origin` check.
- **TLS** is required in front of the hub (Caddy, above). Tokens and cookies never cross
  plain HTTP except with `--insecure` on `localhost`.
- **Permission mode**: the default `[permissions] mode = "auto"` means remote prompts run
  tools without asking. `/web remote` warns (it doesn't switch modes); the page's mode
  pill shows the mode and can change it.
- **Workspace exposure**: the code browser (`tree`, `open`, `grep`) and the artifacts
  gallery work remotely too. That's intended; say so in the README.
- **Rate limits**: per IP on token and ticket failures (`/agent`, `/api/tokens`,
  `/api/ticket`, `/__hub/auth`). The client IP is the TCP peer, or the last
  `X-Forwarded-For` entry when the peer is loopback (Caddy).
- **Logging**: connects, disconnects, pair names and IPs; never tokens, tickets, cookies
  or message payloads.
- **Optional hardening** (later): put sessions on a second registrable domain (dashboard
  `tamale-hub.com`, sessions `*.<another domain>`), as ngrok and GitHub do. Then sessions
  aren't even same-site with the dashboard. Costs a second domain and hosted zone, and the
  `__Host-` cookies plus `Origin` checks already cover the same-site gap.

## Phases

### Phase 1: Relay working end to end

1. Infrastructure steps 1–4 (VPS, DNS, IAM, Caddy). This is independent of the code and
   can be done first; Caddy returning 502 with a valid certificate on both names is done.
2. Refactor `src/web.rs` (transport split, `asset()`, remote-only `Server`), with no
   behaviour change; existing `web.rs` tests pass.
3. `wss:` fix in `app.js`.
4. `hub/protocol` crate with frame types and round-trip tests.
5. `tamale-hub`: state file, `token new|list|revoke|rotate`, `serve` with `Host` routing,
   the dashboard page (fragment → `/api/tokens`, paste box, session list polled every
   5 s), the ticket flow and all routes.
6. `src/web/relay.rs`, `[hub]` config, `/web remote`.
7. Infrastructure step 5 (systemd unit, first token pair) and `hub/README.md`.

**Done when:** two tamales on different machines show up in the hub's list on a phone on
mobile data, either can be driven from it (prompt, interrupt, approve, code browser,
artifacts), restarting the hub or cutting a tamale's network recovers an open session page
without a reload, and a restarted tamale reappears in the list without a reload.

### Phase 2: End-to-end encryption between tamale and browser (built; see `e2e.md`)

The hub sees plaintext. You control the hub, but E2E also protects against someone who
gets into the VPS.

- tamale generates a 256-bit pairing key once per machine, saved in its data dir and
  shared by every session on it (scan once per machine; `/web remote rekey` replaces
  it). `/web remote` shows `https://tamale-hub.com/open/<sid>#k=<key>`; the fragment
  survives the ticket redirects by being carried in `sessionStorage` on the way.
  `app.js` keeps it in IndexedDB as a non-extractable `CryptoKey`.
- `msg.data` becomes AES-256-GCM sealed (random 96-bit nonce; direction byte and counter
  in the associated data to prevent replay and reflection). WebCrypto in the page, the
  `aes-gcm` crate in tamale.
- Artifacts then go over the sealed socket instead of `get`, rendered with
  `<iframe sandbox="allow-scripts allow-popups allow-modals allow-forms" srcdoc=…>` and a
  `<meta>` CSP. `status.title` and `label.cwd` are sealed too.
- Known limit, for the docs: page code still comes through the hub, so a compromised hub
  could alter `app.js` to steal the key. E2E stops passive reading, not an active attacker
  on the hub. Native apps (later) fix this.

### Later

- `tamale remote`: a desktop client that serves the embedded UI on `127.0.0.1` and uses
  the hub only as a pipe.
- Signed releases, then native apps (Tauri 2 wrapping `src/web/*`).
- Web Push notifications when a session waits for approval.
- Built-in ACME so Caddy is optional.

## Testing

- **Hub, in-process**: start `serve` on `127.0.0.1:0` with a temp data dir and
  `--insecure --public-url http://hub.localhost:<port>`; set `Host` explicitly on each
  request. Connect a fake agent with `tokio-tungstenite` and check:
  - `open` / `msg` / `close` forwarding and `get` tunnelling, with only allowlisted
    headers forwarded and `Set-Cookie` dropped;
  - the full ticket flow: `/api/tokens` → `/api/ticket` → `/__hub/auth` → `/` and `/ws`;
  - rejections: bad or revoked tokens, agent token used as a view token and vice versa, a
    view token from another pair (404), a reused or expired ticket, a ticket for another
    sid, a `__Host-ts` for one sid presented on another, wrong `Origin` (including a
    sibling session's origin), an oversize frame, a duplicate `sid` from another pair, a
    path outside the tunnel allowlist, an unknown `Host`;
  - a duplicate `sid` from the same pair replaces the old connection;
  - an agent drop gives `503` during the grace period and `4000` after it.
- **State**: token pair new/list/revoke/rotate, hashes only on disk, concurrent CLI edits,
  `serve` picking up changes.
- **tamale**: extend the `web.rs` tests (`Server::start(false)`) so a relayed client gets
  the same `welcome` and updates as a local tab, and `asset()` returns the same bodies
  and headers as the HTTP routes.
- **Manual**: phone on mobile data, hub on the Hetzner VPS, tamale on two other machines.
  Restart each piece mid-turn. Open a session from the list, from the `/web remote` QR
  code and from a home-screen bookmark.

## Decisions

- **Subdomain per session**, not paths, for browser isolation (see Why subdomains).
- **`/web remote` in `auto` mode only warns**; switching would change the terminal
  session's behaviour too.
- **Session list: one flat list** across view tokens: waiting for approval first, then
  busy, then by last activity. Each row shows host, cwd and title, plus the pair name when
  the browser holds more than one token.
- **Idle sessions stay listed while connected** ("idle 3d"); one disappears 10 minutes
  after its tamale disconnects.
- **Phase 2 key scope: one pairing key per machine.** (Built; see `e2e.md`.)
