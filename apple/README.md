# tamale for iPhone

The iOS app: a client of tamale-hub like the web UI, speaking the same protocols through the
same end-to-end encryption. The plan and design are in [`ios.md`](ios.md); using it is in
[`docs/ios.md`](../docs/ios.md).

```
apple/
  ios.md                 the plan
  project.yml            XcodeGen spec (Tamale.xcodeproj is generated, not checked in)
  Tamale/                the app: SwiftUI, iOS 17+, iPhone
    App/                 entry point, app state, Keychain, the artifact library
    Sessions/            session list, conversation, composer, approvals, markdown
    Artifacts/           sandboxed artifact web views, viewer, gallery, library
    Onboarding/          adding keys (QR scan or paste), managing them
    Voice/               talking with Gemini about an artifact, or to drive a session (Live API)
  TamaleUITests/         drives the app in the simulator against a local hub
  Packages/TamaleKit/    no UI: keys, Noise, sealed labels, hub client, session channel, messages
  Fixtures/              test vectors and message samples, written by Rust tests
  scripts/               integration.sh, uitest.sh, device.sh
```

## Building

Needs Xcode 16+ and `brew install xcodegen`.

```sh
cd apple && xcodegen generate && open Tamale.xcodeproj
```

Onto a connected iPhone from the command line (unlocked, Developer Mode on):

```sh
apple/scripts/device.sh --device <UDID>                            # team ABTR54GY4X (project.yml)
apple/scripts/device.sh --device <UDID> --personal-team <TEAMID>   # a free team
```

`xcrun devicectl list devices` lists the UDIDs. A free team can't use associated domains, so
that build uses `Tamale/Resources/Tamale-PersonalTeam.entitlements` and the bundle id
`com.radkey.tamale.dev`, and its profile runs out after 7 days (run the script again).

## Testing

```sh
cd apple/Packages/TamaleKit && swift test   # unit tests: Rust's vectors and message samples
apple/scripts/integration.sh                # TamaleKit against a real hub and session
apple/scripts/uitest.sh                     # the app in the simulator, the same way
```

`integration.sh` and `uitest.sh` start `tamale-hub serve` on `http://hub.localhost:<port>` and a
stand-in tamale session (`fake_session_for_the_apple_app` in `src/web.rs`), which answers
prompts with a reply and an artifact.

**Fixtures.** `apple/Fixtures` is written by `src/web/fixtures.rs` (e2e vectors and `/web`
messages) and `hub/src/serve.rs` (the session list). `cargo test` fails when they're stale, so a
change in `src/web.rs` that would break the app's decoding fails in the same PR:

```sh
UPDATE_FIXTURES=1 cargo test --workspace   # then check them in
```
