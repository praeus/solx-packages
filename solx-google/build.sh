#!/usr/bin/env bash
# Build the solx-google-actions wasm component and stage it at
# bin/solx-google-actions.wasm.
#
# install.solx reads bin/solx-google-actions.wasm as its first statement, so
# the artifact must be staged before installing. Staging into bin/ rather
# than referencing actions/solx-google-actions/target/ keeps the path stable
# under CARGO_TARGET_DIR.
#
# Usage: ./build.sh [--install]
set -euo pipefail
cd "$(dirname "$0")"

if ! rustup target list --installed | grep -qx 'wasm32-wasip2'; then
    echo "wasm32-wasip2 target is not installed. Run: rustup target add wasm32-wasip2" >&2
    exit 1
fi

cargo build --release --target wasm32-wasip2 --manifest-path actions/solx-google-actions/Cargo.toml

# cargo emits the crate name with underscores on every platform.
src="actions/solx-google-actions/target/wasm32-wasip2/release/solx_google_actions.wasm"
[ -f "$src" ] || { echo "build produced no artifact at $src" >&2; exit 1; }

mkdir -p bin
cp "$src" bin/solx-google-actions.wasm
echo "staged bin/solx-google-actions.wasm ($(wc -c < bin/solx-google-actions.wasm) bytes)"

if [ "${1:-}" = "--install" ]; then
    solx install-package .
fi
