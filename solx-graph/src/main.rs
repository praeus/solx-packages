//! `solx-graph serve`: the package's background process. solx starts it on
//! the first graph operation (see `install.solx`) and forwards every
//! operation to it; it keeps one Oxigraph store open for its lifetime,
//! since an embedded store can only be opened by one process at a time.
//!
//! Operations run one at a time, in arrival order: every one so far writes
//! to the store. Stdout belongs to the background protocol — log with
//! `solx_package_log::{info, warn, error}`, never `println!`.

mod extract;
mod graph;

use std::sync::Arc;

use serde_json::Value;
use solx_package_log::background::{self, Call};

use graph::Graph;

#[tokio::main]
async fn main() {
    solx_package_log::init("solx-graph");
    match std::env::args().nth(1).as_deref() {
        Some("serve") => {}
        other => {
            eprintln!("usage: solx-graph serve (got {other:?})");
            std::process::exit(2);
        }
    }
    if !background::is_background() {
        eprintln!("solx-graph serve runs as a solx background command; see the package README");
        std::process::exit(2);
    }

    let dir = graph::data_dir();
    let graph = match Graph::open(&dir) {
        Ok(graph) => Arc::new(graph),
        Err(e) => {
            // Before `serve`, so stderr (the start action's background.log)
            // is the only place this can go.
            eprintln!("solx-graph: opening the store at {}: {e}", dir.display());
            std::process::exit(1);
        }
    };
    solx_package_log::info(&format!("graph store open at {}", dir.display())).await;

    let served = background::serve_one_at_a_time(move |call| handle(graph.clone(), call)).await;
    if let Err(e) = served {
        eprintln!("solx-graph: serving: {e}");
        std::process::exit(1);
    }
}

async fn handle(graph: Arc<Graph>, call: Call) -> Result<Value, String> {
    match call.op.as_str() {
        "extract-document" => extract::extract_document(&graph, &call).await,
        op => Err(format!("solx-graph has no operation '{op}'")),
    }
}
