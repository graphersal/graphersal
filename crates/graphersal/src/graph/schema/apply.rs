//! Applying a schema to existing data: one O(N) scan that collects ALL violations, grouped by
//! rule, into a bounded, structured [`SchemaViolationReport`].
//!
//! The scan never stops at the first problem, also not inside a value: every nested violation
//! (all keys, all array items, at any depth) is recorded, through the same rule code a write
//! runs fail-fast (`schema/nested.rs`). It never collects per-record data: it keeps one
//! counter and at most [`SAMPLE_IDS`] truncated sample ids per group, and at most [`MAX_GROUPS`]
//! groups (further violations of new groups are only counted).

use crate::error::SchemaError;
use crate::graph::GraphStorage;
use crate::graph::schema::validation::{LabelRoot, join_labels};
use crate::graph::schema::{GraphSchema, SchemaMode, SchemaValidator};
use crate::value::json_path::wildcard_indices;
use crate::value::{ElementProperty, Number, PropertyMap};
use compact_str::CompactString;
use std::collections::HashMap;
use std::fmt;

/// Sample ids kept (and shown) per violation group.
pub const SAMPLE_IDS: usize = 5;
/// Characters of a sampled id kept before it is cut with an ellipsis.
pub const SAMPLE_ID_CHARS: usize = 40;
/// Violation groups the report keeps; a violation of a further group is only counted.
pub const MAX_GROUPS: usize = 1000;
/// Violation groups the [`Display`](fmt::Display) text prints.
pub const PRINTED_GROUPS: usize = 20;
/// The [`ViolationGroup::label`] of a vertex or edge without a label (`Closed` only).
pub const UNLABELED: &str = "<unlabeled>";

/// The rule a group of violations breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ViolationKind {
    /// A label is not declared in a `Closed` schema (or the element has no label at all).
    UnknownLabel,
    /// A declared non-nullable property is absent (or null).
    MissingRequired,
    /// A stored value's logical type differs from the declared type.
    TypeMismatch,
    /// A declared constraint (`min`/`max`/`pattern`/`min_items`/...) is violated.
    ConstraintViolation,
    /// A property is not declared (`Closed`).
    UndeclaredProperty,
    /// An edge connects labels no declared connection allows (`Closed`).
    InvalidTopology,
    /// Anything else the validator reports.
    Other,
}

/// One group of violations: every element that breaks the same rule on the same label and field.
///
/// The group key is `(kind, element, label, field)`, where `field` writes every array index as
/// `[*]`: the violations of all items of an array (`$.addresses[0].zip`, `$.addresses[3].zip`)
/// form one group `$.addresses[*].zip`, which keeps the first concrete path seen in
/// [`example_field`](Self::example_field).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ViolationGroup {
    /// The broken rule.
    pub kind: ViolationKind,
    /// `"vertex"` or `"edge"`.
    pub element: &'static str,
    /// The label the rule belongs to ([`UNLABELED`] for an element without a label).
    pub label: String,
    /// The property key or canonical nested path with every array index written `[*]`
    /// (`$.a.b[*].c`); for a topology violation the connection `(:A)-[:e]->(:B)` (`()` for an
    /// unlabeled endpoint); `None` when the rule is about the label itself.
    pub field: Option<String>,
    /// The FIRST concrete path seen for a `field` with `[*]` (`$.a.b[1].c`); `None` when the
    /// field has no array index.
    pub example_field: Option<String>,
    /// Extra detail of the FIRST violation seen (declared vs actual type, the constraint).
    pub detail: Option<String>,
    /// How many elements break the rule (an element counts once per group).
    pub count: u64,
    /// Up to [`SAMPLE_IDS`] ids of offending elements, each cut to [`SAMPLE_ID_CHARS`]
    /// characters (an ellipsis marks a cut). Not escaped: the `Display` text escapes them.
    pub sample_ids: Vec<String>,
    /// How many offending elements are not in `sample_ids`.
    pub omitted_ids: u64,
}

/// Everything wrong with existing data under a schema, found in one scan.
///
/// Returned (inside [`SchemaError::ExistingDataViolatesSchema`]) when a schema is applied to data
/// that violates it, and by `TraversalGraph::validate_against_schema`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchemaViolationReport {
    /// The mode of the schema that was checked.
    pub mode: SchemaMode,
    /// The violation groups, largest first (ties by label and field).
    pub groups: Vec<ViolationGroup>,
    /// Violations of groups beyond [`MAX_GROUPS`], counted but not listed.
    pub omitted_violations: u64,
    /// Total violations: one per element and group (an element can break several rules, and
    /// one value can break several at different nested paths; all are counted).
    pub total_violations: u64,
    /// Elements that break at least one rule.
    pub violating_elements: u64,
    /// Vertices and edges scanned.
    pub elements_checked: u64,
}

/// `1204` -> `1,204`.
fn digits(n: u64) -> String {
    let raw = n.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && (raw.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn counted(n: u64, element: &str) -> String {
    if n == 1 {
        format!("1 {element}")
    } else if element == "vertex" {
        format!("{} vertices", digits(n))
    } else {
        format!("{} {element}s", digits(n))
    }
}

fn mode_name(mode: SchemaMode) -> &'static str {
    match mode {
        SchemaMode::None => "None",
        SchemaMode::Open => "Open",
        SchemaMode::Closed => "Closed",
    }
}

impl ViolationGroup {
    /// `label.field`, or `label $.path` for a nested path.
    fn target(&self) -> String {
        match self.field.as_deref() {
            None => self.label.clone(),
            Some(f) if f.starts_with('$') => format!("{} {f}", self.label),
            Some(f) => format!("{}.{f}", self.label),
        }
    }

    fn fmt_sentence(&self) -> String {
        let n = counted(self.count, self.element);
        let target = self.target();
        let detail = self
            .detail
            .as_deref()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        let example = self
            .example_field
            .as_deref()
            .map(|e| format!(" (first at {e})"))
            .unwrap_or_default();
        match self.kind {
            ViolationKind::UnknownLabel if self.label == UNLABELED => {
                format!("{n} without a label (Closed admits only declared labels)")
            }
            ViolationKind::UnknownLabel => {
                format!(
                    "{} label {} is not declared (Closed): {n}",
                    self.element, self.label
                )
            }
            ViolationKind::MissingRequired => {
                format!("{target} is declared mandatory but is missing on {n}{example}")
            }
            ViolationKind::TypeMismatch => {
                format!("{target} has the wrong type on {n}{example}{detail}")
            }
            ViolationKind::ConstraintViolation => {
                format!("{target} violates a constraint on {n}{example}{detail}")
            }
            ViolationKind::UndeclaredProperty => {
                format!("{target} is not declared (Closed) on {n}{example}")
            }
            ViolationKind::InvalidTopology => format!(
                "edge {} {} is not a declared connection (Closed): {n}",
                self.label,
                self.field.as_deref().unwrap_or("")
            ),
            ViolationKind::Other => format!(
                "{target}: {}{detail} on {n}{example}",
                self.field.as_deref().unwrap_or("violation")
            ),
        }
    }
}

/// An endpoint of a topology violation: `(:A:B)`, or `()` without a label.
fn endpoint(labels: &[CompactString]) -> String {
    if labels.is_empty() {
        "()".to_string()
    } else {
        format!("(:{})", join_labels(labels))
    }
}

/// An id for display: quoted, with control characters and quotes escaped.
fn escaped(id: &str) -> String {
    format!("\"{}\"", id.escape_debug())
}

impl fmt::Display for SchemaViolationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kinds = self.groups.len() as u64;
        write!(
            f,
            "Cannot apply the {} schema: the existing data violates it ({} on {} of {} checked)",
            mode_name(self.mode),
            if kinds == 1 {
                "1 kind of violation".to_string()
            } else {
                format!("{} kinds of violations", digits(kinds))
            },
            counted(self.violating_elements, "element"),
            digits(self.elements_checked),
        )?;
        for (i, group) in self.groups.iter().take(PRINTED_GROUPS).enumerate() {
            write!(f, "\n  {}) {}", i + 1, group.fmt_sentence())?;
            if !group.sample_ids.is_empty() {
                let ids: Vec<String> = group.sample_ids.iter().map(|id| escaped(id)).collect();
                write!(f, " [ids: {}", ids.join(", "))?;
                if group.omitted_ids > 0 {
                    write!(f, " and {} more", digits(group.omitted_ids))?;
                }
                write!(f, "]")?;
            }
        }
        if self.groups.len() > PRINTED_GROUPS {
            write!(
                f,
                "\n  and {} more kinds of violations",
                digits((self.groups.len() - PRINTED_GROUPS) as u64)
            )?;
        }
        if self.omitted_violations > 0 {
            write!(
                f,
                "\n  and {} more violations of further kinds that were counted but not listed",
                digits(self.omitted_violations)
            )?;
        }
        Ok(())
    }
}

type GroupKey = (ViolationKind, &'static str, String, Option<String>);

struct Accumulator {
    detail: Option<String>,
    example_field: Option<String>,
    count: u64,
    samples: Vec<String>,
    /// The ordinal (`elements_checked`) of the last element counted, so an element that
    /// breaks the same rule at the same place twice counts once.
    last_element: u64,
}

/// Collects the violations of existing elements against one schema. Feed every vertex and edge
/// once, then [`finish`](Self::finish).
pub(crate) struct SchemaViolationScanner<'a> {
    schema: &'a GraphSchema,
    groups: HashMap<GroupKey, Accumulator>,
    omitted_violations: u64,
    total_violations: u64,
    violating_elements: u64,
    elements_checked: u64,
}

fn truncated(id: &str) -> String {
    if id.chars().count() <= SAMPLE_ID_CHARS {
        id.to_string()
    } else {
        let mut cut: String = id.chars().take(SAMPLE_ID_CHARS).collect();
        cut.push('\u{2026}');
        cut
    }
}

impl<'a> SchemaViolationScanner<'a> {
    pub(crate) fn new(schema: &'a GraphSchema) -> Self {
        Self {
            schema,
            groups: HashMap::new(),
            omitted_violations: 0,
            total_violations: 0,
            violating_elements: 0,
            elements_checked: 0,
        }
    }

    fn closed(&self) -> bool {
        matches!(self.schema.mode, SchemaMode::Closed)
    }

    fn record(
        &mut self,
        kind: ViolationKind,
        element: &'static str,
        id: &str,
        label: &str,
        field: Option<&str>,
        detail: Option<String>,
    ) {
        let (field, example_field) = match field.and_then(wildcard_indices) {
            Some(general) => (Some(general), field.map(str::to_string)),
            None => (field.map(str::to_string), None),
        };
        let key: GroupKey = (kind, element, label.to_string(), field);
        let full = self.groups.len() >= MAX_GROUPS;
        let current = self.elements_checked;
        let acc = match self.groups.get_mut(&key) {
            Some(acc) if acc.last_element == current => return,
            Some(acc) => acc,
            None if full => {
                self.total_violations += 1;
                self.omitted_violations += 1;
                return;
            }
            None => self.groups.entry(key).or_insert(Accumulator {
                detail,
                example_field,
                count: 0,
                samples: Vec::new(),
                last_element: current,
            }),
        };
        self.total_violations += 1;
        acc.last_element = current;
        acc.count += 1;
        if acc.samples.len() < SAMPLE_IDS {
            acc.samples.push(truncated(id));
        }
    }

    fn record_error(&mut self, element: &'static str, id: &str, error: SchemaError) {
        match error {
            SchemaError::MissingRequiredProperty {
                label,
                property_name,
                ..
            } => self.record(
                ViolationKind::MissingRequired,
                element,
                id,
                &label,
                Some(&property_name),
                None,
            ),
            SchemaError::TypeMismatch {
                label,
                property_name,
                expected_type,
                actual_type,
                ..
            } => self.record(
                ViolationKind::TypeMismatch,
                element,
                id,
                &label,
                Some(&property_name),
                Some(format!("declared {expected_type}, found {actual_type}")),
            ),
            SchemaError::ConstraintViolation {
                label,
                property_name,
                constraint_rule,
                actual_value,
            } => self.record(
                ViolationKind::ConstraintViolation,
                element,
                id,
                &label,
                Some(&property_name),
                Some(format!("{constraint_rule}, found {actual_value}")),
            ),
            SchemaError::UndeclaredProperty {
                label,
                property_name,
                ..
            } => self.record(
                ViolationKind::UndeclaredProperty,
                element,
                id,
                &label,
                Some(&property_name),
                None,
            ),
            other => {
                let text: String = other.to_string().chars().take(80).collect();
                self.record(ViolationKind::Other, element, id, "", Some(&text), None)
            }
        }
    }

    fn scan_properties(
        &mut self,
        element: &'static str,
        id: &str,
        declarations: &[LabelRoot<'_>],
        properties: &PropertyMap,
    ) {
        let additional_default = self.schema.mode.default_additional_properties();
        let mut errors: Vec<SchemaError> = Vec::new();
        for (name, value) in properties {
            // Collects every violation inside the value, not only the first.
            let _ = SchemaValidator::walk_property(
                declarations,
                element,
                name,
                value,
                additional_default,
                &mut errors,
            );
            for error in errors.drain(..) {
                self.record_error(element, id, error);
            }
        }
        // Absent required keys (a present null is the property's own type check).
        let missing: Vec<(String, String)> =
            SchemaValidator::missing_required(declarations, properties)
                .map(|(label, name)| (label.to_string(), name.to_string()))
                .collect();
        for (label, name) in missing {
            self.record(
                ViolationKind::MissingRequired,
                element,
                id,
                &label,
                Some(&name),
                None,
            );
        }
    }

    /// Scans one vertex.
    pub(crate) fn scan_vertex(
        &mut self,
        id: &str,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) {
        self.elements_checked += 1;
        if matches!(self.schema.mode, SchemaMode::None) {
            return;
        }
        let before = self.total_violations;
        let closed = self.closed();
        if labels.is_empty() && closed {
            self.record(
                ViolationKind::UnknownLabel,
                "vertex",
                id,
                UNLABELED,
                None,
                None,
            );
        }
        let mut declarations = Vec::with_capacity(labels.len());
        for label in labels {
            match self.schema.vertices.get(label.as_str()) {
                Some(v) => declarations.push((label.as_str(), &v.schema)),
                None if closed => {
                    self.record(ViolationKind::UnknownLabel, "vertex", id, label, None, None)
                }
                None => {}
            }
        }
        if !declarations.is_empty() {
            self.scan_properties("vertex", id, &declarations, properties);
        }
        if self.total_violations > before {
            self.violating_elements += 1;
        }
    }

    /// Scans one edge with the label sets of its endpoints.
    pub(crate) fn scan_edge(
        &mut self,
        id: &str,
        label: Option<&str>,
        source_labels: &[CompactString],
        target_labels: &[CompactString],
        properties: &PropertyMap,
    ) {
        self.elements_checked += 1;
        if matches!(self.schema.mode, SchemaMode::None) {
            return;
        }
        let before = self.total_violations;
        let closed = self.closed();
        match label.filter(|l| !l.is_empty()) {
            None => {
                if closed {
                    self.record(
                        ViolationKind::UnknownLabel,
                        "edge",
                        id,
                        UNLABELED,
                        None,
                        None,
                    );
                }
            }
            Some(label) => match self.schema.edges.get(label) {
                None => {
                    if closed {
                        self.record(ViolationKind::UnknownLabel, "edge", id, label, None, None);
                    }
                }
                Some(e_schema) => {
                    if SchemaValidator::check_topology(
                        self.schema,
                        Some(label),
                        source_labels,
                        target_labels,
                    )
                    .is_err()
                    {
                        let connection = format!(
                            "{}-[:{label}]->{}",
                            endpoint(source_labels),
                            endpoint(target_labels)
                        );
                        self.record(
                            ViolationKind::InvalidTopology,
                            "edge",
                            id,
                            label,
                            Some(&connection),
                            None,
                        );
                    }
                    let declarations = [(label, &e_schema.schema)];
                    self.scan_properties("edge", id, &declarations, properties);
                }
            },
        }
        if self.total_violations > before {
            self.violating_elements += 1;
        }
    }

    /// The report, also when nothing was violated (then it has no groups).
    pub(crate) fn finish(self) -> SchemaViolationReport {
        let mut groups: Vec<ViolationGroup> = self
            .groups
            .into_iter()
            .map(|((kind, element, label, field), acc)| ViolationGroup {
                kind,
                element,
                label,
                field,
                example_field: acc.example_field,
                detail: acc.detail,
                omitted_ids: acc.count - acc.samples.len() as u64,
                count: acc.count,
                sample_ids: acc.samples,
            })
            .collect();
        groups.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.kind.cmp(&b.kind))
                .then_with(|| a.element.cmp(b.element))
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.field.cmp(&b.field))
        });
        SchemaViolationReport {
            mode: self.schema.mode,
            groups,
            omitted_violations: self.omitted_violations,
            total_violations: self.total_violations,
            violating_elements: self.violating_elements,
            elements_checked: self.elements_checked,
        }
    }
}

/// Validates every vertex and edge of `storage` against `schema` in one O(N) pass, coercing
/// nothing (reads never coerce: a string held in a field declared `uuid` is a type mismatch).
/// The report has no groups when the data satisfies the schema (always in mode `None`).
pub(crate) fn scan_storage<S: GraphStorage>(
    storage: &S,
    schema: &GraphSchema,
) -> Result<SchemaViolationReport, crate::error::GraphError> {
    let mut scanner = SchemaViolationScanner::new(schema);
    let empty = PropertyMap::default();
    for vertex in storage.vertices()? {
        scanner.scan_vertex(
            storage.vertex_id(vertex)?.unwrap_or(""),
            storage.vertex_labels(vertex)?,
            storage.vertex_properties(vertex)?.unwrap_or(&empty),
        );
    }
    for edge in storage.edges()? {
        let Some((from, to)) = storage.edge_endpoints(edge)? else {
            continue;
        };
        scanner.scan_edge(
            storage.edge_id(edge)?.unwrap_or(""),
            storage.edge_label(edge)?,
            storage.vertex_labels(from)?,
            storage.vertex_labels(to)?,
            storage.edge_properties(edge)?.unwrap_or(&empty),
        );
    }
    Ok(scanner.finish())
}

impl ViolationKind {
    /// The kind's name in the data shape: `unknown_label`, `missing_required`,
    /// `type_mismatch`, `constraint_violation`, `undeclared_property`, `invalid_topology`,
    /// `other`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ViolationKind::UnknownLabel => "unknown_label",
            ViolationKind::MissingRequired => "missing_required",
            ViolationKind::TypeMismatch => "type_mismatch",
            ViolationKind::ConstraintViolation => "constraint_violation",
            ViolationKind::UndeclaredProperty => "undeclared_property",
            ViolationKind::InvalidTopology => "invalid_topology",
            ViolationKind::Other => "other",
        }
    }
}

impl SchemaViolationReport {
    /// Whether nothing is violated.
    pub fn is_empty(&self) -> bool {
        self.total_violations == 0
    }

    /// The report as data: `{mode, elements_checked, violating_elements, total_violations,
    /// omitted_violations, groups: [{kind, element, label, field, example_field, detail, count,
    /// sample_ids, omitted_ids}]}` (`field`/`example_field`/`detail` are `null` when absent).
    pub fn to_value(&self) -> ElementProperty {
        let int = |n: u64| {
            ElementProperty::Number(Number::from_i64(i64::try_from(n).unwrap_or(i64::MAX)))
        };
        let text = |s: Option<&str>| s.map_or(ElementProperty::Null, ElementProperty::from);
        let groups = self
            .groups
            .iter()
            .map(|group| {
                let mut map = PropertyMap::default();
                map.insert("kind", ElementProperty::from(group.kind.as_str()));
                map.insert("element", ElementProperty::from(group.element));
                map.insert("label", ElementProperty::from(group.label.as_str()));
                map.insert("field", text(group.field.as_deref()));
                map.insert("example_field", text(group.example_field.as_deref()));
                map.insert("detail", text(group.detail.as_deref()));
                map.insert("count", int(group.count));
                map.insert(
                    "sample_ids",
                    ElementProperty::Array(
                        group
                            .sample_ids
                            .iter()
                            .map(|id| ElementProperty::from(id.as_str()))
                            .collect(),
                    ),
                );
                map.insert("omitted_ids", int(group.omitted_ids));
                ElementProperty::Object(Box::new(map))
            })
            .collect();
        let mut map = PropertyMap::default();
        map.insert("mode", ElementProperty::from(self.mode.as_str()));
        map.insert("elements_checked", int(self.elements_checked));
        map.insert("violating_elements", int(self.violating_elements));
        map.insert("total_violations", int(self.total_violations));
        map.insert("omitted_violations", int(self.omitted_violations));
        map.insert("groups", ElementProperty::Array(groups));
        ElementProperty::Object(Box::new(map))
    }
}
