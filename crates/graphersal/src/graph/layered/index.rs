//! Property-index overlays of a [`LayeredGraph`] (S4.1, design `ai/roadmap/layered-graph.md`
//! §8 stage 4, survey `S3-S5-survey.md` §4.1): what the layer keeps so that each property
//! index of its base can answer for the layer's data exactly, without copying the base index.
//!
//! For every index the layer serves (today: the indexes installed in the base, inherited with
//! the catalog copy) there is one overlay of three parts over ONE sorted `u32` slot space, the
//! slots of the layer's handles ([`LayeredVertex::slot`], [`LayeredEdge::slot`]):
//!
//! - **base** ([`BasePart::Shared`]): the base's own [`IndexState`], read in place through the
//!   shared `Arc`, never copied. Its slots are base slots (bit 31 clear), which are the layer's
//!   handles of base elements unchanged.
//! - **masked**: the BASE entries of the base slots the layer EXCLUDES: a base element the
//!   layer removed (its tombstone) or holds an image of. A lookup skips such a slot in the base
//!   part by the read path's own bit tests (dirty bit, then the tombstone or image bit; never a
//!   hash lookup); the masked part only keeps the counts O(log n):
//!   `count = base - masked + delta`, each from postings lengths.
//! - **delta**: the entries of the elements whose current state the delta holds, by their layer
//!   slot: an image under its base slot, an added element under bit 31 + its delta slot.
//!
//! So the visible entries are exactly `(base minus the excluded slots) plus delta`, and the
//! masked part equals the base part restricted to the excluded slots (checked by
//! [`LayeredGraph::check_layer_indexes`]). A base slot that is dirty for another reason (a stub
//! with added edges, lost base edges) is not excluded: its values are the base's.
//!
//! **Maintenance** happens in the `Delta` choke points (`delta.rs`), the functions every write
//! AND every undo goes through, so a rollback restores the overlays exactly: an added element's
//! entries go in with its insert and out with its removal; a property write takes the key's
//! entries of the element out before and puts them back after (only the indexes on that key);
//! a label change takes out the entries of the labels it lost and puts in those of the labels
//! it gained (label-less indexes untouched); a copy-up puts the base element's entries into
//! `masked` and the image's into `delta`; dropping an image (an undo of the copy-up, a return
//! to clean) takes both out again; removing a base element takes its image's entries out of
//! `delta` or, without an image, puts its base entries into `masked`. Each costs O(indexes on
//! the touched labels and key x log n); with no index at all one branch.
//!
//! The parts of all overlays are kept as two [`PropertyIndexes`] (masked, delta) with the base's
//! positions and label lookups ([`PropertyIndexes::empty_like`]), so the maintenance is the
//! base's own (`PropertyIndexes::slot_entries`, `IndexState::account`).
//!
//! **Reads** (S4.2): a lookup finds its overlay by the parts' lookups and merges, key by key
//! and slot by slot, the base part's slice minus the excluded slots with the delta part's
//! ([`IndexOverlay::slots`]), so it returns exactly what a `TraversalGraph` with the layer's
//! data returns, in its order (an image at its base slot, an added element after every base
//! slot: plan decision P4 covers a `TraversalGraph` that reused a freed slot); an `each`
//! slice of several keys is collected, sorted and deduplicated as there. An overlay holding
//! nothing of its own (both layer-owned parts empty) is the base's slice, one branch. Counts
//! are `base - masked + delta` from postings lengths, the stats combine the parts
//! ([`IndexOverlay::stats`]).
//!
//! **Unique checks** (S4.3): a write the base's unique indexes cover is checked at the point
//! `TraversalGraph` checks it (the write rules call `WriteView::check_unique_*`), with
//! `PropertyIndexes::unique_conflict`'s order of indexes run over the layer-owned parts'
//! lookups, and each key looked up in its overlay ([`IndexOverlay::other_holder`]): a holder
//! in `delta` other than the written element, or a base holder that is neither the written
//! element nor excluded. So a base element the layer dropped, or changed (its image holds its
//! current values), frees its base values, and the error names the index, the value and the
//! other element's id exactly as `TraversalGraph` does. A unique index stored in the base over
//! repeated values is not installed there (neither served nor enforced), so it is not an
//! overlay either: inherited as disabled, with the base's report (`unique_index_duplicates`).
//!
//! **Indexes the layer defines** (S4.4): the layer's catalog takes property index definitions
//! as `TraversalGraph`'s does (validated, the other index kind's definition of the name
//! replaced, `check_conflict` against the layer's catalog), and every catalog change of an
//! index, also its undo, rebuilds that overlay from the catalog and the data
//! ([`LayeredGraph::layer_index_after_change`], `TraversalGraph::property_index_after_change`'s
//! counterpart): an index whose definition is the base's, installed there, keeps the shared
//! base part ([`BasePart::Shared`]); any other gets a base part of its own
//! ([`BasePart::Owned`], built by `TraversalGraph::build_index` over EVERY base element, as the
//! base would build it, so the masked and delta parts are maintained exactly as for a shared
//! part: one full scan of the base's elements of the label, its bytes in the layer's own
//! footprint). A unique one is first build-checked over the merged view (the layer's elements):
//! repeated values keep it disabled, with the layer's own report, as `TraversalGraph` does. A
//! dropped index (also an inherited one) loses its overlay: lookups, plans and unique checks
//! stop using it, the base keeps its own. Positions follow `PropertyIndexes::install` (replaced
//! in place, appended, removed), the same change applied to the base parts, so `bases`,
//! `masked` and `delta` stay aligned.

use super::LayeredGraph;
use super::handle::{DELTA_BIT, LayeredEdge, LayeredVertex};
use crate::error::GraphError;
use crate::graph::definition::DefinitionKind;
use crate::graph::postings::Iter;
use crate::graph::property_index::{
    IndexKey, IndexState, Installed, PropertyIndexDefinition, PropertyIndexInfo,
    PropertyIndexRange, PropertyIndexStats, PropertyIndexTransform, PropertyIndexes,
    UniqueConflict, UniqueIndexDuplicates, duplicates_report_by,
};
use crate::graph::schema::ElementKind;
use crate::graph::topology::{EdgeIndex, NodeIndex};
use crate::graph::{GraphStorage, TraversalGraph};
use crate::value::{ElementProperty, PropertyMap};
use compact_str::CompactString;

/// Where the base part of an overlay comes from.
#[derive(Debug, Clone)]
pub(crate) enum BasePart {
    /// The base's installed index at this position (`TraversalGraph::property_indexes`): the
    /// layer serves an index inherited from the base's catalog, read in place.
    Shared(usize),
    /// An index the layer defines (or redefines) itself: its entries over every base element,
    /// built by `TraversalGraph::build_index` over the base, owned (and counted) by the layer.
    Owned(Box<IndexState>),
}

/// The parts of one overlay built by [`LayeredGraph::build_overlay`]: base, masked, delta.
type BuiltOverlay = (BasePart, IndexState, IndexState);

/// The index overlays of a layer (module docs): one per index it serves, at the same position
/// in `bases`, `masked` and `delta`.
#[derive(Debug, Default)]
pub(crate) struct LayerIndexes {
    /// The base part of each overlay.
    bases: Vec<BasePart>,
    /// The base entries of the excluded base slots, one state per overlay.
    masked: PropertyIndexes,
    /// The entries of the added elements and the images, by layer slot, one state per overlay.
    delta: PropertyIndexes,
    /// The unique indexes of the layer's catalog kept disabled (their data held repeated
    /// values: no overlay, not served, not enforced), with their reports, by name: the base's
    /// at creation, then the layer's own builds.
    disabled: Vec<(CompactString, UniqueIndexDuplicates)>,
}

/// One overlay as a reader sees it: its three parts (module docs).
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexOverlay<'a> {
    /// The base part (read in place, shared or owned).
    pub(crate) base: &'a IndexState,
    /// Whether the base part is the layer's own ([`BasePart::Owned`]).
    pub(crate) owned: bool,
    /// The base entries of the excluded base slots.
    pub(crate) masked: &'a IndexState,
    /// The entries of the added elements and the images, by layer slot.
    pub(crate) delta: &'a IndexState,
}

impl LayerIndexes {
    /// The overlays of a new layer over `base`: one per index the base has installed (a
    /// disabled unique index is not installed, so it stays unserved as in the base), every
    /// layer-owned part empty.
    pub(crate) fn inherit(base: &TraversalGraph) -> Self {
        let installed = &base.property_indexes;
        Self {
            bases: (0..installed.len()).map(BasePart::Shared).collect(),
            masked: installed.empty_like(),
            delta: installed.empty_like(),
            disabled: installed.disabled().to_vec(),
        }
    }

    /// The report of the unique index `name` kept disabled by the layer; `None` when it is
    /// not disabled.
    pub(crate) fn disabled_report(&self, name: &str) -> Option<UniqueIndexDuplicates> {
        self.disabled
            .iter()
            .find(|(disabled, _)| disabled == name)
            .map(|(_, report)| report.clone())
    }

    /// The base part of the overlay at `position`.
    fn base_part(&self, position: usize) -> Option<&BasePart> {
        self.bases.get(position)
    }

    /// Installs the overlay of the index `name` (`None`: removes it) with
    /// `PropertyIndexes::install`'s position rule, applied alike to the three parts; `disabled`
    /// is the index's report when it is kept disabled (then `built` is `None`).
    fn install(
        &mut self,
        name: &str,
        built: Option<BuiltOverlay>,
        disabled: Option<UniqueIndexDuplicates>,
    ) {
        self.disabled.retain(|(held, _)| held != name);
        if let Some(report) = disabled {
            self.disabled.push((CompactString::from(name), report));
        }
        let (base, masked, delta) = match built {
            Some((base, masked, delta)) => (Some(base), Some(masked), Some(delta)),
            None => (None, None, None),
        };
        self.masked.install(name, masked);
        match (self.delta.install(name, delta), base) {
            (Installed::Replaced(position), Some(base)) => {
                if let Some(slot) = self.bases.get_mut(position) {
                    *slot = base;
                }
            }
            (Installed::Removed(position), _) => {
                if position < self.bases.len() {
                    self.bases.remove(position);
                }
            }
            (Installed::Appended, Some(base)) => self.bases.push(base),
            _ => {}
        }
    }

    /// Whether the layer serves no index (every maintenance call is then one branch).
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.bases.is_empty()
    }

    /// Whether an overlay of an enforced unique index exists (every write's unique check is
    /// then more than one branch).
    #[inline]
    pub(crate) fn has_unique(&self) -> bool {
        self.delta.has_unique()
    }

    /// The number of overlays.
    pub(crate) fn len(&self) -> usize {
        self.bases.len()
    }

    /// The overlay at `position`, its base part read from `base` (the layer's base).
    pub(crate) fn overlay<'a>(
        &'a self,
        base: &'a TraversalGraph,
        position: usize,
    ) -> Option<IndexOverlay<'a>> {
        let (base_part, owned) = match self.bases.get(position)? {
            BasePart::Shared(at) => (base.property_indexes.state(*at)?, false),
            BasePart::Owned(state) => (&**state, true),
        };
        Some(IndexOverlay {
            base: base_part,
            owned,
            masked: self.masked.state(position)?,
            delta: self.delta.state(position)?,
        })
    }

    /// Puts in (`insert`) or takes out the delta entries of the element at layer `slot` with
    /// `labels` and `properties` (its current state); `only_key` limits it to one top-level
    /// key, `label_less` false leaves the label-less indexes out (a label change).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn delta_entries<'l>(
        &mut self,
        element: ElementKind,
        insert: bool,
        slot: u32,
        labels: impl Iterator<Item = &'l str>,
        properties: Option<&PropertyMap>,
        only_key: Option<&str>,
        label_less: bool,
    ) {
        if self.is_empty() {
            return;
        }
        self.delta.slot_entries(
            element, insert, slot, labels, properties, only_key, label_less,
        );
    }

    /// The masked entries of the base vertex at `node` (its base state), in or out.
    pub(crate) fn masked_vertex(
        &mut self,
        base: &TraversalGraph,
        node: crate::graph::VertexHandle,
        insert: bool,
    ) {
        if self.is_empty() {
            return;
        }
        if let Some(vertex) = base.vertex_weight(node) {
            self.masked.slot_entries(
                ElementKind::Vertex,
                insert,
                node.index().slot(),
                base.labels.set(vertex.1).iter().map(|label| label.as_str()),
                Some(&vertex.2),
                None,
                true,
            );
        }
    }

    /// The masked entries of the base edge `edge` (its base state), in or out.
    pub(crate) fn masked_edge(
        &mut self,
        base: &TraversalGraph,
        edge: crate::graph::EdgeHandle,
        insert: bool,
    ) {
        if self.is_empty() {
            return;
        }
        if let Some(weight) = base.edge_weight(edge) {
            self.masked.slot_entries(
                ElementKind::Edge,
                insert,
                edge.index().slot(),
                base.labels.name_str(weight.1).into_iter(),
                weight.2.as_ref(),
                None,
                true,
            );
        }
    }

    /// The heap bytes of the layer-owned parts (masked, delta and the base parts of the
    /// indexes the layer defines): the overlays' share of the layer's own footprint. The
    /// shared base parts belong to the base's.
    pub(crate) fn bytes(&self) -> u64 {
        let owned: u64 = self
            .bases
            .iter()
            .map(|base| match base {
                BasePart::Shared(_) => 0,
                BasePart::Owned(state) => state.stats().bytes,
            })
            .sum();
        self.masked.bytes() + self.delta.bytes() + owned
    }

    /// The position of the overlay of the index on `(element, label, key, transform)`
    /// (`label` `None`: the label-less one), by the lookups the layer-owned parts keep.
    #[inline]
    pub(crate) fn find(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
    ) -> Option<usize> {
        self.delta.find_position(element, label, key, transform)
    }
}

impl<'a> IndexOverlay<'a> {
    /// Whether the layer holds nothing of its own for this index: no base slot of it is
    /// excluded (an excluded slot's base entries are in `masked`) and nothing is added to it.
    /// The base part then answers alone, exactly as the base does.
    #[inline]
    pub(crate) fn is_clean(&self) -> bool {
        self.masked.is_empty() && self.delta.is_empty()
    }

    /// The exact number of visible elements inside `range`: `base - masked + delta`, each
    /// from postings lengths; `None` when a part cannot count its slice exactly (an `each`
    /// slice of more than one key in that part, [`IndexState::count`]). With at most one key
    /// per part no element is counted twice: an element's entries are all in the base part
    /// (not excluded) or all in the delta part.
    pub(crate) fn count(&self, range: &PropertyIndexRange) -> Option<u64> {
        if self.is_clean() {
            return self.base.count(range);
        }
        self.base
            .count(range)?
            .checked_sub(self.masked.count(range)?)?
            .checked_add(self.delta.count(range)?)
    }

    /// The stats of the visible index: `entries` and `skipped` exactly `base - masked +
    /// delta`; `distinct_values` the base's plus the delta's keys, an upper bound (a key whose
    /// base holders are all excluded, or one in both parts, counts as it is: exact would walk
    /// the layer's keys on every run, and the optimizer reads only estimates); `bytes` the
    /// layer-owned parts only (masked and delta, plus an owned base part), since a shared base
    /// part is the base's (`MemoryStats::shared_bytes`).
    pub(crate) fn stats(&self) -> PropertyIndexStats {
        let (base, masked, delta) = (self.base.stats(), self.masked.stats(), self.delta.stats());
        let combine = |b: u64, m: u64, d: u64| b.saturating_sub(m).saturating_add(d);
        let owned = if self.owned { base.bytes } else { 0 };
        PropertyIndexStats::new(
            combine(base.entries, masked.entries, delta.entries),
            base.distinct_values.saturating_add(delta.distinct_values),
            masked
                .bytes
                .saturating_add(delta.bytes)
                .saturating_add(owned),
        )
        .with_skipped(combine(base.skipped, masked.skipped, delta.skipped))
    }

    /// The first visible element other than `me` (a layer slot) holding a key equal to `key`
    /// (`IndexState::holder_entries`' equality), in key-then-slot order over the merged view,
    /// the order `TraversalGraph`'s unique check searches a graph with the layer's data
    /// (plan decision P4 aside): a base slot `excluded` names (removed, or imaged: its current
    /// values are in `delta`) holds nothing, so a base element that dropped or changed the
    /// value frees it. An enforced unique index has at most one other holder of a key except
    /// for numbers (a `float64` equals each `int64` of its value), so the merge is a minimum of
    /// the two parts' first holders.
    pub(crate) fn other_holder(
        &self,
        key: &IndexKey,
        me: Option<u32>,
        excluded: impl Fn(u32) -> bool,
    ) -> Option<u32> {
        let in_base = self
            .base
            .holder_entries(key)
            .find(|(_, slot)| Some(*slot) != me && !excluded(*slot));
        let in_delta = self
            .delta
            .holder_entries(key)
            .find(|(_, slot)| Some(*slot) != me);
        match (in_base, in_delta) {
            (Some(base), Some(delta)) => Some(base.min(delta).1),
            (base, delta) => base.or(delta).map(|(_, slot)| slot),
        }
    }

    /// The visible slots inside `range` (layer slots: a base or image slot with bit 31 clear,
    /// an added element's with it set), in `TraversalGraph`'s order for the same data: by key
    /// then slot, where the base part (minus the slots `excluded` names) and the delta part are
    /// merged key by key and slot by slot ([`Merged`]); an `each` slice whose parts hold two
    /// keys or more is collected, sorted by slot and deduplicated, as `IndexState::slice` does
    /// for a multi-key slice (with at most one key in all, key-then-slot order is slot order,
    /// so either way the order is the one a `TraversalGraph` gives). A clean overlay is the
    /// base part's slice.
    pub(crate) fn slots<X>(&self, range: &PropertyIndexRange, excluded: X) -> SlotsOf<'a>
    where
        X: Fn(u32) -> bool + 'a,
    {
        if self.is_clean() {
            return self.base.slice(range);
        }
        let (Some(base), Some(delta)) = (
            self.base.range_postings(range),
            self.delta.range_postings(range),
        ) else {
            // The range holds nothing (it is the same for every part).
            return Box::new(std::iter::empty());
        };
        if self.base.has_each() {
            let keys = |state: &'a IndexState| {
                state
                    .range_postings(range)
                    .map_or(0, |keys| keys.take(2).count())
            };
            if keys(self.base) + keys(self.delta) > 1 {
                let mut slots: Vec<u32> = Merged::new(base, delta, excluded).collect();
                slots.sort_unstable();
                slots.dedup();
                return Box::new(slots.into_iter());
            }
        }
        Box::new(Merged::new(base, delta, excluded))
    }
}

/// The slots of one lookup: one boxed iterator per call, its type decided once (no enum or
/// `Option` chain in the loop).
pub(crate) type SlotsOf<'a> = Box<dyn Iterator<Item = u32> + 'a>;

/// The key-then-slot merge of an overlay's base part, minus the excluded base slots, and its
/// delta part ([`IndexOverlay::slots`]). Within a key the two sorted postings are merged by
/// slot: an image sits at its base slot among the base's, an added element (bit 31) after
/// every base slot, which is where a `TraversalGraph` with the same data and no reused slot
/// holds it (plan decision P4). An element is never in both parts, so nothing repeats.
struct Merged<'a, B, D, X>
where
    B: Iterator<Item = (&'a IndexKey, Iter<'a>)>,
    D: Iterator<Item = (&'a IndexKey, Iter<'a>)>,
{
    base: std::iter::Peekable<B>,
    delta: std::iter::Peekable<D>,
    /// The postings of the current key in each part (empty when the part lacks the key).
    base_slots: Iter<'a>,
    delta_slots: Iter<'a>,
    /// The next slot of each current postings, read ahead for the merge.
    next_base: Option<u32>,
    next_delta: Option<u32>,
    excluded: X,
}

impl<'a, B, D, X> Merged<'a, B, D, X>
where
    B: Iterator<Item = (&'a IndexKey, Iter<'a>)>,
    D: Iterator<Item = (&'a IndexKey, Iter<'a>)>,
    X: Fn(u32) -> bool,
{
    fn new(base: B, delta: D, excluded: X) -> Self {
        Self {
            base: base.peekable(),
            delta: delta.peekable(),
            base_slots: Iter::One(None),
            delta_slots: Iter::One(None),
            next_base: None,
            next_delta: None,
            excluded,
        }
    }

    /// Moves to the next key of either part (the smaller; both when equal); `false` when
    /// neither part has one.
    fn next_key(&mut self) -> bool {
        let order = match (self.base.peek(), self.delta.peek()) {
            (None, None) => return false,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some((base, _)), Some((delta, _))) => base.cmp(delta),
        };
        if order.is_le()
            && let Some((_, slots)) = self.base.next()
        {
            self.base_slots = slots;
        }
        if order.is_ge()
            && let Some((_, slots)) = self.delta.next()
        {
            self.delta_slots = slots;
        }
        true
    }
}

impl<'a, B, D, X> Iterator for Merged<'a, B, D, X>
where
    B: Iterator<Item = (&'a IndexKey, Iter<'a>)>,
    D: Iterator<Item = (&'a IndexKey, Iter<'a>)>,
    X: Fn(u32) -> bool,
{
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        loop {
            if self.next_base.is_none() {
                let excluded = &self.excluded;
                self.next_base = self.base_slots.find(|slot| !excluded(*slot));
            }
            if self.next_delta.is_none() {
                self.next_delta = self.delta_slots.next();
            }
            match (self.next_base, self.next_delta) {
                (Some(base), Some(delta)) if base < delta => return self.next_base.take(),
                (_, Some(_)) => return self.next_delta.take(),
                (Some(_), None) => return self.next_base.take(),
                (None, None) => {
                    if !self.next_key() {
                        return None;
                    }
                }
            }
        }
    }
}

/// `true` iff the base slot `slot` of kind `element` is excluded from the base part of every
/// overlay: dirty, and removed or imaged (the read path's bit tests, in its order).
fn excluded(layer: &LayeredGraph, element: ElementKind, slot: u32) -> bool {
    match element {
        ElementKind::Vertex => excluded_vertex(layer, slot),
        ElementKind::Edge => excluded_edge(layer, slot),
    }
}

/// [`excluded`] for a base vertex slot.
#[inline]
fn excluded_vertex(layer: &LayeredGraph, slot: u32) -> bool {
    layer.dirty_vertices.contains(slot)
        && (layer.delta.is_removed_vertex(slot) || layer.delta.imaged_vertices().contains(slot))
}

/// [`excluded`] for a base edge slot.
#[inline]
fn excluded_edge(layer: &LayeredGraph, slot: u32) -> bool {
    layer.dirty_edges.contains(slot)
        && (layer.delta.is_removed_edge(slot) || layer.delta.is_imaged_edge(slot))
}

/// The lookups, counts and stats of a layer's property indexes, served from the overlays
/// (S4.2): what `impl GraphStorage for LayeredGraph` answers.
impl LayeredGraph {
    /// The overlay of the index on `(element, label, key, transform)`; `None` without one.
    #[inline]
    fn overlay_for(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
    ) -> Option<IndexOverlay<'_>> {
        let overlays = self.delta.indexes();
        overlays.overlay(&self.base, overlays.find(element, label, key, transform)?)
    }

    /// Every index the layer serves, with the stats of its overlay
    /// ([`IndexOverlay::stats`]), in the base's catalog order.
    pub(crate) fn layer_index_infos(&self) -> Vec<PropertyIndexInfo> {
        let overlays = self.delta.indexes();
        (0..overlays.len())
            .filter_map(|position| overlays.overlay(&self.base, position))
            .map(|overlay| overlay.base.info(overlay.stats()))
            .collect()
    }

    /// The vertices of the index on `(vertex, label, key, transform)` inside `range`
    /// ([`IndexOverlay::slots`]): exactly the slice a `TraversalGraph` with the layer's data
    /// returns, in its order (plan decision P4 aside); `None` without such an index.
    pub(crate) fn layer_index_vertices<'a>(
        &'a self,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = LayeredVertex> + 'a>> {
        let overlay = self.overlay_for(ElementKind::Vertex, label, key, transform)?;
        let slots = overlay.slots(range, move |slot| excluded_vertex(self, slot));
        Some(Box::new(
            slots.filter_map(move |slot| self.slot_vertex(slot)),
        ))
    }

    /// The live vertex at layer `slot` (an added vertex's with bit 31 set, else a base slot);
    /// the generation is the slot's: exact maintenance names only live elements' slots.
    #[inline]
    fn slot_vertex(&self, slot: u32) -> Option<LayeredVertex> {
        if slot & DELTA_BIT != 0 {
            return self.delta.added_vertex_at(slot & !DELTA_BIT);
        }
        let index = NodeIndex::from_slot(slot);
        let vertex = self.base.graph.node_weight(index)?;
        Some(LayeredVertex::from_base(crate::graph::VertexHandle::new(
            index, vertex.3,
        )))
    }

    /// The edge counterpart of [`Self::slot_vertex`].
    #[inline]
    fn slot_edge(&self, slot: u32) -> Option<LayeredEdge> {
        if slot & DELTA_BIT != 0 {
            return self.delta.added_edge_at(slot & !DELTA_BIT);
        }
        let index = EdgeIndex::from_slot(slot);
        let edge = self.base.graph.edge_weight(index)?;
        Some(LayeredEdge::from_base(crate::graph::EdgeHandle::new(
            index, edge.3.own,
        )))
    }

    /// The edge counterpart of [`Self::layer_index_vertices`].
    pub(crate) fn layer_index_edges<'a>(
        &'a self,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<Box<dyn Iterator<Item = LayeredEdge> + 'a>> {
        let overlay = self.overlay_for(ElementKind::Edge, label, key, transform)?;
        let slots = overlay.slots(range, move |slot| excluded_edge(self, slot));
        Some(Box::new(slots.filter_map(move |slot| self.slot_edge(slot))))
    }

    /// The exact number of visible elements of the index on `(element, label, key,
    /// transform)` inside `range` ([`IndexOverlay::count`]); `None` without such an index or
    /// when a part cannot count its slice exactly.
    pub(crate) fn layer_index_count(
        &self,
        element: ElementKind,
        label: Option<&str>,
        key: &str,
        transform: Option<&PropertyIndexTransform>,
        range: &PropertyIndexRange,
    ) -> Option<u64> {
        self.overlay_for(element, label, key, transform)?
            .count(range)
    }
}

/// The unique property indexes of a layer's writes (S4.3), served from the overlays: the
/// layer's side of `WriteView::check_unique_vertex` / `check_unique_edge`.
impl LayeredGraph {
    /// `Err` with the [`GraphError::UniqueViolation`] a `TraversalGraph` with the layer's data
    /// returns for the same write (`PropertyIndexes::unique_conflict`'s arguments, `me` a
    /// layer slot): the indexes are searched in its order by the lookups of the layer-owned
    /// parts, each key over its overlay ([`IndexOverlay::other_holder`]: base elements not
    /// excluded plus the delta's), and the other element's id is resolved through the layer.
    /// One branch without an enforced unique index.
    pub(crate) fn layer_unique_check<'l>(
        &self,
        element: ElementKind,
        me: Option<u32>,
        labels: impl IntoIterator<Item = &'l str>,
        properties: Option<&PropertyMap>,
        write: Option<(&str, &ElementProperty)>,
    ) -> Result<(), GraphError> {
        let overlays = self.delta.indexes();
        if !overlays.has_unique() {
            return Ok(());
        }
        let conflict = overlays.delta.unique_conflict_by(
            element,
            labels,
            properties,
            write,
            |position, _, key| {
                overlays
                    .overlay(&self.base, position)?
                    .other_holder(key, me, |slot| excluded(self, element, slot))
            },
        );
        let Some(conflict) = conflict else {
            return Ok(());
        };
        Err(self.layer_unique_violation(conflict))
    }

    /// The error of a unique `conflict` found over the overlays: the index's name, element and
    /// key, the value written and the other element's id (a base slot's from the base: an
    /// image keeps its element's id; an added element's from the delta).
    #[cold]
    fn layer_unique_violation(&self, conflict: UniqueConflict) -> GraphError {
        let (position, slot, value) = conflict.into_parts();
        let Some(overlay) = self.delta.indexes().overlay(&self.base, position) else {
            return GraphError::invalid_element_state("a unique index moved during a write");
        };
        let index = overlay.base;
        let element = index.element();
        let existing = if slot & DELTA_BIT == 0 {
            self.base.slot_id(element, slot)
        } else {
            let id = match element {
                ElementKind::Vertex => self
                    .delta
                    .added_vertex_at(slot & !DELTA_BIT)
                    .and_then(|node| GraphStorage::vertex_id(self, node).ok().flatten()),
                ElementKind::Edge => self
                    .delta
                    .added_edge_at(slot & !DELTA_BIT)
                    .and_then(|edge| GraphStorage::edge_id(self, edge).ok().flatten()),
            };
            id.map(str::to_string)
        };
        GraphError::unique_violation(
            index.name(),
            element,
            index.key(),
            &value,
            existing.unwrap_or_default(),
        )
    }
}

/// The indexes a layer defines, drops and redefines (S4.4, module docs): the catalog side of
/// the overlays.
impl LayeredGraph {
    /// The definition of the property index `name` in the layer's catalog (either index kind:
    /// one namespace), with its kind.
    fn layer_index_definition(&self, name: &str) -> Option<&crate::graph::definition::Definition> {
        self.catalog
            .get(DefinitionKind::PROPERTY_INDEX, name)
            .or_else(|| self.catalog.get(DefinitionKind::PROPERTY_INDEX_EXT, name))
    }

    /// After a catalog change of kind property index (a define, a drop, either one undone):
    /// rebuilds the overlay of the index `name` from the layer's catalog and data, or removes
    /// it when the catalog no longer serves it; a unique index over repeated values is kept
    /// disabled with the layer's report. `TraversalGraph::property_index_after_change`'s
    /// counterpart, with its position rule.
    pub(crate) fn layer_index_after_change(&mut self, name: &str) {
        // An index of a type this version does not know is kept in the catalog, never built.
        let definition = self.layer_index_definition(name).and_then(|definition| {
            let index = definition.as_property_index()?;
            index
                .is_served_type()
                .then(|| (definition.clone(), index.clone()))
        });
        let (built, disabled) = match definition {
            Some((definition, index)) => match self.build_overlay(name, &definition, &index) {
                Ok(built) => (Some(built), None),
                Err(report) => (None, Some(report)),
            },
            None => (None, None),
        };
        self.delta.indexes_mut().install(name, built, disabled);
    }

    /// Rebuilds every overlay from the layer's catalog and data, as a live layer holding them
    /// would have them (the end of a load, module `persist`): the base's installed indexes
    /// first, at their positions (dropped when the layer's catalog no longer serves them, a
    /// part of its own when the layer redefined them), then the indexes only the layer's catalog
    /// names, appended in catalog order. A unique index the base keeps disabled and the layer
    /// holds unchanged stays disabled with the base's report, as on a live layer.
    #[cfg(feature = "persist")]
    pub(crate) fn rebuild_layer_indexes(&mut self) {
        *self.delta.indexes_mut() = LayerIndexes::inherit(&self.base);
        let base = std::sync::Arc::clone(&self.base);
        let installed = &base.property_indexes;
        let mut names: Vec<CompactString> = (0..installed.len())
            .filter_map(|position| installed.state(position))
            .map(|state| CompactString::from(state.name()))
            .collect();
        for definition in self.catalog.iter() {
            if definition.kind().is_property_index()
                && !names.iter().any(|name| name == definition.name())
            {
                names.push(CompactString::from(definition.name()));
            }
        }
        for name in names {
            let inherited_disabled = installed
                .disabled()
                .iter()
                .any(|(disabled, _)| *disabled == name)
                && self
                    .layer_index_definition(&name)
                    .is_some_and(|definition| {
                        base.definitions.get(definition.kind(), &name) == Some(definition)
                    });
            if !inherited_disabled {
                self.layer_index_after_change(&name);
            }
        }
    }

    /// The three parts of the overlay of the index `name` (`definition` as the layer's catalog
    /// holds it, `index` its body); a unique one whose merged view holds repeated values gives
    /// the report instead (`TraversalGraph::build_checked`'s counterpart over the layer's
    /// elements).
    fn build_overlay(
        &self,
        name: &str,
        definition: &crate::graph::definition::Definition,
        index: &PropertyIndexDefinition,
    ) -> Result<BuiltOverlay, UniqueIndexDuplicates> {
        let template = IndexState::of_definition(name, index);
        if index.is_unique() {
            let visible = self.visible_state(&template);
            let groups = visible.duplicate_groups();
            if !groups.is_empty() {
                tracing::warn!(
                    index = name,
                    repeated = groups.len(),
                    "a unique property index is not served: its data holds repeated values"
                );
                let element = index.element();
                let key = index.key();
                return Err(duplicates_report_by(
                    &visible,
                    &groups,
                    |slot| self.layer_slot_id(element, slot),
                    |slot| self.layer_slot_value(element, slot, key),
                ));
            }
        }
        // The base's own index of this definition, installed there, is read in place.
        let shared = self
            .base
            .definitions
            .get(definition.kind(), name)
            .filter(|base_definition| *base_definition == definition)
            .and_then(|_| self.base.property_indexes.position_of(name));
        let base = match shared {
            Some(position) => BasePart::Shared(position),
            None => BasePart::Owned(Box::new(self.base.build_index(name, index))),
        };
        let (masked, delta) = self.rebuild_parts(&template);
        Ok((base, masked, delta))
    }

    /// The id of the live element of kind `element` at layer `slot`.
    fn layer_slot_id(&self, element: ElementKind, slot: u32) -> Option<String> {
        match element {
            ElementKind::Vertex => GraphStorage::vertex_id(self, self.slot_vertex(slot)?)
                .ok()
                .flatten()
                .map(str::to_string),
            ElementKind::Edge => GraphStorage::edge_id(self, self.slot_edge(slot)?)
                .ok()
                .flatten()
                .map(str::to_string),
        }
    }

    /// The logical value at top-level `key` of the live element of kind `element` at layer
    /// `slot`, as the layer reads it.
    fn layer_slot_value(
        &self,
        element: ElementKind,
        slot: u32,
        key: &str,
    ) -> Option<ElementProperty> {
        let map = match element {
            ElementKind::Vertex => GraphStorage::vertex_properties(self, self.slot_vertex(slot)?),
            ElementKind::Edge => GraphStorage::edge_properties(self, self.slot_edge(slot)?),
        };
        map.ok()??.get(key).map(ElementProperty::to_logical)
    }

    /// The index of `template`'s definition over every element the layer lists through its
    /// read API, keyed by layer slot: what a `TraversalGraph` with the layer's data holds (the
    /// merged view a unique define is checked over, and what `check_layer_indexes` compares
    /// the visible entries with).
    fn visible_state(&self, template: &IndexState) -> IndexState {
        let mut state = template.empty_like();
        let key = template.key();
        match template.element() {
            ElementKind::Vertex => {
                for node in GraphStorage::vertices(self).into_iter().flatten() {
                    let labels = GraphStorage::vertex_labels(self, node).unwrap_or(&[]);
                    if in_index(template, labels.iter().map(|l| l.as_str()))
                        && let Ok(Some(map)) = GraphStorage::vertex_properties(self, node)
                        && let Some(value) = map.get(key)
                    {
                        state.account(true, value, node.slot());
                    }
                }
            }
            ElementKind::Edge => {
                for edge in GraphStorage::edges(self).into_iter().flatten() {
                    let label = GraphStorage::edge_label(self, edge).ok().flatten();
                    if in_index(template, label.into_iter())
                        && let Ok(Some(map)) = GraphStorage::edge_properties(self, edge)
                        && let Some(value) = map.get(key)
                    {
                        state.account(true, value, edge.slot());
                    }
                }
            }
        }
        state
    }
}

/// Every `(key, slot)` entry of `state`, in key-then-slot order.
fn pairs(state: &IndexState) -> Vec<(IndexKey, u32)> {
    state
        .postings()
        .flat_map(|(key, slots)| slots.map(move |slot| (key.clone(), slot)))
        .collect()
}

/// Whether an element with `labels` belongs to the index `state` (every element of the kind
/// for a label-less one).
fn in_index<'l>(state: &IndexState, mut labels: impl Iterator<Item = &'l str>) -> bool {
    match state.label() {
        None => true,
        Some(label) => labels.any(|held| held == label),
    }
}

/// Whether `state` has `template`'s definition (name, element, label, key, transform, unique).
fn same_definition(state: &IndexState, template: &IndexState) -> bool {
    state.name() == template.name()
        && state.element() == template.element()
        && state.label() == template.label()
        && state.key() == template.key()
        && state.transform() == template.transform()
        && state.is_unique() == template.is_unique()
}

/// Compares one maintained part with its rebuild: the same entries, lengths, skipped counts,
/// and the heap counted with every write equal to the walked one.
fn same_part(name: &str, part: &str, kept: &IndexState, fresh: &IndexState) -> Result<(), String> {
    if !kept.same_entries(fresh) || kept.len() != fresh.len() {
        return Err(format!(
            "index '{name}': the {part} part holds {} entries, {} after a rebuild (or other \
             values)",
            kept.len(),
            fresh.len()
        ));
    }
    if kept.skipped() != fresh.skipped() {
        return Err(format!(
            "index '{name}': the {part} part counts {} skipped values, {} after a rebuild",
            kept.skipped(),
            fresh.skipped()
        ));
    }
    if kept.heap() != kept.walked_heap() {
        return Err(format!(
            "index '{name}': the {part} part counts {} heap bytes, holds {}",
            kept.heap(),
            kept.walked_heap()
        ));
    }
    Ok(())
}

impl LayeredGraph {
    /// Compares every index overlay with a rebuild from scratch: `Err` names the first overlay
    /// that differs. A test helper (it walks the base slots the layer excludes, the delta and
    /// every visible element of each overlay's kind); the counterpart of
    /// [`TraversalGraph::check_property_indexes`].
    ///
    /// Checked:
    /// - one overlay per index the layer's catalog serves and does not keep disabled, and no
    ///   other;
    /// - per overlay, every part's definition is the catalog's; a shared base part is the base's
    ///   installed index of the same catalog definition, an owned one equals a build over the
    ///   base (`TraversalGraph::build_index`);
    /// - `masked` equals the base entries of the excluded base slots (removed or imaged),
    ///   rebuilt from the base's data;
    /// - `delta` equals the entries of the added elements and the images under their layer
    ///   slots, rebuilt from the layer's data;
    /// - the visible entries, the base part minus the excluded slots plus `delta`, equal an
    ///   index built over every element the layer lists through its read API (what a
    ///   `TraversalGraph` with the same data holds, keyed by the layer's slots), and so do the
    ///   counts `base - masked + delta` (entries and skipped values).
    #[doc(hidden)]
    pub fn check_layer_indexes(&self) -> Result<(), String> {
        let overlays = self.delta.indexes();
        let mut served: Vec<&str> = self
            .catalog
            .iter()
            .filter(|definition| {
                definition
                    .as_property_index()
                    .is_some_and(PropertyIndexDefinition::is_served_type)
                    && overlays.disabled_report(definition.name()).is_none()
            })
            .map(|definition| definition.name())
            .collect();
        served.sort_unstable();
        let mut kept: Vec<&str> = (0..overlays.len())
            .filter_map(|position| overlays.delta.state(position).map(IndexState::name))
            .collect();
        kept.sort_unstable();
        if served != kept {
            return Err(format!(
                "overlays kept for {kept:?}, the layer's catalog serves {served:?}"
            ));
        }
        for position in 0..overlays.len() {
            let Some(overlay) = overlays.overlay(&self.base, position) else {
                return Err(format!("overlay {position}: a part is missing"));
            };
            let name = overlay.delta.name();
            let Some(definition) = self.layer_index_definition(name) else {
                return Err(format!("index '{name}': not in the layer's catalog"));
            };
            let Some(index) = definition.as_property_index() else {
                return Err(format!("index '{name}': not a property index definition"));
            };
            let template = IndexState::of_definition(name, index);
            for (part, state) in [
                ("base", overlay.base),
                ("masked", overlay.masked),
                ("delta", overlay.delta),
            ] {
                if !same_definition(state, &template) {
                    return Err(format!(
                        "index '{name}': the {part} part's definition differs from the catalog's"
                    ));
                }
            }
            match overlays.base_part(position) {
                Some(BasePart::Shared(at)) => {
                    let base_definition = self.base.definitions.get(definition.kind(), name);
                    if base_definition != Some(definition)
                        || self.base.property_indexes.position_of(name) != Some(*at)
                    {
                        return Err(format!(
                            "index '{name}': a shared base part, but the layer's definition is                              not the base's installed one"
                        ));
                    }
                }
                Some(BasePart::Owned(state)) => {
                    let fresh = self.base.build_index(name, index);
                    same_part(name, "owned base", state, &fresh)?;
                }
                None => return Err(format!("overlay {position}: no base part")),
            }
            let (fresh_masked, fresh_delta) = self.rebuild_parts(&template);
            same_part(name, "masked", overlay.masked, &fresh_masked)?;
            same_part(name, "delta", overlay.delta, &fresh_delta)?;
            self.check_visible(name, overlay)?;
        }
        Ok(())
    }

    /// The masked and delta parts of the overlay of `base_state`, rebuilt from scratch.
    fn rebuild_parts(&self, base_state: &IndexState) -> (IndexState, IndexState) {
        let mut masked = base_state.empty_like();
        let mut delta = base_state.empty_like();
        let key = base_state.key();
        match base_state.element() {
            ElementKind::Vertex => {
                let excluded_slots = self
                    .delta
                    .removed_vertices()
                    .into_iter()
                    .flat_map(|removed| removed.ones())
                    .chain(self.delta.imaged_vertices().ones());
                for slot in excluded_slots {
                    if let Some(vertex) = self.base.graph.node_weight(NodeIndex::from_slot(slot))
                        && in_index(
                            base_state,
                            self.base.labels.set(vertex.1).iter().map(|l| l.as_str()),
                        )
                        && let Some(value) = vertex.2.get(key)
                    {
                        masked.account(true, value, slot);
                    }
                }
                let images = self.delta.imaged_vertices().ones().filter_map(|slot| {
                    let node = self.base.graph.node_weight(NodeIndex::from_slot(slot))?;
                    Some(LayeredVertex::from_base(crate::graph::VertexHandle::new(
                        NodeIndex::from_slot(slot),
                        node.3,
                    )))
                });
                for node in self.delta.vertices().chain(images) {
                    let labels = GraphStorage::vertex_labels(self, node).unwrap_or(&[]);
                    if in_index(base_state, labels.iter().map(|l| l.as_str()))
                        && let Ok(Some(map)) = GraphStorage::vertex_properties(self, node)
                        && let Some(value) = map.get(key)
                    {
                        delta.account(true, value, node.slot());
                    }
                }
            }
            ElementKind::Edge => {
                let excluded_slots = self
                    .delta
                    .removed_edges()
                    .into_iter()
                    .flat_map(|removed| removed.ones())
                    .chain(self.delta.imaged_edges().ones());
                for slot in excluded_slots {
                    if let Some(weight) = self.base.graph.edge_weight(EdgeIndex::from_slot(slot))
                        && in_index(base_state, self.base.labels.name_str(weight.1).into_iter())
                        && let Some(value) = weight.2.as_ref().and_then(|map| map.get(key))
                    {
                        masked.account(true, value, slot);
                    }
                }
                let images = self.delta.imaged_edges().ones().filter_map(|slot| {
                    let weight = self.base.graph.edge_weight(EdgeIndex::from_slot(slot))?;
                    Some(LayeredEdge::from_base(crate::graph::EdgeHandle::new(
                        EdgeIndex::from_slot(slot),
                        weight.3.own,
                    )))
                });
                for edge in self.delta.edges().chain(images) {
                    let label = GraphStorage::edge_label(self, edge).ok().flatten();
                    if in_index(base_state, label.into_iter())
                        && let Ok(Some(map)) = GraphStorage::edge_properties(self, edge)
                        && let Some(value) = map.get(key)
                    {
                        delta.account(true, value, edge.slot());
                    }
                }
            }
        }
        (masked, delta)
    }

    /// The visible entries of `overlay` (base minus the excluded slots, plus delta) against an
    /// index over every element the layer lists, and the counts `base - masked + delta`.
    fn check_visible(&self, name: &str, overlay: IndexOverlay<'_>) -> Result<(), String> {
        let element = overlay.base.element();
        let expected = self.visible_state(overlay.base);
        let mut visible: Vec<(IndexKey, u32)> = pairs(overlay.base)
            .into_iter()
            .filter(|(_, slot)| !excluded(self, element, *slot))
            .chain(pairs(overlay.delta))
            .collect();
        visible.sort();
        let wanted = pairs(&expected);
        if visible != wanted {
            return Err(format!(
                "index '{name}': {} visible entries (base minus excluded plus delta), {} in an \
                 index over the layer's elements (or other values)",
                visible.len(),
                wanted.len()
            ));
        }
        let base_pairs = pairs(overlay.base);
        let masked_in_base = pairs(overlay.masked).into_iter().all(|(key, slot)| {
            excluded(self, element, slot) && base_pairs.binary_search(&(key, slot)).is_ok()
        });
        if !masked_in_base {
            return Err(format!(
                "index '{name}': the masked part holds an entry the base part does not exclude"
            ));
        }
        let entries = (overlay.base.len() + overlay.delta.len()).checked_sub(overlay.masked.len());
        let skipped = (overlay.base.skipped() + overlay.delta.skipped())
            .checked_sub(overlay.masked.skipped());
        if entries != Some(expected.len()) || skipped != Some(expected.skipped()) {
            return Err(format!(
                "index '{name}': base - masked + delta counts {entries:?} entries and \
                 {skipped:?} skipped values, the layer's elements {} and {}",
                expected.len(),
                expected.skipped()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::graph::definition::Definition;
    use crate::graph::property_index::PropertyIndexDefinition;
    use crate::graph::{GraphStorage, RollbackReason};
    use crate::value::ElementProperty;

    /// The modern graph with a vertex index on `person.age`, a label-less one on `name` and an
    /// edge index on `knows.weight`.
    fn indexed_base() -> Arc<TraversalGraph> {
        let mut base = TraversalGraph::tinkerpop_modern();
        for (name, definition) in [
            (
                "age",
                PropertyIndexDefinition::new(ElementKind::Vertex, "person", "age"),
            ),
            (
                "name",
                PropertyIndexDefinition::label_less(ElementKind::Vertex, "name"),
            ),
            (
                "weight",
                PropertyIndexDefinition::new(ElementKind::Edge, "knows", "weight"),
            ),
        ] {
            base.set_definition(Definition::property_index(name, definition))
                .unwrap();
        }
        Arc::new(base)
    }

    #[test]
    fn a_new_layer_inherits_one_empty_overlay_per_installed_index() {
        let layer = LayeredGraph::over(indexed_base()).unwrap();
        let overlays = layer.delta.indexes();
        assert_eq!(overlays.len(), 3);
        assert_eq!(overlays.bytes(), 0);
        for position in 0..3 {
            let overlay = overlays.overlay(&layer.base, position).unwrap();
            assert!(overlay.masked.is_empty() && overlay.delta.is_empty());
            assert!(!overlay.base.is_empty());
        }
        layer.check_layer_indexes().unwrap();
        assert!(
            LayeredGraph::over(Arc::new(TraversalGraph::tinkerpop_modern()))
                .unwrap()
                .delta
                .indexes()
                .is_empty()
        );
    }

    /// A copy-up masks the base entries and puts the image's into the delta part; a return to
    /// clean takes both out; a drop of a base element masks it; a rollback restores it all.
    #[test]
    fn copy_up_return_to_clean_drop_and_rollback() {
        let mut layer = LayeredGraph::over(indexed_base()).unwrap();
        let marko = layer.get_vertex_by_id("1").unwrap().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), 30.into())
            .unwrap();
        layer.check_layer_indexes().unwrap();
        let age = layer.delta.indexes().overlay(&layer.base, 0).unwrap();
        assert_eq!((age.masked.len(), age.delta.len()), (1, 1));
        // The label-less `name` index masks marko's base name and holds the image's.
        let name = layer.delta.indexes().overlay(&layer.base, 1).unwrap();
        assert_eq!((name.masked.len(), name.delta.len()), (1, 1));
        layer
            .set_vertex_property(marko, "age".into(), 29.into())
            .unwrap();
        layer.check_layer_indexes().unwrap();
        assert_eq!(layer.delta.indexes().bytes(), 0, "back to clean");

        let mark = layer.begin_unit().unwrap();
        layer
            .set_vertex_property(marko, "age".into(), ElementProperty::from("old"))
            .unwrap();
        layer.remove_vertex_label(marko, "person").unwrap();
        layer.check_layer_indexes().unwrap();
        let knows = layer.get_edge_by_id("2").unwrap().unwrap();
        layer
            .set_edge_property(knows, "weight".into(), 0.25.into())
            .unwrap();
        layer.drop_vertex(marko).unwrap();
        layer.check_layer_indexes().unwrap();
        let added = layer
            .insert_vertex(None, vec!["person".into()], PropertyMap::default())
            .unwrap();
        layer
            .set_vertex_property(added, "age".into(), 40.into())
            .unwrap();
        layer.check_layer_indexes().unwrap();
        layer
            .rollback_unit(mark, &RollbackReason::failed(&"test"))
            .unwrap();
        layer.check_layer_indexes().unwrap();
        assert_eq!(layer.delta.indexes().bytes(), 0);
    }

    /// The overlays' bytes are the layer's own (in its total, never in the shared figure).
    #[test]
    fn overlay_bytes_count_in_the_layers_own_footprint() {
        let mut layer = LayeredGraph::over(indexed_base()).unwrap();
        let before = layer.memory_usage().unwrap().unwrap();
        assert_eq!(before.property_index_bytes, 0);
        let vadas = layer.get_vertex_by_id("2").unwrap().unwrap();
        layer
            .set_vertex_property(vadas, "age".into(), 28.into())
            .unwrap();
        let after = layer.memory_usage().unwrap().unwrap();
        assert_eq!(
            after.property_index_bytes,
            layer.delta.indexes().bytes(),
            "{after:?}"
        );
        assert!(after.property_index_bytes > 0);
        assert_eq!(after.shared_bytes, before.shared_bytes);
    }
}
