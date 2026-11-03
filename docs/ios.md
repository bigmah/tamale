# tamale for iPhone

A native companion app for tamale sessions reached through a tamale-hub: see what a session is
doing, answer approvals and send prompts from your phone, with artifacts kept on the phone. It's
in this repo under [`apple/`](../apple); the plan behind it is [`apple/ios.md`](../apple/ios.md).

It isn't on the App Store or TestFlight yet, so for now you build it from source onto your own
phone (below).

## Using it

1. On your computer, open your hub's site (tamale-hub.com, or your own) and show your key's QR
   code (**Show**).
2. In the app, tap **Add a key** and point the camera at the code. Or paste the key (`thk_…`); a
   bare key is for tamale-hub.com unless you change the hub.
3. Your sessions are listed, every key's together: waiting for approval first, then busy, then
   the rest. Pull to refresh; the list also refreshes every few seconds while it's open.
4. Tap one to open it. The conversation streams in over the same end-to-end encrypted channel the
   web page uses. Send prompts (they queue while tamale is working), stop a run with the stop
   button, and answer approvals with **Yes**, **Always** or **No** (with a reason).

**Artifacts** show inline at their own height; tap the title for full screen, where you can share
the HTML, export a PDF, or **Revise** (which starts the prompt in the composer). The session menu
has the **gallery** of every artifact the project's sessions saved. Each artifact you open is kept
in the **Library**, readable offline and after the session ends.

**Scanning a terminal's code.** The QR code `/remote` shows in the terminal
(`https://<hub>/open/<sid>`) opens that session when scanned from **Add a key**. With universal
links set up (below), the iPhone camera opens it in the app directly.

**Keys** lists the keys on this phone. Removing one only forgets it here: rotate or delete it on
the hub's site to revoke it everywhere. **Require Face ID** asks for Face ID or the passcode
whenever the app opens.

## What it keeps, and where

- **Keys**: only in the Keychain, readable on this device while it's unlocked, never backed up or
  synced. The hub only ever sees the auth value derived from a key, as with the web page.
- **Artifacts**: in the app's container, encrypted while the phone is locked (data protection
  `complete`).
- Artifacts run like the web UI's sandboxed frames: each in its own web view with no storage,
  no cookies, no access to the app, and the same content security policy. Links open in Safari
  after asking.

## Building it onto your iPhone

You need a Mac with Xcode 16 or later, [XcodeGen](https://github.com/yonaskolb/XcodeGen)
(`brew install xcodegen`) and an Apple developer account signed in to Xcode (a free one works,
with the limits below).

```sh
cd apple
xcodegen generate
open Tamale.xcodeproj    # set your team under Signing & Capabilities, then Run on your phone
```

Or from the command line, with the phone connected and unlocked (see
[`apple/README.md`](../apple/README.md) for options):

```sh
apple/scripts/device.sh                       # the team in apple/project.yml
apple/scripts/device.sh --personal-team TEAM  # a free team: no universal links, 7-day profile
```

## Universal links

For the terminal's `/open/<sid>` links to open the app, the hub serves
`/.well-known/apple-app-site-association` naming the app (`tamale-hub serve --apple-app-id
<team id>.<bundle id>`), and the app lists the hub's domain under associated domains
(`apple/project.yml`). A free developer team can't use associated domains; scanning the code from
**Add a key** works either way.
