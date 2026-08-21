#!/usr/bin/env sh
# Build the demo for the browser into web/dist (a static site — serve it with
# any HTTP server, e.g. `python3 -m http.server -d web/dist`).
#
# Requirements (one-time):
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version <wasm-bindgen version in Cargo.lock>
#   cargo install wasm-opt        # optional: shrinks the .wasm considerably
set -eu
cd "$(dirname "$0")/.."

PROFILE="${PROFILE:-wasm-release}"
OUT=web/dist
DIR="$PROFILE"; [ "$PROFILE" = dev ] && DIR=debug
TARGET_DIR="target/wasm32-unknown-unknown/$DIR/examples"

cargo build --example demo --target wasm32-unknown-unknown --profile "$PROFILE"

rm -rf "$OUT"
mkdir -p "$OUT"
wasm-bindgen --no-typescript --remove-name-section --remove-producers-section --target web --out-dir "$OUT" --out-name demo "$TARGET_DIR/demo.wasm"

if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz -o "$OUT/demo_bg.wasm" "$OUT/demo_bg.wasm"
fi

cp web/index.html "$OUT/"
cp -R assets "$OUT/assets"
echo "built $OUT ($(du -sh "$OUT" | cut -f1)); serve with: python3 -m http.server -d $OUT"
