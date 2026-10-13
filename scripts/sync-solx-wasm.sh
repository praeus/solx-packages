#!/usr/bin/env bash
# Refresh the vendored solx-wasm/ from a checkout of the solx repo.
#
# solx-wasm (the helper crate and the canonical custom-action.wit) lives in
# solx under core/solx-wasm. It rarely changes, so this repo keeps a copy
# instead of depending on that repo: wasm packages point wit-bindgen at
# solx-wasm/wit, and solx-quickjs depends on the crate by path.
#
# Usage: scripts/sync-solx-wasm.sh <path-to-solx-checkout>
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <path-to-solx-checkout>" >&2; exit 2; }
SRC="$1/core/solx-wasm"
[ -f "$SRC/wit/custom-action.wit" ] || { echo "no $SRC/wit/custom-action.wit" >&2; exit 1; }

DEST="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/solx-wasm"
rm -rf "$DEST"
mkdir -p "$DEST"
# Everything but build output.
cp -r "$SRC/.cargo" "$SRC/Cargo.toml" "$SRC/Cargo.lock" "$SRC/build.rs" "$SRC/src" "$SRC/wit" "$DEST/"
echo "synced $SRC -> $DEST"
git -C "$DEST" status --short -- . 2>/dev/null || true
