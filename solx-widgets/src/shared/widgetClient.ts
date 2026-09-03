/**
 * The narrow, `@solx/http`-independent contract a host hands a widget so it
 * can call back into solx-server (solx-core/docs/widget-actions.md §3, step
 * 3: "inject a scoped client"). Structural, not an import of `@solx/surface`
 * types -- solx-widgets has no dependency on the solx-js repo, same
 * reasoning as `WidgetFileSource` in host/mountWidget.ts.
 *
 * Handing a widget this is an ergonomic choice, not a security boundary: a
 * widget bundle runs in the same JS realm as its host (a shadow root, not
 * an iframe -- see defineReactWidget.tsx), so it could already reach the
 * host's own token/storage directly if it wanted to. This is the
 * sanctioned, discoverable path, matching the existing trust model (see
 * solx-packages/README.md's Security section).
 */
export interface WidgetActionResult {
  action: string;
  result: unknown;
  success: boolean;
  message?: string;
}

/**
 * One detached run of an action. Mirrors solx-core's invocation shape
 * (`solx-actions/src/invocations.rs`) structurally. `result` is the action's
 * own return value, already unwrapped -- a wasm component's `output` string
 * is parsed host-side before it gets here.
 */
export interface WidgetInvocation {
  invocation_id: string;
  action_ref: string;
  status: string;
  result: unknown;
  error: string | null;
  console_seq_start: number;
}

export interface WidgetConsoleEntry {
  seq: number;
  ts: string;
  level: string;
  invocation_id: string | null;
  message: string | null;
  data: unknown;
}

export interface WidgetConsoleTail {
  entries: WidgetConsoleEntry[];
  next_cursor: number | null;
}

/** `true` once a status can never change again. Mirrors `invocations::is_terminal`. */
export function isTerminalStatus(status: string): boolean {
  return status !== "running" && status !== "cancelling";
}

export interface WidgetClient {
  actions: {
    exec(path: string, name: string, params?: unknown): Promise<WidgetActionResult>;
  };
  /**
   * Detached execution, for actions too slow to hold a request open. These
   * are ordinary Internal actions under `/builtin/action/*` and
   * `/builtin/console/*`, so a widget *could* reach them through `exec`
   * alone -- they are named here so the long-poll and cancellation logic is
   * written once, in the host, rather than in every widget.
   */
  invocations: {
    start(path: string, name: string, params?: unknown): Promise<WidgetInvocation>;
    poll(invocationId: string, waitSecs?: number): Promise<WidgetInvocation | null>;
    stop(invocationId: string, opts?: { force?: boolean }): Promise<WidgetInvocation | null>;
    tailConsole(
      actionRef: string,
      opts?: { cursor?: number | null; limit?: number; waitSecs?: number },
    ): Promise<WidgetConsoleTail>;
  };
}
