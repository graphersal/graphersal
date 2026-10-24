//! The Graph panel's filter: which vertices and edges a drawing shows.
//!
//! A filter is JSON (`engine.js` `GraphFilter`):
//!
//! ```json
//! { "vertices": { "person": [ {"key": "age", "op": "gt", "value": 30} ], "software": [] },
//!   "edges":    { "knows": [ {"key": "weight", "op": "gte", "value": 0.5} ] },
//!   "hideIsolated": false }
//! ```
//!
//! - `vertices` / `edges` map a label to a list of conditions. A listed label is visible, an
//!   unlisted one hidden; the field absent (or `null`) shows every label without conditions.
//!   The conditions of one label must all hold (AND); an element is shown when its label is
//!   listed and that label's conditions hold (labels are OR). A vertex with several labels is
//!   shown when ANY of its labels is listed with conditions that hold (the engine's
//!   `has_label` sees the whole label set).
//! - A hidden vertex hides its edges. A hidden edge hides no vertex, except with
//!   `hideIsolated: true`: a vertex without a visible edge is hidden too (applied after the edge
//!   filter, before any drawing limit).
//! - `op`: `eq`, `neq`, `lt`, `lte`, `gt`, `gte`, `between` (`value: [low, high]`, low
//!   inclusive, high exclusive: the engine's `P.between`), `contains`, `starts_with`,
//!   `ends_with`, `regex` (the engine's `TextP.regex`, Rust `regex` dialect, unanchored) take a
//!   `value`; `exists` / `missing` (the key is present / absent) take none. The text operators
//!   take a string.
//! - `value` is taken literally (a number stays a number, a string a string; never sniffed). When
//!   the graph's schema (mode `open`/`closed`) declares the key for that label, the value is
//!   coerced like a write would store it ([`GraphSchema::coerce_value`]): `"2024-01-01"`
//!   compares as a date for a key declared `date`.
//!
//! The filter is evaluated by the engine itself: one read-only traversal per listed label with
//! conditions (`g.v().has_label(L).has(k, P)...`, so property indexes and the label index serve
//! it like the equivalent query), a label without conditions is read from the label index.
//! [`filter_query`] writes that equivalent query as Rhai text. Every check runs against a
//! read-only view (`&S`): a filter can never write.

use std::collections::{BTreeMap, HashSet};

use graphersal::error::format_diagnostic;
use graphersal::prelude::*;
use graphersal::storage::ElementKind;
use serde_json::Value;

/// One condition of a label, compiled: the property key and its test.
#[derive(Debug, Clone)]
struct Condition {
    key: String,
    test: Test,
}

/// What a condition checks.
#[derive(Debug, Clone)]
enum Test {
    /// The key is present.
    Exists,
    /// The key is absent.
    Missing,
    /// The key's value satisfies the predicate.
    Pred(P<PValue>),
}

/// Labels with their conditions; `BTreeMap`: labels in name order.
type LabelConditions = BTreeMap<String, Vec<Condition>>;

/// A checked, compiled filter (see the module docs). Build it with [`GraphFilter::compile`].
#[derive(Debug, Clone, Default)]
pub struct GraphFilter {
    vertices: Option<LabelConditions>,
    edges: Option<LabelConditions>,
    hide_isolated: bool,
}

/// The operators a condition may name.
const OPS: &str = "eq, neq, lt, lte, gt, gte, between, contains, starts_with, ends_with, regex, \
                   exists, missing";

impl GraphFilter {
    /// Checks and compiles the JSON filter `spec` against the schema of `g` (for the coercion
    /// of values of declared keys). An error names the label, key and operator at fault.
    pub fn compile<S: GraphStorage>(g: &S, spec: &Value) -> Result<GraphFilter, String> {
        let Value::Object(fields) = spec else {
            return Err("the filter is a JSON object {vertices?, edges?, hideIsolated?}".into());
        };
        for key in fields.keys() {
            if !matches!(key.as_str(), "vertices" | "edges" | "hideIsolated") {
                return Err(format!(
                    "unknown filter field \"{key}\" (expected vertices, edges, hideIsolated)"
                ));
            }
        }
        let schema = g.get_schema();
        let hide_isolated = match fields.get("hideIsolated") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("filter: hideIsolated is true or false".into()),
        };
        Ok(GraphFilter {
            vertices: labels_of(fields.get("vertices"), ElementKind::Vertex, schema.as_ref())?,
            edges: labels_of(fields.get("edges"), ElementKind::Edge, schema.as_ref())?,
            hide_isolated,
        })
    }

    /// Whether the filter hides nothing (no field, no `hideIsolated`).
    pub fn is_empty(&self) -> bool {
        self.vertices.is_none() && self.edges.is_none() && !self.hide_isolated
    }

    /// Whether vertices without a visible edge are hidden.
    pub fn hide_isolated(&self) -> bool {
        self.hide_isolated
    }

    /// Whether the filter restricts vertices.
    pub fn filters_vertices(&self) -> bool {
        self.vertices.is_some()
    }

    /// Whether the filter restricts edges by label/conditions.
    pub fn filters_edges(&self) -> bool {
        self.edges.is_some()
    }

    /// The visible vertices: of the whole graph (`candidates` `None`; `Ok(None)` when the
    /// filter does not restrict vertices), or the visible ones among `candidates`, in their
    /// order. Labels in name order, each in the engine's order, without duplicates.
    pub fn vertices<S: GraphStorage>(
        &self,
        g: &S,
        candidates: Option<&[S::VertexId]>,
    ) -> Result<Option<Vec<S::VertexId>>, String> {
        let Some(labels) = &self.vertices else {
            return Ok(None);
        };
        let ids = candidates.map(|c| {
            c.iter()
                .filter_map(|v| g.vertex_id(*v))
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (label, conditions) in labels {
            let found: Vec<S::VertexId> = if conditions.is_empty() {
                match candidates {
                    None => g.vertices_by_label(label).collect(),
                    Some(c) => c
                        .iter()
                        .copied()
                        .filter(|v| g.vertex_has_label(*v, label))
                        .collect(),
                }
            } else {
                if ids.as_ref().is_some_and(Vec::is_empty) {
                    continue;
                }
                let start = GraphTraversalSource::new(g);
                let start = match &ids {
                    None => start.v(None::<()>),
                    Some(ids) => start.v(ids.clone()),
                };
                let traversal = with_conditions(start.has_label(label.as_str()), conditions);
                element_ids(traversal.id().to_list(), label, ElementKind::Vertex)?
                    .iter()
                    .filter_map(|id| g.get_vertex_by_id(id))
                    .collect()
            };
            out.extend(found.into_iter().filter(|v| seen.insert(*v)));
        }
        if let Some(c) = candidates {
            // Keep the candidates' order (the drawing's breadth-first order).
            let visible: HashSet<_> = out.into_iter().collect();
            return Ok(Some(
                c.iter().copied().filter(|v| visible.contains(v)).collect(),
            ));
        }
        Ok(Some(out))
    }

    /// The edges passing the edge filter (labels and conditions; endpoints are NOT checked
    /// here): of the whole graph (`candidates` `None`; `Ok(None)` when the filter does not
    /// restrict edges), or among `candidates`, in their order.
    pub fn edges<S: GraphStorage>(
        &self,
        g: &S,
        candidates: Option<&[S::EdgeId]>,
    ) -> Result<Option<Vec<S::EdgeId>>, String> {
        let Some(labels) = &self.edges else {
            return Ok(None);
        };
        let ids = candidates.map(|c| {
            c.iter()
                .filter_map(|e| g.edge_id(*e))
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (label, conditions) in labels {
            let found: Vec<S::EdgeId> = if conditions.is_empty() {
                match candidates {
                    None => g.edges_by_label(label).collect(),
                    Some(c) => c
                        .iter()
                        .copied()
                        .filter(|e| g.edge_label(*e) == Some(label.as_str()))
                        .collect(),
                }
            } else {
                if ids.as_ref().is_some_and(Vec::is_empty) {
                    continue;
                }
                let start = GraphTraversalSource::new(g);
                let start = match &ids {
                    None => start.e(None::<()>),
                    Some(ids) => start.e(ids.clone()),
                };
                let traversal = with_conditions(start.has_label(label.as_str()), conditions);
                element_ids(traversal.id().to_list(), label, ElementKind::Edge)?
                    .iter()
                    .filter_map(|id| g.get_edge_by_id(id))
                    .collect()
            };
            out.extend(found.into_iter().filter(|e| seen.insert(*e)));
        }
        if let Some(c) = candidates {
            let visible: HashSet<_> = out.into_iter().collect();
            return Ok(Some(
                c.iter().copied().filter(|e| visible.contains(e)).collect(),
            ));
        }
        Ok(Some(out))
    }
}

/// `traversal` with one `has` step per condition.
fn with_conditions<'g, S: GraphStorage + 'g>(
    mut traversal: GraphTraversalSource<'g, S>,
    conditions: &[Condition],
) -> GraphTraversalSource<'g, S> {
    for condition in conditions {
        traversal = match &condition.test {
            Test::Exists => traversal.has_property(condition.key.as_str()),
            Test::Missing => traversal.has_not(condition.key.as_str()),
            Test::Pred(p) => traversal.has_p(condition.key.as_str(), p.clone()),
        };
    }
    traversal
}

/// The ids a filter traversal returned, or its error naming the label.
fn element_ids(
    result: Result<Vec<ElementProperty>, graphersal::error::BoxedTraverserError>,
    label: &str,
    kind: ElementKind,
) -> Result<Vec<String>, String> {
    let ids = result.map_err(|err| {
        format!(
            "filter: {} label \"{label}\": {}",
            kind_name(kind),
            format_diagnostic(&*err)
        )
    })?;
    Ok(ids
        .iter()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect())
}

fn kind_name(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::Vertex => "vertex",
        ElementKind::Edge => "edge",
    }
}

fn field_name(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::Vertex => "vertices",
        ElementKind::Edge => "edges",
    }
}

/// The labels of `vertices` / `edges`: `None` when the field is absent or `null`.
fn labels_of(
    field: Option<&Value>,
    kind: ElementKind,
    schema: Option<&GraphSchema>,
) -> Result<Option<LabelConditions>, String> {
    let map = match field {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(format!(
                "filter: {} is an object (label -> list of conditions)",
                field_name(kind)
            ));
        }
    };
    let mut labels = LabelConditions::new();
    for (label, conditions) in map {
        let list = match conditions {
            Value::Null => &Vec::new(),
            Value::Array(list) => list,
            _ => {
                return Err(format!(
                    "filter: {}.{label} is a list of conditions",
                    field_name(kind)
                ));
            }
        };
        let compiled = list
            .iter()
            .enumerate()
            .map(|(i, c)| condition_of(c, kind, label, i, schema))
            .collect::<Result<Vec<_>, _>>()?;
        labels.insert(label.clone(), compiled);
    }
    Ok(Some(labels))
}

/// One condition `{key, op, value?}`; `index` is its position in the label's list.
fn condition_of(
    spec: &Value,
    kind: ElementKind,
    label: &str,
    index: usize,
    schema: Option<&GraphSchema>,
) -> Result<Condition, String> {
    let at = format!("filter: {}.{label}[{index}]", field_name(kind));
    let Value::Object(fields) = spec else {
        return Err(format!(
            "{at}: a condition is an object {{key, op, value?}}"
        ));
    };
    for name in fields.keys() {
        if !matches!(name.as_str(), "key" | "op" | "value") {
            return Err(format!(
                "{at}: unknown field \"{name}\" (expected key, op, value)"
            ));
        }
    }
    let key = match fields.get("key") {
        Some(Value::String(key)) if !key.is_empty() => key.clone(),
        _ => return Err(format!("{at}: \"key\" is a non-empty property name")),
    };
    let op = match fields.get("op") {
        Some(Value::String(op)) => op.as_str(),
        _ => return Err(format!("{at} (key \"{key}\"): \"op\" is one of {OPS}")),
    };
    let at = format!("{at} (key \"{key}\", op \"{op}\")");
    let value = fields.get("value");
    let needs_value = |value: Option<&Value>| -> Result<Value, String> {
        value
            .cloned()
            .ok_or_else(|| format!("{at}: this operator needs a \"value\""))
    };
    let scalar = |value: &Value| -> Result<PValue, String> {
        let property = json_scalar(value).map_err(|e| format!("{at}: {e}"))?;
        let property = match schema {
            Some(schema) => schema
                .coerce_value(kind, &[CompactString::from(label)], &key, property)
                .map_err(|e| format!("{at}: {}", format_diagnostic(&e)))?,
            None => property,
        };
        pvalue_of(property).map_err(|e| format!("{at}: {e}"))
    };
    let text = |value: &Value| -> Result<String, String> {
        match value {
            Value::String(s) => Ok(s.clone()),
            _ => Err(format!("{at}: the value of a text operator is a string")),
        }
    };
    let test = match op {
        "exists" | "missing" => {
            if value.is_some_and(|v| !v.is_null()) {
                return Err(format!("{at}: this operator takes no \"value\""));
            }
            if op == "exists" {
                Test::Exists
            } else {
                Test::Missing
            }
        }
        "eq" => Test::Pred(P::Eq(scalar(&needs_value(value)?)?)),
        "neq" => Test::Pred(P::Neq(scalar(&needs_value(value)?)?)),
        "lt" => Test::Pred(P::Lt(scalar(&needs_value(value)?)?)),
        "lte" => Test::Pred(P::Lte(scalar(&needs_value(value)?)?)),
        "gt" => Test::Pred(P::Gt(scalar(&needs_value(value)?)?)),
        "gte" => Test::Pred(P::Gte(scalar(&needs_value(value)?)?)),
        "between" => match needs_value(value)? {
            Value::Array(bounds) if bounds.len() == 2 => {
                Test::Pred(P::Between(scalar(&bounds[0])?, scalar(&bounds[1])?))
            }
            _ => return Err(format!("{at}: the value of between is [low, high]")),
        },
        "contains" => Test::Pred(P::Containing(PValue::from(text(&needs_value(value)?)?))),
        "starts_with" => Test::Pred(P::StartingWith(PValue::from(text(&needs_value(value)?)?))),
        "ends_with" => Test::Pred(P::EndingWith(PValue::from(text(&needs_value(value)?)?))),
        "regex" => {
            let pattern = text(&needs_value(value)?)?;
            Test::Pred(P::regex(&pattern).map_err(|e| format!("{at}: {}", format_diagnostic(&*e)))?)
        }
        other => {
            return Err(format!(
                "{at}: unknown operator \"{other}\" (expected one of {OPS})"
            ));
        }
    };
    Ok(Condition { key, test })
}

/// A JSON scalar as a value, literally: a number stays a number (an integer an `int64`, any
/// other a `float64`), a string a string.
fn json_scalar(value: &Value) -> Result<ElementProperty, String> {
    Ok(match value {
        Value::Null => ElementProperty::Null,
        Value::Bool(b) => ElementProperty::Boolean(*b),
        Value::String(s) => ElementProperty::from(s.as_str()),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => ElementProperty::Number(Number::from_i64(i)),
            (None, Some(f)) => ElementProperty::Number(Number::from_f64(f)),
            _ => return Err(format!("the number {n} is out of range")),
        },
        Value::Array(_) | Value::Object(_) => {
            return Err("the value is a string, number, boolean or null".into());
        }
    })
}

/// A (coerced) scalar as a predicate operand.
fn pvalue_of(value: ElementProperty) -> Result<PValue, String> {
    Ok(match value {
        ElementProperty::Null => PValue::Null,
        ElementProperty::Boolean(b) => PValue::Boolean(b),
        ElementProperty::Number(n) => PValue::Number(n),
        ElementProperty::String(s) => PValue::String(s),
        ElementProperty::Uuid(bits) => PValue::Uuid(bits),
        ElementProperty::Temporal(t) => PValue::Temporal(t),
        _ => return Err("the value cannot be compared (not a scalar)".into()),
    })
}

// ------------------------------------------------------------------------------ query text

/// The Rhai query that selects the vertices and edges `filter` shows (before any drawing
/// limit): `#{vertices: g.v()..., edges: g.e()...}`, every key and value a Rhai literal
/// (strings escaped). Values of declared keys are written coerced (`date("2024-01-01")`).
pub fn filter_query(filter: &GraphFilter) -> String {
    let vertex_pred = filter.vertices.as_ref().map(predicate_text);
    let edge_pred = filter.edges.as_ref().map(predicate_text);
    let mut vertices = String::from("g.v()");
    if let Some(p) = &vertex_pred {
        vertices.push_str(&p.inline);
    }
    let mut edges = String::from("g.e()");
    if let Some(p) = &edge_pred {
        edges.push_str(&p.inline);
    }
    if let Some(p) = &vertex_pred {
        edges.push_str(&format!(
            ".filter(__.out_v(){}).filter(__.in_v(){})",
            p.inline, p.inline
        ));
    }
    if filter.hide_isolated {
        let mut visible_edge = String::from("__.both_e()");
        if let Some(p) = &edge_pred {
            visible_edge.push_str(&p.inline);
        }
        visible_edge.push_str(".other_v()");
        if let Some(p) = &vertex_pred {
            visible_edge.push_str(&p.inline);
        }
        vertices.push_str(&format!(".filter({visible_edge})"));
    }
    format!("#{{\n  vertices: {vertices}.to_list(),\n  edges: {edges}.to_list()\n}}")
}

/// The steps that keep an element of the listed labels whose conditions hold, appended to a
/// traversal (`inline`, starting with `.`).
struct PredicateText {
    inline: String,
}

fn predicate_text(labels: &LabelConditions) -> PredicateText {
    if labels.is_empty() {
        return PredicateText {
            inline: ".limit(0)".into(),
        };
    }
    let plain = labels.values().all(Vec::is_empty);
    let names: Vec<String> = labels
        .keys()
        .map(|l| crate::queries::rhai_string(l))
        .collect();
    if plain {
        return PredicateText {
            inline: format!(".has_label({})", names.join(", ")),
        };
    }
    let branch = |label: &str, conditions: &[Condition]| {
        let mut out = format!(".has_label({})", crate::queries::rhai_string(label));
        for c in conditions {
            out.push_str(&condition_text(c));
        }
        out
    };
    if labels.len() == 1 {
        let (label, conditions) = labels
            .iter()
            .next()
            .map(|(l, c)| (l.as_str(), c.as_slice()))
            .unwrap_or_default();
        return PredicateText {
            inline: branch(label, conditions),
        };
    }
    let branches: Vec<String> = labels
        .iter()
        .map(|(label, conditions)| format!("__{}", branch(label, conditions)))
        .collect();
    PredicateText {
        inline: format!(".or({})", branches.join(", ")),
    }
}

fn condition_text(condition: &Condition) -> String {
    let key = crate::queries::rhai_string(&condition.key);
    match &condition.test {
        Test::Exists => format!(".has({key})"),
        Test::Missing => format!(".has_not({key})"),
        Test::Pred(p) => format!(".has({key}, {})", predicate_literal(p)),
    }
}

fn predicate_literal(p: &P<PValue>) -> String {
    let one = |name: &str, v: &PValue| format!("{name}({})", pvalue_literal(v));
    match p {
        P::Eq(v) => one("P.eq", v),
        P::Neq(v) => one("P.neq", v),
        P::Lt(v) => one("P.lt", v),
        P::Lte(v) => one("P.lte", v),
        P::Gt(v) => one("P.gt", v),
        P::Gte(v) => one("P.gte", v),
        P::Between(a, b) => format!("P.between({}, {})", pvalue_literal(a), pvalue_literal(b)),
        P::Containing(v) => one("TextP.containing", v),
        P::StartingWith(v) => one("TextP.starting_with", v),
        P::EndingWith(v) => one("TextP.ending_with", v),
        P::Regex(r) => format!("TextP.regex({})", crate::queries::rhai_string(r.as_str())),
        // Never built by a filter.
        _ => "P.eq(())".into(),
    }
}

/// The Rhai literal of a predicate operand.
fn pvalue_literal(value: &PValue) -> String {
    match value {
        PValue::Null => "()".into(),
        PValue::Boolean(b) => b.to_string(),
        PValue::String(s) => crate::queries::rhai_string(s),
        PValue::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.is_finite() => {
                let abs = f.abs();
                let mut text = if abs != 0.0 && !(1e-6..1e16).contains(&abs) {
                    format!("{f:e}")
                } else {
                    format!("{f}")
                };
                if !text.contains(['.', 'e']) {
                    text.push_str(".0");
                }
                text
            }
            _ => "()".into(),
        },
        PValue::Uuid(bits) => format!(
            "UUID.from_string({})",
            crate::queries::rhai_string(&uuid_text(*bits))
        ),
        PValue::Temporal(t) => format!(
            "{}({})",
            t.kind().name(),
            crate::queries::rhai_string(&t.to_string())
        ),
        _ => "()".into(),
    }
}

/// The canonical 8-4-4-4-12 lowercase text of a UUID.
fn uuid_text(bits: u128) -> String {
    let hex = format!("{bits:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
