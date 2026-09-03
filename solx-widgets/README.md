# solx-widgets

Shared toolkit for building solx widgets. Not an installable solx package —
no `install.solx`, nothing gets registered in the actions database from this
directory. It's a small TS library that widget-content packages (like
`solx-agent`) and `solx-web` both pull from.

## Why this exists

A widget is an action whose `result_type_ref` is
`/builtin/types/WidgetDescriptor`, returning `{tag_name, bin_name, fields}`.
There is no widget runtime on the backend by design — the bundle at
`bin_name` is fetched through the existing `GET /files/{bin_name}` route and
mounted client-side. See
[`solx-core/docs/widget-actions.md`](../../solx-core/docs/widget-actions.md)
for the full contract; this package exists to make both ends of it (writing
a widget, and mounting one) a few lines instead of hand-rolled boilerplate
every time.

## What it provides

- **`src/wrap/defineReactWidget.tsx`** — the widget-author half. Wraps a
  React component as a custom element: shadow-root mounted (style
  isolation), re-renders on `.fields` assignment. A widget's build entry
  point calls this once:
  ```ts
  defineReactWidget("solx-agent-widget", AgentWidget);
  ```
- **`src/build/widgetViteConfig.ts`** — `createWidgetConfig({ entry, outFile })`,
  a Vite library-mode config that bundles a widget into one self-contained
  ESM file (no code-splitting, CSS injected at runtime rather than emitted
  as a separate asset) — required because a blob-URL'd bundle has no base
  path to resolve a sibling chunk or stylesheet against.
- **`src/host/mountWidget.ts`** — the solx-web half. Given a
  `WidgetDescriptor` and a container element, fetches the bundle (deduped
  per `bin_name` per page), `import()`s it from a blob URL so it
  self-registers, then creates and mounts the element with `fields` and a
  scoped `client` assigned.
- **`src/shared/widgetClient.ts`** + **`src/wrap/SolxWidgetContext.tsx`** —
  the scoped-client half of the contract (widget-actions.md §3, step 3:
  "inject a scoped client so the element can call back in"). A host passes
  a `WidgetClient` (currently just `{ actions: { exec(path, name, params?) } }`)
  into `mountWidget`; `defineReactWidget` threads it through React context,
  and a widget component reads it with `useSolxWidgetClient()`:
  ```ts
  const client = useSolxWidgetClient(); // undefined if the host didn't supply one
  const { result } = await client?.actions.exec("/builtin/document", "search_documents", { q: text });
  ```
  This is an ergonomic, discoverable API, not a security boundary — a
  widget bundle runs in the same JS realm as its host (shadow root, not an
  iframe), so it could already reach the host's own token directly. See the
  doc comment at the top of `widgetClient.ts`.

## How a widget package uses this

Import by relative path — `solx-widgets` lives alongside widget packages in
this same `solx-packages` repo, so there's no aliasing to set up (unlike
`solx-web`'s consumption of it, which is cross-repo — see below):

```ts
// vite.config.ts
import { createWidgetConfig } from "../solx-widgets/src/build/widgetViteConfig";
export default createWidgetConfig({ entry: "src/main.ts", outFile: "my-widget.js" });
```

```ts
// src/main.ts
import { defineReactWidget } from "../../solx-widgets/src/wrap/defineReactWidget";
import { MyWidget } from "./MyWidget";
defineReactWidget("my-widget-tag", MyWidget);
```

See `solx-agent/` for a complete example, including the `install.solx` steps
that upload the built bundle and register the widget action.

## How solx-web uses this

`solx-web/web/vite.config.ts` and `tsconfig.json` alias `@solx/widgets/host`
straight to `src/host/mountWidget.ts`, the same way they already alias
`@solx/http`/`@solx/surface` from the sibling `solx-js` repo.
`ActionRunner.tsx`'s widget panel builds a `WidgetClient` that wraps
`getConnection().actions.exec` and passes it (along with `files`) into
`mountWidget`.

## Build

`npm install && npm run typecheck` — this package has no bundle output of
its own; it's consumed as source by whatever imports it.
