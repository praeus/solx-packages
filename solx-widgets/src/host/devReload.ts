import type { WidgetFileSource } from "./mountWidget";

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/**
 * Dev-only: polls a widget's bundle for changes and reloads the page when it
 * does. Not a scoped remount -- `defineReactWidget` calls
 * `customElements.define(tagName, ...)` once per tag, and the browser will
 * never let a second definition replace the first, so the only way to pick
 * up new code for an already-defined tag is a full reload. Callers gate this
 * behind their own dev/prod check (e.g. `import.meta.env.DEV`) -- this
 * module has no opinion on that, so it stays dead code in a production
 * bundle only if the caller never calls it from a reachable path.
 */
export function startDevBundleWatch(
  files: WidgetFileSource["files"],
  binName: string,
  intervalMs = 1500,
): () => void {
  let baseline: Uint8Array | null = null;
  let stopped = false;

  const tick = async () => {
    if (stopped) return;
    try {
      const bytes = await files.get(binName);
      if (baseline === null) {
        // Seed on first poll rather than reloading immediately -- the
        // bundle we just mounted *is* this baseline.
        baseline = bytes;
      } else if (!bytesEqual(baseline, bytes)) {
        window.location.reload();
        return;
      }
    } catch {
      // A transient fetch failure mid-rebuild isn't worth surfacing --
      // the next tick will pick it back up once the file store settles.
    }
    if (!stopped) setTimeout(tick, intervalMs);
  };

  void tick();
  return () => {
    stopped = true;
  };
}
