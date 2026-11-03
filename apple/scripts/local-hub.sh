# Sourced by integration.sh and uitest.sh: starts `tamale-hub serve` on a free port
# (http://hub.localhost:<port>, which macOS resolves to this machine), makes a key, and
# connects the stand-in session from src/web.rs (`fake_session_for_the_apple_app`).
# Sets $hub, $key and $sid, and stops both on exit.

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

cargo build -q -p tamale-hub
cargo test -q --bin tamale --no-run 2>/dev/null

data=$(mktemp -d)
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
hub="http://hub.localhost:$port"
log="$data/log"

cleanup() {
  [ -n "${hub_pid:-}" ] && kill "$hub_pid" 2>/dev/null || true
  [ -n "${session_pid:-}" ] && kill "$session_pid" 2>/dev/null || true
  rm -rf "$data"
}
trap cleanup EXIT INT TERM

key=$(target/debug/tamale-hub key new it --data-dir "$data" | grep -o 'thk_[a-z2-7]*' | head -1)
target/debug/tamale-hub serve --public-url "$hub" --listen "127.0.0.1:$port" --insecure \
  --data-dir "$data" >"$data/hub.log" 2>&1 &
hub_pid=$!
sleep 1

TAMALE_HUB_URL="ws://hub.localhost:$port" TAMALE_HUB_KEY="$key" TAMALE_FAKE_SECONDS=600 \
  TAMALE_SESSIONS_DIR="$data/sessions" \
  cargo test -q --bin tamale fake_session_for_the_apple_app -- --ignored --nocapture >"$log" 2>&1 &
session_pid=$!

sid=""
for _ in $(seq 1 100); do
  sid=$(grep -o 'SID=[0-9a-f]*' "$log" 2>/dev/null | head -1 | cut -d= -f2 || true)
  [ -n "$sid" ] && break
  sleep 0.2
done
[ -n "$sid" ] || { cat "$log" "$data/hub.log"; echo "the stand-in session didn't start"; exit 1; }
sleep 1
