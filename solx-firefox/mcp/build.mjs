// Compile @mozilla/firefox-devtools-mcp into a standalone binary at
// ../bin/mcp-server[.exe], then smoke-test it. Run with bun after
// `bun install --frozen-lockfile`:
//
//   bun build.mjs                          # host platform, build + smoke test
//   bun build.mjs --target bun-linux-x64   # cross-build, no smoke test
//
// Exits non-zero on any failure, so CI and scripts/build-all.sh never ship a
// stale or missing binary. See docs/bundled-mcp-server.md for why each build
// flag is there.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PKG = "@mozilla/firefox-devtools-mcp";
const ASSET = "snapshot.injected.global.js";

function fail(msg) {
  console.error(`mcp build: ${msg}`);
  process.exit(1);
}

const targetIdx = process.argv.indexOf("--target");
const target = targetIdx >= 0 ? process.argv[targetIdx + 1] : undefined;
const cross = target !== undefined;
const windows = cross ? target.includes("windows") : process.platform === "win32";

// The version must be an exact pin: a bump can change the asset layout or the
// bundle's module loading, and has to go through the checks below on purpose.
const pinned = JSON.parse(fs.readFileSync(path.join(HERE, "package.json"), "utf8")).dependencies?.[PKG];
if (!/^\d+\.\d+\.\d+$/.test(pinned ?? "")) fail(`${PKG} must be pinned to an exact version in mcp/package.json, not "${pinned}"`);

let pkgJson;
try {
  pkgJson = createRequire(path.join(HERE, "package.json")).resolve(`${PKG}/package.json`);
} catch {
  fail(`${PKG} is not installed; run \`bun install --frozen-lockfile\` in ${HERE}`);
}
const installed = JSON.parse(fs.readFileSync(pkgJson, "utf8")).version;
if (installed !== pinned) fail(`installed ${PKG} is ${installed}, pinned is ${pinned}; reinstall`);

const dist = path.join(path.dirname(pkgJson), "dist");
for (const f of ["index.js", ASSET]) {
  if (!fs.existsSync(path.join(dist, f))) fail(`${PKG}@${installed} has no dist/${f}; re-check docs/bundled-mcp-server.md before bumping`);
}

const binDir = path.resolve(HERE, "..", "bin");
const outfile = path.join(binDir, `mcp-server${windows ? ".exe" : ""}`);
fs.mkdirSync(binDir, { recursive: true });

const args = [
  "build", "--compile", path.join(dist, "index.js"),
  "--outfile", outfile,
  // The server reads the snapshot script from next to its own module.
  "--asset", path.join(dist, ASSET),
  // bun build inlines process.env.NODE_ENV as "development", which makes the
  // bundle load dotenv and print its banner to stdout -- the MCP channel.
  "--define", 'process.env.NODE_ENV="production"',
  // The server runs in a caller-chosen cwd; never load .env or bunfig.toml
  // (a bunfig preload runs arbitrary code) from it.
  "--no-compile-autoload-dotenv",
  "--no-compile-autoload-bunfig",
  ...(cross ? ["--target", target] : []),
];
const build = spawnSync(process.execPath, args, { stdio: "inherit" });
if (build.status !== 0) fail(`bun build exited with ${build.status}`);
if (!fs.existsSync(outfile)) fail(`bun build did not produce ${outfile}`);

if (cross) {
  console.log(`built ${outfile} for ${target} (smoke test skipped when cross-compiling)`);
  process.exit(0);
}

// -- smoke test -------------------------------------------------------------

const version = spawnSync(outfile, ["--version"], { encoding: "utf8" });
if (version.stdout.trim() !== installed) fail(`--version printed "${version.stdout.trim()}", expected ${installed}\n${version.stderr}`);

// Run from a scratch cwd with a planted .env and a bunfig.toml preload: the
// binary must load neither, and stdout must carry nothing but JSON-RPC.
const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "solx-mcp-smoke-"));
const marker = path.join(cwd, "preload-ran");
fs.writeFileSync(path.join(cwd, ".env"), "SOLX_SMOKE=1\n");
fs.writeFileSync(path.join(cwd, "evil.cjs"), `require("fs").writeFileSync(${JSON.stringify(marker)}, "");\n`);
fs.writeFileSync(path.join(cwd, "bunfig.toml"), 'preload = ["./evil.cjs"]\n');

const tools = await new Promise((resolve) => {
  const child = spawn(outfile, ["--connect-existing", "--marionette-port", "2828", "--tool-preset", "developer"], { cwd });
  let stdout = "";
  let stderr = "";
  let settled = false;
  const done = (result) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    child.kill();
    resolve({ ...result, stderr });
  };
  const timer = setTimeout(() => done({ error: "timed out waiting for tools/list" }), 30_000);
  child.stderr.on("data", (d) => (stderr += d));
  child.stdout.on("data", (d) => {
    stdout += d;
    const lines = stdout.split("\n");
    stdout = lines.pop();
    for (const line of lines.filter((l) => l.trim())) {
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        return done({ error: `non-JSON-RPC output on stdout: ${line}` });
      }
      if (msg.id === 1) {
        child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
        child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list" }) + "\n");
      } else if (msg.id === 2) {
        return done({ names: (msg.result?.tools ?? []).map((t) => t.name) });
      }
    }
  });
  child.on("exit", (code) => done({ error: `server exited with ${code} before answering tools/list` }));
  child.stdin.write(
    JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "solx-firefox-build", version: "0" } },
    }) + "\n",
  );
});
const planted = fs.existsSync(marker);
fs.rmSync(cwd, { recursive: true, force: true });

if (tools.error) fail(`${tools.error}\n${tools.stderr}`);
if (planted) fail("the binary ran a bunfig.toml preload from its cwd; --no-compile-autoload-bunfig is not taking effect");
if (tools.stderr.includes("Loaded .env")) fail("the binary loaded .env from its cwd; NODE_ENV is not baked in as production");

// Every tool solx-mcp-actions imports must still exist (when that package is
// checked out next to this one).
const servers = path.resolve(HERE, "..", "..", "solx-mcp-actions", "mcp-servers.json");
if (fs.existsSync(servers)) {
  const include = JSON.parse(fs.readFileSync(servers, "utf8")).servers?.firefox?.tool_filter?.include ?? [];
  const absent = include.filter((n) => !n.includes("*") && !tools.names.includes(n));
  if (absent.length) fail(`tools in solx-mcp-actions' firefox tool_filter are gone upstream: ${absent.join(", ")}`);
}

console.log(`built ${outfile} (${PKG}@${installed}, ${tools.names.length} tools, smoke test passed)`);
