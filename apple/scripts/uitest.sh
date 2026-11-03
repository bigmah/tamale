#!/bin/sh
# The app in the simulator against a real tamale-hub and a stand-in tamale session.
#
#   apple/scripts/uitest.sh ["iPhone 17 Pro"]
set -eu
device=${1:-iPhone 17 Pro}
. "$(dirname "$0")/local-hub.sh"

cd apple
xcodegen generate --quiet
xcrun simctl boot "$device" 2>/dev/null || true
xcrun simctl bootstatus "$device" -b >/dev/null
# A fresh install each run: no keys or library left from the last one.
xcrun simctl uninstall "$device" com.radkey.tamale 2>/dev/null || true
TEST_RUNNER_TAMALE_DEBUG_KEY="$key" TEST_RUNNER_TAMALE_DEBUG_HUB="$hub" TEST_RUNNER_TAMALE_DEBUG_SID="$sid" \
  xcodebuild test -project Tamale.xcodeproj -scheme Tamale \
  -destination "platform=iOS Simulator,name=$device" -derivedDataPath build/DerivedData \
  CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM= PROVISIONING_PROFILE_SPECIFIER=
