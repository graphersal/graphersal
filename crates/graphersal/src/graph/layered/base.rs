//! [`LayerBase`]: the frozen base a layer is built over, with what the bare
//! `Arc<TraversalGraph>` lacks: the embedding models of the base's database (the registry a
//! `Graph<TraversalGraph>` holds beside its storage), a display name, and the base's identity
//! ([`BaseRef`], not filled yet).

use std::sync::Arc;

use super::LayeredGraph;
use crate::error::GraphError;
use crate::graph::{Graph, TraversalGraph};
use crate::value::vector::embed::registry::ModelRegistry;

/// The name of a base nobody named ([`LayerBase::name`]).
const DEFAULT_NAME: &str = "base";

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
/// use graphersal::storage::LayerBase;
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
/// assert!(base.base_ref().is_none());
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

/// The identity of a layer's base: which graph, at which commit. A placeholder for now:
/// [`LayerBase::base_ref`] answers `None` in this version; a later one fills it from the
/// base's Store or snapshot file (graph lineage id, commit sequence and commit time), so a saved
/// layer can check it is opened over the base it was built on.
///
/// **EXPERIMENTAL**, like [`LayeredGraph`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct BaseRef {}

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
        Self {
            inner: Arc::new(BaseInner {
                graph,
                models,
                name: DEFAULT_NAME.to_owned(),
                base_ref: None,
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

    /// The base's identity ([`BaseRef`]): `None` in this version.
    pub fn base_ref(&self) -> Option<&BaseRef> {
        self.inner.base_ref.as_ref()
    }

    /// Whether the base carries the embedding models of a database ([`Self::from_graph`] of a
    /// graph with models attached). `false`: its layers embed only with `hash-ngram/1`.
    pub fn serves_models(&self) -> bool {
        self.inner.models.is_some()
    }

    /// A new, empty layer over the base, shared as a [`Graph`] that serves the base's embedding
    /// models (see the type's docs).
    ///
    /// # Errors
    ///
    /// As [`LayeredGraph::over`]: [`GraphError::Unsupported`] when the base cannot carry a
    /// layer.
    pub fn new_layer(&self) -> Result<Graph<LayeredGraph>, GraphError> {
        let graph = Graph::new(LayeredGraph::over_base(self)?);
        if let Some(models) = &self.inner.models {
            // A fresh graph has no registry yet, so the attach cannot be refused.
            graph.attach_model_registry(Arc::clone(models));
        }
        Ok(graph)
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
        Self::over(Arc::clone(base.graph()))
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
        assert_eq!(base.base_ref(), None);
    }

    #[test]
    fn a_name_belongs_to_the_base_it_was_given_to() {
        let base = LayerBase::new(Arc::new(TraversalGraph::new())).with_name("first");
        let renamed = base.clone().with_name("second");
        assert_eq!((base.name(), renamed.name()), ("first", "second"));
        assert!(Arc::ptr_eq(base.graph(), renamed.graph()));
        assert!(format!("{renamed:?}").contains("second"));
    }
}
