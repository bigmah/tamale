#!/bin/sh
# Builds tamale's browser client (web/client) to WebAssembly and writes it to
# src/browser/dist, which tamale embeds. Run it after changing web/client or
# web/protocol, then rebuild tamale.
#
# Needs the wasm32 target and a wasm-bindgen CLI matching web/client's pinned
# wasm-bindgen version:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version <version in web/client/Cargo.toml>
set -eu
cd "$(dirname "$0")/client"

want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' Cargo.toml)
have=$(wasm-bindgen --version 2>/dev/null | cut -d' ' -f2 || true)
if [ "$have" != "$want" ]; then
    echo "wasm-bindgen CLI ${have:-not found}, need $want:" >&2
    echo "  cargo install wasm-bindgen-cli --version $want" >&2
    exit 1
fi

cargo build --release --target wasm32-unknown-unknown
out=../../src/browser/dist
wasm-bindgen --target web --no-typescript --out-name client --out-dir "$out" \
    target/wasm32-unknown-unknown/release/tamale_web_client.wasm
if command -v wasm-opt >/dev/null; then
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int \
        -o "$out/client_bg.wasm" "$out/client_bg.wasm"
fi
ls -l "$out"
