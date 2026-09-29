#!/usr/bin/env bash
# Build the solx-facebook-actions wasm component and stage it at
# bin/solx-facebook-actions.wasm.
#
# install.solx reads bin/solx-facebook-actions.wasm as its first statement, so
# the artifact must be staged before installing. Staging into bin/ rather
# than referencing actions/solx-facebook-actions/target/ keeps the path stable
# under CARGO_TARGET_DIR.
#
# Usage: ./build.sh [--install]
set -euo pipefail
cd "$(dirname "$0")"

if ! rustup target list --installed | grep -qx 'wasm32-wasip2'; then
    echo "wasm32-wasip2 target is not installed. Run: rustup target add wasm32-wasip2" >&2
    exit 1
fi

cargo build --release --target wasm32-wasip2 --manifest-path actions/solx-facebook-actions/Cargo.toml

# cargo emits the crate name with underscores on every platform.
src="actions/solx-facebook-actions/target/wasm32-wasip2/release/solx_facebook_actions.wasm"
[ -f "$src" ] || { echo "build produced no artifact at $src" >&2; exit 1; }

mkdir -p bin
cp "$src" bin/solx-facebook-actions.wasm
echo "staged bin/solx-facebook-actions.wasm ($(wc -c < bin/solx-facebook-actions.wasm) bytes)"

if [ "${1:-}" = "--install" ]; then
    solx install-package .
fi
