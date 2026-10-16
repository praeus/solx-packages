#!/usr/bin/env node
// Plan and assemble the release zips for solx packages.
//
//   scripts/package.mjs plan [--github-output]
//       Print the CI build matrix as JSON (or write it to $GITHUB_OUTPUT).
//   scripts/package.mjs package <package> --platform <name> [--out <dir>]
//       Zip one package, after it has been built (see below).
//   scripts/package.mjs companions <package>
//       List the sibling directories the package builds from.
//   scripts/package.mjs extra-builds <package>
//       List the package's extra_binaries for this host, one
//       "<binary>\t<cwd>\t<build command>" line each (paths from the repo root).
//
// A zip unpacks to `<package>/` and installs like the checked-out directory:
//
//   unzip solx-ollama-0.1.0.zip && solx install-package ./solx-ollama
//
// It holds the package's tracked source files (so no target/, node_modules/
// or other ignored build output) plus the build products install depends on:
//
//   wasm    bin/<name>.wasm, staged by scripts/build-wasm.sh
//   rust    the command binaries, from <package>/target/release (or
//           $CARGO_TARGET_DIR/release), placed in bin/. One zip per platform,
//           and solx-package.json is rewritten so its commands run there
//           (see rewriteManifest). Plus bin/<name> for each
//           `extra_binaries` entry, built by that entry's own `build`.
//   node    dist/ and dist-headless/, from the package's own `npm run build`
//   source  nothing -- install.solx is the whole package
//
// Sibling directories a package builds from by relative path
// (solx-package-lib for the Rust packages, solx-widgets for solx-agent) are
// added to the zip next to it, so the source in the zip still builds.
//
// Needs only node >= 18; the zip writer is below, so no zip/7z on the runner.
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// CI runner -> platform name used in zip names. Matches the solx repo's
// build workflow.
const PLATFORMS = [
  { os: "ubuntu-22.04", platform: "linux-x64", exe: "" },
  { os: "macos-latest", platform: "macos-arm64", exe: "" },
  { os: "windows-latest", platform: "windows-x64", exe: ".exe" },
];

// Programs that run a script rather than being the command itself.
const INTERPRETERS = ["node", "bun", "deno", "python", "python3"];

const posix = (p) => p.replace(/\\/g, "/");
const read = (p) => fs.readFileSync(p, "utf8");

function packageDirs() {
  return fs
    .readdirSync(ROOT, { withFileTypes: true })
    .filter((e) => e.isDirectory() && fs.existsSync(path.join(ROOT, e.name, "solx-package.json")))
    .map((e) => e.name)
    .sort();
}

function manifestOf(pkg) {
  return JSON.parse(read(path.join(ROOT, pkg, "solx-package.json")));
}

/** The program token of each command_actions entry. */
function commandProgram(def) {
  const tokens = def.command.trim().split(/\s+/);
  const prog = INTERPRETERS.includes(tokens[0]) ? tokens[1] : tokens[0];
  return { tokens, prog: posix(prog).replace(/^\.\//, "") };
}

/** bin/*.wasm files install.solx uploads. */
function stagedWasm(pkg) {
  const file = path.join(ROOT, pkg, "install.solx");
  if (!fs.existsSync(file)) return [];
  return [...read(file).matchAll(/--file (bin\/[^ ;]*\.wasm)/g)].map((m) => m[1]);
}

/** `path = "..."` dependencies of every Cargo.toml in the package, as absolute paths. */
function cargoPathDeps(pkg) {
  const dir = path.join(ROOT, pkg);
  const manifests = [path.join(dir, "Cargo.toml")];
  const actions = path.join(dir, "actions");
  if (fs.existsSync(actions)) {
    for (const e of fs.readdirSync(actions, { withFileTypes: true })) {
      if (e.isDirectory()) manifests.push(path.join(actions, e.name, "Cargo.toml"));
    }
  }
  const deps = [];
  for (const m of manifests.filter((m) => fs.existsSync(m))) {
    for (const line of read(m).split("\n")) {
      if (/^\s*#/.test(line) || /^\s*path\s*=/.test(line)) continue; // [[bin]] path = "src/main.rs" is a file, not a dep
      const hit = line.match(/\bpath\s*=\s*"([^"]+)"/);
      if (hit) deps.push(path.resolve(path.dirname(m), hit[1]));
    }
  }
  return deps;
}

/** Why this package can't be built from this repo alone, or null. */
function unbuildable(pkg) {
  for (const dep of cargoPathDeps(pkg)) {
    if (!fs.existsSync(dep) || path.relative(ROOT, dep).startsWith("..")) {
      return `depends on ${posix(dep)}, which is outside this repository`;
    }
  }
  return null;
}

/** Top-level sibling directories this package builds from by relative path. */
function companions(pkg) {
  const found = new Set();
  for (const dep of cargoPathDeps(pkg)) {
    const rel = path.relative(ROOT, dep);
    if (!rel.startsWith("..")) found.add(rel.split(path.sep)[0]);
  }
  for (const file of ["package.json", "vite.config.ts", "tsconfig.json"]) {
    const p = path.join(ROOT, pkg, file);
    if (!fs.existsSync(p)) continue;
    for (const m of read(p).matchAll(/\.\.\/(solx-[\w-]+)\//g)) found.add(m[1]);
  }
  // wit-bindgen's `path: "../solx-wasm/wit"` and the like.
  for (const f of trackedFiles(pkg).filter((f) => f.endsWith(".rs"))) {
    for (const m of read(path.join(ROOT, f)).matchAll(/(?:\.\.\/)+(solx-[\w-]+)\/wit\b/g)) found.add(m[1]);
  }
  found.delete(pkg);
  return [...found].filter((c) => fs.existsSync(path.join(ROOT, c))).sort();
}

/**
 * Native programs a `rust` package ships beside its command binaries, from
 * `extra_binaries` in solx-package.json:
 *
 *   "extra_binaries": {
 *     "mcp-server": { "cwd": "mcp", "build": "bun run build", "needs": "bun" }
 *   }
 *
 * `build` runs in `cwd` (relative to the package) and must leave
 * bin/<name>[.exe] behind or fail. `needs` names a toolchain CI installs
 * first (only "bun" so far).
 */
function extraBinaries(pkg) {
  return Object.entries(manifestOf(pkg).extra_binaries ?? {}).map(([name, def]) => ({
    name,
    cwd: def.cwd ?? ".",
    build: def.build,
    needs: def.needs,
  }));
}

function kindOf(pkg) {
  const manifest = manifestOf(pkg);
  const hasCommands = Object.keys(manifest.command_actions ?? {}).length > 0;
  const has = (f) => fs.existsSync(path.join(ROOT, pkg, f));
  if (stagedWasm(pkg).length > 0) return "wasm";
  if (hasCommands && has("Cargo.toml")) return "rust";
  if (hasCommands && has("package.json")) return "node";
  if (hasCommands) throw new Error(`${pkg} has command_actions but no Cargo.toml or package.json`);
  return "source";
}

// -- plan ---------------------------------------------------------------

function plan() {
  const include = [];
  const skipped = [];
  for (const pkg of packageDirs()) {
    const why = unbuildable(pkg);
    if (why) {
      skipped.push({ package: pkg, reason: why });
      continue;
    }
    const kind = kindOf(pkg);
    const version = manifestOf(pkg).version;
    const extras = extraBinaries(pkg);
    if (extras.length && kind !== "rust") throw new Error(`${pkg}: extra_binaries is only supported for rust packages`);
    const needs_bun = extras.some((e) => e.needs === "bun");
    if (kind === "rust") {
      for (const p of PLATFORMS) include.push({ package: pkg, kind, version, needs_bun, ...p });
    } else {
      // Platform-independent: one build, one zip.
      include.push({ package: pkg, kind, version, needs_bun, os: "ubuntu-22.04", platform: "any", exe: "" });
    }
  }
  return { include, skipped };
}

// -- package ------------------------------------------------------------

/** Tracked files under `dir` (relative to the repo root), so ignored build output stays out. */
function trackedFiles(dir) {
  const out = execFileSync("git", ["ls-files", "-z", "--", dir], { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28 });
  return out.split("\0").filter(Boolean).filter((f) => fs.existsSync(path.join(ROOT, f)));
}

function walk(dir) {
  const files = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) files.push(...walk(p));
    else files.push(p);
  }
  return files;
}

/**
 * Point the manifest's commands at the binaries bundled in bin/. Source
 * manifests spell commands the Windows way (`.\solx-git.exe clone`, run by
 * `cmd /C`) and some run from target/release; on Linux and macOS solx runs
 * them with `sh -c`, which needs `./solx-git clone`. A built zip always has
 * its programs in bin/, so cwd becomes "bin" for every platform.
 */
function rewriteManifest(manifest, platform, exe) {
  const out = structuredClone(manifest);
  for (const def of Object.values(out.command_actions ?? {})) {
    const { tokens } = commandProgram(def);
    if (INTERPRETERS.includes(tokens[0])) continue;
    const base = posix(tokens[0]).replace(/^\.\//, "").replace(/\.exe$/i, "");
    tokens[0] = exe ? `.\\${base}${exe}` : `./${base}`;
    def.command = tokens.join(" ");
    def.cwd = "bin";
  }
  return out;
}

function buildPackage(pkg, platform) {
  const manifest = manifestOf(pkg);
  const kind = kindOf(pkg);
  const why = unbuildable(pkg);
  if (why) throw new Error(`${pkg}: ${why}`);

  const entries = new Map(); // path inside the zip -> { data, mode }
  const addFile = (rel, zipPath = rel, mode = 0o644) => {
    entries.set(zipPath, { data: fs.readFileSync(path.join(ROOT, rel)), mode });
  };

  for (const dir of [pkg, ...companions(pkg)]) {
    for (const f of trackedFiles(dir)) addFile(f, posix(f), fs.statSync(path.join(ROOT, f)).mode & 0o111 ? 0o755 : 0o644);
  }

  let exe = "";
  const missing = [];
  if (kind === "wasm") {
    for (const rel of stagedWasm(pkg)) {
      const src = path.join(pkg, rel);
      if (fs.existsSync(path.join(ROOT, src))) addFile(src, posix(src));
      else missing.push(src);
    }
  } else if (kind === "node") {
    for (const d of ["dist", "dist-headless"]) {
      const abs = path.join(ROOT, pkg, d);
      if (!fs.existsSync(abs)) continue;
      for (const f of walk(abs)) addFile(path.relative(ROOT, f), posix(path.relative(ROOT, f)));
    }
    for (const def of Object.values(manifest.command_actions)) {
      const rel = posix(path.join(pkg, def.cwd ?? ".", commandProgram(def).prog));
      if (!entries.has(rel)) missing.push(rel);
    }
  } else if (kind === "rust") {
    exe = PLATFORMS.find((p) => p.platform === platform)?.exe;
    if (exe === undefined) throw new Error(`unknown platform "${platform}" (expected one of ${PLATFORMS.map((p) => p.platform).join(", ")})`);
    const targetDir = process.env.CARGO_TARGET_DIR ? path.resolve(process.env.CARGO_TARGET_DIR) : path.join(ROOT, pkg, "target");
    const bases = new Set(Object.values(manifest.command_actions).map((d) => posix(commandProgram(d).prog).replace(/\.exe$/i, "")));
    for (const base of bases) {
      const built = path.join(targetDir, "release", base + exe);
      if (fs.existsSync(built)) {
        entries.set(posix(path.join(pkg, "bin", base + exe)), { data: fs.readFileSync(built), mode: 0o755 });
      } else {
        missing.push(built);
      }
    }
    for (const { name } of extraBinaries(pkg)) {
      const staged = path.join(pkg, "bin", name + exe);
      if (fs.existsSync(path.join(ROOT, staged))) addFile(staged, posix(staged), 0o755);
      else missing.push(staged);
    }
    entries.set(`${pkg}/solx-package.json`, {
      data: Buffer.from(JSON.stringify(rewriteManifest(manifest, platform, exe), null, 2) + "\n"),
      mode: 0o644,
    });
  }
  if (missing.length) throw new Error(`${pkg}: build output missing -- ${missing.join(", ")}`);

  const name = kind === "rust" ? `${pkg}-${manifest.version}-${platform}.zip` : `${pkg}-${manifest.version}.zip`;
  return { name, entries };
}

// -- zip ----------------------------------------------------------------

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** A deflate-compressed zip with unix permissions in the attributes, so unzip restores +x. */
function writeZip(file, entries) {
  const now = new Date();
  const dosTime = (now.getHours() << 11) | (now.getMinutes() << 5) | (now.getSeconds() >> 1);
  const dosDate = ((now.getFullYear() - 1980) << 9) | ((now.getMonth() + 1) << 5) | now.getDate();
  const chunks = [];
  const central = [];
  let offset = 0;
  for (const [name, { data, mode }] of [...entries].sort(([a], [b]) => (a < b ? -1 : 1))) {
    const nameBuf = Buffer.from(name, "utf8");
    const packed = zlib.deflateRawSync(data, { level: 9 });
    const method = packed.length < data.length ? 8 : 0;
    const body = method === 8 ? packed : data;
    const crc = crc32(data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4); // version needed
    local.writeUInt16LE(0x0800, 6); // utf-8 names
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(dosTime, 10);
    local.writeUInt16LE(dosDate, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(body.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    chunks.push(local, nameBuf, body);

    const cd = Buffer.alloc(46);
    cd.writeUInt32LE(0x02014b50, 0);
    cd.writeUInt16LE((3 << 8) | 20, 4); // made by: unix
    cd.writeUInt16LE(20, 6);
    cd.writeUInt16LE(0x0800, 8);
    cd.writeUInt16LE(method, 10);
    cd.writeUInt16LE(dosTime, 12);
    cd.writeUInt16LE(dosDate, 14);
    cd.writeUInt32LE(crc, 16);
    cd.writeUInt32LE(body.length, 20);
    cd.writeUInt32LE(data.length, 24);
    cd.writeUInt16LE(nameBuf.length, 28);
    cd.writeUInt32LE(((0o100000 | mode) << 16) >>> 0, 38); // external attrs: regular file + mode
    cd.writeUInt32LE(offset, 42);
    central.push(cd, nameBuf);

    offset += local.length + nameBuf.length + body.length;
  }
  const cdSize = central.reduce((n, b) => n + b.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.size, 8);
  end.writeUInt16LE(entries.size, 10);
  end.writeUInt32LE(cdSize, 12);
  end.writeUInt32LE(offset, 16);
  fs.writeFileSync(file, Buffer.concat([...chunks, ...central, end]));
}

// -- cli ----------------------------------------------------------------

function arg(args, flag) {
  const i = args.indexOf(flag);
  return i >= 0 ? args[i + 1] : undefined;
}

const [command, ...args] = process.argv.slice(2);
try {
  if (command === "plan") {
    const { include, skipped } = plan();
    for (const s of skipped) console.error(`::warning title=${s.package} skipped::${s.reason}`);
    const json = JSON.stringify({ include });
    if (args.includes("--github-output")) {
      fs.appendFileSync(process.env.GITHUB_OUTPUT, `matrix=${json}\n`);
    }
    console.log(json);
  } else if (command === "companions") {
    // Sibling directories the package builds from, one per line.
    console.log(companions(args[0]).join("\n"));
  } else if (command === "extra-builds") {
    const exe = process.platform === "win32" ? ".exe" : "";
    for (const e of extraBinaries(args[0])) {
      console.log([posix(path.join(args[0], "bin", e.name + exe)), posix(path.join(args[0], e.cwd)), e.build].join("\t"));
    }
  } else if (command === "package") {
    const pkg = args[0];
    const platform = arg(args, "--platform");
    if (!pkg || !platform) throw new Error("usage: package.mjs package <package> --platform <name> [--out <dir>]");
    const out = path.resolve(arg(args, "--out") ?? path.join(ROOT, "dist-zips"));
    fs.mkdirSync(out, { recursive: true });
    const { name, entries } = buildPackage(pkg, platform);
    writeZip(path.join(out, name), entries);
    console.log(`${path.join(out, name)} (${entries.size} files)`);
  } else {
    throw new Error("usage: package.mjs plan [--github-output] | package <package> --platform <name> [--out <dir>] | companions <package> | extra-builds <package>");
  }
} catch (err) {
  console.error(`::error::${err.message}`);
  process.exit(1);
}
