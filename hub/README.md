# tamale-hub

A small self-hosted relay that makes tamale sessions reachable from a phone or another
computer, wherever they run, without port forwarding or a VPN. Each tamale connects
**out** to the hub with a key; a browser holding the same key lists the sessions and opens
one in tamale's own web UI. The key design is in [`../single_key.md`](../single_key.md),
the relay's in [`../tunnel_plan.md`](../tunnel_plan.md).

```
https://tamale-hub.com             the hub's pages: making keys, session list, /open/<sid>
https://<sid>.tamale-hub.com       one session, its own origin; its page comes from that tamale
wss://tamale-hub.com/agent         where tamales connect
```

## Using it

1. **Make a key** on the site. It's made in your browser, and the hub never sees it.
2. **On each machine that runs tamale**, set it and run `/web remote` in tamale:

   ```sh
   export TAMALE_HUB_KEY=thk_…
   ```

   tamale never makes a key: without one, `/web remote` says where to get one. It connects to
   tamale-hub.com unless `[hub] url` or `TAMALE_HUB_URL` says otherwise. `/web remote off`
   disconnects, and `[hub] connect = "auto"` connects on startup.
3. **On your phone**, scan the key's QR code from the site, once. Your sessions are listed
   there; tap one to drive it. Add the page to the home screen for an app-like icon.

The site's **Show** button brings a key's export line and code back on a browser that holds it,
which is how a second machine gets it. **New key** replaces it, and every machine and browser
using it needs the new one. **Delete** ends it.

**A key gives access to every machine that uses it.** Treat it like an SSH key. A compromised
machine exposes the others on its key: in tamale's default `auto` permission mode, whoever holds
the key can run commands on them without approval, and tamale warns when connecting. Tools tamale
runs as you can read the key, so a prompt injection could steal it. **So machines you trust alike
share a key, and a throwaway VPS gets its own.** A browser can hold several, and the list merges
them.

**What encryption covers.** Sessions are end-to-end encrypted between tamale and the browser, and
the hub only moves opaque bytes: it can't read prompts, replies, tool output, files, artifacts,
titles or directories, and can't forge messages, so it can't type a prompt or approve a tool.
`[hub] e2e = false` turns this off, for debugging.
**What it doesn't:** the hub still serves the page code, so someone in control of it could
change that code to steal a key the next time a page loads, or as one is made. It still sees
metadata (which sessions exist and when, from which IPs, busy or not, message sizes and timing).
And it can't protect a compromised laptop or phone. Details are in [`../e2e.md`](../e2e.md).

## Keys

**From the page** (with `serve --signup open`, as tamale-hub.com runs): anyone can make a key on
the hub's landing page. Limits: 5 new keys per IP per day, `--max-keys` (default 500) in all;
page-made keys that never connect are deleted after 7 days, and after 90 days unused. With
`--signup closed` (the default) only the CLI makes them.

**From the CLI**, for hubs with closed signup (keys made this way never expire):

```sh
tamale-hub key new <name>       # prints the key once, with a QR code
tamale-hub key list
tamale-hub key revoke <name>    # drops the key's sessions and tabs
```

On the server, run these as the service user so the files keep their owner:
`sudo -u tamale-hub tamale-hub key … --data-dir /var/lib/tamale-hub --public-url https://tamale-hub.com`.
A running hub picks up changes within two seconds (or on `systemctl reload tamale-hub`).

The data dir holds `state.json` (mode 600): SHA-256 hashes of the keys' auth values and the secret
that signs session cookies. No keys, no sessions, no history.

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
   let visitors make their own keys.
5. Make a key on the site, or with `tamale-hub key new mine`.

To try it locally without DNS or TLS, browsers resolve `*.localhost` to this machine:

```sh
tamale-hub serve --insecure --public-url http://hub.localhost:8787 --data-dir /tmp/hub
TAMALE_HUB_URL=ws://hub.localhost:8787 TAMALE_HUB_KEY=thk_… tamale
```

## Testing

`cargo test -p tamale-hub` runs the hub in-process with a fake agent through the whole
ticket, cookie, tunnel and socket flow, and the rejections. Deploy the hub and tamale together:
a tamale from before keys gets a 401 from a hub with them, and the other way round. Against a
deployed hub:

```sh
TAMALE_HUB_URL=wss://tamale-hub.com TAMALE_HUB_KEY=thk_… cargo test -p tamale real_hub -- --ignored
```
