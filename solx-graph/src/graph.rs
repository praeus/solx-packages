//! The Oxigraph store and how extracted concepts are laid out in it.
//!
//! Each solx document gets its own named graph, `urn:solx:graph:<ref>`,
//! holding what was extracted from it; extracting it again replaces that
//! graph in one transaction. Concepts are shared across documents by IRI
//! (`urn:solx:concept:<slug>`), so the default union of graphs links every
//! document that mentions the same concept.
//!
//! - `<doc> rdf:type solx:Document`, `rdfs:label "<title>"`, `solx:ref "/path/name"`
//! - `<doc> solx:mentions <concept>`
//! - `<concept> rdf:type solx:Concept`, `rdfs:label`, `rdfs:comment`
//! - `<concept> <urn:solx:relation:<slug>> <concept>`, the relation's own
//!   wording as that predicate's `rdfs:label`

use std::path::{Path, PathBuf};

use oxigraph::model::vocab::{rdf, rdfs};
use oxigraph::model::{GraphName, Literal, NamedNode, Quad};
use oxigraph::store::Store;

/// Where the store lives: `SOLX_GRAPH_DIR` if set, else
/// `<appdata>/packages-data/solx-graph/store`. solx doesn't pass its
/// appdata dir to a command, but it does pass `SOL_LOG_DIR`, which is
/// `<appdata>/logs/<action>`, so the store sits under the same appdata dir
/// the per-machine background lock is keyed to. Run by hand with neither,
/// it's `./solx-graph-store`. `:memory:` keeps nothing (see [`Graph::open`]).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SOLX_GRAPH_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let appdata = std::env::var_os("SOL_LOG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .and_then(|logs| logs.parent()?.parent().map(Path::to_path_buf));
    match appdata {
        Some(appdata) => appdata.join("packages-data").join("solx-graph").join("store"),
        None => PathBuf::from("solx-graph-store"),
    }
}

/// What one extraction found in a document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extraction {
    pub concepts: Vec<Concept>,
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Concept {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relation {
    pub from: String,
    pub relation: String,
    pub to: String,
}

/// The document an extraction came from.
pub struct DocumentRef<'a> {
    /// `/path/name`.
    pub reference: &'a str,
    pub title: Option<&'a str>,
}

/// What [`Graph::replace_document`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    pub concepts: usize,
    pub relations: usize,
    pub triples: usize,
}

pub struct Graph {
    store: Store,
}

/// [`data_dir`]'s value for a store kept in memory: lost when the process
/// stops. For trying the package out, and for builds without `rocksdb`.
pub const IN_MEMORY: &str = ":memory:";

impl Graph {
    pub fn open(dir: &Path) -> Result<Self, String> {
        if dir.as_os_str() == IN_MEMORY {
            return Ok(Graph {
                store: Store::new().map_err(|e| e.to_string())?,
            });
        }
        Self::open_on_disk(dir)
    }

    #[cfg(feature = "rocksdb")]
    fn open_on_disk(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        Ok(Graph {
            store: Store::open(dir).map_err(|e| e.to_string())?,
        })
    }

    #[cfg(not(feature = "rocksdb"))]
    fn open_on_disk(_dir: &Path) -> Result<Self, String> {
        Err(format!(
            "solx-graph was built without the rocksdb feature, so it has no on-disk store; \
             set SOLX_GRAPH_DIR={IN_MEMORY} to keep the graph in memory"
        ))
    }

    /// In memory, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Graph {
            store: Store::new().unwrap(),
        }
    }

    /// For the query operations to come; tests only, so far.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Replace everything extracted from `doc` with `extraction`.
    pub fn replace_document(&self, doc: &DocumentRef<'_>, extraction: &Extraction) -> Result<Written, String> {
        let graph = GraphName::NamedNode(iri("graph", doc.reference));
        let quads = document_quads(doc, extraction, &graph);
        let mut tx = self.store.start_transaction().map_err(|e| e.to_string())?;
        tx.clear_graph(&graph).map_err(|e| e.to_string())?;
        tx.extend(&quads);
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Written {
            concepts: extraction.concepts.len(),
            relations: extraction.relations.len(),
            triples: quads.len(),
        })
    }
}

fn document_quads(doc: &DocumentRef<'_>, extraction: &Extraction, graph: &GraphName) -> Vec<Quad> {
    let vocab = |term: &str| NamedNode::new_unchecked(format!("urn:solx:vocab:{term}"));
    let mut quads = Vec::new();
    let mut add = |s: &NamedNode, p: &NamedNode, o: oxigraph::model::Term| {
        quads.push(Quad::new(s.clone(), p.clone(), o, graph.clone()));
    };

    let doc_iri = iri("document", doc.reference);
    add(&doc_iri, &rdf::TYPE.into_owned(), vocab("Document").into());
    add(&doc_iri, &vocab("ref"), Literal::new_simple_literal(doc.reference).into());
    if let Some(title) = doc.title.filter(|t| !t.trim().is_empty()) {
        add(&doc_iri, &rdfs::LABEL.into_owned(), Literal::new_simple_literal(title).into());
    }

    for concept in &extraction.concepts {
        let c = iri("concept", &slug(&concept.name));
        add(&doc_iri, &vocab("mentions"), c.clone().into());
        add(&c, &rdf::TYPE.into_owned(), vocab("Concept").into());
        add(&c, &rdfs::LABEL.into_owned(), Literal::new_simple_literal(&concept.name).into());
        if let Some(description) = concept.description.as_deref().filter(|d| !d.trim().is_empty()) {
            add(&c, &rdfs::COMMENT.into_owned(), Literal::new_simple_literal(description).into());
        }
    }
    for relation in &extraction.relations {
        let predicate = iri("relation", &slug(&relation.relation));
        add(
            &iri("concept", &slug(&relation.from)),
            &predicate,
            iri("concept", &slug(&relation.to)).into(),
        );
        add(&predicate, &rdfs::LABEL.into_owned(), Literal::new_simple_literal(&relation.relation).into());
    }
    // A concept named twice, or a relation repeated, is one triple.
    quads.sort_by_key(|q| q.to_string());
    quads.dedup();
    quads
}

/// `urn:solx:<kind>:<value>`, with `value` percent-encoded where an IRI
/// can't hold it as is.
fn iri(kind: &str, value: &str) -> NamedNode {
    let mut out = format!("urn:solx:{kind}:");
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    NamedNode::new_unchecked(out)
}

/// A concept or relation name as the key it's merged on: lowercase words
/// joined by `-`, so "Graph Database" and "graph  database" are one concept.
pub fn slug(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extraction() -> Extraction {
        Extraction {
            concepts: vec![
                Concept { name: "Graph Database".into(), description: Some("Stores nodes and edges.".into()) },
                Concept { name: "RDF".into(), description: None },
            ],
            relations: vec![Relation { from: "graph database".into(), relation: "can store".into(), to: "RDF".into() }],
        }
    }

    fn doc() -> DocumentRef<'static> {
        DocumentRef { reference: "/notes/graphs", title: Some("Graphs") }
    }

    #[test]
    fn slugs_merge_spellings_of_one_name() {
        assert_eq!(slug("Graph  Database"), "graph-database");
        assert_eq!(slug("graph-database!"), "graph-database");
    }

    #[test]
    fn iris_encode_what_an_iri_cannot_hold() {
        assert_eq!(iri("document", "/a b/ü").as_str(), "urn:solx:document:/a%20b/%C3%BC");
    }

    #[test]
    fn a_document_gets_its_own_graph_and_shares_concepts() {
        let g = Graph::in_memory();
        let written = g.replace_document(&doc(), &extraction()).unwrap();
        assert_eq!((written.concepts, written.relations), (2, 1));
        assert_eq!(g.store().len().unwrap(), written.triples);
        let graphs: Vec<_> = g.store().named_graphs().map(|n| n.unwrap().to_string()).collect();
        assert_eq!(graphs, ["<urn:solx:graph:/notes/graphs>"]);

        // The relation's ends are the concepts' own IRIs, however spelled.
        let relation = NamedNode::new_unchecked("urn:solx:relation:can-store");
        let edges: Vec<_> = g
            .store()
            .quads_for_pattern(None, Some(relation.as_ref()), None, None)
            .map(|q| q.unwrap())
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].subject.to_string(), "<urn:solx:concept:graph-database>");
        assert_eq!(edges[0].object.to_string(), "<urn:solx:concept:rdf>");
    }

    #[test]
    fn extracting_again_replaces_the_documents_graph() {
        let g = Graph::in_memory();
        g.replace_document(&doc(), &extraction()).unwrap();
        let again = Extraction {
            concepts: vec![Concept { name: "SPARQL".into(), description: None }],
            relations: vec![],
        };
        let written = g.replace_document(&doc(), &again).unwrap();
        assert_eq!(g.store().len().unwrap(), written.triples);
        let rdf_concept = NamedNode::new_unchecked("urn:solx:concept:rdf");
        assert_eq!(g.store().quads_for_pattern(Some(rdf_concept.as_ref().into()), None, None, None).count(), 0);
    }
}
