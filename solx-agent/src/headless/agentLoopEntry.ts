/**
 * `agent-loop`: the headless entry point for the harness, run as a Command
 * action's Node child. Params arrive as JSON on stdin (the Command action
 * contract -- see `solx-actions/src/exec.rs::run_command`); the result is
 * written to stdout as JSON, which solx-core parses back into the action's
 * result.
 *
 * This is a thin wrapper, not a second implementation: every call below goes
 * through the exact same `src/harness/*` code `AgentWidget.tsx` drives, via
 * an HTTP-based `Host` that mirrors `tests/live.test.ts`'s `client` almost
 * verbatim. The one thing this module owns is the session_id/approve/message
 * dispatch -- deciding whether a call starts a session, continues one,
 * resolves a pending approval, or just resumes a run left mid-turn -- so a
 * caller can drive a whole conversation across repeated `solx exec` calls the
 * same way the widget drives it across page reloads.
 *
 * `grant` is deliberately not a param: it is hard-pinned to `*` here, the
 * same decision `docs/agent-harness-consolidation.md` recorded for the
 * widget (grant editing dropped; reach is controlled by `tool_exclude`
 * config, not an operator-curated allowlist in this package).
 */

import {
  addTurn,
  approveAndContinue,
  createSession,
  driveSession,
  hostFromClient,
  readSession,
  summarize,
} from "../harness/index";
import type { Host, SendOptions } from "../harness/index";
import { resolveServerConfig } from "./serverConfig";

interface AgentLoopParams {
  message?: string;
  model: string;
  session_id?: string;
  approve?: string[];
  system?: string;
  max_iterations?: number;
  memory?: SendOptions["memory"];
  skills?: SendOptions["skills"];
  context?: SendOptions["context"];
  chat_timeout_secs?: number;
}

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  return Buffer.concat(chunks).toString("utf8");
}

function buildHost(): Host {
  const cfg = resolveServerConfig();
  return hostFromClient({
    actions: {
      async exec(path: string, name: string, params?: unknown) {
        const ref = (path === "/" ? "" : path) + "/" + name;
        const res = await fetch(cfg.serverUrl + "/actions" + ref, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: "Bearer " + cfg.serverToken },
          body: JSON.stringify(params ?? {}),
        });
        if (!res.ok) return { success: false, message: await res.text(), result: null };
        return (await res.json()) as { success?: boolean; message?: string | null; result?: unknown };
      },
    },
  });
}

const noopProgress = { onProgress: () => {} };

async function run(params: AgentLoopParams) {
  if (!params.model) throw new Error("model is required");
  const host = buildHost();

  const opts: SendOptions = {
    model: params.model,
    grant: [{ path: "*" }],
    system: params.system ?? null,
    max_iterations: params.max_iterations ?? null,
    memory: params.memory ?? null,
    skills: params.skills ?? null,
    context: params.context ?? null,
    chat_timeout_secs: params.chat_timeout_secs ?? null,
  };

  if (!params.session_id) {
    if (!params.message) throw new Error("message is required to start a new session");
    const session = await createSession(host, params.message, opts);
    return driveSession(host, session, summarize(session, "running"), noopProgress);
  }

  const session = await readSession(host, params.session_id);

  if (params.approve) {
    return approveAndContinue(host, session, params.approve, noopProgress);
  }
  if (params.message) {
    const seed = await addTurn(host, session, params.message, {
      max_iterations: params.max_iterations ?? null,
    });
    return driveSession(host, session, seed, noopProgress);
  }
  // Neither a message nor an approval: just resume whatever the session was
  // doing. If it was left `running` (e.g. the process was killed mid-turn),
  // this continues it; for any quiescent status, `driveSession`'s loop never
  // starts and the current status comes back unchanged.
  return driveSession(host, session, summarize(session, session.status), noopProgress);
}

async function main() {
  const raw = await readStdin();
  const params = raw.trim() ? (JSON.parse(raw) as AgentLoopParams) : ({} as AgentLoopParams);
  const result = await run(params);
  process.stdout.write(JSON.stringify(result));
}

main().catch((err) => {
  process.stderr.write(String((err && (err as Error).stack) || err));
  process.exitCode = 1;
});
