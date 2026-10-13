/**
 * Where solx-server is, and how to authenticate to it, for the `inquiry`
 * Command action's Node child.
 *
 * A near-verbatim copy of `solx-agent/src/headless/serverConfig.ts`, which is
 * itself a mirror of `solx-package-lib/src/server.rs`'s
 * `ServerConfig::from_env()` -- same env var names, same fallback order, same
 * config-file path -- so this package's `install.solx` needs no secret baked
 * in: the server URL comes from `action_config.env`, and the token is resolved
 * from the environment or read out of `solx-config.json`, the file
 * `solx-server` writes it to on first run.
 *
 * Copied rather than imported from solx-agent: this package does not depend on
 * solx-agent, and a 50-line env resolver is not worth coupling two packages
 * for. If a third headless widget appears, this is the obvious candidate to
 * promote into `solx-widgets`, alongside `host.ts` and `chat.ts`.
 */

import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface ServerConfig {
  serverUrl: string;
  serverToken: string;
}

/** `SOLX_APPDATA_DIR` -> `%APPDATA%/praeus/solx` (Windows) -> `$HOME/.praeus/solx` -> temp dir. */
function appdataDir(): string {
  const override = process.env.SOLX_APPDATA_DIR;
  if (override && override.trim()) return override;
  if (process.platform === "win32" && process.env.APPDATA && process.env.APPDATA.trim()) {
    return join(process.env.APPDATA, "praeus", "solx");
  }
  if (process.env.HOME && process.env.HOME.trim()) return join(process.env.HOME, ".praeus", "solx");
  return join(tmpdir(), "praeus", "solx");
}

function tokenFromConfigFile(): string | null {
  try {
    const text = readFileSync(join(appdataDir(), "solx-config.json"), "utf8");
    const value = JSON.parse(text);
    return typeof value?.server_token === "string" ? value.server_token : null;
  } catch {
    return null;
  }
}

/**
 * `SOLX_SERVER_URL` (required, no default) and `SOLX_SERVER_TOKEN` ->
 * `SOLX_TOKEN` -> `solx-config.json`'s `server_token` (required, in that
 * order).
 */
export function resolveServerConfig(): ServerConfig {
  const serverUrl = (process.env.SOLX_SERVER_URL || "").trim();
  if (!serverUrl) throw new Error("SOLX_SERVER_URL is required");

  const serverToken =
    (process.env.SOLX_SERVER_TOKEN || "").trim() ||
    (process.env.SOLX_TOKEN || "").trim() ||
    tokenFromConfigFile();
  if (!serverToken) {
    throw new Error(
      "SOLX_SERVER_TOKEN (or SOLX_TOKEN) is required; set it on the action's " +
        "action_config.env, or ensure solx-config.json has a server_token " +
        "(start solx-server once to generate it)",
    );
  }

  return { serverUrl, serverToken };
}
