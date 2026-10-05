#!/usr/bin/env bash
# Build a package's Rust wasm component and stage it in the package's bin/.
#
# The package's install.solx uploads the staged file as its first statement
# (`save file ... --file bin/<name>.wasm`), so the artifact must be staged
# before installing. That line is also how this script knows what to build:
# <name> is the staged file name, and the crate to build is the one whose
# package name is <name> -- at the package root (solx-names, solx-ollama) or
# under actions/*/ (solx-facebook, solx-google). Staging into bin/ rather than
# pointing install.solx at target/ keeps the path stable under
# CARGO_TARGET_DIR.
#
# Usage: scripts/build-wasm.sh <package-dir> [--install]
#   --install   also run `solx install-package <package-dir>` afterwards
# Env:   SOLX_BIN — path to the solx binary for --install (default: `solx`)
set -euo pipefail

usage() {
  echo "usage: $0 <package-dir> [--install]" >&2
  exit 2
}

# Native .exe tools (cargo, solx) need a real Windows path under Git Bash:
# MSYS only rewrites "/c/..." arguments for them when MSYS_NO_PATHCONV is
# unset, and --install has to set it (see below).
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    echo "$1"
  fi
}

[ $# -ge 1 ] || usage
PKG_DIR="$1"
INSTALL=0
case "${2:-}" in
  "") ;;
  --install) INSTALL=1 ;;
  *) usage ;;
esac

[ -d "$PKG_DIR" ] || { echo "not a directory: $PKG_DIR" >&2; exit 1; }
PKG_DIR="$(cd "$PKG_DIR" && pwd)"
[ -f "$PKG_DIR/install.solx" ] || { echo "no install.solx in $PKG_DIR" >&2; exit 1; }

# The staged artifact install.solx expects, e.g. bin/solx-names.wasm.
mapfile -t staged < <(grep -o -- '--file bin/[^ ;]*\.wasm' "$PKG_DIR/install.solx" | sed 's|^--file bin/||' | sort -u)
case "${#staged[@]}" in
  0) echo "$PKG_DIR/install.solx stages no bin/*.wasm -- nothing to build" >&2; exit 1 ;;
  1) ;;
  *) echo "$PKG_DIR/install.solx stages more than one wasm (${staged[*]}); not supported" >&2; exit 1 ;;
esac
WASM_NAME="${staged[0]}"
CRATE_NAME="${WASM_NAME%.wasm}"

# The crate whose [package] name matches: the first `name = "..."` line of a
# Cargo.toml is its package name.
crate_name_of() {
  sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$1" | head -n 1
}
MANIFEST=""
for candidate in "$PKG_DIR/Cargo.toml" "$PKG_DIR"/actions/*/Cargo.toml; do
  if [ -f "$candidate" ] && [ "$(crate_name_of "$candidate")" = "$CRATE_NAME" ]; then
    MANIFEST="$candidate"
    break
  fi
done
[ -n "$MANIFEST" ] || {
  echo "no crate named '$CRATE_NAME' at $PKG_DIR/Cargo.toml or $PKG_DIR/actions/*/Cargo.toml" >&2
  exit 1
}

if ! rustup target list --installed | grep -qx 'wasm32-wasip2'; then
  echo "wasm32-wasip2 target is not installed. Run: rustup target add wasm32-wasip2" >&2
  exit 1
fi

echo "building $CRATE_NAME ($MANIFEST)"
cargo build --release --target wasm32-wasip2 --manifest-path "$(native_path "$MANIFEST")"

# cargo emits the crate name with underscores on every platform.
TARGET_DIR="${CARGO_TARGET_DIR:-$(dirname "$MANIFEST")/target}"
src="$TARGET_DIR/wasm32-wasip2/release/${CRATE_NAME//-/_}.wasm"
[ -f "$src" ] || { echo "build produced no artifact at $src" >&2; exit 1; }

mkdir -p "$PKG_DIR/bin"
cp "$src" "$PKG_DIR/bin/$WASM_NAME"
echo "staged $PKG_DIR/bin/$WASM_NAME ($(wc -c < "$PKG_DIR/bin/$WASM_NAME") bytes)"

if [ "$INSTALL" = 1 ]; then
  # Same MSYS handling as reinstall-all.sh: solx must see its arguments
  # unrewritten, so it gets a native path explicitly.
  export MSYS_NO_PATHCONV=1
  "${SOLX_BIN:-solx}" install-package "$(native_path "$PKG_DIR")"
fi
