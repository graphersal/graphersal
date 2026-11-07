//! A packed snapshot (`.gsnap`) loaded WITHOUT a Store, with its embedding models (format spec
//! 4.5, 4.6, 8; book page "Embedding Models in a Store", section "A `.gsnap` without a store"):
//! the load records where each model blob the file carries (rows of kind 4) lies in the file,
//! and a model that loads reads its blob from the file then (checked by length, CRC-32C and
//! SHA-256; nothing of it stays in memory but the loaded model); the model rows of a Graphersal
//! 0.4.0 file (kind 3) are converted the way the store upgrade converts them (format spec 15.1),
//! their tar rebuilt from the rows when the model loads. A graph without packed models has the
//! built-in `hash-ngram/1` only. The file must stay as it was: a changed, truncated or removed
//! file makes the model unavailable, naming the file.

// The file loader (`read_snapshot_file`) is native only; on wasm32 only the report type stays.
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use std::sync::Arc;

use super::model_catalog;
use super::upgrade::{self, ConvertedModel, MissingModel, VectorWithoutRule};
use super::{ModelInfo, ModelState};
use crate::graph::GraphStorage;
use crate::graph::note_text::NoteText;
use crate::persist::PersistError;

/// What [`read_snapshot_file`](crate::persist::read_snapshot_file) found in a packed snapshot
/// besides the graph: the embedding models it serves and what an earlier release left that this
/// version does not keep current. [`Self::notes`] is the one text every front end shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SnapshotLoadReport {
    /// The embedding models of the file's catalog (their blobs read from the file when a model
    /// loads; a model whose blob the file does not carry, a `--without-models` export, is
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
    /// The saved queries writing a vector property directly where a vector rule refuses it, or
    /// will once the rule [`Self::vectors_without_rule`] suggests exists (feature `script`).
    pub saved_queries_writing_vectors: Vec<crate::graph::definition::VectorPropertyWrite>,
}

impl SnapshotLoadReport {
    /// The notes for the file at `file` (the CLI prints each after `note:`, Python as a
    /// `UserWarning`): the converted models, the models stored but none the default, the
    /// missing models, the vectors without a rule and the saved queries calling a removed
    /// function. Each is a short headline, then indented lines (one detail `key: value` each;
    /// a shell command on a line of its own under a `# comment` saying what it does). Empty
    /// when there is nothing to say.
    pub fn notes(&self, file: &str) -> Vec<String> {
        let mut notes = Vec::new();
        let into_store = || {
            (
                "put the graph into a store (the file stays as it is)".to_string(),
                format!("graphersal store create <dir> --from {file}"),
            )
        };
        for converted in &self.models_converted {
            if converted.renamed() {
                notes.push(
                    NoteText::new(format!(
                        "the embedding model {} of {file} (Graphersal 0.4.0) is served as {}",
                        converted.former_identity(),
                        converted.identity()
                    ))
                    .detail(
                        "why",
                        "the catalog keeps one model per repository; the schema names the new \
                         identity",
                    )
                    .finish(),
                );
            } else {
                notes.push(
                    NoteText::new(format!(
                        "the embedding model {} of {file} (Graphersal 0.4.0) was converted in \
                         memory",
                        converted.repo()
                    ))
                    .detail("file", "stays as it is")
                    .command(
                        "keep the model converted: put the graph into a store",
                        &format!("graphersal store create <dir> --from {file}"),
                    )
                    .finish(),
                );
            }
        }
        // The only model the schema needs, in a file without a default: its import into the
        // store makes it the default.
        let needed: Vec<&str> = self
            .models_missing
            .iter()
            .map(MissingModel::identity)
            .chain(
                self.models
                    .iter()
                    .filter(|m| !m.needed_by.is_empty())
                    .map(|m| m.identity.as_str()),
            )
            .collect();
        let default = |identity: &str| {
            self.default_model.is_none()
                && needed.iter().all(|n| {
                    model_catalog::base_identity(n) == model_catalog::base_identity(identity)
                })
        };
        for model in &self.models {
            if model.state == ModelState::Missing {
                let mut steps = vec![into_store()];
                steps.extend(model_catalog::model_import_steps(
                    &model.repo,
                    "<dir>",
                    !model.default && default(&model.identity),
                    false,
                ));
                notes.push(
                    NoteText::new(format!(
                        "{file} names the embedding model {} but does not carry it",
                        model.identity
                    ))
                    .detail("repository", &model.repo)
                    .detail("why", "an export without models")
                    .detail("effect", "text queries and vector rules with it fail")
                    .commands(&steps)
                    .finish(),
                );
            }
        }
        if !self.models.is_empty() && self.default_model.is_none() {
            notes.push(
                NoteText::new(format!(
                    "{file} holds {} embedding model(s) but none is the default",
                    self.models.len()
                ))
                .detail("effect", "embed() without a model name uses hash-ngram/1")
                .detail(
                    "name one",
                    format!(
                        "g.v().embed(\"text\", #{{embedder: \"{}\"}})",
                        self.models[0].repo
                    ),
                )
                .command(
                    "or make one the default, in a store",
                    "graphersal store models default <dir> <repo>",
                )
                .finish(),
            );
        }
        for missing in &self.models_missing {
            let repo = missing.repo().unwrap_or("<repo>");
            let mut steps = vec![into_store()];
            steps.extend(model_catalog::model_import_steps(
                repo,
                "<dir>",
                default(missing.identity()),
                false,
            ));
            notes.push(
                NoteText::new(format!(
                    "the schema of {file} names the embedding model {repo}, which the file does \
                     not carry"
                ))
                .detail("identity", missing.identity())
                .detail("needed by", missing.needed_by().join(", "))
                .detail("effect", "text queries on those properties fail")
                .commands(&steps)
                .finish(),
            );
        }
        // ONE note per model; its command runs in a store: a schema change of the loaded file
        // stays in memory.
        notes.extend(upgrade::vector_notes(
            &self.vectors_without_rule,
            |_, patch| VectorWithoutRule::packed_steps(file, patch),
        ));
        for removed in &self.saved_queries_using_removed {
            notes.push(removed.note_for_packed(file));
        }
        notes.extend(
            self.saved_queries_writing_vectors
                .iter()
                .map(crate::catalog::VectorPropertyWrite::note),
        );
        notes
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod file {
    //! The models of a `.gsnap` read from the file itself (native only: `std::fs`).

    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::PathBuf;

    use crate::persist::error::PersistError;
    use std::sync::{Arc, Mutex, OnceLock, Weak};
    use std::time::SystemTime;

    use super::super::blobs::{BlobFaultError, CheckedBlob};
    use super::super::migrate_models::{LegacyFileSource, LegacyTar, hash_stream};
    use crate::graph::system_journal::SystemJournal;
    use crate::value::BlobRef;
    use crate::value::vector::embed::model_tar::{TarContent, TarFile, TarStream};
    use crate::value::vector::embed::registry::{LoadFailure, ModelBytes};

    /// Where the model blobs of a packed snapshot lie in its file.
    pub(super) struct PackedFileModels {
        pub(super) path: PathBuf,
        /// The blob rows (kind 4) as last read from the file.
        pub(super) located: Mutex<Located>,
        /// The 0.4.0 model rows (kind 3): row name, offset, length, CRC-32C.
        pub(super) rows: Vec<(String, u64, u64, u32)>,
        /// The converted 0.4.0 models: how each tar is built again.
        pub(super) converted: Vec<LegacyTar>,
        pub(super) journal: OnceLock<Weak<SystemJournal>>,
    }

    /// Where the blobs lay when the file looked like `stamp`: each blob and the offset of its
    /// bytes.
    pub(super) struct Located {
        pub(super) stamp: Option<(u64, Option<SystemTime>)>,
        pub(super) blobs: Vec<(BlobRef, u64)>,
    }

    /// What tells a rewritten file from the one that was read: its length and time.
    pub(super) fn stamp_of(path: &std::path::Path) -> Option<(u64, Option<SystemTime>)> {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.len(), meta.modified().ok()))
    }

    /// The blob rows of the packed file at `path` and where their bytes lie, by the section heads
    /// alone (format spec 8: a section's length is in its head; nothing else is read).
    fn scan_blobs(path: &std::path::Path) -> Result<Vec<(BlobRef, u64)>, PersistError> {
        use crate::persist::manifest::Manifest;
        use crate::persist::snapshot::extra_file_prefix;
        let bad = |what: &str| PersistError::corrupt(path.display().to_string(), 0, what);
        let io = |err| PersistError::io("reading a packed snapshot", err);
        let mut file = std::io::BufReader::new(File::open(path).map_err(io)?);
        let mut head = [0u8; 16];
        file.read_exact(&mut head).map_err(io)?;
        if !head.starts_with(crate::persist::snapshot::PACKED_MAGIC) {
            return Err(bad("it is not a packed snapshot"));
        }
        let section = |file: &mut std::io::BufReader<File>| -> Result<u64, PersistError> {
            let mut head = [0u8; 9];
            file.read_exact(&mut head).map_err(io)?;
            let mut len = [0u8; 8];
            len.copy_from_slice(&head[1..]);
            Ok(u64::from_le_bytes(len))
        };
        let len = section(&mut file)?;
        if len > crate::persist::snapshot::MAX_META_SECTION_BYTES {
            return Err(bad("its manifest is too large"));
        }
        let mut bytes = vec![0u8; len as usize];
        file.read_exact(&mut bytes).map_err(io)?;
        let manifest = Manifest::decode(&bytes).map_err(|_| bad("its manifest does not decode"))?;
        let schema = section(&mut file)?;
        file.seek_relative(schema as i64).map_err(io)?;
        let mut found = Vec::new();
        for entry in &manifest.segments {
            let len = section(&mut file)?;
            let at = file.stream_position().map_err(io)?;
            if entry.kind == super::super::blobs::PACKED_BLOB_KIND {
                let blob = super::super::blobs::packed_blob_ref(entry, &manifest.blobs)?;
                found.push((blob, at + extra_file_prefix(&entry.path).len() as u64));
            }
            file.seek(SeekFrom::Start(at + len)).map_err(io)?;
        }
        Ok(found)
    }

    impl PackedFileModels {
        /// Where `blob` lies now. A save over the file (the dev server's "Save to file", an export
        /// to this path) moves its bytes, so the file is read again when it changed; a blob it no
        /// longer lists keeps its old place, whose read then fails naming the changed file.
        fn offset_of(&self, blob: &BlobRef) -> Option<u64> {
            let mut located = self
                .located
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let stamp = stamp_of(&self.path);
            if stamp.is_some() && stamp != located.stamp {
                located.stamp = stamp;
                for (found, at) in scan_blobs(&self.path).unwrap_or_default() {
                    match located.blobs.iter_mut().find(|(known, _)| *known == found) {
                        Some((_, known_at)) => *known_at = at,
                        None => located.blobs.push((found, at)),
                    }
                }
            }
            located
                .blobs
                .iter()
                .find(|(known, _)| known == blob)
                .map(|(_, at)| *at)
        }

        /// Whether the file can serve `blob` (it lists it).
        pub(super) fn holds(&self, blob: &BlobRef) -> bool {
            self.offset_of(blob).is_some() || self.converted.iter().any(|t| &t.blob == blob)
        }

        fn failure(&self, what: impl std::fmt::Display) -> LoadFailure {
            LoadFailure::Damaged(format!(
                "its blob in the file {} {what} (the file changed after it was loaded: load it \
                 again, or keep the graph in a store)",
                self.path.display()
            ))
        }

        /// The bytes `blob` streams from: a blob row, or a converted model's tar built again.
        fn stream(&self, blob: &BlobRef) -> Result<Box<dyn Read + Send>, LoadFailure> {
            if let Some(at) = self.offset_of(blob) {
                let mut file = File::open(&self.path)
                    .map_err(|err| self.failure(format_args!("cannot be read: {err}")))?;
                file.seek(SeekFrom::Start(at))
                    .map_err(|err| self.failure(format_args!("cannot be read: {err}")))?;
                return Ok(Box::new(file.take(blob.length())));
            }
            let Some(tar) = self.converted.iter().find(|t| &t.blob == blob) else {
                return Err(LoadFailure::Missing);
            };
            let mut files = Vec::with_capacity(tar.files.len());
            for (name, size, sha256, source) in &tar.files {
                let content = match source {
                    LegacyFileSource::Bytes(bytes) => TarContent::Bytes(bytes.clone()),
                    LegacyFileSource::Row(row) => {
                        let Some((_, at, len, _)) = self.rows.iter().find(|(r, ..)| r == row)
                        else {
                            return Err(self.failure(format_args!("lacks the row {row}")));
                        };
                        let (path, at, len) = (self.path.clone(), *at, *len);
                        TarContent::Open(Box::new(move || {
                            let mut file = File::open(&path)?;
                            file.seek(SeekFrom::Start(at))?;
                            Ok(Box::new(file.take(len)) as Box<dyn Read + Send>)
                        }))
                    }
                };
                files.push(TarFile {
                    name: name.clone(),
                    size: *size,
                    sha256: sha256.clone(),
                    content,
                });
            }
            Ok(Box::new(TarStream::new(files)))
        }
    }

    impl ModelBytes for PackedFileModels {
        fn open(&self, blob: &BlobRef) -> Result<Box<dyn Read + Send + '_>, LoadFailure> {
            let reader = self.stream(blob)?;
            Ok(Box::new(CheckedBlob::new(reader, blob, true)))
        }

        fn read_failure(&self, err: std::io::Error) -> LoadFailure {
            match BlobFaultError::of(&err) {
                Some(_) => self
                    .failure("is not the model's blob (its length, checksum or SHA-256 differs)"),
                None => self.failure(format_args!("cannot be read: {err}")),
            }
        }

        fn check(&self, blob: &BlobRef) -> Result<(), LoadFailure> {
            let mut reader = self.stream(blob)?;
            let found = hash_stream(&mut reader.by_ref().take(blob.length().saturating_add(1)))
                .map_err(|err| self.failure(err))?;
            if &found != blob {
                return Err(self
                    .failure("is not the model's blob (its length, checksum or SHA-256 differs)"));
            }
            Ok(())
        }

        fn journal(&self) -> Option<Arc<SystemJournal>> {
            self.journal.get().and_then(Weak::upgrade)
        }
    }
}

/// Loads the packed snapshot at `path` with its embedding models: the graph as
/// [`read_snapshot`](crate::persist::read_snapshot) reads it, shared as a
/// [`Graph`](crate::graph::Graph) that serves the models the file carries, and the
/// [`SnapshotLoadReport`] of what it found. The file is read once; a model's blob is read from
/// it again only when the model loads (checked by its length, CRC-32C and SHA-256, the buffer
/// dropped after the load), so the graph keeps no copy of a blob. The 0.4.0 model rows are
/// converted (their tar streamed once through its hashes at the load, built again from the rows
/// when the model loads). This load never writes the file; a save of this graph over it keeps
/// the models, which are then found at their new place; any other changed, truncated or removed
/// file makes the model unavailable, naming the file.
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
    use crate::value::vector::embed::registry::ModelRegistry;
    use std::io::{Read, Seek, SeekFrom};
    let path = path.as_ref();
    let open = || {
        std::fs::File::open(path).map_err(|err| PersistError::io("opening a packed snapshot", err))
    };
    // One read: the graph, and where every blob and 0.4.0 model row lies.
    let mut blobs = Vec::new();
    let mut rows: Vec<(String, u64, u64, u32)> = Vec::new();
    let mut graph = crate::graph::TraversalGraph::new();
    crate::persist::snapshot::load_snapshot_with(
        std::io::BufReader::with_capacity(1 << 20, open()?),
        options,
        &mut graph,
        &mut |manifest, entry, at| {
            if entry.kind == super::blobs::PACKED_BLOB_KIND {
                blobs.push((super::blobs::packed_blob_ref(entry, &manifest.blobs)?, at));
            } else if entry.kind == super::models::PACKED_MODEL_KIND
                && super::models::parse_packed_name(&entry.path).is_some()
            {
                rows.push((entry.path.clone(), at, entry.byte_len, entry.crc));
            }
            Ok(())
        },
    )?;
    let mut report = SnapshotLoadReport::default();
    let mut converted = Vec::new();
    if !rows.is_empty() {
        // The 0.4.0 model rows, staged for the conversion in a temporary directory in memory
        // (dropped at the end of this block; only the tar recipes stay).
        let dir = super::files::DirPath::root(Arc::new(crate::persist::MemDir::named(
            path.display().to_string(),
        )));
        let mut staged: Vec<(String, super::files::DirPath)> = Vec::new();
        for (row, at, len, crc) in &rows {
            let Some((id, name)) = super::models::parse_packed_name(row) else {
                continue;
            };
            let target = match staged.iter().find(|(staged_id, _)| staged_id == id) {
                Some((_, target)) => target.clone(),
                None => {
                    let target = super::models::models_dir(&dir).join(format!(
                        "{}{:032x}",
                        super::files::TMP_PREFIX,
                        crate::value::uuid::new_v7()
                    ));
                    target
                        .create_dir_all()
                        .map_err(|err| PersistError::io("staging a model row", err))?;
                    staged.push((id.to_string(), target.clone()));
                    target
                }
            };
            crate::traversal::memory_budget::ensure_room("model row conversion", *len, 0)
                .map_err(PersistError::Graph)?;
            let mut file = open()?;
            file.seek(SeekFrom::Start(*at))
                .map_err(|err| PersistError::io("reading a model row", err))?;
            let mut bytes = Vec::new();
            file.take(*len)
                .read_to_end(&mut bytes)
                .map_err(|err| PersistError::io("reading a model row", err))?;
            if bytes.len() as u64 != *len || crate::persist::codec::crc(&bytes) != *crc {
                return Err(PersistError::corrupt(
                    path.display().to_string(),
                    *at,
                    format!("the model row {row:?} changed while the file was read"),
                ));
            }
            target
                .join(name)
                .write_atomic(&bytes)
                .map_err(|err| PersistError::io("staging a model row", err))?;
        }
        for (id, target) in &staged {
            super::models::install_staged(&dir, id, target)?;
        }
        for (model, recipe) in super::migrate_models::convert_in_memory(&dir, &mut graph)? {
            report.models_converted.push(ConvertedModel {
                repo: model.repo,
                identity: model.identity,
                former_identity: model.former_identity,
                bytes: model.bytes,
            });
            converted.extend(recipe);
        }
    }
    fill_report(&graph, &mut report)?;
    let definitions = model_catalog::model_definitions(&graph).map_err(PersistError::Graph)?;
    if definitions.is_empty() {
        return Ok((crate::graph::Graph::new_persistent(graph), report));
    }
    let source = Arc::new(file::PackedFileModels {
        path: path.to_path_buf(),
        located: std::sync::Mutex::new(file::Located {
            stamp: file::stamp_of(path),
            blobs,
        }),
        rows,
        converted,
        journal: std::sync::OnceLock::new(),
    });
    let schema = graph.get_schema().map_err(PersistError::Graph)?;
    report.models = model_catalog::infos_of(&definitions, schema.as_ref(), &|model| {
        if source.holds(model.blob()) {
            ModelState::NotLoaded
        } else {
            ModelState::Missing
        }
    });
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

/// The parts of the report read from the loaded graph: missing models, vectors without a rule
/// (counted), saved queries calling a removed function, the default model.
#[cfg(not(target_arch = "wasm32"))]
fn fill_report(
    graph: &crate::graph::TraversalGraph,
    report: &mut SnapshotLoadReport,
) -> Result<(), PersistError> {
    let definitions = model_catalog::model_definitions(graph).map_err(PersistError::Graph)?;
    let schema = graph.get_schema().map_err(PersistError::Graph)?;
    report.models_missing = upgrade::missing_models(schema.as_ref(), &definitions);
    let (wanted, mut found) = upgrade::vectors_without_rule(schema.as_ref(), &definitions);
    for ((edge, label, key), entry) in wanted.iter().zip(found.iter_mut()) {
        entry.elements = count_vectors(graph, *edge, label, key)?;
    }
    report.vectors_without_rule = found;
    report.saved_queries_using_removed = upgrade::saved_queries_using_removed(graph)?;
    report.saved_queries_writing_vectors = upgrade::saved_queries_writing_vectors(graph)?;
    report.default_model = definitions
        .iter()
        .filter_map(|d| d.as_model())
        .find(|m| m.is_default())
        .map(|m| m.repo().to_string());
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
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
