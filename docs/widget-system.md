# Widget system: solx-widgets + solx-web

## Status

**Implemented**, in two passes. First pass: the build/publish toolkit
(`solx-widgets`) and the mount logic on the `solx-web` side, plus a first
real widget package (`solx-agent`) as a working example. Second pass: a
scoped client so a widget's own code can call back into `solx-server`.

The backend contract this system fulfills — what a widget *is*, why there's
no backend widget runtime, the wire format — is specified in
[`solx-core/docs/widget-actions.md`](../../solx-core/docs/widget-actions.md).
This document doesn't repeat that; it covers how `solx-widgets` actually
implements the frontend half, the decisions behind it, and the conventions
a new widget package should follow.

---

## The three pieces of solx-widgets

`solx-widgets` (`solx-packages/solx-widgets/`) is a shared TS toolkit, not
an installable solx package — no `install.solx`, nothing registered in the
actions database from that directory. It has three independent halves,
used by different consumers:

| Module | Consumer | Runs |
|---|---|---|
| `src/build/widgetViteConfig.ts` | a widget package's `vite.config.ts` | Node, build time |
| `src/wrap/*` | a widget package's React source | browser, inside the mounted widget |
| `src/host/mountWidget.ts` | `solx-web` | browser, inside the host page |
| `src/shared/widgetClient.ts` | both `wrap` and `host` | type-only |

Nothing here talks to a persistent process. That's not an oversight — an
earlier version of this whole idea *was* a persistent widget-serving
process, and it was built and removed; see `widget-actions.md` §4 for why,
and don't re-litigate it without reading that first. A widget bundle is a
static build artifact, produced once, uploaded once, served forever after
by the file store that already exists (`GET /files/{bin_name}`).

## The full flow

```
 build time (Node)                          install time (solx CLI)
┌─────────────────────┐                    ┌──────────────────────────┐
│ src/*.tsx  (author)  │                    │ install.solx:            │
│ src/main.ts:         │   vite build       │  save file .../*.js      │
│  defineReactWidget(  ├───────────────────►│    (the bundle)          │
│    tag, Component)   │  dist/widget.js    │  save file .../*.solx    │
└─────────────────────┘  (single ESM file)  │    (the descriptor       │
                                             │     script)              │
                                             │  save action ...         │
                                             │    resultTypeRef:        │
                                             │    WidgetDescriptor      │
                                             └──────────────────────────┘

 runtime (browser, inside solx-web)
┌──────────────────────────────────────────────────────────────────────┐
│ 1. exec the action  ──►  { tag_name, bin_name, fields }               │
│ 2. mountWidget(descriptor, container, { files, client })              │
│      fetch bundle bytes  →  blob URL  →  import()                     │
│      (bundle's top-level code calls customElements.define)            │
│      document.createElement(tag_name)                                 │
│      el.solxClient = client;  el.fields = descriptor.fields           │
│      container.replaceChildren(el)   ← connectedCallback fires here   │
│ 3. inside the element: React renders Component into a shadow root,    │
│    wrapped in <SolxWidgetProvider value={client}>                     │
│ 4. Component reads useSolxWidgetClient() and, later, calls            │
│    client.actions.exec(path, name, params) for its own backend work   │
└──────────────────────────────────────────────────────────────────────┘
```

`ActionRunner.tsx`'s `WidgetPanel` is the current (and so far only) host
implementation of step 2/3 in `solx-web`.

## Build-time: why a single self-contained ESM file

`createWidgetConfig` (`widgetViteConfig.ts`) builds in library mode with
`formats: ['es']`, `rollupOptions.output.inlineDynamicImports: true`, and a
CSS-injection plugin instead of a separate stylesheet asset. All three
follow from one constraint: the bundle is fetched as bytes and `import()`ed
from a **blob URL**, which has no base path. A second chunk, a dynamic
`import()` inside the bundle, or an external stylesheet link would all
resolve against nothing and fail. Everything has to live in the one file.

**Gotcha worth knowing before you hit it again:** React's own source is
full of `process.env.NODE_ENV` checks. A normal Vite *app* build replaces
these automatically as part of building `index.html`'s entry graph; a
widget bundle never goes through that — it's `import()`ed standalone into a
page that has no `process` global at all. Without `define:
{ "process.env.NODE_ENV": JSON.stringify("production") }` set explicitly in
`createWidgetConfig`, the bundle throws `process is not defined` at mount
time, and only at mount time — the build itself succeeds silently. (Bonus:
setting it also lets esbuild dead-code-eliminate React's dev-only warning
paths, which cut the example bundle from ~967 KB to ~321 KB.)

Because React/ReactDOM are bundled in rather than treated as externals
(there's nothing to share them with — no app shell, no window-global React),
every widget currently ships its own full copy. There's no dedup across
widgets on the same page yet. Fine for now; worth knowing if a page ever
mounts many distinct widgets at once.

## Isolation: shadow DOM, not an iframe

`defineReactWidget` mounts into `this.attachShadow({ mode: 'open' })`. This
gives CSS/DOM scoping (the widget's styles can't leak out, the host's can't
leak in) but **not** JS isolation — the widget runs in the same document,
same JS realm, same `window`, as `solx-web` itself.

The scoping cuts both ways, which is what the `styles` option is for:

```ts
defineReactWidget("solx-agent-widget", AgentWidget, { styles: WIDGET_STYLES });
```

Injected into the shadow root, outside the React root so a re-render never
touches it. A widget that wants the host's design tokens has to bring its own
copy — `solx-agent/src/theme.ts` re-declares the `--bg` / `--border` /
`--text-muted` vocabulary from solx-web's `App.css` so the two read as one
product. An iframe would give
real process-level isolation at the cost of `postMessage` plumbing for
everything, including the client-injection story below.

This matters for how to think about the scoped client (next section): it's
convenience and API shape, not a security boundary. A widget bundle could
already read `localStorage.getItem("solx.serverToken")` directly and build
its own full-power client, regardless of what's handed to it explicitly.
That's consistent with the existing trust model — see
`solx-packages/README.md`'s Security section: packages are trusted, there's
no signing today — not a regression introduced by this system.

## The scoped client

`src/shared/widgetClient.ts` defines the contract a host hands a widget:

```ts
export interface WidgetClient {
  actions: {
    exec(path: string, name: string, params?: unknown): Promise<WidgetActionResult>;
  };
}
```

plus an `invocations` namespace (`start` / `poll` / `stop` / `tailConsole`)
for actions too slow to hold a request open.

`solx-agent` no longer uses `invocations`: it drives its agent loop in the
page, so the only action it holds open is one `ollama-chat` call at a time.
The namespace stays because the need is real for any widget wrapping a
long-running action, and keeping the long-poll and cancellation logic in the
host beats re-implementing it per widget.

Deliberately structural types, not imports of `@solx/surface`'s
`ActionExecResult` — `solx-widgets` has no dependency on the `solx-js` repo,
same reasoning as `WidgetFileSource` (the file-fetching half) right next to
it. The interface is additive, so `docs`/`types`/`files` access can be added
later without another breaking change to `mountWidget`'s signature.

Note that `invocations` earns its place rather than being convenience: the
underlying `/builtin/action/*` routes are ordinary actions, so a widget
*could* reach them through `exec` alone. Naming them keeps the long-poll and
cancellation logic in the host — the same `api.ts` helpers `ActionRunner`
itself is tested against — instead of re-implemented in every widget.

Wiring, end to end:

1. `mountWidget(descriptor, container, { files, client })` assigns
   `el.solxClient = client` (before the element connects, alongside
   `el.fields`, so the one resulting render already has both).
2. `defineReactWidget`'s `ReactWidgetElement` has a `solxClient` property
   (own getter/setter, same shape as `fields`) and wraps its render output:
   ```tsx
   <SolxWidgetProvider value={this.currentClient}>
     <Component fields={this.currentFields} />
   </SolxWidgetProvider>
   ```
3. Any component in the tree calls `useSolxWidgetClient()`
   (`src/wrap/SolxWidgetContext.tsx`) to read it — `undefined` if the host
   didn't supply one (tests, Storybook, or a read-only host).
4. The host (`solx-web`'s `WidgetPanel`) builds the client from its own
   connection rather than handing over the raw one:
   ```ts
   const conn = getConnection();
   mountWidget(descriptor, container, {
     files: conn.files,
     client: { actions: { exec: (path, name, params) => conn.actions.exec(path, name, params ?? {}) } },
   });
   ```

## Package layout: what a new widget package needs

`solx-agent` is the reference example
(`solx-packages/docs` convention — see its own
[`DESIGN.md`](../solx-agent/DESIGN.md) for the walkthrough). In brief, a
widget package needs:

- `src/<Widget>.tsx` + `src/main.ts` calling `defineReactWidget(tag,
  Component)` as the Vite entry.
- `vite.config.ts` calling `createWidgetConfig({ entry, outFile })`.
- `package.json` — ordinary npm metadata (deps, `vite build`), read by npm
  and nobody else.
- `solx-package.json` — the solx manifest: `name`, `version`, `description`,
  and any grants. A widget package normally needs no `command_actions` or
  `allowed_base_urls` at all: nothing here runs a shell command, and the
  widget's own fetches come from the browser rather than through
  `/builtin/web/*`. The two manifests are kept apart because a widget
  directory is the one place that is genuinely both an npm project and a
  solx package, and `name`/`version` mean different things to each.
- `install.solx` — upload the built bundle (`save file <bin_name> --file
  dist/<name>.js`) and register a `Script`-type action whose
  `resultTypeRef` is `/builtin/types/WidgetDescriptor`. The action's
  `binName` points at a tiny `.solx` script uploaded separately, whose only
  job is `json '{"tag_name":...,"bin_name":...,"fields":...}'`.

  **Upload that descriptor script with `save file <path> --file
  <local-file>`, not `exec /builtin/file/file_put --json '...'` with the
  script text embedded as a JSON string.** The latter needs the script's
  own `'...'` shell-quoted arguments re-escaped as `'` inside the outer
  JSON string (see `solx-firefox/install.solx` for that pattern done
  correctly) — easy to get wrong, and it *silently* fails with a generic
  `parse --json params` error that doesn't point at which line. Writing the
  descriptor as a real local `.solx` file and uploading it with `save file`
  sidesteps the whole problem: no nested-quote escaping, because there's no
  JSON string wrapping a JSON string.
- `uninstall.solx` — `delete action ... --if-exists`.
- Register the package in `solx-packages/README.md`'s table and
  `solx-packages/scripts/reinstall-all.sh`'s `PACKAGES` array.

`solx-widgets` itself is never added to `reinstall-all.sh` — it has nothing
to install.

## Testing a widget bundle without a browser

There's no browser-automation tool wired into this environment. A jsdom
harness substitutes for one and catches real runtime errors (not just
string-matching the output): construct a minimal DOM (`document`,
`customElements`, `HTMLElement`, etc.) on `globalThis`, `import()` the built
`dist/*.js` file directly, `document.createElement(tag)`, assign
`.fields`/`.solxClient`, append to `document.body`, and inspect
`el.shadowRoot.innerHTML`. This is how the `process.env.NODE_ENV` bug above
was actually caught and confirmed fixed, and how the client-injection wiring
was confirmed end to end (a stub client's `exec` reachable from inside the
rendered component). Not committed anywhere as a reusable script yet — it
was scratch tooling — but worth reaching for again before assuming a widget
change works just because it typechecks and builds.

## Open questions (carried over from widget-actions.md, still open)

- **Package signing** — no integrity check on a bundle served out of the
  file store. Unchanged by this system.
- **Shared React runtime across widgets** — each bundle currently ships its
  own copy; fine at today's scale (one widget package exists), worth
  revisiting if several widgets end up mounted on one page at once.
- **Broader client surface** — `WidgetClient` only exposes `actions.exec`
  today. Extend `widgetClient.ts` when a widget actually needs `docs`,
  `types`, or `files` access, rather than speculatively adding it now.
