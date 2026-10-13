/**
 * The single `exec` seam everything in this pipeline talks through.
 *
 * Re-exported from `solx-widgets` rather than defined here, exactly as
 * `solx-agent/src/harness/host.ts` does: `Host`/`compact()` were promoted to a
 * shared module once a second widget wanted them (see the doc comment on the
 * real implementation). Keep importing it as `./host` within this harness --
 * only this file's target changed.
 */
export * from "../../../solx-widgets/src/wrap/host";
