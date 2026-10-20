#!/usr/bin/env bash
# Build every package that has something to build, then check that every
# artifact an install depends on was actually (re)written by this run.
#
# - WASM packages: any package whose install.solx stages a bin/*.wasm, built
#   with scripts/build-wasm.sh.
# - Command packages: any package whose solx-package.json declares
#   command_actions.
#   - Rust ones get `cargo build --release`; their build.rs stages into bin/
#     where they have one, otherwise the command runs from target/release.
#     Then each `extra_binaries` build command runs (solx-firefox's bun
#     compile of the MCP server).
#   - Node ones (a package.json, no Cargo.toml) get their `build` and
#     `build:headless` scripts, run with whichever of npm/bun their lockfile
#     belongs to (installing dependencies first if node_modules is missing).
#
# The check matters because the build.rs staging step only *warns* when it
# fails, so a green cargo build can still leave a stale (or no) binary in
# bin/. Each command's program is resolved from its command_actions entry
# (cwd + its program token), then checked as described at check_rust and
# check_rewritten below.
#
# Usage: scripts/build-all.sh
# Needs: cargo + the wasm32-wasip2 target; node (to read the manifests) and
#        npm (or bun) for the node packages; bun for solx-firefox's MCP server.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGES_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PACKAGES_ROOT"

# Native .exe tools need real Windows paths under Git Bash; see build-wasm.sh.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    echo "$1"
  fi
}

command -v node >/dev/null 2>&1 || { echo "node is required to read the package manifests" >&2; exit 1; }

# Prints one "<cwd>\t<program>" line per command_actions entry of manifest
# $1: the file the command runs, relative to the package dir. The program is
# the command's first token, or its second when the first is an interpreter
# (`node dist-headless/agent-loop.mjs`).
command_programs() {
  node -e '
    const m = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    for (const def of Object.values(m.command_actions || {})) {
      const tokens = def.command.trim().split(/\s+/);
      const prog = ["node", "bun", "deno", "python", "python3"].includes(tokens[0]) ? tokens[1] : tokens[0];
      const clean = (p) => p.replace(/\\/g, "/").replace(/^\.\//, "");
      console.log(`${clean(def.cwd || ".")}\t${clean(prog)}`);
    }' "$(native_path "$1")"
}

has_command_actions() {
  [ -f "$1/solx-package.json" ] && [ -n "$(command_programs "$1/solx-package.json")" ]
}

stages_wasm() {
  [ -f "$1/install.solx" ] && grep -q -- '--file bin/[^ ;]*\.wasm' "$1/install.solx"
}

MARKER="$(mktemp)"
trap 'rm -f "$MARKER"' EXIT
# Coarse-grained filesystem timestamps: make sure anything written from here
# on compares as newer than the marker.
sleep 1

failed=()
built=()

run_step() {
  local pkg="$1"; shift
  echo
  echo "== $pkg: $*"
  if "$@"; then
    built+=("$pkg")
  else
    echo "   FAILED: $pkg" >&2
    failed+=("$pkg")
  fi
}

# `cargo build --release`, then stage every command program that runs from
# outside target/release (bin/) from this build's own output. The packages'
# build.rs files stage too, via a nested build, but only when cargo decides
# to rerun them -- they watch a few files each, and a failed nested build
# just warns and is not retried -- so their bin/ can lag the source.
rust_build() {
  local pkg="$1" target_dir rel prog base
  cargo build --release --manifest-path "$(native_path "$PACKAGES_ROOT/$pkg/Cargo.toml")" || return 1
  target_dir="${CARGO_TARGET_DIR:-$pkg/target}"
  while IFS=$'\t' read -r cwd prog; do
    rel="${cwd%/}/$prog"
    rel="${rel#./}"
    case "$rel" in target/release/*) continue ;; esac
    base="$(basename "$prog")"
    if [ ! -f "$target_dir/release/$base" ]; then
      echo "no build output $target_dir/release/$base to stage as $pkg/$rel" >&2
      return 1
    fi
    mkdir -p "$(dirname "$pkg/$rel")"
    cp "$target_dir/release/$base" "$pkg/$rel" || return 1
    echo "staged $pkg/$rel"
  done < <(command_programs "$pkg/solx-package.json" | tr -d '\r' | sort -u) || return 1
  extra_builds "$pkg"
}

# The package's `extra_binaries` (see scripts/package.mjs): run each build
# command in its directory. check_rewritten later confirms each produced its
# binary.
extra_builds() {
  local pkg="$1" binary dir build
  while IFS=$'\t' read -r binary dir build; do
    echo "-- $binary: $build"
    (cd "$dir" && bash -c "$build") || return 1
  done < <(node "$(native_path "$SCRIPT_DIR/package.mjs")" extra-builds "$pkg" | tr -d '\r')
}

node_build() {
  local pkg="$1" runner
  # Follow the package's own lockfile, so a build never writes a second one.
  if [ -f "$pkg/package-lock.json" ]; then runner=npm
  elif [ -f "$pkg/bun.lock" ] || [ -f "$pkg/bun.lockb" ]; then runner=bun
  elif command -v bun >/dev/null 2>&1; then runner=bun
  else runner=npm
  fi
  (
    cd "$pkg" || exit 1
    if [ ! -d node_modules ]; then
      "$runner" install || exit 1
    fi
    for script in build build:headless; do
      if node -e 'process.exit(require("./package.json").scripts?.[process.argv[1]] ? 0 : 1)' "$script"; then
        "$runner" run "$script" || exit 1
      fi
    done
  )
}

for dir in solx-*/; do
  pkg="${dir%/}"
  if stages_wasm "$pkg"; then
    run_step "$pkg" "$SCRIPT_DIR/build-wasm.sh" "$pkg"
  elif has_command_actions "$pkg"; then
    if [ -f "$pkg/Cargo.toml" ]; then
      run_step "$pkg" rust_build "$pkg"
    elif [ -f "$pkg/package.json" ]; then
      run_step "$pkg" node_build "$pkg"
    else
      echo "== $pkg: has command_actions but no Cargo.toml or package.json; skipped" >&2
    fi
  fi
done

echo
echo "== checking artifacts"
stale=()
report() {
  echo "   $1"
  case "$1" in MISSING*|STALE*|DIFFERS*) stale+=("$2") ;; esac
}

# Rewritten by this run (build-wasm.sh's cp, a node bundler): newer than the
# marker.
check_rewritten() {
  local path="$1"
  if [ ! -f "$path" ]; then report "MISSING  $path" "$path"
  elif [ "$path" -nt "$MARKER" ]; then report "updated  $path" "$path"
  else report "STALE    $path" "$path"
  fi
}

# A Rust command's program. Under target/release, cargo just built it, so it
# is current if it exists. Anywhere else (bin/), it is current if it matches
# this run's cargo output byte for byte -- not by mtime, since an up-to-date
# build legitimately produces nothing new.
check_rust() {
  local pkg="$1" rel="$2" path="$1/$2" src
  if [ ! -f "$path" ]; then report "MISSING  $path" "$path"; return; fi
  case "$rel" in
    target/release/*) report "current  $path (cargo output)" "$path"; return ;;
  esac
  src="${CARGO_TARGET_DIR:-$pkg/target}/release/$(basename "$rel")"
  if [ ! -f "$src" ]; then report "MISSING  build output $src for $path" "$path"
  elif cmp -s "$path" "$src"; then report "current  $path (matches $src)" "$path"
  else report "DIFFERS  $path from $src" "$path"
  fi
}

for pkg in "${built[@]}"; do
  if stages_wasm "$pkg"; then
    while read -r wasm; do
      check_rewritten "$pkg/bin/$wasm"
    done < <(grep -o -- '--file bin/[^ ;]*\.wasm' "$pkg/install.solx" | sed 's|^--file bin/||' | sort -u)
  else
    while IFS=$'\t' read -r cwd prog; do
      rel="${cwd%/}/$prog"
      rel="${rel#./}"
      if [ -f "$pkg/Cargo.toml" ]; then
        check_rust "$pkg" "$rel"
      else
        check_rewritten "$pkg/$rel"
      fi
    done < <(command_programs "$pkg/solx-package.json" | tr -d '\r' | sort -u)
    while IFS=$'\t' read -r binary _; do
      check_rewritten "$binary"
    done < <(node "$(native_path "$SCRIPT_DIR/package.mjs")" extra-builds "$pkg" | tr -d '\r')
  fi
done

echo
echo "built: ${#built[@]} package(s); failed: ${#failed[@]}; stale or missing artifacts: ${#stale[@]}"
[ ${#failed[@]} -eq 0 ] || echo "failed packages: ${failed[*]}" >&2
[ ${#failed[@]} -eq 0 ] && [ ${#stale[@]} -eq 0 ]
