/**
 * Where solx-server is, and how to authenticate to it, for the `agent-loop`
 * Command action's Node child.
 *
 * Mirrors `solx-package-lib/src/server.rs`'s `ServerConfig::from_env()`:
 * solx sets `SOLX_SERVER_URL` and `SOLX_SERVER_TOKEN` on every Command action
 * it spawns, and those are the only supported source. The token is never read
 * from `solx-config.json`; solx keeps it in the OS credential manager.
 */

export interface ServerConfig {
  serverUrl: string;
  serverToken: string;
}

function requiredEnv(name: string): string {
  const value = (process.env[name] || "").trim();
  if (!value) {
    throw new Error(
      `${name} is required; solx sets it when it runs this action ` +
        "(run it through solx, or set it yourself when running by hand)",
    );
  }
  return value;
}

/** `SOLX_SERVER_URL` and `SOLX_SERVER_TOKEN`, both required. */
export function resolveServerConfig(): ServerConfig {
  return {
    serverUrl: requiredEnv("SOLX_SERVER_URL"),
    serverToken: requiredEnv("SOLX_SERVER_TOKEN"),
  };
}
