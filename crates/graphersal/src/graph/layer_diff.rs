//! Delta visibility of a layered storage: where an element comes from ([`LayerOrigin`]) and what
//! a layer changed against its base ([`LayerDiff`]), the answers of
//! [`GraphStorage::vertex_layer`] / [`GraphStorage::edge_layer`] and
//! [`GraphStorage::layer_diff`].
//!
//! A storage without layers keeps the trait defaults: every element is
//! [`LayerOrigin::Base`] and the diff is empty, so a query asking for them stays portable.
//! [`LayeredGraph`](crate::graph::LayeredGraph) answers from its delta.
//!
//! [`GraphStorage::vertex_layer`]: crate::graph::GraphStorage::vertex_layer
//! [`GraphStorage::edge_layer`]: crate::graph::GraphStorage::edge_layer
//! [`GraphStorage::layer_diff`]: crate::graph::GraphStorage::layer_diff

use compact_str::CompactString;

use crate::graph::layered::BaseRef;
use crate::graph::schema::ElementKind;
use crate::value::{ElementProperty, PropertyMap};

/// Where an element of a layered storage comes from, as the layer sees it.
///
/// - [`Base`](Self::Base): the base element, unchanged by the layer (also every element of a
///   storage without layers). A base vertex that only gained or lost incident edges is `Base`:
///   the edges are reported on their own.
/// - [`Added`](Self::Added): created in the layer.
/// - [`Modified`](Self::Modified): a base element whose own labels or properties the layer
///   changed. A write of the base's value changes nothing, so an element changed back to its
///   base state is `Base` again.
///
/// Removed base elements cannot be read, so they have no origin: they appear only in
/// [`LayerDiff::removed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LayerOrigin {
    /// The base's element, unchanged by the layer.
    Base,
    /// An element the layer created.
    Added,
    /// A base element whose labels or properties the layer changed.
    Modified,
}

impl LayerOrigin {
    /// The name the query languages print: `"base"`, `"added"` or `"modified"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Added => "added",
            Self::Modified => "modified",
        }
    }

    /// The origin named `name` ([`Self::as_str`]), `None` for any other text.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "base" => Some(Self::Base),
            "added" => Some(Self::Added),
            "modified" => Some(Self::Modified),
            _ => None,
        }
    }
}

impl std::fmt::Display for LayerOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What [`GraphStorage::layer_diff`](crate::graph::GraphStorage::layer_diff) reports: the
/// element kinds, a label filter and a limit. The default reports everything.
///
/// ```
/// use graphersal::storage::{ElementKind, LayerDiffRequest};
///
/// let request = LayerDiffRequest::new().kind(ElementKind::Vertex).labels(["person"]).limit(10);
/// assert_eq!(request.kind, Some(ElementKind::Vertex));
/// assert_eq!(request.limit, Some(10));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerDiffRequest {
    /// Only this element kind; `None` (the default): vertices and edges.
    pub kind: Option<ElementKind>,
    /// Only elements carrying one of these labels; `None` (the default): every element. A
    /// vertex matches with any label of its label set; an edge with its label (an unlabeled
    /// edge matches no filter). A modified or removed element matches with its base labels or
    /// with its labels in the layer.
    pub labels: Option<Vec<CompactString>>,
    /// At most this many entries in the whole diff (added, removed and modified together);
    /// `None` (the default): no limit. [`LayerDiff::truncated`] says whether it cut entries.
    pub limit: Option<usize>,
}

impl LayerDiffRequest {
    /// Everything: both kinds, every label, no limit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only elements of `kind`.
    pub fn kind(mut self, kind: ElementKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Only elements carrying one of `labels` (see [`Self::labels`](Self#structfield.labels)).
    pub fn labels<L: Into<CompactString>>(mut self, labels: impl IntoIterator<Item = L>) -> Self {
        let mut set: Vec<CompactString> = Vec::new();
        for label in labels {
            let label = label.into();
            if !set.contains(&label) {
                set.push(label);
            }
        }
        self.labels = Some(set);
        self
    }

    /// At most `limit` entries in the whole diff.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// `true` when the request reports elements of `kind`.
    pub fn wants_kind(&self, kind: ElementKind) -> bool {
        self.kind.is_none_or(|wanted| wanted == kind)
    }

    /// `true` when an element with `labels` passes the label filter.
    pub fn matches_labels(&self, labels: &[CompactString]) -> bool {
        match &self.labels {
            None => true,
            Some(wanted) => labels.iter().any(|label| wanted.contains(label)),
        }
    }
}

/// One added or removed element of a [`LayerDiff`]: named by its user-visible id (a removed
/// element has no live handle), with its labels and properties (the layer's for an added
/// element, the base's for a removed one; values as a read returns them).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LayerDiffEntry {
    /// Vertex or edge.
    pub kind: ElementKind,
    /// The element's id.
    pub id: CompactString,
    /// A vertex's label set (primary first); an edge's label (none for an unlabeled edge).
    pub labels: Vec<CompactString>,
    /// An edge's (out vertex id, in vertex id); `None` for a vertex.
    pub endpoints: Option<(CompactString, CompactString)>,
    /// The element's properties (empty for an edge without a map).
    pub properties: PropertyMap,
}

impl LayerDiffEntry {
    /// The entry of a vertex.
    pub fn vertex(
        id: impl Into<CompactString>,
        labels: Vec<CompactString>,
        properties: PropertyMap,
    ) -> Self {
        Self {
            kind: ElementKind::Vertex,
            id: id.into(),
            labels,
            endpoints: None,
            properties,
        }
    }

    /// The entry of an edge from `out` to `into` (vertex ids).
    pub fn edge(
        id: impl Into<CompactString>,
        label: Option<CompactString>,
        out: impl Into<CompactString>,
        into: impl Into<CompactString>,
        properties: PropertyMap,
    ) -> Self {
        Self {
            kind: ElementKind::Edge,
            id: id.into(),
            labels: label.into_iter().collect(),
            endpoints: Some((out.into(), into.into())),
            properties,
        }
    }
}

/// One property of a modified element that differs between the base and the layer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LayerPropertyChange {
    /// The property name.
    pub name: CompactString,
    /// The base's value; `None` when the base element lacks the property.
    pub base: Option<ElementProperty>,
    /// The layer's value; `None` when the layer removed the property.
    pub layer: Option<ElementProperty>,
}

impl LayerPropertyChange {
    /// The change of `name` from `base` to `layer`.
    pub fn new(
        name: impl Into<CompactString>,
        base: Option<ElementProperty>,
        layer: Option<ElementProperty>,
    ) -> Self {
        Self {
            name: name.into(),
            base,
            layer,
        }
    }
}

/// One modified element of a [`LayerDiff`]: a base element whose own labels or properties the
/// layer changed ([`LayerOrigin::Modified`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LayerDiffChange {
    /// Vertex or edge.
    pub kind: ElementKind,
    /// The element's id (ids never change).
    pub id: CompactString,
    /// The base's labels (as [`LayerDiffEntry::labels`]).
    pub base_labels: Vec<CompactString>,
    /// The layer's labels: equal to `base_labels` when only properties changed.
    pub labels: Vec<CompactString>,
    /// An edge's (out vertex id, in vertex id); `None` for a vertex.
    pub endpoints: Option<(CompactString, CompactString)>,
    /// The properties that differ, in the layer's property order, then the ones the layer
    /// removed in the base's order. Empty when only the labels, or only the order of the
    /// properties, changed.
    pub properties: Vec<LayerPropertyChange>,
}

impl LayerDiffChange {
    /// The change of a vertex.
    pub fn vertex(
        id: impl Into<CompactString>,
        base_labels: Vec<CompactString>,
        labels: Vec<CompactString>,
        properties: Vec<LayerPropertyChange>,
    ) -> Self {
        Self {
            kind: ElementKind::Vertex,
            id: id.into(),
            base_labels,
            labels,
            endpoints: None,
            properties,
        }
    }

    /// The change of an edge from `out` to `into` (vertex ids).
    pub fn edge(
        id: impl Into<CompactString>,
        base_label: Option<CompactString>,
        label: Option<CompactString>,
        out: impl Into<CompactString>,
        into: impl Into<CompactString>,
        properties: Vec<LayerPropertyChange>,
    ) -> Self {
        Self {
            kind: ElementKind::Edge,
            id: id.into(),
            base_labels: base_label.into_iter().collect(),
            labels: label.into_iter().collect(),
            endpoints: Some((out.into(), into.into())),
            properties,
        }
    }
}

/// What a layer changed against its base
/// ([`GraphStorage::layer_diff`](crate::graph::GraphStorage::layer_diff)).
///
/// Ordered: vertices before edges in each list; added elements in the layer's slot order
/// (creation order, a slot freed by a drop is reused), removed and modified ones in base slot
/// order. A request's limit counts the entries of all three lists, filled vertices first
/// (added, removed, modified), then edges in the same order. Empty for a storage without
/// layers ([`LayerDiff::default`]).
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct LayerDiff {
    /// Elements the layer created, with the layer's labels and properties.
    pub added: Vec<LayerDiffEntry>,
    /// Base elements the layer removed (also the base edges a removed vertex took along), with
    /// the base's labels and properties.
    pub removed: Vec<LayerDiffEntry>,
    /// Base elements whose labels or properties the layer changed.
    pub modified: Vec<LayerDiffChange>,
    /// `true` when the request's limit left out at least one entry.
    pub truncated: bool,
}

impl LayerDiff {
    /// The number of entries in the three lists.
    pub fn len(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }

    /// `true` when the diff holds no entry.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The diff as a value: the one form the query languages answer (`g.layer_diff()`) and
    /// every front end shows, rendered like any other map (`value/physical.rs`).
    ///
    /// ```text
    /// #{added: [entry..], removed: [entry..], modified: [change..], truncated: bool}
    /// entry  = #{kind, id, labels: [..], properties: #{..}, out, in}
    /// change = #{kind, id, base_labels: [..], labels: [..], out, in,
    ///            properties: #{name: #{base: value, layer: value}}}
    /// ```
    ///
    /// `kind` is `"vertex"` or `"edge"`; `out`/`in` (the endpoint vertex ids) only for an edge;
    /// an edge's `labels` is a list of its one label (empty for an unlabeled edge), as a
    /// vertex's. In a change, a property the base lacks has no `base` key and a property the
    /// layer removed no `layer` key: a stored value may itself be `null`, so `null` cannot
    /// stand for "absent".
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::storage::LayerDiff;
    ///
    /// let value = LayerDiff::default().to_property();
    /// let ElementProperty::Object(map) = value else { unreachable!() };
    /// let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
    /// assert_eq!(keys, ["added", "removed", "modified", "truncated"]);
    /// ```
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert(
            "added",
            ElementProperty::Array(self.added.iter().map(LayerDiffEntry::to_property).collect()),
        );
        map.insert(
            "removed",
            ElementProperty::Array(
                self.removed
                    .iter()
                    .map(LayerDiffEntry::to_property)
                    .collect(),
            ),
        );
        map.insert(
            "modified",
            ElementProperty::Array(
                self.modified
                    .iter()
                    .map(LayerDiffChange::to_property)
                    .collect(),
            ),
        );
        map.insert("truncated", ElementProperty::from(self.truncated));
        ElementProperty::Object(Box::new(map))
    }

    /// The diff as compact JSON text: [`Self::to_property`] written by the physical codec
    /// (keys in the order listed there).
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}

/// A list of labels as a value.
fn label_list(labels: &[CompactString]) -> ElementProperty {
    ElementProperty::Array(
        labels
            .iter()
            .map(|label| ElementProperty::from(label.as_str()))
            .collect(),
    )
}

/// `out`/`in` of an edge's entry or change.
fn insert_endpoints(map: &mut PropertyMap, endpoints: &Option<(CompactString, CompactString)>) {
    if let Some((out, into)) = endpoints {
        map.insert("out", ElementProperty::from(out.as_str()));
        map.insert("in", ElementProperty::from(into.as_str()));
    }
}

impl LayerDiffEntry {
    /// The entry as a value (see [`LayerDiff::to_property`]).
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("kind", ElementProperty::from(self.kind.as_str()));
        map.insert("id", ElementProperty::from(self.id.as_str()));
        map.insert("labels", label_list(&self.labels));
        map.insert(
            "properties",
            ElementProperty::Object(Box::new(self.properties.clone())),
        );
        insert_endpoints(&mut map, &self.endpoints);
        ElementProperty::Object(Box::new(map))
    }
}

impl LayerDiffChange {
    /// The change as a value (see [`LayerDiff::to_property`]).
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("kind", ElementProperty::from(self.kind.as_str()));
        map.insert("id", ElementProperty::from(self.id.as_str()));
        map.insert("base_labels", label_list(&self.base_labels));
        map.insert("labels", label_list(&self.labels));
        insert_endpoints(&mut map, &self.endpoints);
        let mut properties = PropertyMap::default();
        for change in &self.properties {
            let mut sides = PropertyMap::default();
            if let Some(base) = &change.base {
                sides.insert("base", base.clone());
            }
            if let Some(layer) = &change.layer {
                sides.insert("layer", layer.clone());
            }
            properties.insert(
                change.name.clone(),
                ElementProperty::Object(Box::new(sides)),
            );
        }
        map.insert("properties", ElementProperty::Object(Box::new(properties)));
        ElementProperty::Object(Box::new(map))
    }
}

/// Vertex and edge counts of one kind of layer change ([`LayerSummary`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct LayerCounts {
    /// Vertices.
    pub vertices: u64,
    /// Edges.
    pub edges: u64,
}

impl LayerCounts {
    /// `vertices` vertices and `edges` edges.
    pub fn new(vertices: u64, edges: u64) -> Self {
        Self { vertices, edges }
    }

    /// The counts as a value: `#{vertices, edges}`.
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("vertices", count_value(self.vertices));
        map.insert("edges", count_value(self.edges));
        ElementProperty::Object(Box::new(map))
    }
}

/// A count as a value (an integer; a count beyond `i64::MAX` cannot occur in memory).
fn count_value(count: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(count).unwrap_or(i64::MAX))
}

/// What a layered storage is at a glance ([`GraphStorage::layer_summary`]): the base's display
/// name and identity, how many elements the layer added, modified and removed, and its own
/// memory beside the shared base's. Cheap: the counts come from the delta's own bookkeeping
/// (never a diff walk) and the bytes from the cached footprints. `None` for a storage without
/// layers.
///
/// The counts follow [`LayerDiff`]: `added` + `modified` + `removed` of a kind equal the
/// lengths of an unlimited diff's lists of that kind (a modified element whose image differs
/// from the base only in property order counts, as it is listed there).
///
/// [`GraphStorage::layer_summary`]: crate::graph::GraphStorage::layer_summary
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerSummary {
    /// The base's display name ([`LayerBase::name`](crate::graph::LayerBase::name); `"base"`
    /// for a layer made without a named base).
    pub base: CompactString,
    /// The base's identity, `None` when the storage cannot tell it.
    pub base_ref: Option<BaseRef>,
    /// Elements the layer created.
    pub added: LayerCounts,
    /// Base elements whose labels or properties the layer changed.
    pub modified: LayerCounts,
    /// Base elements the layer removed.
    pub removed: LayerCounts,
    /// The layer's own memory ([`MemoryStats::total_bytes`] of the storage:
    /// `GraphStorage::memory_footprint_bytes`).
    ///
    /// [`MemoryStats::total_bytes`]: crate::graph::MemoryStats::total_bytes
    pub own_bytes: u64,
    /// The memory of the shared base, outside `own_bytes`
    /// ([`MemoryStats::shared_bytes`](crate::graph::MemoryStats::shared_bytes)).
    pub shared_bytes: u64,
}

impl LayerSummary {
    /// A summary of a layer over the base named `base`, with no change and no memory counted
    /// yet (the `with_*` builders fill them).
    pub fn new(base: impl Into<CompactString>, base_ref: Option<BaseRef>) -> Self {
        Self {
            base: base.into(),
            base_ref,
            added: LayerCounts::default(),
            modified: LayerCounts::default(),
            removed: LayerCounts::default(),
            own_bytes: 0,
            shared_bytes: 0,
        }
    }

    /// The summary with these change counts.
    pub fn with_counts(
        mut self,
        added: LayerCounts,
        modified: LayerCounts,
        removed: LayerCounts,
    ) -> Self {
        self.added = added;
        self.modified = modified;
        self.removed = removed;
        self
    }

    /// The summary with these memory figures.
    pub fn with_bytes(mut self, own_bytes: u64, shared_bytes: u64) -> Self {
        self.own_bytes = own_bytes;
        self.shared_bytes = shared_bytes;
        self
    }

    /// The summary as a value, the form every front end shows:
    ///
    /// ```text
    /// #{base, base_ref: #{graph_id, commit_seq, committed_at} | (), added: #{vertices, edges},
    ///   modified: #{..}, removed: #{..}, own_bytes, shared_bytes}
    /// ```
    ///
    /// `graph_id` is the canonical UUID text, `committed_at` microseconds since the epoch.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::storage::{LayerCounts, LayerSummary};
    ///
    /// let summary = LayerSummary::new("modern", None)
    ///     .with_counts(LayerCounts::new(1, 0), LayerCounts::default(), LayerCounts::new(0, 2));
    /// let ElementProperty::Object(map) = summary.to_property() else { unreachable!() };
    /// let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
    /// assert_eq!(
    ///     keys,
    ///     ["base", "base_ref", "added", "modified", "removed", "own_bytes", "shared_bytes"]
    /// );
    /// ```
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("base", ElementProperty::from(self.base.as_str()));
        let base_ref = match &self.base_ref {
            Some(base_ref) => {
                let mut identity = PropertyMap::default();
                identity.insert(
                    "graph_id",
                    ElementProperty::from(
                        crate::value::uuid::format_canonical(base_ref.graph_id()).as_str(),
                    ),
                );
                identity.insert("commit_seq", count_value(base_ref.commit_seq()));
                identity.insert(
                    "committed_at",
                    ElementProperty::from(base_ref.committed_at()),
                );
                ElementProperty::Object(Box::new(identity))
            }
            None => ElementProperty::Null,
        };
        map.insert("base_ref", base_ref);
        map.insert("added", self.added.to_property());
        map.insert("modified", self.modified.to_property());
        map.insert("removed", self.removed.to_property());
        map.insert("own_bytes", count_value(self.own_bytes));
        map.insert("shared_bytes", count_value(self.shared_bytes));
        ElementProperty::Object(Box::new(map))
    }

    /// The summary as compact JSON text: [`Self::to_property`] written by the physical codec.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_names_round_trip() {
        for origin in [LayerOrigin::Base, LayerOrigin::Added, LayerOrigin::Modified] {
            assert_eq!(LayerOrigin::from_name(origin.as_str()), Some(origin));
            assert_eq!(origin.to_string(), origin.as_str());
        }
        assert_eq!(LayerOrigin::from_name("removed"), None);
    }

    #[test]
    fn request_filters() {
        let all = LayerDiffRequest::new();
        assert!(all.wants_kind(ElementKind::Vertex) && all.wants_kind(ElementKind::Edge));
        assert!(all.matches_labels(&[]));
        let some = LayerDiffRequest::new()
            .kind(ElementKind::Edge)
            .labels(["a", "b", "a"]);
        assert_eq!(some.labels.as_deref().map(<[_]>::len), Some(2));
        assert!(!some.wants_kind(ElementKind::Vertex));
        assert!(some.matches_labels(&["x".into(), "b".into()]));
        assert!(!some.matches_labels(&["x".into()]));
        assert!(!some.matches_labels(&[]));
    }
}
