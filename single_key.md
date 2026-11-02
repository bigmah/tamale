# One key for tamale-hub

**Status:** deployed to tamale-hub.com on 2026-11-02, and checked there in Chromium and WebKit. Not
yet tried on a real phone. Replaces the token pairs in `tunnel_plan.md` and the per-machine
pairing key in `e2e.md`. The encrypted channel in `e2e.md` stays as it is.

Today there are three secrets: an agent token and a view token per pair, and a pairing key per
machine. Using a session takes two scans, and the second is easy to miss. This plan makes it one
key, on every machine and browser that share it, and one scan per browser.

Keys come from one place: the hub's site. tamale never makes one; `/web remote` only checks that
one is set.

## How it's used

```
site:      Make a key             shows the export line and a QR code
phone:     scan the QR code       the phone now holds the key
laptop:    export TAMALE_HUB_KEY=thk_…, then /web remote
           its sessions appear on the phone; tap one to drive it
mac mini:  the same export line, then /web remote
vps:       Make a key again on the site, for a separate group
```

A key is a trust group. A browser can hold several, and the list merges them.

## Security

The key is an SSH key to every machine that shares it. A compromised machine exposes the others on
its key: in `auto` mode, whoever holds the key can run commands on them without approval. Tools
tamale runs as your user can read the key, so a prompt injection could steal it.

**So machines you trust alike share a key, and a throwaway VPS gets its own.** This is a deliberate
trade: today's per-machine pairing keys contain a compromised machine, and this gives that up for
simplicity. The page and README have to say so, in place of today's "treat the view link like an
SSH key", which has been out of date since encryption shipped.

The hub never receives the key, because the browser generates it. But the hub serves the page that
generates it, so a hub that tampers with its page code could steal a key as it's made. That's the
limit `e2e.md` already states for keys held by that page, and it already reaches every pairing key a
browser has, so it doesn't widen what such an attack gets.

Made better: a stolen agent token no longer lets anyone add sessions to your list, because only
holders of the key (and the hub itself) have what it takes to register one.

## The key

`thk_` and 32 random bytes in lowercase base32. Everything else is derived with HKDF-SHA256 (empty
salt):

| Info string | Result | Who sees it |
|---|---|---|
| `tamale-hub auth` | The auth value (32 bytes, hex): proves access to the hub | Hub, which stores only its SHA-256 |
| `tamale-hub e2e` | The PSK for the Noise handshake | tamale and browsers |

The machine id and label key keep coming from the PSK as they do now (`Keys::from_psk`), so the
handshake, sealed labels and `src/web/app.js` don't change. The session page still gets only the
PSK in its fragment: never the key or the auth value.

## tamale

- **The key comes only from `TAMALE_HUB_KEY`,** never a settings file or a file of its own. It
  replaces `TAMALE_HUB_TOKEN`.
- **`/web remote` with no key set** stops with: `Set TAMALE_HUB_KEY to use the hub. Make a key at
  https://tamale-hub.com` (the configured hub's address). It says so in the conversation and doesn't
  open the web session screen. If `TAMALE_HUB_TOKEN` is set, the message says it's been replaced.
- **Hub URL:** defaults to `wss://tamale-hub.com`; `[hub] url` or `TAMALE_HUB_URL` overrides it.
- **The terminal never shows the key.** Its QR code becomes `https://<hub>/open/<sid>` with nothing
  secret in it: a shortcut into the session for a browser that already holds the key. A browser
  that doesn't gets the page's "add your key" form.
- **Removed:** `hub-pairing.key` handling (the old file is deleted on the first connection with a
  key), `/web remote pair` and `/web remote rekey`, along with `Keys::load_or_create` and
  `Keys::rekey`.

## Hub

- **State:** `Pair { agent_hash, view_hash }` becomes `Key { name, auth_hash, created, last_used,
  web }`. Renames follow through `serve.rs`, including the `th` and `ts` cookies, which now use the
  auth value and its hash.
- **Agents:** `/agent` takes the auth value as the bearer token. The frames don't change, so no
  `PROTO` bump. An unknown key gets a 401, and tamale's error points to the site.
- **Routes:**
  - `POST /api/keys`: the browser registers a new auth value, with an optional label. Refused with
    `--signup closed`. Keeps the limits of 5 per IP per day and `--max-pairs`, renamed `--max-keys`.
  - `POST /api/keys/rotate`: swaps a held key's auth value for a new one, which disconnects every
    machine and browser on it until they get the new key.
  - `POST /api/keys/delete` replaces `/api/pairs/delete`.
  - `POST /api/tokens`: takes an auth value instead of a view token.
  - `/api/pairs` and `/api/pairs/rotate` go.
- **Expiry:** as now, 7 days if never connected and 90 days idle.
- **CLI:** `tamale-hub key new|list|revoke`, for whoever runs a hub with closed signup, where the
  site can't make keys. `key new` generates a key, prints it once and keeps only the hash. The
  derivation goes in `hub/protocol`, so the hub and tamale share it.
- **Removed:** `qr_svg` and `new_tokens`, since the hub must not see a key to draw its QR code.

## Page

- **Making a key:** `hub.js` generates it, derives the auth value and PSK, registers with `POST
  /api/keys`, and stores the key in IndexedDB. It then shows the export line and a QR code of
  `https://<hub>/#thk_…`, both drawn in the browser. This needs a small QR encoder vendored into
  `hub/static`. Hubs other than tamale-hub.com also get a `TAMALE_HUB_URL` line.
- **A key in the fragment:** derives the same values, adds the auth value with `/api/tokens`, stores
  the key and clears the fragment.
- **Pasting a key:** the "have a view link?" form becomes "have a key?", and takes a `thk_…` key
  on this side only: the key is never sent.
- **Lost cookie:** keys in IndexedDB re-add their auth value on load, so an expired cookie doesn't
  mean a rescan.
- **Key list:** each key gets Show (the export line and QR code again, since this browser holds the
  key, which is how a second machine gets it), New key (rotate) and Delete. A key this browser
  has only as a cookie, with IndexedDB cleared, can't be shown and has to be rotated.
- **Landing page:** three steps (make a key, set it where tamale runs and run `/web remote`, scan
  the code on your phone).
- **Removed:** the two-part secrets card, the scan screen and "unknown machine" rows. A session on
  a key this browser doesn't hold isn't listed.
- **Kept:** Forget. The trust section is rewritten per *Security*.

## Also fixing

Both of these made the two-token problem hard to diagnose, and neither depends on the key:

- **Hub errors are hidden.** While the web session screen is up, a refused or failed connection
  shows nothing; the error is only in the conversation behind it. The screen will show "connecting"
  and the latest hub error, in place of the hub section.
- **Esc drops the hub connection silently.** "Back to the terminal" also stops serving. The label
  will say so.

## Existing tokens

A clean break, with no transition. `state.json`'s `pairs` are ignored and dropped on the next write
(its cookie secret stays), and tamales using `tha_` tokens get a 401. Anyone using the live hub has
to make a key, update tamale and scan again, and hub and tamale have to deploy together. Before
deploying, check how many pairs on tamale-hub.com have been used by anyone else.

## Phases

1. **Fixes:** the two under *Also fixing*.
2. **Hub and derivation:** key state, routes, CLI, shared derivation, with the hub's tests moved
   over.
3. **tamale:** the env var and its checks, the keyless QR code, the default URL, removing pairing
   key handling.
4. **Page:** making keys in the browser, the QR encoder, fragment and paste handling, restoring
   cookies, the key list, removing the old screens.
5. **Docs:** README, `hub/README.md`, `config/default.toml`, `/help`, and notes in `tunnel_plan.md`
   and `e2e.md` pointing here.
6. **Deploy:** only with the go-ahead, after the check under *Existing tokens*.

## Testing

- **Derivation:** fixed key, with Rust and `seal.js` producing the same auth value and PSK, run
  through `noise.test.mjs` alongside the handshake vectors.
- **Hub never sees the key:** a stand-in hub logs everything from tamale and the page, and neither
  the key nor the PSK appears.
- **Rejections:** a rotated-away key gets a 401 and its sockets drop; `POST /api/keys` is refused
  with closed signup, past the limits, and for an auth value that isn't 64 hex characters or is
  already taken; `/api/tokens` refuses a `thk_` key itself, so a bug can't send one to the hub
  unnoticed.
- **tamale:** `/web remote` with no key shows the message and opens nothing; `TAMALE_HUB_TOKEN` gets
  its message; no key material on the terminal screen.
- **Screen:** a refused key shows its error on the web session screen.
- **QR code:** a code the page draws decodes to the exact link, at the length a real key gives.
- **Browser and phone, by hand:** the one thing never tested so far. On mobile data: make a key and
  scan it; the session opens without a second scan; a second machine with the same key appears;
  restart tamale and reopen; Show on the laptop gives the same key; rotate and see the old phone
  lose access; two keys merge in one list.
