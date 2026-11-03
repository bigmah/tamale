# Remote access

tamale-hub is a small relay that makes tamale sessions reachable from a phone or another
computer, wherever they run, without port forwarding or a VPN. Each tamale connects **out** to
the hub with a key; a browser holding the same key lists the sessions and opens one in tamale's
own web UI. tamale uses [tamale-hub.com](https://tamale-hub.com) unless you
[host your own](self-hosting.md).

```
https://tamale-hub.com             the hub's pages: making keys, session list, /open/<sid>
https://<sid>.tamale-hub.com       one session, its own origin; its page comes from that tamale
wss://tamale-hub.com/agent         where tamales connect
```

The designs are in [`single_key.md`](../single_key.md) (keys),
[`tunnel_plan.md`](../tunnel_plan.md) (the relay) and [`e2e.md`](../e2e.md) (encryption).

## Using it

1. **Make a key** on the site. It's made in your browser, and the hub never sees it.
2. **On each machine that runs tamale**, set it and run `/remote` in tamale, or pick the **Remote**
   tab in its footer:

   ```sh
   export TAMALE_HUB_KEY=thk_…
   ```

   tamale never makes a key: without one, `/remote` says where to get one. It connects to
   tamale-hub.com unless `[hub] url` or `TAMALE_HUB_URL` says otherwise. `/remote off`
   disconnects, and `[hub] connect = "auto"` connects on startup.
3. **On your phone**, scan the key's QR code from the site, once. Your sessions are listed
   there; tap one to drive it. Add the page to the home screen for an app-like icon.

The site's **Show** button brings a key's export line and code back on a browser that holds it,
which is how a second machine gets it. **New key** replaces it, and every machine and browser
using it needs the new one. **Delete** ends it.

Keys made on the site are limited to 5 new keys per IP per day. Keys that never connect are
deleted after 7 days, and keys unused for 90 days are deleted too.

## What a key gives access to

**A key gives access to every machine that uses it.** Treat it like an SSH key. A compromised
machine exposes the others on its key: in tamale's default `auto` permission mode, whoever holds
the key can run commands on them without approval. Tools tamale runs as you can read the key, so
a prompt injection could steal it. **So machines you trust alike share a key, and a throwaway VPS
gets its own.** A browser can hold several, and the list merges them.

## What encryption covers

Sessions are end-to-end encrypted between tamale and the browser, and the hub only moves opaque
bytes: it can't read prompts, replies, tool output, files, artifacts, titles or directories, and
can't forge messages, so it can't type a prompt or approve a tool. `[hub] e2e = false` turns this
off, for debugging.

**What it doesn't cover:** the hub still serves the page code, so someone in control of it could
change that code to steal a key the next time a page loads, or as one is made. It still sees
metadata (which sessions exist and when, from which IPs, busy or not, message sizes and timing).
And it can't protect a compromised laptop or phone. Details are in [`e2e.md`](../e2e.md).
