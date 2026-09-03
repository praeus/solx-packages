import type { WidgetClient } from "../shared/widgetClient";

export type {
  WidgetClient,
  WidgetActionResult,
  WidgetInvocation,
  WidgetConsoleEntry,
  WidgetConsoleTail,
} from "../shared/widgetClient";

/**
 * The solx-web-facing half of the widget contract
 * (solx-core/docs/widget-actions.md §3): fetch a widget's bundle from the
 * file store, import it so it registers its custom element, then create and
 * mount that element with its fields (and scoped client) assigned.
 *
 * `source` is deliberately a narrow structural type rather than the full
 * `HttpSolx` from @solx/http -- solx-widgets has no dependency on the
 * solx-js repo, and a host only ever needs to hand this loader the specific
 * capabilities (fetching a file by path, execing an action) that mounting
 * and widget-to-backend calls actually require.
 */
export interface WidgetDescriptor {
  tag_name: string;
  bin_name: string;
  fields?: unknown;
}

export interface WidgetFileSource {
  files: {
    get(relPath: string): Promise<Uint8Array>;
  };
}

export interface WidgetMountSource extends WidgetFileSource {
  /** Scoped client so the widget's own code can call back into solx-server (see wrap/SolxWidgetContext.tsx). Optional -- omit for a host that only wants read-only rendering. */
  client?: WidgetClient;
}

// Per-page dedupe: a bundle is a self-registering ESM module (it calls
// customElements.define on import), so importing it twice would throw on
// the second `customElements.define` call for the same tag.
const importedBundles = new Set<string>();

/**
 * Mounts `descriptor` into `container`, replacing any existing children.
 * Returns the mounted element. Bundles are expected to be self-contained
 * single-file ESM -- a blob URL has no useful base, so relative imports
 * inside the bundle will not resolve.
 */
export async function mountWidget(
  descriptor: WidgetDescriptor,
  container: HTMLElement,
  source: WidgetMountSource,
): Promise<HTMLElement> {
  if (!importedBundles.has(descriptor.bin_name)) {
    const bytes = await source.files.get(descriptor.bin_name);
    // Cast: `fetch`-sourced bytes are always backed by a plain ArrayBuffer,
    // never a SharedArrayBuffer, but Uint8Array's type doesn't say so.
    const blob = new Blob([bytes as unknown as BlobPart], { type: "text/javascript" });
    const url = URL.createObjectURL(blob);
    try {
      await import(/* @vite-ignore */ url);
    } finally {
      URL.revokeObjectURL(url);
    }
    importedBundles.add(descriptor.bin_name);
  }

  const el = document.createElement(descriptor.tag_name) as HTMLElement & {
    fields?: unknown;
    solxClient?: WidgetClient;
  };
  // Assign before connecting the element (replaceChildren triggers
  // connectedCallback) so the one resulting render already has both.
  el.solxClient = source.client;
  el.fields = descriptor.fields;
  container.replaceChildren(el);
  return el;
}
