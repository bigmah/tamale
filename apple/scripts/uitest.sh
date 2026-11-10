#!/bin/sh
# The app in the simulator against a real tamale-hub and a stand-in tamale session.
#
#   apple/scripts/uitest.sh ["iPhone 17 Pro"]
set -eu
device=${1:-iPhone 17 Pro}
# A long conversation, so every test also shows it starts at the end and pages back.
export TAMALE_FAKE_HISTORY=300
. "$(dirname "$0")/local-hub.sh"

# A real tamale chef on the hub too, for the chef's test (which stops it). It never asks a model.
cargo build -q --bin tamale
TAMALE_HUB_URL="ws://hub.localhost:${hub##*:}" TAMALE_HUB_KEY="$key" TAMALE_SESSIONS_DIR="$data/chef" \
  TAMALE_BACKEND=api ANTHROPIC_API_KEY=unused target/debug/tamale chef --server >/dev/null 2>&1 &
chef_pid=$!
trap 'kill "$chef_pid" 2>/dev/null || true; cleanup' EXIT INT TERM
sleep 2

cd apple
xcodegen generate --quiet
xcrun simctl boot "$device" 2>/dev/null || true
xcrun simctl bootstatus "$device" -b >/dev/null
# A fresh install each run: no keys or library left from the last one.
xcrun simctl uninstall "$device" com.radkey.tamale 2>/dev/null || true
TEST_RUNNER_TAMALE_DEBUG_KEY="$key" TEST_RUNNER_TAMALE_DEBUG_HUB="$hub" TEST_RUNNER_TAMALE_DEBUG_SID="$sid" \
  TEST_RUNNER_TAMALE_DEBUG_CHEF=1 \
  xcodebuild test -project Tamale.xcodeproj -scheme Tamale ${UITEST_ONLY:+-only-testing:"TamaleUITests/TamaleUITests/$UITEST_ONLY"} \
  -destination "platform=iOS Simulator,name=$device" -derivedDataPath build/DerivedData \
  CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM= PROVISIONING_PROFILE_SPECIFIER=
