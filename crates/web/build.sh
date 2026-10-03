#!/usr/bin/env bash
# Builds the web version into target/site. Needs the wasm32-unknown-unknown target and a
# wasm-bindgen-cli of the same version as the wasm-bindgen crate in Cargo.lock.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build --release --target wasm32-unknown-unknown -p grim-web
out=target/site
rm -rf "$out"
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out/pkg" target/wasm32-unknown-unknown/release/grim_web.wasm
cp crates/web/www/* "$out/"
echo "built $out"
