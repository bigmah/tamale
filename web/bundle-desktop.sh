#!/bin/sh
# Bundles the desktop app: a window that runs the same /web page (see src/shell.rs). On macOS
# it's TamaleWeb.app, which may use the microphone for talking to tamale with Gemini; a bare
# executable can't (macOS stops one that touches it without saying why), so it types instead.
#
# Needs the dioxus CLI (`cargo install dioxus-cli --version 0.7.10`).
set -eu
cd "$(dirname "$0")"
dx bundle --platform desktop --release --no-default-features --features desktop
app=$(ls -d target/dx/tamale-web/bundle/macos/macos/*.app 2>/dev/null | head -n 1 || true)
if [ -n "$app" ]; then
    plist="$app/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Delete :NSMicrophoneUsageDescription" "$plist" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c "Add :NSMicrophoneUsageDescription string Talk to tamale with Gemini." "$plist"
    # The plist is part of the signature: sign it again, ad hoc.
    codesign --force --deep --sign - "$app"
    echo "$app"
fi
