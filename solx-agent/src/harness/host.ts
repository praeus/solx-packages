/**
 * The single `exec` seam everything in this harness talks through.
 *
 * Re-exported from `solx-widgets` rather than defined here: `solx-xprompt`
 * needed the same `Host`/`compact()` convenience, so it was promoted to a
 * shared module (see the doc comment on the real implementation) once a
 * second widget wanted it. Keep importing it as `./host` within this
 * harness — only this file's target changed.
 */
export * from "../../../solx-widgets/src/wrap/host";
