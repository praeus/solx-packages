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
import atomsPlugin from "./atoms-plugin.mjs";

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

// Built through Bun.build's JS API rather than `bun build … --plugin`:
// Bun 1.4.2 accepts the CLI flag but never runs the plugin (verified with a
// minimal patched-entry project), whereas the JS API applies it. Do not
// "simplify" this back to a CLI invocation.
const build = await Bun.build({
  entrypoints: [path.join(dist, "index.js")],
  outdir: binDir,
  naming: path.basename(outfile),
  plugins: [atomsPlugin],
  // selenium-webdriver loads its atoms through a dynamic require() that a
  // bundler cannot follow; atomsPlugin rewrites them to static ones. Without
  // it the binary throws "Cannot find module './atoms/get-attribute.js'" as
  // soon as it touches selenium, which is why the smoke test below drives a
  // tool call and not just tools/list.
  compile: {
    outfile,
    // The server reads the snapshot script from next to its own module.
    // `compile.assets` is the JS-API form of the CLI's `--asset`.
    assets: [path.join(dist, ASSET)],
    // The server runs in a caller-chosen cwd; never load .env or bunfig.toml
    // (a bunfig preload runs arbitrary code) from it.
    autoloadDotenv: false,
    autoloadBunfig: false,
    ...(cross ? { target } : {}),
  },
  // bun build inlines process.env.NODE_ENV as "development", which makes the
  // bundle load dotenv and print its banner to stdout -- the MCP channel.
  define: { "process.env.NODE_ENV": '"production"' },
});
if (!build.success) fail(`Bun.build failed:\n${build.logs.map(String).join("\n")}`);
if (!fs.existsSync(outfile)) fail(`Bun.build did not produce ${outfile}`);

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
  let names;
  let callResult;
  const done = (result) => {
    if (settled) return;
    settled = true;
    clearTimeout(timer);
    child.kill();
    resolve({ ...result, stderr, names, callResult });
  };
  const timer = setTimeout(() => done({ error: "timed out waiting for tools/list + tools/call" }), 60_000);
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
        names = (msg.result?.tools ?? []).map((t) => t.name);
        // Drive one tool call: requireAtom() runs at selenium's module scope,
        // so merely reaching the browser connection is enough to prove the
        // atoms were bundled. No Firefox needs to be running -- a
        // "no listener on 2828" answer still means the modules loaded.
        child.stdin.write(
          JSON.stringify({
            jsonrpc: "2.0",
            id: 3,
            method: "tools/call",
            params: { name: "new_page", arguments: { url: "about:blank" } },
          }) + "\n",
        );
      } else if (msg.id === 3) {
        callResult = msg.result?.content?.[0]?.text ?? msg.error?.message ?? "";
        return done({});
      }
    }
  });
  child.on("exit", (code) => done({ error: `server exited with ${code} before finishing the smoke test` }));
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
// The failure this guards against is a module-resolve throw that arrives as a
// tool-level error, not a non-zero exit, so check the text of the answer.
if (/Failed to import atoms module|Cannot find module '\.\/atoms/.test(tools.callResult ?? "")) {
  fail(`selenium's atoms were not bundled -- mcp/atoms-plugin.mjs is not being applied:\n${tools.callResult}`);
}

// Every tool solx-mcp-actions imports must still exist (when that package is
// checked out next to this one).
const servers = path.resolve(HERE, "..", "..", "solx-mcp-actions", "mcp-servers.json");
if (fs.existsSync(servers)) {
  const include = JSON.parse(fs.readFileSync(servers, "utf8")).servers?.firefox?.tool_filter?.include ?? [];
  const absent = include.filter((n) => !n.includes("*") && !tools.names.includes(n));
  if (absent.length) fail(`tools in solx-mcp-actions' firefox tool_filter are gone upstream: ${absent.join(", ")}`);
}

console.log(`built ${outfile} (${PKG}@${installed}, ${tools.names.length} tools, smoke test passed)`);
