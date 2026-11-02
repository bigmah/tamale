# tamale-hub

A small self-hosted relay that makes tamale sessions reachable from a phone or another
computer, wherever they run, without port forwarding or a VPN. Each tamale connects
**out** to the hub with an agent token; a browser holding the matching view token lists
the sessions and opens one in tamale's own web UI. The design and threat model are in
[`../tunnel_plan.md`](../tunnel_plan.md).

```
https://tamale-hub.com             the hub's pages: session list, tokens, /open/<sid>
https://<sid>.tamale-hub.com       one session, its own origin; its page comes from that tamale
wss://tamale-hub.com/agent         where tamales connect
```

## Using it

On every machine that runs tamale (the agent token can't read or drive anything):

```sh
export TAMALE_HUB_URL=wss://tamale-hub.com
export TAMALE_HUB_TOKEN=tha_…
```

Then in tamale, `/web remote` connects and shows the session's link with a QR code
(`/web remote off` disconnects). With `[hub] connect = "auto"` in tamale's settings it
connects on startup instead, and the footer shows `hub ●`.

On your phone, open the **view link** (`https://tamale-hub.com/#thv_…`) once, by scanning
its QR code. The browser keeps the token in an `HttpOnly` cookie, and the hub page lists
your sessions from then on. Add it to the home screen for an app-like icon.

**The view token controls every session of its pair.** Treat it like an SSH key, and
`rotate --view` it if it leaks. In tamale's default `auto` permission mode, a remote
prompt runs tools without asking; tamale warns when connecting.

## Tokens

**From the page** (with `serve --signup open`, as tamale-hub.com runs): anyone can make a
token pair on the hub's landing page. The browser that makes it keeps the view half; the
page shows the agent half's `export` lines and a QR code of the view link once. Whoever
holds a pair's view token can make a new agent token, a new view link, or delete the
pair from the same page. Limits: 5 new pairs per IP per day, `--max-pairs` (default 500)
in all; page-made pairs that never connect are deleted after 7 days, and after 90 days
unused. With `--signup closed` (the default) only the CLI makes pairs.

**From the CLI** (pairs made this way never expire):

```sh
tamale-hub token new <name>                  # prints both halves once, with a QR code
tamale-hub token list
tamale-hub token rotate <name> --agent       # re-export on your machines; phone unaffected
tamale-hub token rotate <name> --view        # rescan on your phone; machines unaffected
tamale-hub token revoke <name>               # drops the pair's sessions and tabs
```

On the server, run these as the service user so the files keep their owner:
`sudo -u tamale-hub tamale-hub token … --data-dir /var/lib/tamale-hub --public-url https://tamale-hub.com`.
A running hub picks up changes within two seconds (or on `systemctl reload tamale-hub`).

The data dir holds `state.json` (mode 600): SHA-256 hashes of the tokens and the secret
that signs session cookies. No tokens in plain text, no sessions, no history.

## Installing

Needs a box with a public IP, a domain whose DNS has an API Caddy can use (for the
wildcard certificate), and Caddy in front for TLS. The production setup on
`5.78.153.154` is recorded step by step in `tunnel_plan.md` under *Infrastructure setup*;
in short:

1. DNS: `A` records for `hub.example` and `*.hub.example` to the box.
2. A DNS-API credential for Caddy limited to the `_acme-challenge` TXT record.
3. Caddy built with the DNS plugin (`xcaddy build --with github.com/caddy-dns/route53`,
   with the Go version Caddy's release was built with), and:

   ```
   hub.example, *.hub.example {
       tls {
           dns route53
       }
       reverse_proxy 127.0.0.1:8787
   }
   ```

4. `cargo build -p tamale-hub --release`, then a systemd unit running
   `tamale-hub serve --listen 127.0.0.1:8787 --public-url https://hub.example --data-dir /var/lib/tamale-hub`
   as its own user (see `tunnel_plan.md` for the hardened unit). Add `--signup open` to
   let visitors make their own tokens.
5. `tamale-hub token new mine`.

To try it locally without DNS or TLS, browsers resolve `*.localhost` to this machine:

```sh
tamale-hub serve --insecure --public-url http://hub.localhost:8787 --data-dir /tmp/hub
TAMALE_HUB_URL=ws://hub.localhost:8787 TAMALE_HUB_TOKEN=tha_… tamale
```

## Testing

`cargo test -p tamale-hub` runs the hub in-process with a fake agent through the whole
ticket, cookie, tunnel and socket flow, and the rejections. Against a deployed hub:

```sh
TAMALE_HUB_URL=wss://tamale-hub.com TAMALE_HUB_TOKEN=tha_… TAMALE_HUB_VIEW=thv_… \
    cargo test -p tamale real_hub -- --ignored
```
