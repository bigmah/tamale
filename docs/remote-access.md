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
   tamale-hub.com unless `[hub] url` or `TAMALE_HUB_URL` says otherwise. It shows the session's
   QR code; `esc` goes back to the TUI and stays connected, and the footer's tab says **Remote
   on**, **connecting** or **off**, and how many are viewing. `/remote` or the tab shows the code again. `/remote off`, or `x` on that
   screen, disconnects, and `[hub] connect = "auto"` connects on startup.
3. **On your phone**, scan the key's QR code from the site, once. Your sessions are listed
   there; tap one to drive it. Add the page to the home screen for an app-like icon, or use the
   [iPhone app](ios.md), which keeps the key in the Keychain and artifacts on the phone.

The site lists your sessions beside the one you're in, and sessions you've opened keep running
behind it (the four most recent), so switching back is instant. `Alt+↑`/`Alt+↓` moves between
them and `Alt+1`–`9` jumps. Each session still runs on its own origin, in a frame only the hub's
own page may embed.

The site's **Show** button brings a key's export line and code back on a browser that holds it,
which is how a second machine gets it. **New key** replaces it, and every machine and browser
using it needs the new one. **Delete** ends it.

Keys made on the site are limited to 5 new keys per IP per day. Keys that never connect are
deleted after 7 days, and keys unused for 90 days are deleted too.

## The chef

`tamale chef` (or `/chef` in any session) starts the [tamale chef](chef.md), an agent you can ask
about all your sessions on the key: what they're doing, which need you, and questions they answer
on the side without it touching their conversations.

## Driving a session from a terminal

With `TAMALE_HUB_KEY` set, `/sessions` or the footer's **Sessions** tab also lists your sessions on
other machines. `Enter` opens one in place of this session, which goes on behind it: its transcript,
its approvals, and a prompt that sends there, slash commands included. `esc` interrupts it if it's
working, and otherwise comes back; `ctrl+c` always comes back, leaving it running. `x` on one in the
list stops it, as `tamale kill` would on its machine (twice if it's mid-turn): it stays saved there,
for `tamale --continue`.

A session that ends (closed from a terminal or the iPhone app, `/quit`, `tamale kill`) tells the
hub, which drops it from the list straight away. One that just loses its connection is listed as
offline for ten minutes, in case it comes back.

It uses the same encrypted channel as a browser, and refuses anything unsealed, so a session with
`[hub] e2e = false` only opens in a browser. Replies to prompts sent from a terminal are ordinary
ones rather than artifacts. An artifact someone else asked for shows as one line with its title, and
opens in the web UI.

## What a key gives access to

**A key gives access to every machine that uses it.** Treat it like an SSH key. A compromised
machine exposes the others on its key: in tamale's default `auto` permission mode, whoever holds
the key can run commands on them without approval. tamale keeps the key out of the environment of
what it runs, so it doesn't turn up in a transcript by accident, but tools run as you and can read
it wherever else you keep it, like your shell profile, so a prompt injection could steal it. **So machines you trust alike share a key, and a throwaway VPS
gets its own.** A browser can hold several, and the list merges them.

## What encryption covers

Sessions are end-to-end encrypted between tamale and the browser, and the hub only moves opaque
bytes: it can't read prompts, replies, tool output, files, artifacts, titles or directories, and
can't forge messages, so it can't type a prompt or approve a tool. `[hub] e2e = false` turns this
off, for debugging. Whether a session is encrypted is only the hub's word, so the terminal and the
iPhone app refuse anything unsealed: such a session only opens in a browser.

**What it doesn't cover:** the hub still serves the page code, so someone in control of it could
change that code to steal a key the next time a page loads, or as one is made. It still sees
metadata (which sessions exist and when, from which IPs, busy or not, which one is the
[chef](chef.md), message sizes and timing).
And it can't protect a compromised laptop or phone. Details are in [`e2e.md`](../e2e.md).
