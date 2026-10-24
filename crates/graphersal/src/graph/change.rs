// Change capture; its public documentation is the module docs of `crate::changes`.

use compact_str::CompactString;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::graph::definition::{Definition, DefinitionKind};
use crate::graph::schema::{ElementKind, GraphSchema};
use crate::value::{ElementProperty, PropertyMap};

/// Names one graph element in a [`Mutation`]: its kind and its string id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct ElementRef {
    /// Vertex or edge.
    pub kind: ElementKind,
    /// The element's id.
    pub id: String,
}

impl ElementRef {
    /// A reference to the vertex with `id`.
    pub fn vertex(id: impl Into<String>) -> Self {
        Self {
            kind: ElementKind::Vertex,
            id: id.into(),
        }
    }

    /// A reference to the edge with `id`.
    pub fn edge(id: impl Into<String>) -> Self {
        Self {
            kind: ElementKind::Edge,
            id: id.into(),
        }
    }
}

/// The full state of a vertex: the before-image of a [`Mutation::DropVertex`].
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct VertexState {
    /// The label set, primary label first.
    pub labels: Vec<String>,
    /// The properties, in stored order.
    #[cfg_attr(feature = "serde", serde(with = "tagged_element"))]
    pub properties: PropertyMap,
}

/// The full state of an edge: the before-image of a [`Mutation::DropEdge`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct EdgeState {
    /// The edge label (`None` for an unlabeled edge).
    pub label: Option<String>,
    /// The id of the source vertex.
    pub out_id: String,
    /// The id of the target vertex.
    pub in_id: String,
    /// The properties, in stored order.
    #[cfg_attr(feature = "serde", serde(with = "tagged_element"))]
    pub properties: PropertyMap,
}

impl VertexState {
    /// The state of a vertex with this label set (primary label first) and these properties.
    pub fn new(
        labels: impl IntoIterator<Item = impl Into<String>>,
        properties: PropertyMap,
    ) -> Self {
        Self {
            labels: labels.into_iter().map(Into::into).collect(),
            properties,
        }
    }
}

impl EdgeState {
    /// The state of an edge from `out_id` to `in_id` with this label (`None`: unlabeled) and
    /// these properties.
    pub fn new(
        label: Option<&str>,
        out_id: impl Into<String>,
        in_id: impl Into<String>,
        properties: PropertyMap,
    ) -> Self {
        Self {
            label: label.map(str::to_string),
            out_id: out_id.into(),
            in_id: in_id.into(),
            properties,
        }
    }
}

/// One change to the graph, with the values before and after it.
///
/// `#[non_exhaustive]`, like every variant: more change kinds and fields may be added (match
/// with `..`). A storage that captures changes itself builds values with the constructors
/// ([`Mutation::add_vertex`], [`Mutation::set_property`], ...; one per kind) and puts them into
/// a [`ChangeSet::builder`].
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(tag = "op", rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum Mutation {
    /// A vertex was added with these labels and properties (whichever call made it: a query's
    /// create, an import, ...). Applying or replaying it stores exactly this
    /// ([`GraphStorage::insert_vertex`](crate::storage::GraphStorage::insert_vertex)): the
    /// schema defaults a create filled in are part of `properties` and never applied again.
    #[non_exhaustive]
    AddVertex {
        /// The new vertex's id.
        id: String,
        /// Its label set, primary label first.
        labels: Vec<String>,
        /// Its properties as stored (after schema coercion and create-time defaults).
        #[cfg_attr(feature = "serde", serde(with = "tagged_element"))]
        properties: PropertyMap,
    },
    /// A vertex was removed; its incident edges were removed by the [`Mutation::DropEdge`]s
    /// right before this entry.
    #[non_exhaustive]
    DropVertex {
        /// The removed vertex's id.
        id: String,
        /// Its state right before the removal.
        before: VertexState,
    },
    /// An edge was added; applied or replayed exactly as recorded
    /// ([`GraphStorage::insert_edge`](crate::storage::GraphStorage::insert_edge)), like
    /// [`Mutation::AddVertex`].
    #[non_exhaustive]
    AddEdge {
        /// The new edge's id.
        id: String,
        /// Its label (`None` for an unlabeled edge).
        label: Option<String>,
        /// The id of the source vertex.
        out_id: String,
        /// The id of the target vertex.
        in_id: String,
        /// Its properties as stored (after schema coercion and create-time defaults).
        #[cfg_attr(feature = "serde", serde(with = "tagged_element"))]
        properties: PropertyMap,
    },
    /// An edge was removed (directly, or as part of a vertex drop).
    #[non_exhaustive]
    DropEdge {
        /// The removed edge's id.
        id: String,
        /// Its state right before the removal.
        before: EdgeState,
    },
    /// A top-level property was written (also by a jpath write, with the whole new value).
    #[non_exhaustive]
    SetProperty {
        /// The element that owns the property.
        element: ElementRef,
        /// The top-level property key.
        key: String,
        /// The value before the write; `None` when the property was absent.
        #[cfg_attr(
            feature = "serde",
            serde(
                default,
                skip_serializing_if = "Option::is_none",
                serialize_with = "tagged_option::serialize",
                deserialize_with = "tagged_option::deserialize"
            )
        )]
        before: Option<ElementProperty>,
        /// The value after the write, as stored.
        #[cfg_attr(feature = "serde", serde(with = "tagged_value"))]
        after: ElementProperty,
    },
    /// A top-level property was removed.
    #[non_exhaustive]
    RemoveProperty {
        /// The element that owned the property.
        element: ElementRef,
        /// The top-level property key.
        key: String,
        /// The value before the removal.
        #[cfg_attr(feature = "serde", serde(with = "tagged_value"))]
        before: ElementProperty,
    },
    /// A label was added to a vertex's label set.
    #[non_exhaustive]
    AddLabel {
        /// The vertex's id.
        id: String,
        /// The added label.
        label: String,
    },
    /// A label was removed from a vertex's label set.
    #[non_exhaustive]
    RemoveLabel {
        /// The vertex's id.
        id: String,
        /// The removed label.
        label: String,
    },
    /// The label of an edge was changed (edges are single-labeled).
    #[non_exhaustive]
    SetEdgeLabel {
        /// The edge's id.
        id: String,
        /// The label before; `None` when the edge was unlabeled.
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Option::is_none")
        )]
        before: Option<String>,
        /// The label after.
        after: String,
    },
    /// The stored schema was replaced (an empty mode-`None` schema stands for "no schema").
    #[non_exhaustive]
    SetSchema {
        /// The schema before.
        before: Box<GraphSchema>,
        /// The schema after.
        after: Box<GraphSchema>,
    },
    /// A definition of the catalog (a saved query, ...) was stored, replaced or removed: one
    /// mutation for every kind and every change (define: no `before`; remove: no `after`;
    /// replace, move, describe: both).
    #[non_exhaustive]
    SetDefinition {
        /// The kind of the definition.
        kind: DefinitionKind,
        /// Its name.
        name: String,
        /// The definition before; `None` when there was none.
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Option::is_none")
        )]
        before: Option<Box<Definition>>,
        /// The definition after; `None` when it was removed.
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Option::is_none")
        )]
        after: Option<Box<Definition>>,
    },
}

impl Mutation {
    /// [`Mutation::AddVertex`]: the vertex `id` was added with this label set (primary label
    /// first) and these properties (as stored).
    pub fn add_vertex(
        id: impl Into<String>,
        labels: impl IntoIterator<Item = impl Into<String>>,
        properties: PropertyMap,
    ) -> Self {
        Mutation::AddVertex {
            id: id.into(),
            labels: labels.into_iter().map(Into::into).collect(),
            properties,
        }
    }

    /// [`Mutation::DropVertex`]: the vertex `id` was removed; `before` is its state right
    /// before (its incident edges are [`Mutation::drop_edge`]s right before this one).
    pub fn drop_vertex(id: impl Into<String>, before: VertexState) -> Self {
        Mutation::DropVertex {
            id: id.into(),
            before,
        }
    }

    /// [`Mutation::AddEdge`]: the edge `id` from `out_id` to `in_id` was added with this label
    /// (`None`: unlabeled) and these properties (as stored).
    pub fn add_edge(
        id: impl Into<String>,
        label: Option<&str>,
        out_id: impl Into<String>,
        in_id: impl Into<String>,
        properties: PropertyMap,
    ) -> Self {
        Mutation::AddEdge {
            id: id.into(),
            label: label.map(str::to_string),
            out_id: out_id.into(),
            in_id: in_id.into(),
            properties,
        }
    }

    /// [`Mutation::DropEdge`]: the edge `id` was removed; `before` is its state right before.
    pub fn drop_edge(id: impl Into<String>, before: EdgeState) -> Self {
        Mutation::DropEdge {
            id: id.into(),
            before,
        }
    }

    /// [`Mutation::SetProperty`]: the top-level property `key` of `element` was written
    /// (`before`: `None` when it was absent).
    pub fn set_property(
        element: ElementRef,
        key: impl Into<String>,
        before: Option<ElementProperty>,
        after: ElementProperty,
    ) -> Self {
        Mutation::SetProperty {
            element,
            key: key.into(),
            before,
            after,
        }
    }

    /// [`Mutation::RemoveProperty`]: the top-level property `key` of `element`, which held
    /// `before`, was removed.
    pub fn remove_property(
        element: ElementRef,
        key: impl Into<String>,
        before: ElementProperty,
    ) -> Self {
        Mutation::RemoveProperty {
            element,
            key: key.into(),
            before,
        }
    }

    /// [`Mutation::AddLabel`]: `label` was added to the label set of the vertex `id`.
    pub fn add_label(id: impl Into<String>, label: impl Into<String>) -> Self {
        Mutation::AddLabel {
            id: id.into(),
            label: label.into(),
        }
    }

    /// [`Mutation::RemoveLabel`]: `label` was removed from the label set of the vertex `id`.
    pub fn remove_label(id: impl Into<String>, label: impl Into<String>) -> Self {
        Mutation::RemoveLabel {
            id: id.into(),
            label: label.into(),
        }
    }

    /// [`Mutation::SetEdgeLabel`]: the label of the edge `id` changed from `before` (`None`:
    /// unlabeled) to `after`.
    pub fn set_edge_label(
        id: impl Into<String>,
        before: Option<&str>,
        after: impl Into<String>,
    ) -> Self {
        Mutation::SetEdgeLabel {
            id: id.into(),
            before: before.map(str::to_string),
            after: after.into(),
        }
    }

    /// [`Mutation::SetSchema`]: the stored schema was replaced (an empty mode-`None` schema
    /// stands for "no schema").
    pub fn set_schema(before: GraphSchema, after: GraphSchema) -> Self {
        Mutation::SetSchema {
            before: Box::new(before),
            after: Box::new(after),
        }
    }

    /// [`Mutation::SetDefinition`]: the catalog definition of `kind` named `name` was stored
    /// (`before` `None`), replaced (both) or removed (`after` `None`).
    /// [`ChangeSetBuilder::build`] checks that at least one image is present and that both
    /// have this kind and name.
    pub fn set_definition(
        kind: DefinitionKind,
        name: impl Into<String>,
        before: Option<Definition>,
        after: Option<Definition>,
    ) -> Self {
        Mutation::SetDefinition {
            kind,
            name: name.into(),
            before: before.map(Box::new),
            after: after.map(Box::new),
        }
    }

    /// Why this mutation cannot be part of a change set (`None`: it can).
    fn invalid_reason(&self) -> Option<String> {
        match self {
            Mutation::SetDefinition {
                kind,
                name,
                before,
                after,
            } => {
                if before.is_none() && after.is_none() {
                    return Some(format!(
                        "the definition change of {kind} '{name}' has neither a before nor an \
                         after image"
                    ));
                }
                let mismatch = [before, after]
                    .into_iter()
                    .flatten()
                    .find(|image| image.kind() != *kind || image.name() != name.as_str());
                mismatch.map(|image| {
                    format!(
                        "the definition change of {kind} '{name}' carries an image of {} '{}'",
                        image.kind(),
                        image.name()
                    )
                })
            }
            Mutation::AddVertex { .. }
            | Mutation::DropVertex { .. }
            | Mutation::AddEdge { .. }
            | Mutation::DropEdge { .. }
            | Mutation::SetProperty { .. }
            | Mutation::RemoveProperty { .. }
            | Mutation::AddLabel { .. }
            | Mutation::RemoveLabel { .. }
            | Mutation::SetEdgeLabel { .. }
            | Mutation::SetSchema { .. } => None,
        }
    }

    /// Decompresses every compressed string the mutation's values hold (a change set carries
    /// logical values only).
    pub(crate) fn make_logical(&mut self) {
        fn element(properties: &mut PropertyMap) {
            properties
                .values_mut()
                .for_each(ElementProperty::make_logical);
        }
        match self {
            Mutation::AddVertex { properties, .. } | Mutation::AddEdge { properties, .. } => {
                element(properties)
            }
            Mutation::DropVertex { before, .. } => element(&mut before.properties),
            Mutation::DropEdge { before, .. } => element(&mut before.properties),
            Mutation::SetProperty { before, after, .. } => {
                if let Some(before) = before {
                    before.make_logical();
                }
                after.make_logical();
            }
            Mutation::RemoveProperty { before, .. } => before.make_logical(),
            Mutation::AddLabel { .. }
            | Mutation::RemoveLabel { .. }
            | Mutation::SetEdgeLabel { .. }
            | Mutation::SetSchema { .. }
            | Mutation::SetDefinition { .. } => {}
        }
    }
}

/// Serde of one property value in the tagged form ([`crate::value::physical::Tagged`]): a
/// `uuid` is `{"@type": "g:UUID", "@value": ".."}`, so a change set replays exactly without a
/// schema.
#[cfg(feature = "serde")]
mod tagged_value {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::value::ElementProperty;
    use crate::value::physical::{Tagged, TaggedOwned};

    pub(super) fn serialize<S: Serializer>(
        value: &ElementProperty,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Tagged(value).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ElementProperty, D::Error> {
        TaggedOwned::deserialize(deserializer).map(|TaggedOwned(value)| value)
    }
}

/// [`tagged_value`] for an optional value. A present JSON value reads as `Some(value)`, so a
/// stored `null` (`Some(Null)`) stays distinct from an absent property (field omitted, `None`).
#[cfg(feature = "serde")]
mod tagged_option {
    use serde::{Deserializer, Serializer};

    use crate::value::ElementProperty;

    pub(super) fn serialize<S: Serializer>(
        value: &Option<ElementProperty>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => super::tagged_value::serialize(value, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ElementProperty>, D::Error> {
        super::tagged_value::deserialize(deserializer).map(Some)
    }
}

/// [`tagged_value`] for a property map: the map itself is a plain JSON object, its values are
/// tagged.
#[cfg(feature = "serde")]
mod tagged_element {
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};

    use crate::value::PropertyMap;
    use crate::value::physical::{Tagged, TaggedOwned};

    pub(super) fn serialize<S: Serializer>(
        element: &PropertyMap,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(element.len()))?;
        for (key, value) in element {
            map.serialize_entry(key.as_str(), &Tagged(value))?;
        }
        map.end()
    }

    struct ElementVisitor;

    impl<'de> Visitor<'de> for ElementVisitor {
        type Value = PropertyMap;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a property map")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<PropertyMap, A::Error> {
            let mut element = PropertyMap::with_capacity(access.size_hint().unwrap_or(0));
            while let Some((key, TaggedOwned(value))) =
                access.next_entry::<String, TaggedOwned>()?
            {
                element.insert(key, value);
            }
            Ok(element)
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PropertyMap, D::Error> {
        deserializer.deserialize_map(ElementVisitor)
    }
}

/// Commit metadata: an optional principal and free attributes, stored and passed through with a
/// [`ChangeSet`], never interpreted by the graph.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct CommitMetadata {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    principal: Option<String>,
    #[cfg_attr(
        feature = "serde",
        serde(
            default,
            skip_serializing_if = "PropertyMap::is_empty",
            with = "tagged_element"
        )
    )]
    attributes: PropertyMap,
}

impl CommitMetadata {
    /// Empty metadata: no principal, no attributes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the principal (who committed).
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// Adds (or replaces) a free attribute.
    pub fn with_attribute(mut self, key: &str, value: impl Into<ElementProperty>) -> Self {
        self.attributes
            .insert(CompactString::from(key), value.into());
        self
    }

    /// The principal, if one was set.
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    /// The free attributes, in insertion order.
    pub fn attributes(&self) -> &PropertyMap {
        &self.attributes
    }

    /// Adds what `self` does not have yet from `other`: its principal when none is set, its
    /// attributes whose keys are not set (a nested transaction's metadata merged into the
    /// enclosing one, which wins).
    pub(crate) fn merge_missing(&mut self, other: CommitMetadata) {
        if self.principal.is_none() {
            self.principal = other.principal;
        }
        for (key, value) in other.attributes {
            self.attributes.insert_if_absent(&key, || value);
        }
    }
}

/// What one committed unit changed: its commit sequence number, commit time, the auto-id
/// sequences after it, its metadata and the [`Mutation`]s in execution order (see the
/// [module docs](crate::changes)). With `serde`, the position fields that are missing from a serialized
/// change set read as `0` (unknown: [`TraversalGraph::replay`](crate::graph::TraversalGraph::replay)
/// then raises nothing).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub struct ChangeSet {
    commit_seq: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    committed_at: i64,
    #[cfg_attr(feature = "serde", serde(default))]
    node_sequence: u64,
    #[cfg_attr(feature = "serde", serde(default))]
    edge_sequence: u64,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "is_default_metadata")
    )]
    metadata: CommitMetadata,
    mutations: Vec<Mutation>,
}

/// The position stamp of a [`ChangeSet`]: its commit sequence number, commit time and the
/// auto-id sequences after the unit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CommitStamp {
    pub(crate) commit_seq: u64,
    pub(crate) committed_at: i64,
    pub(crate) node_sequence: u64,
    pub(crate) edge_sequence: u64,
}

/// Builds a [`ChangeSet`] outside the graph: what a storage that captures changes itself hands
/// to its [`CommitHooks`](crate::storage::CommitHooks) (and what a test or a replication feed
/// replays). Start it with [`ChangeSet::builder`].
///
/// ```rust
/// use graphersal::changes::{ChangeSet, CommitMetadata, ElementRef, Mutation};
/// use graphersal::prelude::*;
///
/// let mut builder = ChangeSet::builder(1, 1_700_000_000_000_000)
///     .with_sequences(0, 0)
///     .with_metadata(CommitMetadata::new().with_principal("importer"));
/// builder
///     .push(Mutation::add_vertex("ann", ["person"], Default::default()))
///     .push(Mutation::set_property(
///         ElementRef::vertex("ann"),
///         "age",
///         None,
///         ElementProperty::from(29i64),
///     ));
/// let changes = builder.build().unwrap();
///
/// let mut graph = TraversalGraph::new();
/// graph.apply_change_set(&changes).unwrap();
/// assert_eq!(graph.vertex_count(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct ChangeSetBuilder {
    stamp: CommitStamp,
    metadata: CommitMetadata,
    mutations: Vec<Mutation>,
}

impl ChangeSetBuilder {
    /// Sets the auto-id sequences after the unit ([`ChangeSet::node_sequence`],
    /// [`ChangeSet::edge_sequence`]; default `0`: nothing to raise).
    pub fn with_sequences(mut self, node_sequence: u64, edge_sequence: u64) -> Self {
        self.stamp.node_sequence = node_sequence;
        self.stamp.edge_sequence = edge_sequence;
        self
    }

    /// Sets the commit metadata (default: none).
    pub fn with_metadata(mut self, metadata: CommitMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Appends a mutation (the change set keeps execution order).
    pub fn push(&mut self, mutation: Mutation) -> &mut Self {
        self.mutations.push(mutation);
        self
    }

    /// The change set, after the cheap checks: a `commit_seq` of `0` is a never-committed
    /// change set (a dry run) and must have `committed_at` `0`; a commit time is never negative;
    /// a definition change has a before or an after image of its own kind and name. Otherwise
    /// [`GraphError::InvalidChangeSet`](crate::error::GraphError::InvalidChangeSet).
    pub fn build(mut self) -> Result<ChangeSet, crate::error::GraphError> {
        let invalid = |reason: String| crate::error::GraphError::InvalidChangeSet { reason };
        if self.stamp.committed_at < 0 {
            return Err(invalid(format!(
                "the commit time {} is before the Unix epoch",
                self.stamp.committed_at
            )));
        }
        if self.stamp.commit_seq == 0 && self.stamp.committed_at != 0 {
            return Err(invalid(
                "commit_seq 0 marks a change set that was never committed (a dry run); it has \
                 no commit time"
                    .to_string(),
            ));
        }
        for (position, mutation) in self.mutations.iter_mut().enumerate() {
            if let Some(reason) = mutation.invalid_reason() {
                return Err(invalid(format!("mutation {position}: {reason}")));
            }
            mutation.make_logical();
        }
        Ok(ChangeSet::new(self.stamp, self.metadata, self.mutations))
    }
}

#[cfg(feature = "serde")]
fn is_default_metadata(metadata: &CommitMetadata) -> bool {
    metadata.principal.is_none() && metadata.attributes.is_empty()
}

impl ChangeSet {
    /// Starts a change set committed as `commit_seq` at `committed_at` (microseconds since the
    /// Unix epoch, UTC; both `0` for one that was never committed); see [`ChangeSetBuilder`].
    pub fn builder(commit_seq: u64, committed_at: i64) -> ChangeSetBuilder {
        ChangeSetBuilder {
            stamp: CommitStamp {
                commit_seq,
                committed_at,
                node_sequence: 0,
                edge_sequence: 0,
            },
            metadata: CommitMetadata::default(),
            mutations: Vec::new(),
        }
    }

    /// Built by the graph at commit (or at the end of a dry run, with `commit_seq` and
    /// `committed_at` 0).
    pub(crate) fn new(
        stamp: CommitStamp,
        metadata: CommitMetadata,
        mutations: Vec<Mutation>,
    ) -> Self {
        Self {
            commit_seq: stamp.commit_seq,
            committed_at: stamp.committed_at,
            node_sequence: stamp.node_sequence,
            edge_sequence: stamp.edge_sequence,
            metadata,
            mutations,
        }
    }

    /// The commit sequence number: monotonic per graph, advanced only by units that changed
    /// something. `0` for a change set that was never committed (a
    /// [dry run](crate::graph::Transactional::dry_run)).
    pub fn commit_seq(&self) -> u64 {
        self.commit_seq
    }

    /// The commit time in microseconds since the Unix epoch (UTC), taken from the system clock
    /// at the outermost commit and made monotonic per graph: never earlier than the previous
    /// commit's time (a clock that steps back repeats the previous time instead). `0` for a
    /// change set that was never committed (a [dry run](crate::graph::Transactional::dry_run)),
    /// like [`Self::commit_seq`]. A point-in-time recovery target compares against it.
    pub fn committed_at(&self) -> i64 {
        self.committed_at
    }

    /// The vertex auto-id sequence right after the unit (the last auto id handed out, or the
    /// highest numeric explicit id seen). It is carried on every change set because ids handed
    /// out by rolled-back units or of dropped elements leave no other trace:
    /// [`TraversalGraph::replay`](crate::graph::TraversalGraph::replay) raises the rebuilt
    /// graph's sequence to it, so no auto id is ever handed out twice.
    pub fn node_sequence(&self) -> u64 {
        self.node_sequence
    }

    /// The edge auto-id sequence right after the unit; see [`Self::node_sequence`].
    pub fn edge_sequence(&self) -> u64 {
        self.edge_sequence
    }

    /// The commit metadata.
    pub fn metadata(&self) -> &CommitMetadata {
        &self.metadata
    }

    /// The mutations, in execution order.
    pub fn mutations(&self) -> &[Mutation] {
        &self.mutations
    }

    /// Consumes the change set and returns its mutations.
    pub fn into_mutations(self) -> Vec<Mutation> {
        self.mutations
    }

    /// The number of mutations.
    pub fn len(&self) -> usize {
        self.mutations.len()
    }

    /// `true` when there are no mutations: only a sequence-only commit (a unit that changed
    /// nothing but raised an auto-id sequence, [`GraphStorage::raise_id_sequences`]) commits
    /// such a change set; its [`Self::node_sequence`] and [`Self::edge_sequence`] carry the
    /// raise.
    ///
    /// [`GraphStorage::raise_id_sequences`]: crate::graph::GraphStorage::raise_id_sequences
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }

    /// The net effect: at most one entry per element, property key and label, with the
    /// before-value of the first change and the after-value of the last one.
    ///
    /// - Added and then dropped: nothing. Added and then changed: one add with the final state.
    /// - Changed and then dropped: one drop whose before-image is the state before the change
    ///   set (a reconstructed image keeps its labels' order where it can; a restored label goes
    ///   to the end).
    /// - Dropped and re-added with the same id: the drop and the add (it is another element).
    /// - A property set and then removed, or set back to its old value: nothing (likewise for
    ///   a label added and removed again, and an edge label changed back). A write of an equal
    ///   value: nothing.
    ///
    /// Schema changes are barriers: the mutations between two [`Mutation::SetSchema`]s are
    /// compacted separately, consecutive schema changes merge into one (dropped when it ends
    /// where it started). Inside a segment the result is ordered so that it replays: edge
    /// drops, vertex drops, vertex adds, vertex label and property changes, edge adds, edge
    /// label and property changes, each group in order of the element's first appearance.
    ///
    /// Catalog changes do not interact with data or schema: the changes of one definition
    /// (kind and name) merge into one, with the before of the first and the after of the last
    /// (dropped when they are equal), emitted at the end in order of first appearance.
    pub fn compact(&self) -> ChangeSet {
        ChangeSet {
            commit_seq: self.commit_seq,
            committed_at: self.committed_at,
            node_sequence: self.node_sequence,
            edge_sequence: self.edge_sequence,
            metadata: self.metadata.clone(),
            mutations: compact_mutations(&self.mutations),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// compact()
// ---------------------------------------------------------------------------------------------

fn compact_mutations(mutations: &[Mutation]) -> Vec<Mutation> {
    let mut out = Vec::new();
    let mut segment = Compactor::default();
    // A run of consecutive schema changes: (before of the first, after of the last).
    let mut schema: Option<(Box<GraphSchema>, Box<GraphSchema>)> = None;
    // The net change of each definition: (before of the first, after of the last).
    type DefinitionImage = Option<Box<Definition>>;
    let mut definitions: indexmap::IndexMap<
        (DefinitionKind, String),
        (DefinitionImage, DefinitionImage),
    > = indexmap::IndexMap::new();
    for mutation in mutations {
        if let Mutation::SetDefinition {
            kind,
            name,
            before,
            after,
        } = mutation
        {
            definitions
                .entry((*kind, name.clone()))
                .and_modify(|net| net.1 = after.clone())
                .or_insert_with(|| (before.clone(), after.clone()));
            continue;
        }
        if let Mutation::SetSchema { before, after } = mutation {
            if schema.is_none() {
                segment.finish(&mut out);
                segment = Compactor::default();
            }
            schema = Some(match schema.take() {
                Some((first, _)) => (first, after.clone()),
                None => (before.clone(), after.clone()),
            });
            continue;
        }
        if let Some((before, after)) = schema.take()
            && before != after
        {
            out.push(Mutation::SetSchema { before, after });
        }
        segment.apply(mutation);
    }
    if let Some((before, after)) = schema.take()
        && before != after
    {
        out.push(Mutation::SetSchema { before, after });
    }
    segment.finish(&mut out);
    for ((kind, name), (before, after)) in definitions {
        if before != after {
            out.push(Mutation::SetDefinition {
                kind,
                name,
                before,
                after,
            });
        }
    }
    out
}

/// The net state of one element within a segment.
#[derive(Debug)]
struct Net<S> {
    /// Whether the element existed before the segment (its first entry was not an add).
    existed: bool,
    /// First before-value of each key changed on the original element.
    first_props: Vec<(String, Option<ElementProperty>)>,
    /// Last after-value of each key changed on the original element.
    last_props: Vec<(String, Option<ElementProperty>)>,
    /// First presence of each label changed on the original vertex.
    first_labels: Vec<(String, bool)>,
    /// Last presence of each label changed on the original vertex.
    last_labels: Vec<(String, bool)>,
    /// The label of the original edge before its first label change (`None`: no change yet).
    first_edge_label: Option<Option<String>>,
    /// The label of the original edge after its last label change.
    last_edge_label: Option<Option<String>>,
    /// The image of the original element at its drop.
    dropped: Option<S>,
    current: Current<S>,
}

/// Where the element stands at the current position of the segment.
#[derive(Debug)]
enum Current<S> {
    /// The element of before the segment, with only the tracked changes known.
    Original,
    /// Not there.
    Gone,
    /// Added in the segment: fully known.
    New(S),
}

impl<S> Net<S> {
    fn new(existed: bool) -> Self {
        Self {
            existed,
            first_props: Vec::new(),
            last_props: Vec::new(),
            first_labels: Vec::new(),
            last_labels: Vec::new(),
            first_edge_label: None,
            last_edge_label: None,
            dropped: None,
            current: if existed {
                Current::Original
            } else {
                Current::Gone
            },
        }
    }

    /// The property changes of the original element whose end differs from its start.
    fn changed_props(
        &self,
    ) -> impl Iterator<Item = (&str, &Option<ElementProperty>, &Option<ElementProperty>)> {
        self.last_props.iter().filter_map(|(key, last)| {
            let first = lookup(&self.first_props, key)?;
            (first != last).then_some((key.as_str(), first, last))
        })
    }

    /// The label change of the original edge, when its end differs from its start.
    fn changed_edge_label(&self) -> Option<(&Option<String>, &str)> {
        match (&self.first_edge_label, &self.last_edge_label) {
            (Some(first), Some(Some(last))) if first.as_deref() != Some(last.as_str()) => {
                Some((first, last.as_str()))
            }
            _ => None,
        }
    }

    /// The label changes of the original vertex whose end differs from its start.
    fn changed_labels(&self) -> impl Iterator<Item = (&str, bool)> {
        self.last_labels.iter().filter_map(|(label, last)| {
            let first = lookup(&self.first_labels, label)?;
            (first != last).then_some((label.as_str(), *last))
        })
    }
}

fn lookup<'a, V>(entries: &'a [(String, V)], key: &str) -> Option<&'a V> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn upsert<V>(entries: &mut Vec<(String, V)>, key: &str, value: V) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some((_, slot)) => *slot = value,
        None => entries.push((key.to_string(), value)),
    }
}

fn insert_first<V>(entries: &mut Vec<(String, V)>, key: &str, value: V) {
    if !entries.iter().any(|(k, _)| k == key) {
        entries.push((key.to_string(), value));
    }
}

fn set_in(properties: &mut PropertyMap, key: &str, value: Option<ElementProperty>) {
    match value {
        Some(value) => {
            properties.insert(CompactString::from(key), value);
        }
        None => {
            properties.shift_remove(key);
        }
    }
}

/// Compacts one segment (no schema change inside).
#[derive(Debug, Default)]
struct Compactor {
    vertices: indexmap::IndexMap<String, Net<VertexState>>,
    edges: indexmap::IndexMap<String, Net<EdgeState>>,
}

impl Compactor {
    fn vertex(&mut self, id: &str, existed: bool) -> &mut Net<VertexState> {
        self.vertices
            .entry(id.to_string())
            .or_insert_with(|| Net::new(existed))
    }

    fn edge(&mut self, id: &str, existed: bool) -> &mut Net<EdgeState> {
        self.edges
            .entry(id.to_string())
            .or_insert_with(|| Net::new(existed))
    }

    fn apply(&mut self, mutation: &Mutation) {
        match mutation {
            Mutation::AddVertex {
                id,
                labels,
                properties,
            } => {
                self.vertex(id, false).current = Current::New(VertexState {
                    labels: labels.clone(),
                    properties: properties.clone(),
                });
            }
            Mutation::DropVertex { id, before } => {
                let net = self.vertex(id, true);
                if matches!(net.current, Current::Original) {
                    net.dropped = Some(before.clone());
                }
                net.current = Current::Gone;
            }
            Mutation::AddEdge {
                id,
                label,
                out_id,
                in_id,
                properties,
            } => {
                self.edge(id, false).current = Current::New(EdgeState {
                    label: label.clone(),
                    out_id: out_id.clone(),
                    in_id: in_id.clone(),
                    properties: properties.clone(),
                });
            }
            Mutation::DropEdge { id, before } => {
                let net = self.edge(id, true);
                if matches!(net.current, Current::Original) {
                    net.dropped = Some(before.clone());
                }
                net.current = Current::Gone;
            }
            Mutation::SetProperty {
                element,
                key,
                before,
                after,
            } => self.property(element, key, before.clone(), Some(after.clone())),
            Mutation::RemoveProperty {
                element,
                key,
                before,
            } => self.property(element, key, Some(before.clone()), None),
            Mutation::AddLabel { id, label } => self.label(id, label, true),
            Mutation::RemoveLabel { id, label } => self.label(id, label, false),
            Mutation::SetEdgeLabel { id, before, after } => {
                let net = self.edge(id, true);
                match &mut net.current {
                    Current::Original => {
                        if net.first_edge_label.is_none() {
                            net.first_edge_label = Some(before.clone());
                        }
                        net.last_edge_label = Some(Some(after.clone()));
                    }
                    Current::New(state) => state.label = Some(after.clone()),
                    Current::Gone => {}
                }
            }
            // Schema changes (segment barriers) and catalog changes are handled by
            // `compact_mutations`.
            Mutation::SetSchema { .. } | Mutation::SetDefinition { .. } => {}
        }
    }

    fn property(
        &mut self,
        element: &ElementRef,
        key: &str,
        before: Option<ElementProperty>,
        after: Option<ElementProperty>,
    ) {
        fn track<S>(
            net: &mut Net<S>,
            key: &str,
            before: Option<ElementProperty>,
            after: Option<ElementProperty>,
            properties: impl FnOnce(&mut S) -> &mut PropertyMap,
        ) {
            match &mut net.current {
                Current::Original => {
                    insert_first(&mut net.first_props, key, before);
                    upsert(&mut net.last_props, key, after);
                }
                Current::New(state) => set_in(properties(state), key, after),
                Current::Gone => {}
            }
        }
        match element.kind {
            ElementKind::Vertex => track(self.vertex(&element.id, true), key, before, after, |s| {
                &mut s.properties
            }),
            ElementKind::Edge => track(self.edge(&element.id, true), key, before, after, |s| {
                &mut s.properties
            }),
        }
    }

    fn label(&mut self, id: &str, label: &str, added: bool) {
        let net = self.vertex(id, true);
        match &mut net.current {
            Current::Original => {
                insert_first(&mut net.first_labels, label, !added);
                upsert(&mut net.last_labels, label, added);
            }
            Current::New(state) => {
                if added {
                    if !state.labels.iter().any(|l| l == label) {
                        state.labels.push(label.to_string());
                    }
                } else {
                    state.labels.retain(|l| l != label);
                }
            }
            Current::Gone => {}
        }
    }

    /// Emits the net mutations of the segment in replayable order.
    fn finish(self, out: &mut Vec<Mutation>) {
        // 1. Edge drops, 2. vertex drops.
        for (id, net) in &self.edges {
            if net.existed && !matches!(net.current, Current::Original) {
                out.push(Mutation::DropEdge {
                    id: id.clone(),
                    before: original_edge(net),
                });
            }
        }
        for (id, net) in &self.vertices {
            if net.existed && !matches!(net.current, Current::Original) {
                out.push(Mutation::DropVertex {
                    id: id.clone(),
                    before: original_vertex(net),
                });
            }
        }
        // 3. Vertex adds, 4. vertex label and property changes.
        for (id, net) in &self.vertices {
            if let Current::New(state) = &net.current {
                out.push(Mutation::AddVertex {
                    id: id.clone(),
                    labels: state.labels.clone(),
                    properties: state.properties.clone(),
                });
            }
        }
        for (id, net) in &self.vertices {
            if !matches!(net.current, Current::Original) {
                continue;
            }
            for (label, added) in net.changed_labels() {
                out.push(if added {
                    Mutation::AddLabel {
                        id: id.clone(),
                        label: label.to_string(),
                    }
                } else {
                    Mutation::RemoveLabel {
                        id: id.clone(),
                        label: label.to_string(),
                    }
                });
            }
            push_property_changes(out, ElementRef::vertex(id.clone()), net);
        }
        // 5. Edge adds, 6. edge label and property changes.
        for (id, net) in &self.edges {
            if let Current::New(state) = &net.current {
                out.push(Mutation::AddEdge {
                    id: id.clone(),
                    label: state.label.clone(),
                    out_id: state.out_id.clone(),
                    in_id: state.in_id.clone(),
                    properties: state.properties.clone(),
                });
            }
        }
        for (id, net) in &self.edges {
            if matches!(net.current, Current::Original) {
                if let Some((before, after)) = net.changed_edge_label() {
                    out.push(Mutation::SetEdgeLabel {
                        id: id.clone(),
                        before: before.clone(),
                        after: after.to_string(),
                    });
                }
                push_property_changes(out, ElementRef::edge(id.clone()), net);
            }
        }
    }
}

fn push_property_changes<S>(out: &mut Vec<Mutation>, element: ElementRef, net: &Net<S>) {
    for (key, first, last) in net.changed_props() {
        out.push(match last {
            Some(after) => Mutation::SetProperty {
                element: element.clone(),
                key: key.to_string(),
                before: first.clone(),
                after: after.clone(),
            },
            // `first != last`, so `first` is present.
            None => Mutation::RemoveProperty {
                element: element.clone(),
                key: key.to_string(),
                before: first.clone().unwrap_or(ElementProperty::Null),
            },
        });
    }
}

/// The original image of a dropped vertex: the image at its drop with the tracked changes
/// before the drop reverted.
fn original_vertex(net: &Net<VertexState>) -> VertexState {
    let mut state = net.dropped.clone().unwrap_or_default();
    for (key, first) in &net.first_props {
        set_in(&mut state.properties, key, first.clone());
    }
    for (label, present) in &net.first_labels {
        if *present {
            if !state.labels.iter().any(|l| l == label) {
                state.labels.push(label.clone());
            }
        } else {
            state.labels.retain(|l| l != label);
        }
    }
    state
}

/// Edge counterpart of [`original_vertex`].
fn original_edge(net: &Net<EdgeState>) -> EdgeState {
    let mut state = net.dropped.clone().unwrap_or(EdgeState {
        label: None,
        out_id: String::new(),
        in_id: String::new(),
        properties: PropertyMap::default(),
    });
    for (key, first) in &net.first_props {
        set_in(&mut state.properties, key, first.clone());
    }
    if let Some(first) = &net.first_edge_label {
        state.label = first.clone();
    }
    state
}
