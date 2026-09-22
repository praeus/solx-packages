#!/usr/bin/env node
// Dev loop for a widget package: `vite build --watch` rebuilds the bundle on
// every source change, and this script re-runs `solx install-package` (the
// same upsert-based command `reinstall-all.sh` uses per package) whenever
// that rebuild writes to dist/. No new backend capability, no dev server --
// just automating the rebuild-reinstall-refresh cycle documented as manual
// in solx-core/docs/widget-actions.md §5.
import { spawn, exec } from "node:child_process";
import { watch } from "node:fs";
import path from "node:path";

const dir = path.resolve(process.argv[2] ?? ".");
const distDir = path.join(dir, "dist");

const vite = spawn("npx", ["vite", "build", "--watch"], {
  cwd: dir,
  stdio: "inherit",
  shell: true,
});
vite.on("exit", (code) => process.exit(code ?? 0));

let pending = null;
function scheduleReinstall() {
  clearTimeout(pending);
  // A single Rollup write can fire more than one fs event -- debounce so a
  // save triggers one reinstall, not several racing ones.
  pending = setTimeout(() => {
    exec(`solx install-package "${dir}"`, (err, stdout, stderr) => {
      if (err) {
        console.error(`[watch-and-install] reinstall failed:\n${stderr || err.message}`);
      } else {
        console.log(`[watch-and-install] reinstalled ${path.basename(dir)}`);
      }
    });
  }, 200);
}

// Wait for dist/ to exist -- the first `vite build` run creates it, and
// watching a missing directory throws on some platforms.
function startWatching() {
  try {
    watch(distDir, { recursive: false }, scheduleReinstall);
  } catch {
    setTimeout(startWatching, 300);
  }
}
startWatching();
