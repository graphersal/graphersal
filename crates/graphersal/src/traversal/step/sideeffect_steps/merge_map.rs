//! The merge-map core shared by `mergeV()` (and `mergeE()`): parsing and validating the merge
//! argument and the `option(Merge.onCreate|onMatch, ..)` maps, the onCreate override rule, the
//! storage lookup and the property writes.
//!
//! A merge map is a map with property keys plus the reserved token keys `"T.id"`, `"T.label"`
//! and (edges only) `"Direction.OUT"` / `"Direction.IN"`, whose value may be the placeholder
//! `"Merge.outV"` / `"Merge.inV"`. Hidden (`~`) keys and every other `T.` / `Direction.` key are
//! errors, never silent property keys.

use std::borrow::Cow;
use std::fmt;

use super::step_property::check_cardinality;
use super::step_property_map::{PropertyEntry, WriteTarget, write_entries};
use crate::graph::GraphStorage;
use crate::traversal::step::filter_steps::step_has::{edge_has, vertex_has};
use crate::traversal::step::flatmap_steps::step_choose::option_without_branch;
use crate::*;

/// The reserved key of the element id.
pub(crate) const KEY_ID: &str = "T.id";
/// The reserved key of the element label.
pub(crate) const KEY_LABEL: &str = "T.label";
/// The reserved key of an edge's out-vertex.
pub(crate) const KEY_OUT: &str = "Direction.OUT";
/// The reserved key of an edge's in-vertex.
pub(crate) const KEY_IN: &str = "Direction.IN";
/// The placeholder value resolved from `option(Merge.outV, ..)`.
pub(crate) const PLACEHOLDER_OUT: &str = "Merge.outV";
/// The placeholder value resolved from `option(Merge.inV, ..)`.
pub(crate) const PLACEHOLDER_IN: &str = "Merge.inV";

/// Which element a merge step merges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeKind {
    Vertex,
    Edge,
}

impl MergeKind {
    /// The step name used in messages.
    pub(crate) fn step(self) -> &'static str {
        match self {
            MergeKind::Vertex => "merge_v",
            MergeKind::Edge => "merge_e",
        }
    }
}

/// Which map of a merge step is parsed; decides which token keys are allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeRole {
    /// The merge (search) argument.
    Merge,
    /// `option(Merge.onCreate, ..)`.
    OnCreate,
    /// `option(Merge.onMatch, ..)`.
    OnMatch,
    /// A vertex search map of `option(Merge.outV|inV, ..)` (`mergeE()`).
    VertexLookup,
}

impl MergeRole {
    /// The map's name in messages.
    pub(crate) fn argument(self) -> &'static str {
        match self {
            MergeRole::Merge => "the merge() map",
            MergeRole::OnCreate => "the option(onCreate) map",
            MergeRole::OnMatch => "the option(onMatch) map",
            MergeRole::VertexLookup => "the option(outV/inV) map",
        }
    }
}

/// An edge endpoint named by a `Direction.OUT` / `Direction.IN` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EndpointRef {
    /// A vertex id.
    Id(CompactString),
    /// `Merge.outV` / `Merge.inV`: resolved from the matching `option(Merge.outV|inV, ..)`.
    Placeholder,
}

/// A validated merge map.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParsedMap {
    pub(crate) id: Option<CompactString>,
    pub(crate) label: Option<CompactString>,
    pub(crate) out_v: Option<EndpointRef>,
    pub(crate) in_v: Option<EndpointRef>,
    pub(crate) entries: Vec<PropertyEntry>,
    /// The argument as written when it was rejected (a constant that is not a valid map): the
    /// step renders it instead of the empty map that stands in for it, so the step location of
    /// the error shows the query as written (`merge_v(1)`).
    pub(crate) rejected: Option<String>,
}

impl ParsedMap {
    /// Gives every entry without its own cardinality the option's default cardinality.
    pub(crate) fn with_default_cardinality(mut self, default: Option<Cardinality>) -> Self {
        if default.is_some() {
            for entry in &mut self.entries {
                entry.cardinality = entry.cardinality.or(default);
            }
        }
        self
    }

    /// The map with the keys of `on_create` added (after [`check_override`] passed).
    pub(crate) fn inherit(&self, on_create: &ParsedMap) -> ParsedMap {
        let mut merged = self.clone();
        merged.id = merged.id.or_else(|| on_create.id.clone());
        merged.label = merged.label.or_else(|| on_create.label.clone());
        merged.out_v = merged.out_v.or_else(|| on_create.out_v.clone());
        merged.in_v = merged.in_v.or_else(|| on_create.in_v.clone());
        for entry in &on_create.entries {
            if !self.entries.iter().any(|own| own.key == entry.key) {
                merged.entries.push(entry.clone());
            }
        }
        merged
    }
}

impl fmt::Display for ParsedMap {
    /// Renders the map as the DSL writes it, e.g. `#{"T.label": "person", name: "marko"}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::value::literal::{rhai_key, rhai_literal, rhai_string};
        if let Some(rejected) = &self.rejected {
            return f.write_str(rejected);
        }
        let mut parts: Vec<String> = Vec::new();
        let quote = |text: &str| rhai_string(text);
        if let Some(id) = &self.id {
            parts.push(format!("\"{KEY_ID}\": {}", quote(id)));
        }
        if let Some(label) = &self.label {
            parts.push(format!("\"{KEY_LABEL}\": {}", quote(label)));
        }
        for (key, endpoint, placeholder) in [
            (KEY_OUT, &self.out_v, PLACEHOLDER_OUT),
            (KEY_IN, &self.in_v, PLACEHOLDER_IN),
        ] {
            match endpoint {
                Some(EndpointRef::Id(id)) => parts.push(format!("\"{key}\": {}", quote(id))),
                Some(EndpointRef::Placeholder) => parts.push(format!("\"{key}\": {placeholder}")),
                None => {}
            }
        }
        for entry in &self.entries {
            match entry.cardinality {
                Some(cardinality) => parts.push(format!(
                    "{}: Cardinality.{cardinality}({})",
                    rhai_key(&entry.key),
                    rhai_literal(&entry.value)
                )),
                None => parts.push(format!(
                    "{}: {}",
                    rhai_key(&entry.key),
                    rhai_literal(&entry.value)
                )),
            }
        }
        write!(f, "#{{{}}}", parts.join(", "))
    }
}

/// A merge-step argument or option: a constant map (parsed when the traversal is built) or a
/// traversal evaluated per traverser whose first result must be a map.
pub(crate) enum MergeArg<'g, S: GraphStorage + 'g> {
    Constant(ParsedMap),
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> Clone for MergeArg<'g, S> {
    fn clone(&self) -> Self {
        match self {
            MergeArg::Constant(map) => MergeArg::Constant(map.clone()),
            MergeArg::Traversal(traversal) => MergeArg::Traversal(traversal.clone()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for MergeArg<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MergeArg::Constant(map) => write!(f, "{map}"),
            MergeArg::Traversal(traversal) => write!(f, "{traversal}"),
        }
    }
}

/// A problem with a merge map, kept by the step (which must be `Clone`) until it is raised as a
/// [`TraverserError`].
#[derive(Clone, Debug)]
pub(crate) enum MergeProblem {
    /// [`TraverserError::InvalidMergeMap`].
    Invalid {
        kind: MergeKind,
        /// Where the problem is, e.g. `the merge() map` or `option(outV)`.
        argument: &'static str,
        reason: String,
    },
    /// [`TraverserError::MergeOverride`].
    Override {
        kind: MergeKind,
        key: String,
        merge_value: String,
        on_create_value: String,
    },
    /// [`TraverserError::UnsupportedCardinality`]: a cardinality on an edge property.
    EdgeCardinality {
        kind: MergeKind,
        cardinality: Cardinality,
    },
}

impl MergeProblem {
    pub(crate) fn invalid(kind: MergeKind, role: MergeRole, reason: impl Into<String>) -> Self {
        Self::invalid_at(kind, role.argument(), reason)
    }

    pub(crate) fn invalid_at(
        kind: MergeKind,
        argument: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        MergeProblem::Invalid {
            kind,
            argument,
            reason: reason.into(),
        }
    }
}

impl From<MergeProblem> for BoxedTraverserError {
    fn from(problem: MergeProblem) -> Self {
        Box::new(match problem {
            MergeProblem::Invalid {
                kind,
                argument,
                reason,
            } => TraverserError::InvalidMergeMap {
                step: kind.step().to_string(),
                argument: argument.to_string(),
                reason,
            },
            MergeProblem::Override {
                kind,
                key,
                merge_value,
                on_create_value,
            } => TraverserError::MergeOverride {
                step: kind.step().to_string(),
                key,
                merge_value,
                on_create_value,
            },
            MergeProblem::EdgeCardinality { kind, cardinality } => {
                TraverserError::UnsupportedCardinality {
                    step: kind.step().to_string(),
                    cardinality,
                    on_edge: true,
                }
            }
        })
    }
}

/// The value type name of `value`, for messages.
fn type_name(value: &ElementProperty) -> String {
    crate::graph::type_of(value)
        .map(|ty| ty.to_string())
        .unwrap_or_else(|| match value {
            ElementProperty::Vertex(_) => "vertex".to_string(),
            ElementProperty::Edge(_) => "edge".to_string(),
            _ => "unknown".to_string(),
        })
}

/// The entries of a map value: `null` is the empty map, an object one entry per key (without a
/// cardinality); anything else is an error ("mergeV() expects a map, got <type>").
pub(crate) fn entries_of(
    value: &ElementProperty,
    kind: MergeKind,
    role: MergeRole,
) -> Result<Vec<PropertyEntry>, MergeProblem> {
    match value {
        ElementProperty::Null => Ok(Vec::new()),
        ElementProperty::Object(object) => Ok(object
            .iter()
            .map(|(key, value)| PropertyEntry {
                key: key.clone(),
                value: value.clone(),
                cardinality: None,
            })
            .collect()),
        other => Err(MergeProblem::invalid(
            kind,
            role,
            format!("{}() expects a map, got {}", kind.step(), type_name(other)),
        )),
    }
}

/// Parses a map value (an object or `null`); see [`parse_entries`].
pub(crate) fn parse(
    map: &ElementProperty,
    kind: MergeKind,
    role: MergeRole,
) -> Result<ParsedMap, MergeProblem> {
    parse_entries(entries_of(map, kind, role)?, kind, role)
}

/// An id given as a string or an integer (its decimal string).
pub(crate) fn id_text(value: &ElementProperty) -> Option<CompactString> {
    match value {
        ElementProperty::String(text) => Some(text.clone()),
        ElementProperty::Number(number) => number
            .as_i64()
            .map(|int| compact_str::format_compact!("{int}")),
        _ => None,
    }
}

/// Validates the entries of a merge map and splits off the token keys.
pub(crate) fn parse_entries(
    entries: Vec<PropertyEntry>,
    kind: MergeKind,
    role: MergeRole,
) -> Result<ParsedMap, MergeProblem> {
    let invalid = |reason: String| MergeProblem::invalid(kind, role, reason);
    let mut parsed = ParsedMap::default();
    for entry in entries {
        let key = entry.key.as_str();
        if key.starts_with('~') {
            return Err(invalid(format!(
                "Property key can not be a hidden key: {key}"
            )));
        }
        let is_token = key == KEY_ID || key == KEY_LABEL || key == KEY_OUT || key == KEY_IN;
        if is_token && role == MergeRole::OnMatch {
            return Err(invalid(
                "option(onMatch) cannot change T.id, T.label or the edge's vertices of an existing element".to_string(),
            ));
        }
        match key {
            KEY_ID => match id_text(&entry.value) {
                Some(id) => parsed.id = Some(id),
                None => {
                    return Err(invalid(format!(
                        "T.id must be a string or an integer, got {}",
                        type_name(&entry.value)
                    )));
                }
            },
            KEY_LABEL => match &entry.value {
                ElementProperty::String(label) if label.starts_with('~') => {
                    return Err(invalid(format!("Label can not be a hidden key: {label}")));
                }
                ElementProperty::String(label) if !label.is_empty() => {
                    parsed.label = Some(label.clone())
                }
                other => {
                    return Err(invalid(format!(
                        "T.label must be a non-empty string, got {}",
                        type_name(other)
                    )));
                }
            },
            KEY_OUT | KEY_IN => {
                if kind == MergeKind::Vertex {
                    return Err(invalid(format!(
                        "merge_v() does not accept Direction keys ({key}); they name the vertices of an edge in merge_e()"
                    )));
                }
                if role == MergeRole::VertexLookup {
                    return Err(invalid(format!(
                        "a vertex lookup map does not accept Direction keys ({key})"
                    )));
                }
                let (own, other) = if key == KEY_OUT {
                    (PLACEHOLDER_OUT, PLACEHOLDER_IN)
                } else {
                    (PLACEHOLDER_IN, PLACEHOLDER_OUT)
                };
                let endpoint = match &entry.value {
                    ElementProperty::String(text) if text == own => EndpointRef::Placeholder,
                    ElementProperty::String(text) if text == other => {
                        return Err(invalid(format!(
                            "{key} cannot take the placeholder {other}; use {own}"
                        )));
                    }
                    ElementProperty::Vertex(vertex) => {
                        EndpointRef::Id(CompactString::new(vertex.id()))
                    }
                    value => match id_text(value) {
                        Some(id) => EndpointRef::Id(id),
                        None => {
                            return Err(invalid(format!(
                                "{key} must be a vertex, a vertex id or {own}, got {}",
                                type_name(value)
                            )));
                        }
                    },
                };
                if key == KEY_OUT {
                    parsed.out_v = Some(endpoint);
                } else {
                    parsed.in_v = Some(endpoint);
                }
            }
            _ if key.starts_with("T.") || key.starts_with("Direction.") => {
                return Err(invalid(format!(
                    "unsupported token key \"{key}\": a merge map takes only the token keys \"T.id\", \"T.label\", \"Direction.OUT\" and \"Direction.IN\""
                )));
            }
            _ => {
                if let (MergeKind::Edge, Some(cardinality)) = (kind, entry.cardinality) {
                    return Err(MergeProblem::EdgeCardinality { kind, cardinality });
                }
                parsed.entries.push(entry);
            }
        }
    }
    Ok(parsed)
}

/// The onCreate override rule: a key present in both maps must have the same value.
pub(crate) fn check_override(
    kind: MergeKind,
    merge: &ParsedMap,
    on_create: &ParsedMap,
) -> Result<(), MergeProblem> {
    let conflict =
        |key: &str, merge_value: String, on_create_value: String| MergeProblem::Override {
            kind,
            key: key.to_string(),
            merge_value,
            on_create_value,
        };
    let quoted = |text: &str| format!("\"{text}\"");
    if let (Some(a), Some(b)) = (&merge.id, &on_create.id)
        && a != b
    {
        return Err(conflict(KEY_ID, quoted(a), quoted(b)));
    }
    if let (Some(a), Some(b)) = (&merge.label, &on_create.label)
        && a != b
    {
        return Err(conflict(KEY_LABEL, quoted(a), quoted(b)));
    }
    for (key, a, b, placeholder) in [
        (KEY_OUT, &merge.out_v, &on_create.out_v, PLACEHOLDER_OUT),
        (KEY_IN, &merge.in_v, &on_create.in_v, PLACEHOLDER_IN),
    ] {
        if let (Some(a), Some(b)) = (a, b)
            && a != b
        {
            let render = |endpoint: &EndpointRef| match endpoint {
                EndpointRef::Id(id) => quoted(id),
                EndpointRef::Placeholder => placeholder.to_string(),
            };
            return Err(conflict(key, render(a), render(b)));
        }
    }
    for entry in &on_create.entries {
        if let Some(own) = merge.entries.iter().find(|own| own.key == entry.key)
            && own.value != entry.value
        {
            return Err(conflict(
                &entry.key,
                crate::value::literal::rhai_literal(&own.value),
                crate::value::literal::rhai_literal(&entry.value),
            ));
        }
    }
    Ok(())
}

/// How a merge lookup reaches its candidates, shown in the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// By `T.id`, through the id index.
    Id,
    /// By `T.label`, through the label index.
    Label,
    /// Every element.
    Scan,
    /// The out-edges of the resolved out-vertex (`mergeE()`).
    OutEdges,
    /// The in-edges of the resolved in-vertex (`mergeE()`).
    InEdges,
}

impl Lookup {
    /// The strategy [`find_vertices`] uses for `map`.
    pub(crate) fn of(map: &ParsedMap) -> Self {
        if map.id.is_some() {
            Lookup::Id
        } else if map.label.is_some() {
            Lookup::Label
        } else {
            Lookup::Scan
        }
    }

    /// The strategy [`find_edges`] uses for `map`: an endpoint's adjacency first, then the id
    /// and label indexes.
    pub(crate) fn of_edge(map: &ParsedMap) -> Self {
        if map.out_v.is_some() {
            Lookup::OutEdges
        } else if map.in_v.is_some() {
            Lookup::InEdges
        } else {
            Lookup::of(map)
        }
    }
}

impl fmt::Display for Lookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Lookup::Id => "id",
            Lookup::Label => "label",
            Lookup::Scan => "scan",
            Lookup::OutEdges => "out-edges",
            Lookup::InEdges => "in-edges",
        })
    }
}

/// The vertices matching `map`: the id, label and every property entry must match, with the
/// equality of `has(key, value)`. `T.id` uses the id index, else `T.label` the label index,
/// else every vertex is scanned.
pub(crate) fn find_vertices<S: GraphStorage>(
    storage: &S,
    map: &ParsedMap,
) -> Result<Vec<S::VertexId>, GraphError> {
    let matches = |node: S::VertexId| -> Result<bool, GraphError> {
        if let Some(label) = map.label.as_deref()
            && !storage.vertex_has_label(node, label)?
        {
            return Ok(false);
        }
        for entry in &map.entries {
            if !vertex_has(storage, node, &entry.key, &entry.value)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    let mut found = Vec::new();
    let mut keep = |node: S::VertexId| -> Result<(), GraphError> {
        if matches(node)? {
            found.push(node);
        }
        Ok(())
    };
    match Lookup::of(map) {
        Lookup::Id => {
            if let Some(node) = storage.get_vertex_by_id(map.id.as_deref().unwrap_or_default())? {
                keep(node)?;
            }
        }
        Lookup::Label => {
            for node in storage.vertices_by_label(map.label.as_deref().unwrap_or_default())? {
                keep(node)?;
            }
        }
        // `Lookup::of` never names an edge adjacency for a vertex search.
        Lookup::Scan | Lookup::OutEdges | Lookup::InEdges => {
            for node in storage.vertices()? {
                keep(node)?;
            }
        }
    }
    Ok(found)
}

/// An edge endpoint after resolution: a stored vertex, or an id that names no vertex (which
/// matches no edge and fails only when an edge would be created).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Endpoint<V> {
    Vertex(V),
    Unknown(CompactString),
}

/// The edges matching `map`, whose endpoints are already resolved to `out_v` / `in_v`: the
/// id, label, both endpoints and every property entry must match, with the equality of
/// `has(key, value)`. The out-vertex's out-edges are searched first, then the in-vertex's
/// in-edges, the id index, the label index, and finally every edge ([`Lookup::of_edge`]).
pub(crate) fn find_edges<S: GraphStorage>(
    storage: &S,
    map: &ParsedMap,
    out_v: Option<&Endpoint<S::VertexId>>,
    in_v: Option<&Endpoint<S::VertexId>>,
) -> Result<Vec<S::EdgeId>, GraphError> {
    let vertex = |endpoint: Option<&Endpoint<S::VertexId>>| match endpoint {
        Some(Endpoint::Vertex(node)) => Ok(Some(*node)),
        // An id naming no vertex has no edges.
        Some(Endpoint::Unknown(_)) => Err(()),
        None => Ok(None),
    };
    let (Ok(out_v), Ok(in_v)) = (vertex(out_v), vertex(in_v)) else {
        return Ok(Vec::new());
    };
    let matches = |edge: S::EdgeId| -> Result<bool, GraphError> {
        let endpoints = storage.edge_endpoints(edge)?;
        if let Some(id) = map.id.as_deref()
            && storage.edge_id(edge)? != Some(id)
        {
            return Ok(false);
        }
        if let Some(label) = map.label.as_deref()
            && storage.edge_label(edge)? != Some(label)
        {
            return Ok(false);
        }
        if !(out_v.is_none_or(|node| endpoints.is_some_and(|(from, _)| from == node))
            && in_v.is_none_or(|node| endpoints.is_some_and(|(_, to)| to == node)))
        {
            return Ok(false);
        }
        for entry in &map.entries {
            if !edge_has(storage, edge, &entry.key, &entry.value)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    let mut found = Vec::new();
    let mut keep = |edge: S::EdgeId| -> Result<(), GraphError> {
        if matches(edge)? {
            found.push(edge);
        }
        Ok(())
    };
    match (out_v, in_v, Lookup::of_edge(map)) {
        (Some(node), _, _) => {
            for edge in storage.out_edges(node)? {
                keep(edge)?;
            }
        }
        (None, Some(node), _) => {
            for edge in storage.in_edges(node)? {
                keep(edge)?;
            }
        }
        (None, None, Lookup::Id) => {
            if let Some(edge) = storage.get_edge_by_id(map.id.as_deref().unwrap_or_default())? {
                keep(edge)?;
            }
        }
        (None, None, Lookup::Label) => {
            for edge in storage.edges_by_label(map.label.as_deref().unwrap_or_default())? {
                keep(edge)?;
            }
        }
        (None, None, Lookup::Scan | Lookup::OutEdges | Lookup::InEdges) => {
            for edge in storage.edges()? {
                keep(edge)?;
            }
        }
    }
    Ok(found)
}

/// Creates the edge `map` describes from `out_v` to `in_v`, in one storage call. Edge
/// properties have no cardinality (rejected when the map was parsed).
pub(crate) fn create_edge<S: GraphStorage>(
    storage: &mut S,
    map: &ParsedMap,
    out_v: S::VertexId,
    in_v: S::VertexId,
    create: &crate::graph::schema::defaults::CreateContext,
) -> TraverserResult<S::EdgeId> {
    let properties: PropertyMap = map
        .entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect();
    Ok(storage.add_edge_with(
        map.id.clone(),
        map.label.clone(),
        out_v,
        in_v,
        properties,
        create,
    )?)
}

/// Writes `entries` to an existing element (onMatch), each with its own cardinality or
/// `default`; list/set fail with [`TraverserError::UnsupportedCardinality`] before any write.
pub(crate) fn apply_entries<S: GraphStorage>(
    storage: &mut S,
    target: WriteTarget<S>,
    entries: &[PropertyEntry],
    default: Option<Cardinality>,
    kind: MergeKind,
) -> TraverserResult<()> {
    write_entries(storage, target, entries, default, kind.step())
}

/// Creates the vertex `map` describes, in one storage call. List/set cardinalities fail before
/// the vertex is created.
pub(crate) fn create_vertex<S: GraphStorage>(
    storage: &mut S,
    map: &ParsedMap,
    create: &crate::graph::schema::defaults::CreateContext,
) -> TraverserResult<S::VertexId> {
    for entry in &map.entries {
        check_cardinality(MergeKind::Vertex.step(), entry.cardinality, false)?;
    }
    let properties: PropertyMap = map
        .entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect();
    let labels = map.label.iter().cloned().collect();
    Ok(storage.add_vertex_with(map.id.clone(), labels, properties, create)?)
}

/// A merge argument or option as handed to the builder, before the step parses it for its
/// element kind and the map's role.
pub(crate) enum MergeInput<'g, S: GraphStorage + 'g> {
    /// A map value (`ElementProperty::Object` or `Null`) from the Rust API.
    Value(ElementProperty),
    /// Map entries with per-entry cardinality (the DSL's `Cardinality.single(v)` values).
    Entries(Vec<PropertyEntry>),
    /// A traversal whose first result is the map.
    Traversal(AnonymousTraversal<'g, S>),
}

impl<'g, S: GraphStorage + 'g> MergeInput<'g, S> {
    /// Parses the input for `kind`/`role`: a constant becomes a [`ParsedMap`] (or the problem
    /// with it, which the step raises before execution); a traversal stays as is.
    pub(crate) fn into_arg(
        self,
        kind: MergeKind,
        role: MergeRole,
        default: Option<Cardinality>,
    ) -> (MergeArg<'g, S>, Option<MergeProblem>) {
        // What the step renders when the constant is rejected: the argument as written.
        let (parsed, written): (_, Box<dyn FnOnce() -> String>) = match self {
            MergeInput::Traversal(traversal) => return (MergeArg::Traversal(traversal), None),
            MergeInput::Value(value) => {
                let parsed = parse(&value, kind, role);
                (
                    parsed,
                    Box::new(move || crate::value::literal::rhai_literal(&value)),
                )
            }
            MergeInput::Entries(entries) => {
                let as_written = ParsedMap {
                    entries: entries.clone(),
                    ..ParsedMap::default()
                };
                (
                    parse_entries(entries, kind, role),
                    Box::new(move || as_written.to_string()),
                )
            }
        };
        let parsed = parsed.and_then(|map| {
            let map = map.with_default_cardinality(default);
            if kind == MergeKind::Edge
                && let Some(cardinality) = default
            {
                return Err(MergeProblem::EdgeCardinality { kind, cardinality });
            }
            Ok(map)
        });
        match parsed {
            Ok(map) => (MergeArg::Constant(map), None),
            Err(problem) => (
                MergeArg::Constant(ParsedMap {
                    rejected: Some(written()),
                    ..ParsedMap::default()
                }),
                Some(problem),
            ),
        }
    }
}

/// A merge step that `option(Merge.x, ..)` attaches to (`mergeV()`, `mergeE()`).
pub(crate) trait MergeStep<'g, S: GraphStorage + 'g> {
    /// Sets the `token` option; a second option with the same token replaces the first, as in
    /// TinkerPop. A token the step does not take is recorded as a problem raised before
    /// execution.
    fn set_option(
        &mut self,
        token: Merge,
        input: MergeInput<'g, S>,
        cardinality: Option<Cardinality>,
    );
}

/// An `option(Merge.x, ..)` of a merge step.
pub(crate) struct MergeOption<'g, S: GraphStorage + 'g> {
    pub(crate) arg: MergeArg<'g, S>,
    /// The default cardinality of `option(token, map, Cardinality)`.
    pub(crate) cardinality: Option<Cardinality>,
}

impl<'g, S: GraphStorage + 'g> Clone for MergeOption<'g, S> {
    fn clone(&self) -> Self {
        Self {
            arg: self.arg.clone(),
            cardinality: self.cardinality,
        }
    }
}

/// Evaluates a map traversal on `traverser`: its first result must be a map, parsed for
/// `kind`/`role`; `None` when the traversal produces nothing.
pub(crate) fn evaluate_map<'g, S: GraphStorage + 'g>(
    traversal: &AnonymousTraversal<'g, S>,
    context: &mut TraverserContext<'_, S>,
    traverser: &Traverser<S>,
    kind: MergeKind,
    role: MergeRole,
) -> TraverserResult<Option<ParsedMap>> {
    match traversal.first(context, traverser)? {
        Some(first) => {
            let value = first.value.into_node_property(context.storage)?;
            Ok(Some(parse(&value, kind, role)?))
        }
        None => Ok(None),
    }
}

/// The merge map of a merge step for `traverser`: the constant, or the first result of the
/// traversal ("<step>() traversal produced no map" when there is none).
pub(crate) fn merge_map<'a, 'g, S: GraphStorage + 'g>(
    merge: &'a MergeArg<'g, S>,
    context: &mut TraverserContext<'_, S>,
    traverser: &Traverser<S>,
    kind: MergeKind,
) -> TraverserResult<Cow<'a, ParsedMap>> {
    match merge {
        MergeArg::Constant(map) => Ok(Cow::Borrowed(map)),
        MergeArg::Traversal(traversal) => {
            match evaluate_map(traversal, context, traverser, kind, MergeRole::Merge)? {
                Some(map) => Ok(Cow::Owned(map)),
                None => Err(MergeProblem::invalid(
                    kind,
                    MergeRole::Merge,
                    format!("{}() traversal produced no map", kind.step()),
                )
                .into()),
            }
        }
    }
}

/// The map of an onCreate/onMatch option for `traverser`; `None` when there is no option or
/// its traversal produced nothing.
pub(crate) fn option_map<'a, 'g, S: GraphStorage + 'g>(
    option: &'a Option<MergeOption<'g, S>>,
    context: &mut TraverserContext<'_, S>,
    traverser: &Traverser<S>,
    kind: MergeKind,
    role: MergeRole,
) -> TraverserResult<Option<Cow<'a, ParsedMap>>> {
    let Some(option) = option else {
        return Ok(None);
    };
    match &option.arg {
        MergeArg::Constant(map) => Ok(Some(Cow::Borrowed(map))),
        MergeArg::Traversal(traversal) => {
            Ok(evaluate_map(traversal, context, traverser, kind, role)?
                .map(|map| Cow::Owned(map.with_default_cardinality(option.cardinality))))
        }
    }
}

/// The first build-time problem of a merge step (in the order the query was written), else
/// the eager override check of two constant maps, as in TinkerPop.
pub(crate) fn first_problem<'g, S: GraphStorage + 'g>(
    kind: MergeKind,
    problems: &[&Option<MergeProblem>],
    merge: &MergeArg<'g, S>,
    on_create: &Option<MergeOption<'g, S>>,
) -> Option<MergeProblem> {
    if let Some(problem) = problems.iter().find_map(|problem| problem.as_ref()) {
        return Some(problem.clone());
    }
    if let (
        MergeArg::Constant(merge),
        Some(MergeOption {
            arg: MergeArg::Constant(on_create),
            ..
        }),
    ) = (merge, on_create)
    {
        return check_override(kind, merge, on_create).err();
    }
    None
}

/// Attaches `option(token, ..)` to the merge step that is the last step of `steps` (only
/// modulators and `as()` labels may follow it); pushes the option-without-branch placeholder
/// otherwise, which fails with [`TraverserError::OptionWithoutBranch`].
pub(crate) fn attach_merge_option<'g, S: GraphStorage + 'g>(
    steps: &mut Vec<GraphStep<'g, S>>,
    token: Merge,
    input: MergeInput<'g, S>,
    cardinality: Option<Cardinality>,
) {
    let target = steps
        .iter_mut()
        .rev()
        .find(|step| !matches!(step, GraphStep::Modulator(_, _) | GraphStep::As(_, _)));
    if let Some(GraphStep::SideEffectMut(_, step)) = target
        && let Some(merge) = step.as_merge_mut()
    {
        merge.set_option(token, input, cardinality);
        return;
    }
    let rendered = match &input {
        MergeInput::Value(value) => format!(
            "option(Merge.{token}, {})",
            crate::value::literal::rhai_literal(value)
        ),
        MergeInput::Entries(entries) => {
            let map = ParsedMap {
                entries: entries.clone(),
                ..ParsedMap::default()
            };
            format!("option(Merge.{token}, {map})")
        }
        MergeInput::Traversal(traversal) => format!("option(Merge.{token}, {traversal})"),
    };
    steps.push(option_without_branch(rendered));
}
