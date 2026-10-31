//! A packed snapshot (`.gsnap`) loaded WITHOUT a Store, with its embedding models (format spec
//! 4.5, 4.6, 8; book page "Embedding Models in a Store", section "A `.gsnap` without a store"):
//! the model blobs the file carries (rows of kind 4) are kept in memory beside the graph and
//! serve its traversals like a Store's; the model rows of a Graphersal 0.4.0 file (kind 3) are
//! converted in memory the way the store upgrade converts them (format spec 15.1). A graph
//! without packed models has the built-in `hash-ngram/1` only.

// The file loader (`read_snapshot_file`) is native only; on wasm32 its parts are unused.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::sync::Arc;

use super::files::DirPath;
use super::model_catalog::{self, StoreModelBytes};
use super::upgrade::{self, ConvertedModel, MissingModel, VectorWithoutRule};
use super::{ModelInfo, migrate_models};
use crate::graph::GraphStorage;
use crate::persist::PersistError;
use crate::value::vector::embed::registry::ModelRegistry;

/// What [`read_snapshot_file`](crate::persist::read_snapshot_file) found in a packed snapshot
/// besides the graph: the embedding models it serves and what an earlier release left that this
/// version does not keep current. [`Self::notes`] is the one text every front end shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SnapshotLoadReport {
    /// The embedding models of the file's catalog (their blobs kept in memory with the graph;
    /// a model whose blob the file does not carry, a `--without-models` export, is
    /// [`ModelState::Missing`](crate::persist::ModelState::Missing)).
    pub models: Vec<ModelInfo>,
    /// The repository of the default model (the one `embed` uses without a name); `None`: the
    /// built-in `hash-ngram/1` is the default.
    pub default_model: Option<String>,
    /// The models of a Graphersal 0.4.0 file (its model rows) converted in memory into blobs
    /// with a catalog definition each (format spec 15.1); the file is not changed.
    pub models_converted: Vec<ConvertedModel>,
    /// The models the schema needs (a vector property's `embedder`) that the file does not
    /// carry: text queries on those properties fail until a store holding the model serves the
    /// graph.
    pub models_missing: Vec<MissingModel>,
    /// The vector properties with a model but no vector rule (`from`), counted in the loaded
    /// graph ([`VectorWithoutRule::note`]).
    pub vectors_without_rule: Vec<VectorWithoutRule>,
    /// The saved queries calling a DSL function this version removed (feature `script`).
    pub saved_queries_using_removed: Vec<crate::graph::definition::RemovedFunctionUse>,
}

impl SnapshotLoadReport {
    /// The notes for the file at `file` (one line each; the CLI prints them as `note:`): the
    /// converted models, the models stored but none the default, the missing models, the
    /// vectors without a rule and the saved queries calling a removed function. Empty when
    /// there is nothing to say.
    pub fn notes(&self, file: &str) -> Vec<String> {
        let mut notes = Vec::new();
        for converted in &self.models_converted {
            if converted.renamed() {
                notes.push(format!(
                    "the embedding model {} of {file} (Graphersal 0.4.0) is served as {} (the \
                     catalog keeps one model per repository; the schema names the new identity)",
                    converted.former_identity(),
                    converted.identity()
                ));
            } else {
                notes.push(format!(
                    "the embedding model {} of {file} (Graphersal 0.4.0) was converted in \
                     memory; the file stays as it is (graphersal store create <dir> --from \
                     {file} keeps it converted)",
                    converted.repo()
                ));
            }
        }
        for model in &self.models {
            if model.state == super::ModelState::Missing {
                notes.push(format!(
                    "{file} names the embedding model {} but does not carry it (an export \
                     without models): text queries and vector rules with it fail; put the graph \
                     into a store and import the model: graphersal store create <dir> --from \
                     {file}, graphersal embed-model download {repo}, graphersal embed-model \
                     import {repo} --into <dir>",
                    model.identity,
                    repo = model.repo
                ));
            }
        }
        if !self.models.is_empty() && self.default_model.is_none() {
            notes.push(format!(
                "{file} holds {} embedding model(s) but none is the default: embed() without a \
                 name uses hash-ngram/1; name one (g.v().embed(\"text\", #{{embedder: \"{}\"}})) \
                 or set a default in a store (graphersal store models default <dir> <repo>)",
                self.models.len(),
                self.models[0].repo
            ));
        }
        for missing in &self.models_missing {
            let repo = missing.repo().unwrap_or("<repo>");
            notes.push(format!(
                "the schema of {file} names the embedding model {} ({}) that the file does not \
                 carry: text queries on those properties fail; put the graph into a store and \
                 import the model: graphersal store create <dir> --from {file}, graphersal \
                 embed-model download {repo}, graphersal embed-model import {repo} --into <dir>",
                missing.identity(),
                missing.needed_by().join(", ")
            ));
        }
        for vector in &self.vectors_without_rule {
            notes.push(vector.note(file));
        }
        for removed in &self.saved_queries_using_removed {
            notes.push(removed.note(file));
        }
        notes
    }
}

/// Serves the models of a loaded packed snapshot: the blobs staged in `dir` (in memory), the
/// legacy rows converted, the registry attached to the new graph. Returns the shared graph and
/// the report.
pub(crate) fn finish_load(
    mut graph: crate::graph::TraversalGraph,
    dir: Option<DirPath>,
) -> Result<(crate::graph::Graph, SnapshotLoadReport), PersistError> {
    let mut report = SnapshotLoadReport::default();
    if let Some(dir) = &dir
        && dir_has_models(dir)?
    {
        report.models_converted = migrate_models::convert_in_memory(dir, &mut graph)?
            .into_iter()
            .map(|c| ConvertedModel {
                repo: c.repo,
                identity: c.identity,
                former_identity: c.former_identity,
                bytes: c.bytes,
            })
            .collect();
    }
    let definitions = model_catalog::model_definitions(&graph).map_err(PersistError::Graph)?;
    let schema = graph.get_schema().map_err(PersistError::Graph)?;
    report.models_missing = upgrade::missing_models(schema.as_ref(), &definitions);
    let (wanted, mut found) = upgrade::vectors_without_rule(schema.as_ref(), &definitions);
    for ((edge, label, key), entry) in wanted.iter().zip(found.iter_mut()) {
        entry.elements = count_vectors(&graph, *edge, label, key)?;
    }
    report.vectors_without_rule = found;
    report.saved_queries_using_removed = upgrade::saved_queries_using_removed(&graph)?;
    report.default_model = definitions
        .iter()
        .filter_map(|d| d.as_model())
        .find(|m| m.is_default())
        .map(|m| m.repo().to_string());
    if definitions.is_empty() {
        return Ok((crate::graph::Graph::new_persistent(graph), report));
    }
    // The blobs live in memory (a file without blob rows: an empty directory, every model
    // missing).
    let dir = match dir {
        Some(dir) => dir,
        None => DirPath::root(Arc::new(crate::persist::MemDir::named("packed snapshot"))),
    };
    report.models = model_catalog::infos_of(&definitions, schema.as_ref(), &|model| {
        model_catalog::blob_state(&dir, model.blob())
    });
    let source = Arc::new(StoreModelBytes::new(dir));
    let registry = ModelRegistry::new(source.clone(), definitions);
    graph
        .register_commit_hook(registry.hook())
        .map_err(PersistError::Graph)?;
    let graph = crate::graph::Graph::new_persistent(graph);
    let _ = source.journal.set(Arc::downgrade(
        &graph.system_journal().map_err(PersistError::Graph)?,
    ));
    graph.attach_model_registry(registry);
    Ok((graph, report))
}

/// Whether the in-memory directory holds staged 0.4.0 model rows (`models/`).
fn dir_has_models(dir: &DirPath) -> Result<bool, PersistError> {
    Ok(super::models::models_dir(dir)
        .try_kind()
        .map_err(|err| PersistError::io("reading the staged models", err))?
        == Some(crate::persist::EntryKind::Dir))
}

/// The elements of `label` holding a vector in `key`.
fn count_vectors(
    graph: &crate::graph::TraversalGraph,
    edge: bool,
    label: &str,
    key: &str,
) -> Result<u64, PersistError> {
    let holds = |props: Option<&crate::value::PropertyMap>| {
        matches!(
            props.and_then(|p| p.get(key)),
            Some(crate::value::ElementProperty::Vector(_))
        )
    };
    let mut count = 0u64;
    if edge {
        for e in graph.edges_by_label(label).map_err(PersistError::Graph)? {
            count += u64::from(holds(
                graph.edge_properties(e).map_err(PersistError::Graph)?,
            ));
        }
    } else {
        for v in graph
            .vertices_by_label(label)
            .map_err(PersistError::Graph)?
        {
            count += u64::from(holds(
                graph.vertex_properties(v).map_err(PersistError::Graph)?,
            ));
        }
    }
    Ok(count)
}

/// Loads the packed snapshot at `path` with its embedding models: the graph as
/// [`read_snapshot`](crate::persist::read_snapshot) reads it, shared as a
/// [`Graph`](crate::graph::Graph) that serves the models the file carries (their blobs kept in
/// memory; the 0.4.0 model rows converted in memory), and the [`SnapshotLoadReport`] of what
/// it found. The file is read once for the graph and, only when its manifest lists model or
/// blob rows, once more for them; it is never written.
///
/// This is how a `.gsnap` without a Store is served (`GraphSource::from_file`, CLI
/// `--graph file.gsnap`, Python `Graph.load`): `embed()`, `embed(text)`, text queries and
/// `g.embedders()` use its models. A file without models serves the built-in `hash-ngram/1`
/// only. Errors as [`read_snapshot`](crate::persist::read_snapshot), plus
/// [`PersistError::Io`] when the file cannot be opened and
/// [`StoreFailure::UpgradeModelDamaged`](crate::persist::StoreFailure::UpgradeModelDamaged) for
/// a 0.4.0 model row that does not convert.
///
/// ```rust
/// use graphersal::persist::{ReadOptions, read_snapshot_file};
/// use graphersal::prelude::*;
/// use graphersal::storage::AnyGraph;
///
/// let path = std::env::temp_dir().join(format!("doc-read-snapshot-file-{}.gsnap", std::process::id()));
/// let graph = GraphSource::tinkerpop_modern();
/// graph.write_snapshot(&mut std::fs::File::create(&path).unwrap()).unwrap();
/// let (loaded, report) = read_snapshot_file(&path, &ReadOptions::new()).unwrap();
/// assert_eq!(loaded.read().vertex_count(), 6);
/// assert!(report.models.is_empty() && report.notes("modern.gsnap").is_empty());
/// # std::fs::remove_file(&path).unwrap();
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn read_snapshot_file(
    path: impl AsRef<std::path::Path>,
    options: &crate::persist::ReadOptions,
) -> Result<(crate::graph::Graph, SnapshotLoadReport), PersistError> {
    use crate::persist::snapshot::PackedReader;
    let path = path.as_ref();
    let open = || {
        std::fs::File::open(path)
            .map(|file| std::io::BufReader::with_capacity(1 << 20, file))
            .map_err(|err| PersistError::io("opening a packed snapshot", err))
    };
    let graph = crate::persist::read_snapshot(open()?, options)?;
    let rows = PackedReader::open(open()?)?
        .manifest
        .segments
        .iter()
        .any(|entry| {
            entry.kind == super::blobs::PACKED_BLOB_KIND
                || entry.kind == super::models::PACKED_MODEL_KIND
        });
    let dir = if rows {
        let dir = DirPath::root(Arc::new(crate::persist::MemDir::named(
            path.display().to_string(),
        )));
        super::snapshot_dir::import_packed_models(&dir, open()?, &mut |_| {})?;
        Some(dir)
    } else {
        None
    };
    finish_load(graph, dir)
}
