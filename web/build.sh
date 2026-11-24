#!/bin/sh
# Builds tamale's web UI (this crate) to WebAssembly and writes it to src/web/dist, which
# tamale embeds and serves as /app.js and /app_bg.wasm. Run it after changing web/, then
# rebuild tamale. The built files are checked in, so building tamale needs no wasm toolchain.
#
# Needs the dioxus CLI matching this crate's dioxus (`cargo install dioxus-cli --version
# 0.7.10`) and the wasm32-unknown-unknown target. The module keeps a fixed name with a version
# on its URL, so a browser can cache it for good, and it's gzipped. `--no-build` repackages
# the last build.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out="$here/../src/web/dist"

cd "$here"
if [ "${1:-}" != "--no-build" ]; then
    dx build --platform web --release
fi

public="$here/target/dx/tamale-web/release/web/public"
js=$(ls -t "$public"/assets/tamale-web-*.js | head -n 1)
wasm=$(ls -t "$public"/assets/tamale-web_bg-*.wasm | head -n 1)
version=$(cat "$js" "$wasm" | shasum | cut -c1-12)

mkdir -p "$out"
gzip -9 -n -c "$wasm" >"$out/app_bg.wasm.gz"
sed "s#/\./assets/$(basename "$wasm")#/app_bg.wasm?v=$version#g" "$js" >"$out/app.js"
grep -q "/app_bg.wasm?v=$version" "$out/app.js" || {
    echo "couldn't point app.js at the module" >&2
    exit 1
}
ls -l "$out"
