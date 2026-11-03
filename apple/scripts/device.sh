#!/bin/sh
# Builds the app and installs it on a connected iPhone.
#
#   apple/scripts/device.sh [--device UDID] [--personal-team TEAMID] [--debug]
#
# With no --device, the one connected iPhone. --personal-team signs with a free team: no
# associated domains (universal links), a separate bundle id (com.radkey.tamale.dev), and a
# profile that lasts 7 days. The phone must be unlocked, with Developer Mode on.
set -eu

device=""
team=""
config=Release
while [ $# -gt 0 ]; do
  case "$1" in
    --device) device=$2; shift 2 ;;
    --personal-team) team=$2; shift 2 ;;
    --debug) config=Debug; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."
if [ -z "$device" ]; then
  found=$(xcrun devicectl list devices 2>/dev/null | awk '/physical/ && /iPhone/ && /available/ { for (i = 1; i <= NF; i++) if ($i ~ /^[0-9A-F]{8}-[0-9A-F]{16}$/) print $i }')
  case $(printf '%s\n' "$found" | grep -c .) in
    0) echo "No connected iPhone found (xcrun devicectl list devices)." >&2; exit 1 ;;
    1) device=$found ;;
    *) xcrun devicectl list devices | grep -E "physical" >&2
       echo "Several iPhones are connected; pick one with --device <UDID>." >&2; exit 1 ;;
  esac
fi

set --
if [ -n "$team" ]; then
  set -- DEVELOPMENT_TEAM="$team" PRODUCT_BUNDLE_IDENTIFIER=com.radkey.tamale.dev \
    CODE_SIGN_ENTITLEMENTS=Tamale/Resources/Tamale-PersonalTeam.entitlements
fi

xcodegen generate --quiet
log=$(mktemp)
if ! xcodebuild -project Tamale.xcodeproj -scheme Tamale -configuration "$config" \
  -destination "generic/platform=iOS" -derivedDataPath build/DerivedData \
  -allowProvisioningUpdates "$@" build >"$log" 2>&1; then
  grep -E "error" "$log" | sort -u >&2
  echo "The build failed; the whole log is $log." >&2
  exit 1
fi
rm -f "$log"

app="build/DerivedData/Build/Products/$config-iphoneos/Tamale.app"
xcrun devicectl device install app --device "$device" "$app"
bundle=$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$app/Info.plist")
xcrun devicectl device process launch --device "$device" "$bundle" || \
  echo "Installed. Open tamale on the phone (the first time, trust the developer under Settings > General > VPN & Device Management)."
