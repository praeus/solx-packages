#!/usr/bin/env bash
# Reinstalls every package in this repo against a running solx-server, so a
# fix to shared install.solx conventions (e.g. the actionType/paramTypeRef/etc.
# camelCase rename) lands in every package's already-persisted action/type rows
# rather than just new installs.
#
# This deliberately does NOT uninstall first. `save action` is an upsert, so
# install.solx alone brings every row up to date -- and running uninstall
# first actively breaks packages that carry secrets. solx-google's
# install.solx reuses its OAuth encryption key by passing the "***" sentinel,
# which `save action` resolves against the *existing action row*; deleting
# those rows first either rotates the key (silently orphaning every persisted
# credential and forcing a fresh login) or, if the teardown aborted partway,
# fails outright with "no stored value to restore".
#
# The trade-off: an action or type that a *previous* version of a package
# registered and the current install.solx no longer does is left behind. Run
# `solx uninstall-package <pkg>` by hand when a genuine clean teardown is
# what you want.
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
# them with a backslash and looks for a literal "/d/...\solx-package.json"), so
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
  solx-agent
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
