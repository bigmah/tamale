#!/bin/sh
# TamaleKit against a real tamale-hub and a stand-in tamale session, all on this machine.
#
#   apple/scripts/integration.sh
set -eu
. "$(dirname "$0")/local-hub.sh"

cd apple/Packages/TamaleKit
TAMALE_IT_HUB="$hub" TAMALE_IT_KEY="$key" TAMALE_IT_SID="$sid" swift test --filter IntegrationTests
