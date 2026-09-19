# solx-prompt

**Status: scaffold.** The package layout, build and installation are in place; the
`prompt` action's three phases and the widget's UI are being filled in.

Turn a natural-language prompt into a message plus a runnable plan, and run that
plan in a loop. Two actions, one package, two toolchains:

| action | kind | what it does |
|---|---|---|
| `/packages/solx-prompt/prompt` | wasm (`crate/`) | prompt in, message + concrete action steps + a `next_prompt` out. Two llm calls. Saves nothing. |
| `/packages/solx-prompt/prompt-widget` | script + bundle (`widget/`) | a chat interface that owns the loop: send, run the steps, feed the results and `next_prompt` back in. |

## The `prompt` action

Three phases, two model calls, no fan-out:

```text
baseline recall (skills + memories)   no llm
context documents                     no llm
history block (from params)           no llm
intent                                1 llm    message + abstract steps + searches to run
  no steps and no searches -> return the message and stop
search                                no llm    resolve those against actions and documents
steps                                 1 llm    concrete {action_ref, params, capture}, validated
```

The intent phase proposes *abstract* steps — a `goal` and a `search` prompt, never
an `action_ref`. Naming the action is the search phase's job; asking a model for
one invites invented references. The steps phase then gets each candidate action
with its parameter **and** result schema already fetched, and every step it
proposes is validated against what the search actually surfaced: a step naming an
action that was not found, or referencing a `$capture` no earlier step defines, is
dropped with a note saying why.

### It has no session ref

`prompt` takes prior turns inline as `history` and returns this turn's record. It
never reads or writes a session document — the widget assembles, addresses and
saves that itself.

What settles it is where the step results live. The widget executes the steps, so
it is the only party that ever knows what they returned. If `prompt` read history
from a document instead, the widget would have to successfully *save* those
results before the next call — a write-then-read ordering dependency between
turns, where a failed save means the model silently never sees what it just did.

Two things fall out of it: `prompt` is a pure function of its arguments, testable
with no document fakes, and "this action saves nothing" is structural rather than
a convention — there is no `entity-save-document` reference anywhere in `crate/`.

## Layout

The package carries two toolchains, kept in separate subtrees so neither walks
the other's files:

```
crate/     the wasm32-wasip2 component. Its own cargo workspace.
widget/    the vite/React bundle. Its own npm project.
```

`install.solx`, `uninstall.solx`, `verify.solx` and the build scripts live at the
root, because `solx install-package <dir>` runs the install script with the cwd
set to the package root.

Three consequences worth knowing:

- **`.cargo/config.toml` is discovered from the cwd, not the manifest.** Running
  `cargo --manifest-path crate/Cargo.toml` from the root silently ignores
  `crate/.cargo/config.toml`, so `cargo wasm` would not exist. The build scripts
  `cd crate` instead.
- Both halves sit one level deeper than the packages they were copied from, so
  imports into `../../solx-widgets/` gained a `../` relative to solx-xprompt's.
- rust-analyzer finds `crate/Cargo.toml` by scanning rather than at the opened
  folder root. It works; it is just not the usual shape.

## Build

```sh
./build.sh                  # both halves
./build.sh --skip-rust      # widget only - iterating on the UI
./build.sh --skip-widget    # wasm only
./build.sh --install        # both, then solx install-package .
```

`build.ps1` is the PowerShell twin (`-Install`, `-SkipRust`, `-SkipWidget`).

## Test

```sh
cd crate  && cargo test          # host target - never `cargo test --release`
cd crate  && cargo wasm          # the component
cd widget && npm run typecheck
cd widget && npm run test
```

`cargo test --release` is unusable: `[profile.release]` sets `panic = "abort"`.

Smoke tests against a running install:

```sh
solx script -f verify.solx
```


## Provenance

The Rust half copies its generic modules (`host`, `llm`, `search`, `script`,
`console`, `recall`, `context`, `terms`, the tolerant model-output parser) from
`solx-inquiry`, and the widget copies `console/`, `theme.ts` and
`components/Composer.tsx` from `solx-xprompt`.

Both are copies on purpose, and both are known debt:

- **Rust.** A cargo `path` dependency on solx-inquiry is not viable: its
  `guest.rs` is gated to `wasm32`, the exact target this package builds for, so
  linking it would compile a second `export!(Component)` into the same component.
  The right fix is an rlib-only, wit-bindgen-free crate both packages depend on —
  worth doing when a third wasm consumer appears and there are two working call
  sites to validate the API against.
- **TypeScript.** This is the *third* copy of the widget console mechanism, after
  solx-agent and solx-xprompt. `console/` should be promoted into `solx-widgets`
  (it parameterises cleanly — its only package-specific content is `refs.ts`),
  and `theme.ts` after it. `Composer.tsx` is better left copied: the three
  consumers already disagree about what Stop means.
