#!/usr/bin/env bash
# Build both halves of solx-prompt and stage the two artifacts install.solx
# reads: crate/bin/solx-prompt.wasm and widget/dist/solx-prompt.js.
#
# Staging the wasm into crate/bin/ rather than referencing crate/target/ keeps
# the path stable under CARGO_TARGET_DIR.
#
# --skip-rust / --skip-widget exist because this package's two build costs are
# very different: iterating on the widget should not pay for a release LTO wasm
# build, and vice versa. A plain run does both, which is what install needs.
#
# Usage: ./build.sh [--install] [--skip-rust] [--skip-widget]
set -euo pipefail
cd "$(dirname "$0")"

do_install=0
do_rust=1
do_widget=1
for arg in "$@"; do
    case "$arg" in
        --install)     do_install=1 ;;
        --skip-rust)   do_rust=0 ;;
        --skip-widget) do_widget=0 ;;
        *) echo "unknown argument: $arg" >&2
           echo "usage: ./build.sh [--install] [--skip-rust] [--skip-widget]" >&2
           exit 2 ;;
    esac
done

# Rust first: its only prerequisite failure is instant, so failing here costs
# nothing, where failing after a vite build would have wasted it.
if [ "$do_rust" = 1 ]; then
    if ! rustup target list --installed | grep -qx 'wasm32-wasip2'; then
        echo "wasm32-wasip2 target is not installed. Run: rustup target add wasm32-wasip2" >&2
        exit 1
    fi

    # cd, not --manifest-path: cargo discovers .cargo/config.toml from the cwd,
    # and crate/.cargo/config.toml is where `cargo wasm` is defined.
    ( cd crate && cargo build --release --target wasm32-wasip2 )

    # cargo emits the crate name with underscores on every platform.
    src="crate/target/wasm32-wasip2/release/solx_prompt.wasm"
    [ -f "$src" ] || { echo "build produced no artifact at $src" >&2; exit 1; }

    mkdir -p crate/bin
    cp "$src" crate/bin/solx-prompt.wasm
    echo "staged crate/bin/solx-prompt.wasm ($(wc -c < crate/bin/solx-prompt.wasm) bytes)"
fi

if [ "$do_widget" = 1 ]; then
    # npm ci, not install: package-lock.json is committed.
    [ -d widget/node_modules ] || ( cd widget && npm ci )
    ( cd widget && npm run build )
    out="widget/dist/solx-prompt.js"
    [ -f "$out" ] || { echo "vite produced no artifact at $out" >&2; exit 1; }
    echo "built $out ($(wc -c < "$out") bytes)"
fi

if [ "$do_install" = 1 ]; then
    solx install-package .
fi
