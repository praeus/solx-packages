#!/usr/bin/env bash
# Uninstalls and reinstalls every package in this repo against a running
# solx-server, so a fix to shared install.solx conventions (e.g. the
# actionType/paramTypeRef/etc. camelCase rename) lands in every package's
# already-persisted action/type rows rather than just new installs.
#
# Usage: scripts/reinstall-all.sh
# Env:   SOLX_BIN — path to the solx binary (default: first `solx` on PATH)
set -uo pipefail

# Git Bash (MSYS) rewrites argv entries that look like POSIX absolute paths
# before a native .exe ever sees them; solx's `/path/name` references and
# package dir paths must reach it unmodified.
export MSYS_NO_PATHCONV=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGES_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# The native solx.exe doesn't resolve MSYS-style "/d/..." paths (it joins
# them with a backslash and looks for a literal "/d/...\package.json"), so
# package directories must be handed to it as real "D:\..." paths.
to_native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    echo "$1"
  fi
}

SOLX_BIN="${SOLX_BIN:-}"
if [ -z "$SOLX_BIN" ]; then
  SOLX_BIN="$(command -v solx || true)"
fi
if [ -z "$SOLX_BIN" ]; then
  echo "error: no 'solx' on PATH and SOLX_BIN is not set" >&2
  exit 1
fi
echo "[reinstall-all] using solx binary: $SOLX_BIN"

# Dependency order matters here: solx-livejournal's install.solx execs
# solx-quickjs's build-javascript-file action to compile its wasm component,
# so solx-quickjs must be reinstalled first. The rest have no install-time
# ordering constraint on each other.
PACKAGES=(
  solx-quickjs
  solx-livejournal
  solx-firefox
  solx-google
  solx-mcp-actions
  solx-media
  solx-ollama
  solx-omniparse
)

fail_count=0
for pkg in "${PACKAGES[@]}"; do
  pkg_dir="$PACKAGES_ROOT/$pkg"
  echo
  echo "== $pkg =="

  if [ ! -d "$pkg_dir" ]; then
    echo "   skipped: no directory at $pkg_dir"
    continue
  fi

  echo "   uninstalling..."
  if ! "$SOLX_BIN" uninstall-package "$pkg" >/dev/null 2>&1; then
    echo "   (not previously installed, or uninstall failed -- continuing)"
  fi

  echo "   installing..."
  if "$SOLX_BIN" install-package "$(to_native_path "$pkg_dir")"; then
    echo "   OK: $pkg"
  else
    echo "   FAILED: $pkg"
    fail_count=$((fail_count + 1))
  fi
done

echo
if [ "$fail_count" -eq 0 ]; then
  echo "[reinstall-all] all packages reinstalled successfully."
else
  echo "[reinstall-all] $fail_count package(s) failed to install -- see output above."
  exit 1
fi
