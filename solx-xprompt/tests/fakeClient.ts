/**
 * A fake `WidgetClient`, so the console-merging mechanism can be exercised
 * without a real solx-server. Ported down from `solx-agent/tests/fakeHost.ts`'s
 * approach (the harness's own injectable seam *is* a `WidgetClient`-shaped
 * object, so a fake can stand in for one exactly) — this one only needs to
 * fake `entity-save-document`/`entity-get-document` and the `invocations`
 * namespace, not the whole action catalogue.
 */
import type {
  WidgetActionResult,
  WidgetClient,
  WidgetConsoleEntry,
  WidgetConsoleTail,
  WidgetInvocation,
} from "../../solx-widgets/src/shared/widgetClient";

interface DocRecord {
  path: string;
  name: string;
  typeRef?: string;
  title?: string;
  summary?: string;
  contents: unknown;
  updatedAt: string;
}

export interface FakeClient {
  client: WidgetClient;
  /** Write directly into an action's fake console, as if some guest code had called `console-print`. */
  pushEntry(actionRef: string, invocationId: string, message: string): void;
  /**
   * Registers what `invocations.poll` resolves an action ref's next-started
   * invocation to, terminal on the first poll — enough for dispatch tests to
   * exercise the start-then-wait flow without a real long-poll loop.
   */
  respondTo(actionRef: string, outcome: { result?: unknown; error?: string }): void;
}

export function createFakeClient(): FakeClient {
  const docs = new Map<string, DocRecord>();
  const consoles = new Map<string, WidgetConsoleEntry[]>();
  const invocations = new Map<string, WidgetInvocation>();
  const responses = new Map<string, { result?: unknown; error?: string }>();
  const pollCounts = new Map<string, number>();
  let invCounter = 0;
  let entryClock = 0;
  let docClock = 0;
  let nameCounter = 0;

  const docKey = (path: string, name: string) => path + " " + name;

  const client: WidgetClient = {
    actions: {
      async exec(path, name, params): Promise<WidgetActionResult> {
        const ref = path + "/" + name;
        const p = (params ?? {}) as Record<string, unknown>;

        if (ref === "/builtin/document/entity-save-document") {
          const key = docKey(p.path as string, p.name as string);
          docClock += 1;
          docs.set(key, {
            path: p.path as string,
            name: p.name as string,
            typeRef: p.typeRef as string | undefined,
            title: p.title as string | undefined,
            summary: p.summary as string | undefined,
            contents: p.contents,
            updatedAt: new Date(docClock).toISOString(),
          });
          return { action: ref, result: { path: p.path, name: p.name }, success: true };
        }

        if (ref === "/builtin/document/entity-get-document") {
          const doc = docs.get(docKey(p.path as string, p.name as string));
          if (!doc) {
            return { action: ref, result: null, success: false, message: "not found: " + p.path + "/" + p.name };
          }
          return { action: ref, result: doc, success: true };
        }

        if (ref === "/builtin/document/entity-list-documents") {
          const prefix = (p.path_prefix as string | undefined) ?? "";
          const items = [...docs.values()]
            .filter((d) => !prefix || d.path === prefix || d.path.startsWith(prefix + "/"))
            .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : a.updatedAt > b.updatedAt ? -1 : 0));
          return { action: ref, result: { items, total: items.length }, success: true };
        }

        if (ref === "/packages/solx-names/random-name") {
          nameCounter += 1;
          const withId = p.with_id === true;
          const name = "fake-name-" + nameCounter + (withId ? "-" + nameCounter.toString(16).padStart(8, "0") : "");
          return { action: ref, result: { name }, success: true };
        }

        return { action: ref, result: null, success: false, message: "unknown action in fake client: " + ref };
      },
    },

    invocations: {
      async start(path, name): Promise<WidgetInvocation> {
        const actionRef = path + "/" + name;
        invCounter += 1;
        const invocationId = "inv-" + invCounter;
        const consoleSeqStart = (consoles.get(actionRef)?.length ?? 0) + 1;
        const inv: WidgetInvocation = {
          invocation_id: invocationId,
          action_ref: actionRef,
          status: "running",
          result: null,
          error: null,
          console_seq_start: consoleSeqStart,
        };
        invocations.set(invocationId, inv);
        return inv;
      },

      async poll(invocationId): Promise<WidgetInvocation | null> {
        const inv = invocations.get(invocationId);
        if (!inv) return null;
        if (inv.status !== "running") return inv;
        const outcome = responses.get(inv.action_ref);
        if (!outcome) {
          // Unlike the real long-poll, this fake resolves immediately, so a
          // caller's poll-until-terminal loop spins hot with no `respondTo`
          // registered for this action ref — fail loud instead of hanging
          // the whole test run.
          const count = (pollCounts.get(invocationId) ?? 0) + 1;
          pollCounts.set(invocationId, count);
          if (count > 50) {
            throw new Error(
              `fakeClient: invocations.poll on ${inv.action_ref} (${invocationId}) never settled — ` +
                "call respondTo() with this exact action ref before awaiting the result.",
            );
          }
          return inv; // still "running" — nothing registered yet
        }
        const settled: WidgetInvocation = {
          ...inv,
          // Must be one of isTerminalStatus's recognized values (see
          // solx-widgets/src/shared/widgetClient.ts) — "ok", not "succeeded".
          status: outcome.error ? "failed" : "ok",
          result: outcome.result ?? null,
          error: outcome.error ?? null,
        };
        invocations.set(invocationId, settled);
        return settled;
      },

      async stop(invocationId): Promise<WidgetInvocation | null> {
        const inv = invocations.get(invocationId);
        if (!inv) return null;
        const stopped: WidgetInvocation = { ...inv, status: "cancelled", error: "stopped" };
        invocations.set(invocationId, stopped);
        return stopped;
      },

      async tailConsole(actionRef, opts): Promise<WidgetConsoleTail> {
        const list = consoles.get(actionRef) ?? [];
        const from = opts?.cursor ?? 0;
        const entries = list.filter((e) => e.seq >= from);
        const next_cursor = entries.length ? entries[entries.length - 1].seq + 1 : from;
        return { entries, next_cursor };
      },
    },
  };

  function pushEntry(actionRef: string, invocationId: string, message: string): void {
    const list = consoles.get(actionRef) ?? [];
    entryClock += 1;
    list.push({
      seq: list.length + 1,
      // Millisecond-spaced so entries across different fake actions still sort deterministically by ts.
      ts: new Date(entryClock).toISOString(),
      level: "info",
      invocation_id: invocationId,
      message,
      data: null,
    });
    consoles.set(actionRef, list);
  }

  function respondTo(actionRef: string, outcome: { result?: unknown; error?: string }): void {
    responses.set(actionRef, outcome);
  }

  return { client, pushEntry, respondTo };
}
