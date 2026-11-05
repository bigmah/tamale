# Plan: tamale for iPhone and iPad

**Status:** phases 0–2 are built for iPhone, except TestFlight (decision 4). iPad (phase 3,
apart from the artifact library) and notifications (phase 4) aren't started. Building and
testing are in [`README.md`](README.md); using the app is in [`docs/ios.md`](../docs/ios.md).

What differs from the plan below:

- Each key's name and hub live in its Keychain item rather than SwiftData, so the SwiftData
  store is only the artifact library: a cache that can be rebuilt if it won't open.
- `md` marks where each artifact was with a line `<!-- artifact N -->`, N indexing `artifacts`
  (which also carry `hash`). The app falls back to the page's HTML for tamales from before `md`.
- Hubs from before bearer auth ignore the header and list no keys; then the app signs in as the
  page does (the `th` cookie, a ticket, the `ts` cookie, `Origin`), so it works with
  tamale-hub.com before the hub change is deployed.
- Markdown is rendered by a small block parser in the app plus Foundation's inline markdown,
  not swift-markdown, so the app has no package dependencies.
- The hub serves `apple-app-site-association` only with `--apple-app-id`.

A native companion app for tamale sessions reached through tamale-hub. On iPhone it's the
remote control: see what a session is doing, answer approvals, send a prompt. On iPad it's
**the best place to read artifacts**: a full-screen library of every artifact your sessions
have made, with Apple Pencil notes on top, and a way to send those notes back to the agent as a
revision.

It's a client like the web UI (`src/web/app.js`). It speaks the same protocols, the hub
(`hub/protocol`) and the `/web` JSON messages (`src/web.rs`), through the same end-to-end
encryption (`e2e.md`). It doesn't need a new server, and tamale's behaviour stays the same.

## Why native, and not the web page in a wrapper

The hub site already works on a phone and can be added to the home screen. What a native app
adds:

| | Web page | App |
|---|---|---|
| Who serves the client code | The hub, every load (the weak spot named in `e2e.md`) | Signed by the App Store, so a hub that tampers with page code can't steal the key. **This is Phase 2e of `e2e.md`** |
| Where the key lives | IndexedDB on the hub's origin | Keychain, `ThisDeviceOnly`, optionally behind Face ID |
| Artifacts | Live only while the session runs | Cached locally, readable offline, kept after the session ends |
| Notes | None | PencilKit drawing and text notes on each artifact |
| Approvals | You only see them with the page open | Notifications (phase 4) |
| iPad layout | One stretched web page | Sidebar, split view, Stage Manager windows, keyboard shortcuts |

A `WKWebView` wrapper around the hub site gets none of the first three, so we don't build one,
not even as a first step.

## Scope

**In v1**

- Add a key by scanning the site's QR code (`https://<hub>/#thk_…`) or pasting `thk_…`.
  Several keys, as the site allows.
- Session list: every key's sessions, with labels decrypted, busy/approval badges, and
  online/offline state.
- Session view: the conversation (user, assistant, tool steps, notes, info, errors), the composer
  (send, queue, interrupt) and approvals (yes / always / no plus a reason).
- Artifacts: inline in the conversation, full-screen viewer, and the gallery (`artifacts`
  message). They're cached on the device.
- iPad: Pencil and text notes on artifacts, and **Revise with notes**.

**Later**

- Code browser (`tree`, `open`, `grep`), read-only with Ask (quote lines into the prompt).
- Approval notifications (needs the hub to change, see phase 4).
- iCloud sync of notes between iPhone and iPad.
- Mac Catalyst / macOS. The code should allow it, but it isn't a goal.

**Not planned:** making keys in the app (the site does that, and `single_key.md` keeps it
there), the `/browser` terminal mirror, or running tamale on the device.

## Architecture

```
apple/
  ios.md                 this plan
  project.yml            XcodeGen spec (the .xcodeproj is generated, not checked in)
  Tamale/                app target: SwiftUI, iOS + iPadOS 17+
    App/                 entry point, scenes, deep links
    Sessions/            list, session screen, composer, approvals
    Artifacts/           viewer, gallery, local library
    Notes/               PencilKit canvas, note model, revise flow
  Packages/
    TamaleKit/           Swift package, no UI, unit-tested on its own
      Key.swift          thk_ parsing, HKDF derivations (auth, e2e PSK, mid, meta key)
      Hub.swift          REST: tokens, sessions, ticket; session-host auth
      Noise.swift        NNpsk0_25519_AESGCM_SHA256, chunked transport
      Sealed.swift       sealed labels and status
      Channel.swift      one session socket: handshake, box/unbox, reconnect
      Messages.swift     Codable for the /web messages
      Session.swift      @Observable model: entries, status, approval, artifacts
  Fixtures/              test vectors and message samples produced by Rust
```

### Crypto: Swift on CryptoKit, checked against Rust

Everything e2e needs is in CryptoKit: `Curve25519.KeyAgreement`, `AES.GCM`, `SHA256`,
`HKDF<SHA256>`, `HMAC`. `src/web/noise.js` does the same job in about 230 lines over WebCrypto,
and a Swift port should be about that size.

Divergence is the risk, and test vectors control it. A small Rust test (or a `cargo run
--example`) writes `apple/Fixtures/e2e-vectors.json`, using tamale's own `src/web/e2e.rs` and `snow`
with fixed ephemerals. It covers key → auth/PSK/mid/meta key, handshake messages, transport
messages including chunked ones, and sealed labels. `TamaleKit` tests replay them byte for
byte. `noise.test.mjs` already does the same thing for the browser.

> [!NOTE]
> **Option considered:** take `e2e.rs` out into a `tamale-e2e` crate and ship it to Swift with
> UniFFI. That gives one implementation, but it adds an xcframework build, Rust iOS targets in
> CI, and a binding layer, all for about 300 lines of code that implement a fixed spec. Use the
> vectors now, and revisit if the protocol starts changing often.

### Talking to the hub

Today a browser authenticates with cookies and `Origin` checks:

1. `POST /api/tokens {"add": <auth value>}` (never the `thk_` key itself, which the hub
   refuses) adds it to the `th` cookie.
2. `GET /api/sessions` returns the list (with `sealed_label`, `sealed_status`, `mid`, `e2e`).
3. `POST /api/ticket {sid}` returns `<sid-origin>/__hub/auth?ticket=…`.
4. `GET` that URL to get the session host's `ts` cookie (the redirect lands on `/?e2e=1`).
5. `wss://<sid>.<hub>/ws`, which needs `Origin: https://<sid>.<hub>` and the `ts` cookie.
   (The page's `?t=` query is tamale's token for local `/web`; relayed sockets don't use it.)

A native client *can* do all of that with `URLSession` (it may set `Origin` itself, and it has
a cookie store). That's enough for a prototype, but it pretends to be a browser. **Hub change
(small, phase 0):** accept `Authorization: Bearer <auth value>` in place of the cookie on
`/api/sessions`, `/api/ticket` and the session socket, and skip the `Origin` check when a bearer
token is present. Origin checks protect against cookies that browsers attach on their own,
which can't happen with a header. The hub already reads `Authorization: Bearer` for tamale's
own `/agent` socket, so the parsing, `by_auth` lookup and `hub.fail(ip)` rate limiting are
there to reuse. On the session socket, a bearer request must name a key that owns `sid` (the
check `session_allowed` makes for the cookie), so no ticket round trip is needed. That needs no
frame changes and no `PROTO` bump, and it gets tests in `hub/src/serve.rs` next to the existing
ones (wrong key → 401 and counted as a failure, another key's session → 404, no `Origin` needed
with a bearer, cookie path unchanged).

The `/d/` and `/s/{sid}/` prefixes are internal (`rewrite` maps hosts onto them), so the app
uses the public hosts: `https://<hub>/api/...` and `wss://<sid>.<hub>/ws`.

### The /web messages

The app reads `entries`, `status`, `approval`, `artifacts`, `artifact`, `loading`, `older` and
`diff`, and sends `send`, `interrupt`, `approve`, `artifact`, `artifacts`, `older`, `diff` and
`close` (plus `tree`/`open`/`grep` later).
`close` ends the session as `tamale kill` would; tamale then tells the hub (`end`), which drops
it from the list and closes its sockets with 4000.
`entries {from, items}` replaces everything from `from` onward, so the model is just an array
that gets spliced.

**Long sessions.** The app's first handshake message says `{"v":1,"tail":60}`, because tamale
welcomes a tab before any message could ask. tamale then sends `loading {bytes}` and only the last
60 entries (at most 512 KB, and always at least one), so `from` is above 0. The model keeps that
offset as `base`. Scrolling up sends `older {before}`, answered by `older {before, from, items}`,
which the model drops if `before` isn't its `base` any more. Older tamales ignore the handshake
field and send everything; the web page and terminal clients don't ask, so they're unchanged.

**Diffs.** `diff {seq}` runs the same `diff::load` as the terminal's `/diff` and answers with whole
files, which the app folds as the terminal does (`Diff.swift`). A file over 4,000 lines keeps only
the lines near its changes, and files past 6 MB in total keep their counts but not their lines.

Three gaps for a native client:

1. **Assistant and note entries arrive as HTML** (`render::markdown_with` /
   `render::markdown`), with each artifact as a
   `<div class="artifact" data-src="/artifact/<id>">` placeholder. Showing that HTML in a web
   view per message would be slow and look out of place on iOS.
   **tamale change (additive):** also send `md` (the raw markdown, with artifact blocks taken
   out) on assistant and note entries, and `artifacts: [{id, title, done}]` on assistant
   entries. The web page ignores all three. The app renders markdown natively
   (swift-markdown, plus a small highlighter or none in v1) and puts artifact cards where the
   blocks were.
2. **Artifact ids aren't stable.** `/artifact/<id>` is a random id a tamale process hands out
   (`Artifacts::register`), so the same document gets a new id after a restart, and the saved
   file names use a 32-bit `DefaultHasher`, which isn't stable across Rust versions. The HTML
   the `artifact` reply carries also has tamale's fit and resizer scripts appended.
   **tamale change (additive):** add `hash` (SHA-256 hex of the document as the model wrote
   it, before tamale's scripts) to the `artifact` reply and to gallery items. The app keys its
   library on that. `sha2` is already a dependency.
3. **The messages are untyped `json!`** spread across `src/web.rs` and `src/web/files.rs`.
   Rather than write a schema up front, a Rust test writes one sample of each message to
   `apple/Fixtures/messages/*.json`, and `TamaleKit` decodes all of them in its tests. If a
   change in `src/web.rs` breaks the app's decoding, `cargo test` fails in the same change.
   That's the main reason the app lives in this repo.

If those messages settle down, move them into typed structs in a crate (with `typeshare` for
the Swift side). That's not required to start.

### Showing artifacts safely

Artifacts are model-written HTML with scripts. The web UI keeps them in a sandbox with an opaque
origin, and the app keeps the same guarantee:

- One `WKWebView` per artifact, with `WKWebsiteDataStore.nonPersistent()`, no
  `WKScriptMessageHandler`s, and no cookies, storage or access to the app.
- Load with `loadHTMLString(html, baseURL: nil)` and inject a CSP `<meta>` with the same
  source lists as tamale's `ARTIFACT_CSP` in `src/web.rs`: `default-src 'none'`, inline and
  https scripts and styles, images/fonts/media from `data:`/`blob:`/https, `connect-src https:`.
  (Its `sandbox` and `frame-ancestors` directives are ignored in a `<meta>`; the
  non-persistent store, nil base URL and lack of message handlers do that job here.)
- `WKNavigationDelegate` cancels every navigation after the first load, and sends link taps
  to Safari after asking.
- Sizing: the HTML tamale sends already includes `artifact-fit.js` (narrow-screen reflow) and
  a resizer that posts its height to `parent`. A `WKWebView` has no parent to hear it, so for
  inline cards read `document.documentElement.scrollHeight` with `evaluateJavaScript` after
  load and when the web view's `scrollView.contentSize` changes (KVO), not through a message
  handler (measuring the content, since `scrollHeight` is never less than the frame). The
  full-screen viewer just scrolls, edge to edge under a floating bar that hides as you read.
- The artifacts guide already designs for 340–1400px and touch, so iPhone and iPad widths
  are covered.

### Local library

SwiftData store, in the app's container with `NSFileProtectionComplete`:

- `Artifact`: `hash` (from tamale, see above), title, HTML, session title, host/cwd,
  created. Saved whenever an artifact finishes, or is opened from the gallery.
- `Note`: artifact hash, `PKDrawing` data, text, an anchor (scroll offset + page width
  at the time it was drawn), created/updated.
- `HubKey` metadata (name, hub URL). **The key itself stays only in the Keychain.**

Revisions have different hashes, so a revised artifact is a new item linked to the one before it
by title + session, and notes stay on the version they were made on.

### Notes on iPad

- The viewer shows the artifact with a `PKCanvasView` overlay that scrolls along with it (the
  canvas content size tracks the page height the viewer measures). Pencil draws,
  a finger scrolls (`drawingPolicy = .pencilOnly`), and the tool picker sits in the toolbar.
- A notes sidebar holds text notes for the artifact. "Quote" puts selected artifact text
  (from `window.getSelection()` through `evaluateJavaScript`) into a note.
- **Revise with notes:** builds the same prompt the web UI's Revise button does
  (`Revise the "<title>" artifact: `) followed by the text notes, and sends it to the session
  that made it. For drawings, v1 attaches a description of where they are ("circled near
  'Root cause'") taken from the visible text under the stroke bounds. Sending the drawing as an
  image needs image input in tamale's `send`, which is a later change.
- If the artifact's session has ended but another session of the same project is live, Revise
  sends the gallery's form to it: `Revise the "<title>" artifact from an earlier session (saved
  at <file>; read it first): `. With no live session, it offers to copy the prompt instead.
  Resuming sessions from the app isn't in scope.

## Phases

### Phase 0: groundwork (this repo, Rust)

- [x] Hub: bearer auth on `/api/sessions`, `/api/ticket`, the session socket. Tests.
- [x] tamale: `md` on assistant and note entries, `artifacts` on assistant entries, `hash` on
      the `artifact` reply and gallery items. Tests in `src/web.rs`.
- [x] Fixture writer: e2e vectors + message samples into `apple/Fixtures`, run from
      `cargo test`, which fails if the checked-in files are stale (as `src/browser/dist` is
      checked in today).
- [x] `docs/ios.md` stub and a line in `docs/remote-access.md` once there's a TestFlight.
      (Done now, pointing at building from source until there is one.)

### Phase 1: TamaleKit (no UI)

- [x] Key parsing and derivations, against the vectors.
- [x] Noise NNpsk0 + chunked transport, against the vectors.
- [x] Sealed label/status decryption.
- [x] `Hub` REST client, `Channel` (handshake, reconnect with backoff, the 503-while-offline
      and 4000 "session ended" cases the page handles).
- [x] `Session` model applying `entries`/`status`/`approval`.
- [x] An integration test against a real `tamale-hub serve` + tamale on localhost
      (`apple/scripts/integration.sh`, run locally; there's no CI for the app).

### Phase 2: iPhone app

- [x] Onboarding: scan (VisionKit `DataScannerViewController`) or paste a key → Keychain.
- [x] Session list with pull-to-refresh and polling while it's on screen.
- [x] Session screen: conversation, tool steps that open, composer, interrupt, approvals.
- [x] Artifact cards inline, full-screen viewer, share/export (HTML, PDF via
      `WKWebView.createPDF`).
- [x] Universal links for `https://<hub>/open/<sid>` (the terminal's QR code) → open that
      session in the app. Needs an `apple-app-site-association` route on the hub.
- [ ] TestFlight. Waits on decision 4. (Team ABTR54GY4X also has a Program License Agreement
      to accept in the developer account before it can sign anything.)

### Phase 3: iPad and notes

- [ ] `NavigationSplitView`: keys/sessions | conversation | artifact. Multiple windows.
- [x] Artifact library (all cached artifacts, search by title, group by project/session).
- [ ] PencilKit overlay, text notes, Revise with notes.
- [ ] Hardware keyboard: ⌘↩ send, Esc interrupt, Y/A/N on approvals, ⌘K session switcher.

### Phase 4: notifications

The hub knows `busy` and `approval` in plaintext, so it can send an APNs push with no content in
it ("A session is waiting for approval") without breaking e2e. The phone decrypts the title when
it opens the app. Needs: device-token registration per key, an APNs key on the hub, and a
setting for it on self-hosted hubs. This is a separate design doc when we get there.

## Repo

- Generate `apple/Tamale.xcodeproj` from `project.yml` with XcodeGen and gitignore it, so
  there's no `.pbxproj` churn. `TamaleKit` is a plain SwiftPM package (`swift test` works
  without Xcode).
- Add `xcuserdata/`, `DerivedData/`, `*.xcuserstate` and `apple/*.xcodeproj` to `.gitignore`.
- No GitHub workflows or releases for the app: it's built and tested locally
  (`apple/README.md`). The Rust fixture check runs with `cargo test`.
- Don't tag app builds with an `x.y.z` version: `release.yml` matches any tag containing one
  (`'**[0-9]+.[0-9]+.[0-9]+*'`), so a tag like `ios-v1.0.0` would start a cargo-dist release.

## Decisions

The phases above assume the first answer in 1–3; change them here before Phase 0 if not.

1. **Bearer auth on the hub** (assumed) or the prototype approach of acting like a browser.
2. **Swift crypto + vectors** (assumed) or a Rust core over UniFFI.
3. **Minimum OS: iOS 17** (assumed) for `@Observable` and SwiftData. iOS 18 would add newer
   SwiftUI APIs but drops some older iPads.

Still open, and not needed until the phase that uses them:

4. **Licence/distribution** (before TestFlight in Phase 2): the app is open source alongside tamale (the reason it's in this
   repo), and goes out on the App Store under whose developer account?
5. **Self-hosted hubs** (before universal links in Phase 2 and push in Phase 4): the app takes any hub URL from the key's QR code, so it works with
   them. Universal links and push only work for tamale-hub.com unless we add per-hub setup.
