#!/usr/bin/env bash
# Build the solx-livejournal wasm component and stage it at bin/solx-livejournal.wasm.
#
# Unlike the Rust packages this one is JavaScript: the component is produced by
# solx-quickjs's `build-javascript-action`, which shells out to componentize-qjs.
# That action must already be installed (see ../solx-quickjs/README.md) and its
# CLI built at ../solx-quickjs/target/release/solx-quickjs.exe.
#
# install.solx reads bin/solx-livejournal.wasm as its first statement, so the
# artifact has to be staged before installing.
#
# Usage: ./build.sh [--install]
set -euo pipefail
cd "$(dirname "$0")"

root="$(pwd -W 2>/dev/null || pwd)/src"

solx exec /packages/solx-quickjs/build-javascript-action --json "$(cat <<EOF
{"action_name":"solx-livejournal",
 "entry_artifact_name":"solx-livejournal.js",
 "source_artifact_names":["solx-livejournal.js"],
 "artifact_root":"$root"}
EOF
)"

src="src/solx-livejournal.js.wasm"
[ -f "$src" ] || { echo "build produced no artifact at $src" >&2; exit 1; }

mkdir -p bin
cp "$src" bin/solx-livejournal.wasm
echo "staged bin/solx-livejournal.wasm ($(wc -c < bin/solx-livejournal.wasm) bytes)"

if [ "${1:-}" = "--install" ]; then
    solx install-package .
fi
