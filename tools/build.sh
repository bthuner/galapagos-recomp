#!/usr/bin/env sh
# Build the engine to WebAssembly and drop it where web/ expects it.
set -eu
cd "$(dirname "$0")/.."
cargo build --release --target wasm32-unknown-unknown -p galapagos-engine
mkdir -p web/pkg
cp target/wasm32-unknown-unknown/release/galapagos_engine.wasm web/pkg/
echo "built web/pkg/galapagos_engine.wasm ($(wc -c < web/pkg/galapagos_engine.wasm) bytes)"
echo "serve with: python3 -m http.server -d web 8000"
