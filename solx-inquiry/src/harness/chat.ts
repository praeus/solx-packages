/**
 * The pipeline's model call: the shared detached loop in `solx-widgets`,
 * re-exported.
 *
 * Re-exported rather than wrapped, because inquiry's call is exactly the
 * shared one — it takes a `ChatRequest` and returns a `ChatResult` — and the
 * shared module's generic request was shaped *for* this caller when it was
 * promoted (the planner needs `format` and no tools; the writer needs
 * neither). So unlike `solx-agent/src/harness/chat.ts`, which still has a
 * payload to bind, there is nothing to add here.
 *
 * Keep importing it as `./chat` within this harness — only this file's target
 * changed, the same as `host.ts`.
 */
export * from "../../../solx-widgets/src/wrap/chat";
