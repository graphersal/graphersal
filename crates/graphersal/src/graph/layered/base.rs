//! [`LayerBase`]: the frozen base a layer is built over, with what the bare
//! `Arc<TraversalGraph>` lacks: the embedding models of the base's database (the registry a
//! `Graph<TraversalGraph>` holds beside its storage), a display name, and the base's identity
//! ([`BaseRef`]: lineage id, commit sequence number and commit time, read from the frozen graph).

use std::sync::Arc;

use super::LayeredGraph;
use crate::error::GraphError;
use crate::graph::{Graph, TraversalGraph};
use crate::value::vector::embed::registry::ModelRegistry;

/// The name of a base nobody named ([`LayerBase::name`]).
pub(super) const DEFAULT_NAME: &str = "base";

/// The frozen base of any number of layers: a [`TraversalGraph`] nobody writes any more, the
/// embedding models of its database, a display name and its identity ([`Self::base_ref`]).
/// Book page "Layered Graphs".
///
/// **EXPERIMENTAL**, like [`LayeredGraph`].
///
/// A clone is cheap (one `Arc`): every layer made from a `LayerBase` and every clone of it share
/// one base graph and one model registry. [`Self::new_layer`] makes a shared
/// `Graph<LayeredGraph>` serving the base's models, so the vector rules of the layer's schema
/// that name a model of the base's catalog embed on the layer, and `embed()` and text `nearest`
/// on the layer use the base's models, as they do on the base's own `Graph`. The layer only uses
/// them: model definitions on a layer stay refused ([`GraphError::Unsupported`]).
///
/// ```
/// use std::sync::Arc;
/// use graphersal::prelude::*;
/// use graphersal::storage::{BaseRef, LayerBase};
///
/// let base = LayerBase::from_graph(Graph::new(TraversalGraph::tinkerpop_modern()))
///     .unwrap()
///     .with_name("modern");
/// let first = base.new_layer().unwrap();
/// let second = base.clone().new_layer().unwrap();
/// GraphTraversalSource::new(&mut *first.write()).add_v("person").iterate().unwrap();
/// assert_eq!(first.read().vertex_count().unwrap(), 7);
/// assert_eq!(second.read().vertex_count().unwrap(), 6);
/// assert_eq!(base.name(), "modern");
/// let identity = base.base_ref().unwrap();
/// assert_eq!(*identity, BaseRef::of(base.graph()));
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct LayerBase {
    inner: Arc<BaseInner>,
}

/// What every clone of one [`LayerBase`] shares.
#[derive(Clone)]
struct BaseInner {
    graph: Arc<TraversalGraph>,
    models: Option<Arc<ModelRegistry>>,
    name: String,
    base_ref: Option<BaseRef>,
}

/// The identity of a layer's base: which graph, at which commit. Read from the frozen base
/// graph itself ([`Self::of`]): its lineage id (`PersistentStorage::graph_id`, what a snapshot
/// or Store of the base records), its commit sequence number and the time of that commit. A
/// layer snapshot records the identity of the base it was built over, and a load over another
/// base is refused (`PersistError::LayerBaseMismatch`, feature `persist`). The time tells two branches apart that reached the same commit sequence number
/// after an in-place rollback.
///
/// **EXPERIMENTAL**, like [`LayeredGraph`].
///
/// ```
/// use graphersal::prelude::*;
/// use graphersal::storage::BaseRef;
///
/// let base = TraversalGraph::tinkerpop_modern();
/// let identity = BaseRef::of(&base);
/// assert_eq!(identity, BaseRef::of(&base));
/// assert_eq!(identity.commit_seq(), base.last_commit_seq());
/// assert_eq!(BaseRef::new(7, 3, 1_000).graph_id(), 7);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct BaseRef {
    graph_id: u128,
    commit_seq: u64,
    committed_at: i64,
}

impl BaseRef {
    /// The identity of the base graph of lineage `graph_id` at commit `commit_seq`, committed at
    /// `committed_at` (microseconds since the Unix epoch, UTC).
    pub fn new(graph_id: u128, commit_seq: u64, committed_at: i64) -> Self {
        Self {
            graph_id,
            commit_seq,
            committed_at,
        }
    }

    /// The identity of `graph` as it is now: its lineage id (created on first use, as a
    /// snapshot write creates it), its last commit sequence number and that commit's time.
    pub fn of(graph: &TraversalGraph) -> Self {
        Self::new(
            *graph.graph_id.get_or_init(crate::value::uuid::new_v7),
            graph.undo.last_commit_seq,
            graph.undo.last_committed_at,
        )
    }

    /// The base's lineage id (`PersistentStorage::graph_id`).
    pub fn graph_id(&self) -> u128 {
        self.graph_id
    }

    /// The base's last commit sequence number.
    pub fn commit_seq(&self) -> u64 {
        self.commit_seq
    }

    /// The time of the base's last commit (microseconds since the Unix epoch, UTC).
    pub fn committed_at(&self) -> i64 {
        self.committed_at
    }
}

/// `graph <lineage id> at commit <seq> (commit time <micros>)`.
impl std::fmt::Display for BaseRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "graph {} at commit {} (commit time {})",
            crate::value::uuid::format_canonical(self.graph_id),
            self.commit_seq,
            self.committed_at
        )
    }
}

impl std::fmt::Debug for LayerBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerBase")
            .field("name", &self.inner.name)
            .field("models", &self.inner.models.is_some())
            .field("base_ref", &self.inner.base_ref)
            .finish_non_exhaustive()
    }
}

impl LayerBase {
    /// A base of the graph alone, without embedding models: its layers embed only with the
    /// built-in `hash-ngram/1`, like a `Graph<TraversalGraph>` no database attached models to.
    pub fn new(graph: Arc<TraversalGraph>) -> Self {
        Self::of(graph, None)
    }

    /// A base of `graph`'s storage together with the embedding models of its database (those a
    /// Store, a recovered view or a `.gsnap` read attached to it). The storage is moved out of
    /// the `Graph` and is never written again.
    ///
    /// # Errors
    ///
    /// [`GraphError::Poisoned`] when `graph` is poisoned ([`Graph::is_poisoned`]): what its
    /// storage holds after a failed rollback is unknown, so it cannot be a base.
    pub fn from_graph(graph: Graph<TraversalGraph>) -> Result<Self, GraphError> {
        let (storage, models) = graph.into_storage_and_models()?;
        Ok(Self::of(Arc::new(storage), models))
    }

    fn of(graph: Arc<TraversalGraph>, models: Option<Arc<ModelRegistry>>) -> Self {
        let base_ref = Some(BaseRef::of(&graph));
        Self {
            inner: Arc::new(BaseInner {
                graph,
                models,
                name: DEFAULT_NAME.to_owned(),
                base_ref,
            }),
        }
    }

    /// The base with the display name `name` (hosts show it; the engine never reads it). The
    /// clones made before keep their name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        Arc::make_mut(&mut self.inner).name = name.into();
        self
    }

    /// The display name: the one [`Self::with_name`] gave, else `"base"`.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The frozen base graph.
    pub fn graph(&self) -> &Arc<TraversalGraph> {
        &self.inner.graph
    }

    /// The base's identity ([`BaseRef::of`] the frozen graph, read when the base was made):
    /// always `Some` in this version (`None` is kept for a base without an identity).
    pub fn base_ref(&self) -> Option<&BaseRef> {
        self.inner.base_ref.as_ref()
    }

    /// Whether the base carries the embedding models of a database ([`Self::from_graph`] of a
    /// graph with models attached). `false`: its layers embed only with `hash-ngram/1`.
    pub fn serves_models(&self) -> bool {
        self.inner.models.is_some()
    }

    /// The base graph back as a writable [`Graph`] serving the base's embedding models, when
    /// this is the last handle of the base: every clone of it and every layer over it are gone.
    /// Otherwise `Err` with the base itself, unchanged (like `Arc::try_unwrap`: nothing failed,
    /// the base is still shared). The way back from [`Self::from_graph`] once the layers are
    /// dropped (the playground's "drop the layer").
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::storage::LayerBase;
    ///
    /// let base = LayerBase::from_graph(Graph::new(TraversalGraph::tinkerpop_modern())).unwrap();
    /// let layer = base.new_layer().unwrap();
    /// let base = base.into_graph().unwrap_err(); // the layer still reads it
    /// drop(layer);
    /// let graph = base.into_graph().unwrap();
    /// GraphTraversalSource::new(&mut *graph.write()).add_v("person").iterate().unwrap();
    /// assert_eq!(graph.read().vertex_count(), 7);
    /// ```
    pub fn into_graph(self) -> Result<Graph<TraversalGraph>, Self> {
        let inner = Arc::try_unwrap(self.inner).map_err(|inner| Self { inner })?;
        let BaseInner {
            graph,
            models,
            name,
            base_ref,
        } = inner;
        match Arc::try_unwrap(graph) {
            Ok(storage) => {
                let graph = Graph::new(storage);
                if let Some(models) = models {
                    // A fresh graph has no registry yet, so the attach cannot be refused.
                    graph.attach_model_registry(models);
                }
                Ok(graph)
            }
            Err(graph) => Err(Self {
                inner: Arc::new(BaseInner {
                    graph,
                    models,
                    name,
                    base_ref,
                }),
            }),
        }
    }

    /// A new, empty layer over the base, shared as a [`Graph`] that serves the base's embedding
    /// models (see the type's docs).
    ///
    /// # Errors
    ///
    /// As [`LayeredGraph::over`]: [`GraphError::Unsupported`] when the base cannot carry a
    /// layer.
    pub fn new_layer(&self) -> Result<Graph<LayeredGraph>, GraphError> {
        Ok(self.share(LayeredGraph::over_base(self)?))
    }

    /// `layer` (a layer over this base) shared as a [`Graph`] that serves the base's embedding
    /// models, as [`Self::new_layer`] makes it (a layer snapshot load uses it too).
    pub(crate) fn share(&self, layer: LayeredGraph) -> Graph<LayeredGraph> {
        self.attach_models(Graph::new(layer))
    }

    /// [`Self::share`] as a persistent graph ([`Graph::new_persistent`]: it keeps the
    /// snapshot writer), the live graph of a layer store.
    #[cfg(feature = "persist")]
    pub(crate) fn share_persistent(&self, layer: LayeredGraph) -> Graph<LayeredGraph> {
        self.attach_models(Graph::new_persistent(layer))
    }

    /// The base's embedding models (a layer store's views serve them).
    #[cfg(feature = "persist")]
    pub(crate) fn models(&self) -> Option<Arc<ModelRegistry>> {
        self.inner.models.clone()
    }

    fn attach_models(&self, graph: Graph<LayeredGraph>) -> Graph<LayeredGraph> {
        if let Some(models) = &self.inner.models {
            // A fresh graph has no registry yet, so the attach cannot be refused.
            graph.attach_model_registry(Arc::clone(models));
        }
        graph
    }
}

impl LayeredGraph {
    /// A new, empty layer over `base`'s graph, as [`LayeredGraph::over`]. A bare storage holds
    /// no embedding models: [`LayerBase::new_layer`] makes the shared `Graph` that serves the
    /// base's.
    ///
    /// # Errors
    ///
    /// As [`LayeredGraph::over`].
    pub fn over_base(base: &LayerBase) -> Result<Self, GraphError> {
        let mut layer = Self::over(Arc::clone(base.graph()))?;
        layer.base_name = base.name().into();
        Ok(layer)
    }

    /// The display name of the base the layer was made over ([`LayerBase::name`]; `"base"`
    /// for a layer made by [`LayeredGraph::over`]).
    pub fn base_name(&self) -> &str {
        &self.base_name
    }

    /// The identity of the layer's base ([`BaseRef::of`] the shared base graph): what a layer
    /// snapshot records and checks at load.
    pub fn base_ref(&self) -> BaseRef {
        BaseRef::of(&self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Embedders;

    /// A graph serving a registry of its own (a database's models, here the built-in only).
    fn graph_with_models() -> (Graph<TraversalGraph>, Arc<ModelRegistry>) {
        let graph = Graph::new(TraversalGraph::tinkerpop_modern());
        let registry = ModelRegistry::of_embedders(Embedders::default());
        assert!(graph.attach_model_registry(Arc::clone(&registry)));
        (graph, registry)
    }

    #[test]
    fn every_layer_of_a_base_serves_the_bases_registry() {
        let (graph, registry) = graph_with_models();
        let base = LayerBase::from_graph(graph).unwrap();
        assert!(base.serves_models());
        let clone = base.clone();
        for layer in [base.new_layer().unwrap(), clone.new_layer().unwrap()] {
            let attached = layer.model_registry().unwrap();
            assert!(Arc::ptr_eq(&attached, &registry));
        }
        assert!(Arc::ptr_eq(base.graph(), clone.graph()));
    }

    #[test]
    fn a_base_without_models_attaches_none() {
        let base = LayerBase::new(Arc::new(TraversalGraph::tinkerpop_modern()));
        assert!(!base.serves_models());
        assert!(base.new_layer().unwrap().model_registry().is_none());
        assert_eq!(base.name(), "base");
        assert_eq!(base.base_ref(), Some(&BaseRef::of(base.graph())));
    }

    #[test]
    fn a_name_belongs_to_the_base_it_was_given_to() {
        let base = LayerBase::new(Arc::new(TraversalGraph::new())).with_name("first");
        let renamed = base.clone().with_name("second");
        assert_eq!((base.name(), renamed.name()), ("first", "second"));
        assert!(Arc::ptr_eq(base.graph(), renamed.graph()));
        assert!(format!("{renamed:?}").contains("second"));
    }

    /// A layer read from its snapshot over a base serves the base's registry, as a new layer
    /// does.
    #[cfg(feature = "persist")]
    #[test]
    fn a_layer_read_from_a_snapshot_serves_the_bases_registry() {
        use crate::persist::{ReadOptions, WriteOptions, read_layer_snapshot, write_snapshot};
        let (graph, registry) = graph_with_models();
        let base = LayerBase::from_graph(graph).unwrap();
        let layer = base.new_layer().unwrap();
        let mut bytes = Vec::new();
        write_snapshot(&*layer.read(), &mut bytes, &WriteOptions::new()).unwrap();
        let loaded = read_layer_snapshot(bytes.as_slice(), &ReadOptions::new(), &base).unwrap();
        assert!(Arc::ptr_eq(&loaded.model_registry().unwrap(), &registry));
        assert_eq!(loaded.read().base_ref(), *base.base_ref().unwrap());
    }
}
