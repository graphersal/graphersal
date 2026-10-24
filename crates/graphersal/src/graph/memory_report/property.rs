//! The memory report as a value ([`MemoryReport::to_property`]): the one form every front end
//! shows (the DSL map, the session's and the MCP tool's JSON, Python's dict), so that they all
//! carry the same keys. Keys are snake_case; byte figures are integers; a missing figure is
//! `null` (the structure of a storage that does not report one, a label's slot bytes).

use super::{
    DegreeStats, LabelMemory, MemoryReport, PropertyMemory, PropertyTarget, RuleMemory,
    StringMemory, StructureMemory,
};
use crate::graph::memory::MemoryStats;
use crate::graph::property_index::PropertyIndexInfo;
use crate::value::{ElementProperty, PropertyMap};

/// An integer figure (saturated at `i64::MAX`).
fn int(value: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(value).unwrap_or(i64::MAX))
}

fn text(value: &str) -> ElementProperty {
    ElementProperty::from(value)
}

fn object(map: PropertyMap) -> ElementProperty {
    ElementProperty::Object(Box::new(map))
}

fn list<T>(items: &[T], each: impl Fn(&T) -> ElementProperty) -> ElementProperty {
    ElementProperty::Array(items.iter().map(each).collect())
}

fn optional(value: Option<u64>) -> ElementProperty {
    value.map_or(ElementProperty::Null, int)
}

fn target(map: &mut PropertyMap, target: &PropertyTarget) {
    map.insert("element", text(target.element.as_str()));
    map.insert(
        "label",
        target.label.as_deref().map_or(ElementProperty::Null, text),
    );
    map.insert("path", text(&target.path.to_string()));
}

fn strings(strings: &StringMemory) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("values", int(strings.values));
    map.insert("inline_values", int(strings.inline_values));
    map.insert("heap_bytes", int(strings.heap_bytes));
    map.insert("plain_bytes", int(strings.plain_bytes));
    map.insert("distinct_estimate", int(strings.distinct_estimate));
    map.insert(
        "distinct_reliable",
        ElementProperty::from(strings.distinct_reliable),
    );
    object(map)
}

fn property(row: &PropertyMemory) -> ElementProperty {
    let mut map = PropertyMap::default();
    target(&mut map, &row.target);
    map.insert("leaf", ElementProperty::from(row.leaf));
    map.insert("nested_values", int(row.nested_values));
    map.insert("elements", int(row.elements));
    map.insert("label_elements", int(row.label_elements));
    map.insert(
        "value_types",
        list(&row.value_types, |count| {
            let mut map = PropertyMap::default();
            map.insert("type", text(&count.name));
            map.insert("count", int(count.count));
            object(map)
        }),
    );
    map.insert("bytes", int(row.bytes()));
    map.insert("slot_bytes", int(row.slot_bytes));
    map.insert("heap_bytes", int(row.heap_bytes));
    // Only non-empty buckets; each names its inclusive upper bound (`null`: above 1 MiB).
    let sizes: Vec<ElementProperty> = row
        .sizes
        .iter()
        .filter(|bucket| bucket.values > 0)
        .map(|bucket| {
            let mut map = PropertyMap::default();
            map.insert("max_bytes", optional(bucket.max_bytes));
            map.insert("values", int(bucket.values));
            map.insert("bytes", int(bucket.bytes));
            object(map)
        })
        .collect();
    map.insert("sizes", ElementProperty::Array(sizes));
    map.insert("max_value_bytes", int(row.max_value_bytes));
    map.insert(
        "strings",
        row.strings.as_ref().map_or(ElementProperty::Null, strings),
    );
    map.insert(
        "vectors",
        list(&row.vectors, |vector| {
            let mut map = PropertyMap::default();
            map.insert("element", text(vector.element.name()));
            map.insert("dimension", int(vector.dimension));
            map.insert("count", int(vector.count));
            map.insert("bytes", int(vector.bytes));
            object(map)
        }),
    );
    map.insert("compressed_values", int(row.compressed_values));
    map.insert("compressed_plain_bytes", int(row.compressed_plain_bytes));
    map.insert("compressed_stored_bytes", int(row.compressed_stored_bytes));
    map.insert("deduped_values", int(row.deduped_values));
    map.insert("deduped_plain_bytes", int(row.deduped_plain_bytes));
    map.insert("rules", list(&row.rules, |name| text(name)));
    map.insert("indexes", list(&row.indexes, |name| text(name)));
    object(map)
}

fn degree(degree: &DegreeStats) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("max_out", int(degree.max_out));
    map.insert("max_in", int(degree.max_in));
    map.insert("max", int(degree.max));
    map.insert("p95", int(degree.p95));
    map.insert("edge_ends", int(degree.edge_ends));
    object(map)
}

fn label(row: &LabelMemory) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("element", text(row.element.as_str()));
    map.insert(
        "label",
        row.label.as_deref().map_or(ElementProperty::Null, text),
    );
    map.insert("elements", int(row.elements));
    map.insert("id_heap_bytes", int(row.id_heap_bytes));
    map.insert("label_bytes", int(row.label_bytes));
    map.insert("slot_bytes", optional(row.slot_bytes));
    map.insert("property_bytes", int(row.property_bytes));
    map.insert("map_overhead_bytes", int(row.map_overhead_bytes));
    map.insert("values", int(row.values));
    map.insert(
        "degree",
        row.degree.as_ref().map_or(ElementProperty::Null, degree),
    );
    object(map)
}

fn index(info: &PropertyIndexInfo) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("name", text(&info.name));
    map.insert("element", text(info.element.as_str()));
    map.insert("label", text(&info.label));
    map.insert("key", text(&info.key));
    map.insert("entries", int(info.stats.entries));
    map.insert("distinct_values", int(info.stats.distinct_values));
    map.insert("bytes", int(info.stats.bytes));
    object(map)
}

fn rule(rule: &RuleMemory) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("name", text(&rule.name));
    target(&mut map, &rule.target);
    map.insert("codec", text(&rule.codec));
    if let Some(stats) = &rule.stats {
        map.insert("values", int(stats.compressed_values));
        map.insert("plain_bytes", int(stats.plain_bytes));
        map.insert("stored_bytes", int(stats.stored_bytes));
        map.insert("dictionary_bytes", int(stats.dictionary_bytes));
        map.insert("distinct_values", int(stats.distinct_values));
    }
    object(map)
}

/// The totals of [`MemoryStats`] (the keys of the DSL's `g.memory_usage()`).
fn totals(stats: &MemoryStats) -> ElementProperty {
    let mut map = PropertyMap::default();
    map.insert("structure_bytes", int(stats.structure_bytes));
    map.insert("index_bytes", int(stats.index_bytes));
    map.insert("property_bytes", int(stats.property_bytes));
    map.insert(
        "compression_dictionary_bytes",
        int(stats.compression_dictionary_bytes),
    );
    map.insert("property_index_bytes", int(stats.property_index_bytes));
    map.insert("dedup_table_bytes", int(stats.dedup_table_bytes));
    map.insert("shape_bytes", int(stats.shape_bytes));
    map.insert("total_bytes", int(stats.total_bytes()));
    object(map)
}

fn structure(structure: &StructureMemory) -> ElementProperty {
    let slots = |usage: &super::SlotUsage| {
        let mut map = PropertyMap::default();
        map.insert("live", int(usage.live));
        map.insert("tombstoned", int(usage.tombstoned));
        map.insert("spare", int(usage.spare));
        map.insert("slot_bytes", int(usage.slot_bytes));
        map.insert("bytes", int(usage.bytes()));
        object(map)
    };
    let mut map = PropertyMap::default();
    map.insert("totals", totals(&structure.totals));
    map.insert("vertex_slots", slots(&structure.vertex_slots));
    map.insert("edge_slots", slots(&structure.edge_slots));
    map.insert("id_index_bytes", int(structure.id_index_bytes));
    map.insert("label_index_bytes", int(structure.label_index_bytes));
    map.insert("shape_table_shapes", int(structure.shape_table_shapes));
    object(map)
}

impl MemoryReport {
    /// The report as a value: an object with `properties`, `rows_omitted`, `labels`,
    /// `labels_omitted`, `shapes`,
    /// `indexes`, `compression_rules`, `structure` (`null` when the storage does not report
    /// one), `property_bytes`, `estimated_total_bytes`, `tracked_live_bytes`,
    /// `unattributed_bytes`, `sampled`, `sample_every`, `visited_elements`, `path_depth`,
    /// `commit_seq` and `elapsed_ms`. A property row names its `element`, `label` and `path`
    /// (`"$.name"`) and carries the figures of [`PropertyMemory`] under their field names, with
    /// `bytes` (slots plus heap) and only the non-empty size buckets (`max_bytes` `null`: the
    /// open bucket). The DSL's `g.memory_report()` answers this map.
    ///
    /// ```
    /// use graphersal::prelude::*;
    /// use graphersal::storage::MemoryReportRequest;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let report = graph.traversal().memory_report(&MemoryReportRequest::new()).unwrap();
    /// let json = report.to_json();
    /// assert!(json.contains(r#""path":"$.name""#), "{json}");
    /// ```
    pub fn to_property(&self) -> ElementProperty {
        let mut map = PropertyMap::default();
        map.insert("properties", list(&self.properties, property));
        map.insert("rows_omitted", int(self.rows_omitted));
        map.insert("labels", list(&self.labels, label));
        map.insert("labels_omitted", int(self.labels_omitted));
        let mut shapes = PropertyMap::default();
        shapes.insert("shapes", int(self.shapes.shapes));
        shapes.insert("shaped_elements", int(self.shapes.shaped_elements));
        shapes.insert("unshaped_elements", int(self.shapes.unshaped_elements));
        shapes.insert(
            "histogram",
            list(&self.shapes.histogram, |bucket| {
                let mut map = PropertyMap::default();
                map.insert("max_elements", int(bucket.max_elements));
                map.insert("shapes", int(bucket.shapes));
                map.insert("elements", int(bucket.elements));
                object(map)
            }),
        );
        map.insert("shapes", object(shapes));
        map.insert("indexes", list(&self.indexes, index));
        map.insert("compression_rules", list(&self.compression_rules, rule));
        map.insert(
            "structure",
            self.structure
                .as_ref()
                .map_or(ElementProperty::Null, structure),
        );
        map.insert("property_bytes", int(self.property_bytes()));
        map.insert(
            "estimated_total_bytes",
            optional(self.estimated_total_bytes()),
        );
        map.insert("tracked_live_bytes", optional(self.tracked_live_bytes));
        map.insert(
            "unattributed_bytes",
            self.unattributed_bytes()
                .map_or(ElementProperty::Null, ElementProperty::from),
        );
        map.insert("sampled", ElementProperty::from(self.sampled));
        map.insert("sample_every", int(self.sample_every));
        map.insert("visited_elements", int(self.visited_elements));
        map.insert("path_depth", int(u64::from(self.path_depth)));
        map.insert("commit_seq", int(self.commit_seq));
        map.insert(
            "elapsed_ms",
            ElementProperty::from(self.elapsed_micros as f64 / 1000.0),
        );
        object(map)
    }

    /// The report as compact JSON text: [`Self::to_property`] written by the physical codec.
    pub fn to_json(&self) -> String {
        crate::value::physical::to_json_text(&self.to_property()).unwrap_or_default()
    }
}
