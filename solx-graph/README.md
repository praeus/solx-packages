# solx-graph

A knowledge graph of solx documents, kept in an embedded
[Oxigraph](https://crates.io/crates/oxigraph) RDF store. An Ollama model
(through `solx-ollama`) reads a document and lists its concepts and how they
relate; solx-graph writes them to the graph.

It's a **background package** (see the "Background commands" section of
[HOWTO.md](../HOWTO.md)): one `solx-graph serve` process keeps the store open
and serves every operation, because an embedded store can only be opened by
one process at a time. solx starts it on the first operation and stops it on
`stop` or when the host exits. Operations run one at a time.

## Actions

| Action | Does |
| --- | --- |
| `/packages/solx-graph/extract-document` | `{path?, name, model?}`: extract a document into the graph, replacing what an earlier extraction of it wrote. |
| `/packages/solx-graph/start` | Start the process (operations start it themselves). |
| `/packages/solx-graph/stop` | Stop it, letting the operation in progress finish. |

`model` defaults to `SOLX_GRAPH_MODEL`, else `llama3.2`. Extraction reads
the document's title, summary and contents, cut to 24,000 characters.

## The graph

Each document has its own named graph, `urn:solx:graph:/path/name`.
Concepts are shared between documents by name (`urn:solx:concept:<slug>`,
the name lowercased with its words joined by `-`), so a SPARQL query over
the default graph links every document that mentions a concept:

- `<urn:solx:document:/path/name>` is a `urn:solx:vocab:Document` with an
  `rdfs:label` (its title) and a `urn:solx:vocab:ref` (`/path/name`), and
  `urn:solx:vocab:mentions` each concept.
- A concept is a `urn:solx:vocab:Concept` with an `rdfs:label` and, when
  the model gave one, an `rdfs:comment`.
- A relation is a triple between two concepts with the predicate
  `urn:solx:relation:<slug>`, labelled with the model's wording. Relations
  to concepts the model didn't list are dropped.

## Where the store lives

`<appdata>/packages-data/solx-graph/store`, under the same appdata dir as
the solx host that runs it, or `SOLX_GRAPH_DIR` if set.
`SOLX_GRAPH_DIR=:memory:` keeps the graph in memory, so it's lost when the
process stops.

## Scopes

The start action declares what the process may call back into solx for,
and the manifest lists the same:

- `read:document` and `exec:/builtin/document/entity-get-document`, to
  read the document it's asked to extract (any document);
- `exec:/packages/solx-ollama/ollama-chat`, to ask the model.

Every operation runs its host calls with these, whichever one started the
process. `solx-ollama` must be installed.

## Build

```sh
cargo build --release
solx install-package ./solx-graph
```

The on-disk store is RocksDB, and building it needs **libclang**
(`oxrocksdb-sys` generates its bindings with bindgen). On Windows, install
LLVM and set `LIBCLANG_PATH` to its `bin` directory if it isn't found.
Without it, `cargo build --release --no-default-features` builds a binary
that only runs with `SOLX_GRAPH_DIR=:memory:`, and
`cargo test --no-default-features` runs every test.
