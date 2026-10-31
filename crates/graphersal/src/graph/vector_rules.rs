//! Vector rules (book page "Vector Rules"): a vector property `to` of a label declares in the
//! schema the top-level property `from` of the same label it is computed from (`"from"` beside
//! `"embedder"` on the vector node). The engine keeps `to` in step with `from` inside the
//! writing unit:
//!
//! - [`VectorRules`] is compiled from the schema at every schema change, load and rollback of
//!   a schema (`TraversalGraph::refresh_vector_rules`); a graph without a rule holds `None`, so
//!   every check below is one branch.
//! - A direct write or removal of a rule's `to` inside a live unit is refused
//!   ([`GraphError::DerivedProperty`]); replay, recovery, loads and `apply_change_set` take it as
//!   given (rules off).
//! - At the end of the OUTERMOST unit (`TraversalGraph::commit_undo_unit`, before the memory
//!   check and the commit number; also in the discard of a dry run) the rule pass walks the
//!   undo log, collects every element whose `from` the unit touched (a property write of a
//!   rule source, an element insert, a label change) and, for a rule the unit added or changed,
//!   EVERY element of the label (the first fill), embeds the texts in ONE batch per model and
//!   writes the vectors as ordinary writes of the unit (undo log, change set, WAL). A text-less
//!   `from` (absent, `null`, `""`, not a string, or a text without features) removes `to`.
//! - Only the store's models (catalog definitions) and the built-in `hash-ngram/1` run
//!   (`set_schema` refuses any other model: [`GraphError::VectorRuleModelMissing`]).
//! - A first fill or a model replacement writes `vector_fill.started` / `done` / `failed` to
//!   the system journal and reports its progress ([`crate::graph::Graph::vector_fill_progress`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use compact_str::CompactString;
use fnv::{FnvHashMap, FnvHashSet};

use crate::error::{GraphError, SchemaError};
use crate::graph::schema::{ElementKind, GraphSchema, PropertySchema};
use crate::graph::system_journal::catalogue;
use crate::graph::system_journal::{Event, SystemJournal};
use crate::graph::undo::UndoEntry;
use crate::graph::{EdgeHandle, GraphStorage, TraversalGraph, VertexHandle};
use crate::value::vector::embed::{EmbedContext, EmbedError, Embedder, HashNgram, checked_embed};
use crate::value::{ElementProperty, VectorElement};

/// The operation a memory-ceiling refusal of the rule pass names.
pub(crate) const FILL_OPERATION: &str = "vector rule fill";

/// The `operation` of [`GraphError::Unsupported`] for a schema with vector rules on a storage
/// without change capture.
pub(crate) const UNSUPPORTED_OPERATION: &str = "vector rules";

/// Texts per second of the built-in `hash-ngram/1` (measured on a laptop, short texts):
/// the estimate of a fill's time.
const HASH_NGRAM_TEXTS_PER_SECOND: f64 = 200_000.0;

/// The estimate from which a fill asks for a confirmation (MCP `confirm_lock`, the playground
/// dialog).
const CONFIRM_ESTIMATE_MS: u64 = 1_000;

/// The elements from which a fill without an estimate asks for a confirmation.
const CONFIRM_ELEMENTS: u64 = 10_000;

// ---------------------------------------------------------------------------------------------
// Compiled rules
// ---------------------------------------------------------------------------------------------

/// One vector rule: `to` of `label` is embedded from `from` with `model`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rule {
    pub(crate) kind: ElementKind,
    pub(crate) label: CompactString,
    pub(crate) from: CompactString,
    pub(crate) to: CompactString,
    pub(crate) model: CompactString,
    pub(crate) element: VectorElement,
    pub(crate) dimension: Option<u32>,
}

impl Rule {
    /// Whether `other` is a rule for the same property.
    fn same_target(&self, other: &Rule) -> bool {
        self.kind == other.kind && self.label == other.label && self.to == other.to
    }

    /// Whether `other` (of the same property) computes the same vectors: the same source,
    /// model and element, a dimension left out matching any ([`same_model`]).
    fn equivalent(&self, other: &Rule) -> bool {
        self.same_target(other)
            && self.from == other.from
            && self.element == other.element
            && (self.dimension == other.dimension
                || self.dimension.is_none()
                || other.dimension.is_none())
            && same_model(&self.model, &other.model)
    }
}

/// Whether two `embedder` values name the same model: equal, or one names the repository of
/// the other's identity (the schema stores the identity of a repository name, K2).
pub(crate) fn same_model(a: &str, b: &str) -> bool {
    a == b || ((!a.contains('@') || !b.contains('@')) && display_model(a) == display_model(b))
}

/// The vector rules of a schema, by element kind and label.
#[derive(Debug, Default)]
pub(crate) struct VectorRules {
    rules: Vec<Rule>,
    /// Label -> positions in `rules`, in declaration order.
    vertex: FnvHashMap<CompactString, Vec<usize>>,
    edge: FnvHashMap<CompactString, Vec<usize>>,
    /// Every `from` of any rule (a write of another key never touches a rule).
    sources: FnvHashSet<CompactString>,
    /// Every `to` of any rule (a write of another key is never refused).
    targets: FnvHashSet<CompactString>,
}

impl VectorRules {
    /// The rules of `schema`, or `None` when it declares none.
    pub(crate) fn compile(schema: &GraphSchema) -> Option<Arc<VectorRules>> {
        let mut rules = VectorRules::default();
        let labels = schema
            .vertices
            .iter()
            .map(|(label, v)| (ElementKind::Vertex, label, &v.schema))
            .chain(
                schema
                    .edges
                    .iter()
                    .map(|(label, e)| (ElementKind::Edge, label, &e.schema)),
            );
        for (kind, label, root) in labels {
            for (key, node) in &root.properties {
                let Some(vector) = &node.vector else {
                    continue;
                };
                let (Some(from), Some(model)) = (&vector.from, &vector.embedder) else {
                    continue;
                };
                let position = rules.rules.len();
                rules.rules.push(Rule {
                    kind,
                    label: CompactString::from(label.as_str()),
                    from: CompactString::from(from.as_str()),
                    to: CompactString::from(key.as_str()),
                    model: CompactString::from(model.as_str()),
                    element: vector.element,
                    dimension: vector.dimension,
                });
                let map = match kind {
                    ElementKind::Vertex => &mut rules.vertex,
                    ElementKind::Edge => &mut rules.edge,
                };
                map.entry(CompactString::from(label.as_str()))
                    .or_default()
                    .push(position);
                rules.sources.insert(CompactString::from(from.as_str()));
                rules.targets.insert(CompactString::from(key.as_str()));
            }
        }
        (!rules.rules.is_empty()).then(|| Arc::new(rules))
    }

    /// Every rule, in declaration order.
    pub(crate) fn all(&self) -> &[Rule] {
        &self.rules
    }

    fn by_label(&self, kind: ElementKind) -> &FnvHashMap<CompactString, Vec<usize>> {
        match kind {
            ElementKind::Vertex => &self.vertex,
            ElementKind::Edge => &self.edge,
        }
    }

    /// The positions of the rules that apply to an element of `kind` with `labels`: per `to`
    /// the rule of the FIRST label that declares one (the convention of coercion).
    pub(crate) fn applicable(&self, kind: ElementKind, labels: &[CompactString]) -> Vec<usize> {
        let map = self.by_label(kind);
        let mut out: Vec<usize> = Vec::new();
        for label in labels {
            if let Some(positions) = map.get(label) {
                for &position in positions {
                    let to = &self.rules[position].to;
                    if !out.iter().any(|&p| &self.rules[p].to == to) {
                        out.push(position);
                    }
                }
            }
        }
        out
    }

    /// The rule that computes `key` of an element of `kind` with `labels`, if any.
    pub(crate) fn derived(
        &self,
        kind: ElementKind,
        labels: &[CompactString],
        key: &str,
    ) -> Option<&Rule> {
        if !self.targets.contains(key) {
            return None;
        }
        let map = self.by_label(kind);
        labels.iter().find_map(|label| {
            map.get(label)?
                .iter()
                .map(|&p| &self.rules[p])
                .find(|rule| rule.to == key)
        })
    }

    /// Whether `key` is the `from` of any rule.
    pub(crate) fn is_source(&self, key: &str) -> bool {
        self.sources.contains(key)
    }

    /// Whether any rule computes `key` (of any label).
    #[inline]
    pub(crate) fn is_target(&self, key: &str) -> bool {
        self.targets.contains(key)
    }

    /// The refusal of a direct write of `key` (a rule's `to`) of an element of `kind` with
    /// `labels`.
    pub(crate) fn refuse(
        &self,
        kind: ElementKind,
        labels: &[CompactString],
        key: &str,
    ) -> Result<(), GraphError> {
        match self.derived(kind, labels, key) {
            None => Ok(()),
            Some(rule) => Err(GraphError::DerivedProperty {
                label: rule.label.to_string(),
                key: rule.to.to_string(),
                from: rule.from.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Schema checks
// ---------------------------------------------------------------------------------------------

/// The structural checks of the vector rules of `schema` (no catalog needed): `from` needs
/// `embedder`, names another property than its own, is not the `to` of another rule of the
/// same label (no chains), and, when the label declares it, admits a string.
pub(crate) fn check_rule_structure(schema: &GraphSchema) -> Result<(), SchemaError> {
    let labels = schema
        .vertices
        .iter()
        .map(|(label, v)| ("vertices", label, &v.schema))
        .chain(
            schema
                .edges
                .iter()
                .map(|(label, e)| ("edges", label, &e.schema)),
        );
    for (section, label, root) in labels {
        let targets: Vec<&str> = root
            .properties
            .iter()
            .filter(|(_, node)| node.vector.as_ref().is_some_and(|v| v.from.is_some()))
            .map(|(key, _)| key.as_str())
            .collect();
        for (key, node) in &root.properties {
            let Some(from) = node.vector.as_ref().and_then(|v| v.from.as_deref()) else {
                continue;
            };
            let at = format!(
                "/{section}/{}/schema/properties/{}/from",
                escape(label),
                escape(key)
            );
            let fail = |reason: String, hint: String| SchemaError::InvalidSchema {
                path: at.clone(),
                reason,
                hint,
            };
            if node.vector.as_ref().is_some_and(|v| v.embedder.is_none()) {
                return Err(fail(
                    "from needs \"embedder\" on the same node: the vector rule embeds with \
                     that model"
                        .to_string(),
                    format!(
                        "Name the model the rule embeds with: {{\"type\": \"array\", \
                         \"format\": \"vector\", \"embedder\": \"hash-ngram/1\", \
                         \"from\": \"{from}\"}}."
                    ),
                ));
            }
            if from == key {
                return Err(fail(
                    format!("the vector rule of {key} names its own property as from"),
                    "from names the text property the vector is computed from, e.g. \
                     \"from\": \"text\"."
                        .to_string(),
                ));
            }
            if targets.contains(&from) {
                return Err(fail(
                    format!("{from} is itself computed by a vector rule: rules cannot be chained"),
                    "Compute every vector from a text property that is written directly."
                        .to_string(),
                ));
            }
            if let Some(source) = root.properties.get(from)
                && !admits_text(source)
            {
                return Err(fail(
                    format!(
                        "{from} is declared {}: the rule would always remove {key}",
                        source.type_name()
                    ),
                    format!(
                        "A vector rule embeds a text: declare {from} as {{\"type\": \
                         \"string\"}} or name a string property in from."
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Whether a property node admits a plain string.
fn admits_text(node: &PropertySchema) -> bool {
    match node.declared_kinds() {
        None => true,
        Some(kinds) => kinds.iter().any(|(kind, format)| {
            *kind == crate::graph::schema::JsonType::String && format.is_none()
        }),
    }
}

/// JSON Pointer escaping of one segment.
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// The apply-time check of the rules of `schema` against the catalog `definitions` (the
/// schema's repository names already resolved to identities): each rule's model is
/// `hash-ngram/1` or a model definition of the catalog
/// ([`GraphError::VectorRuleModelMissing`]), and its dimension fits the declared one (an
/// undeclared dimension is declared from the model). Returns the schema to store.
pub(crate) fn check_rule_models(
    definitions: &crate::graph::definition::Catalog,
    mut schema: GraphSchema,
) -> Result<GraphSchema, GraphError> {
    let sections = schema
        .vertices
        .iter_mut()
        .map(|(label, v)| ("vertices", label.clone(), &mut v.schema))
        .chain(
            schema
                .edges
                .iter_mut()
                .map(|(label, e)| ("edges", label.clone(), &mut e.schema)),
        );
    for (section, label, root) in sections {
        for (key, node) in root.properties.iter_mut() {
            let Some(vector) = node.vector.as_mut() else {
                continue;
            };
            let (Some(_), Some(model)) = (&vector.from, &vector.embedder) else {
                continue;
            };
            let dimension = if model == HashNgram::NAME {
                HashNgram::DEFAULT_DIMENSION as u64
            } else {
                match definitions
                    .get(crate::graph::DefinitionKind::MODEL, model)
                    .and_then(|d| d.as_model())
                    .filter(|m| m.served())
                {
                    Some(definition) => definition.dimension(),
                    None => {
                        return Err(GraphError::VectorRuleModelMissing {
                            label: label.clone(),
                            key: key.clone(),
                            model: model.clone(),
                        });
                    }
                }
            };
            match vector.dimension {
                Some(declared) if u64::from(declared) != dimension => {
                    return Err(SchemaError::InvalidSchema {
                        path: format!(
                            "/{section}/{}/schema/properties/{}/dimension",
                            escape(&label),
                            escape(key)
                        ),
                        reason: format!(
                            "the model {model} of the vector rule makes vectors of dimension \
                             {dimension}, the property declares {declared}"
                        ),
                        hint: format!(
                            "Declare \"dimension\": {dimension}, or leave it out (the rule \
                             declares the model's dimension)."
                        ),
                    }
                    .into());
                }
                Some(_) => {}
                None => vector.dimension = u32::try_from(dimension).ok(),
            }
        }
    }
    Ok(schema)
}

/// The places (`label.key`) whose vectors the model `identity` made WITHOUT a vector rule (an
/// `embedder` but no `from`): a model replacement cannot re-embed them (K3).
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn places_without_rule(schema: Option<&GraphSchema>, identity: &str) -> Vec<String> {
    let mut places = Vec::new();
    let Some(schema) = schema else {
        return places;
    };
    let roots = schema
        .vertices
        .iter()
        .map(|(label, v)| (label, &v.schema))
        .chain(schema.edges.iter().map(|(label, e)| (label, &e.schema)));
    for (label, root) in roots {
        for (key, node) in &root.properties {
            if let Some(vector) = &node.vector
                && vector.from.is_none()
                && vector
                    .embedder
                    .as_deref()
                    .is_some_and(|model| model.split('?').next() == Some(identity))
            {
                places.push(format!("{label}.{key}"));
            }
        }
    }
    places
}

/// The model replacement (`graphersal embed-model import --replace`, book page "Vector Rules"):
/// `schema` with every vector rule naming `old` switched to `new` (and its declared dimension
/// to `dimension` when it differs), or `None` when no rule names `old`. Stored in the unit that
/// swaps the model definitions, the changed rules refill every element (`model_replaced`).
#[cfg_attr(not(feature = "persist"), allow(dead_code))]
pub(crate) fn replace_model_in_schema(
    schema: &GraphSchema,
    old: &str,
    new: &str,
    dimension: u64,
) -> Option<GraphSchema> {
    let mut schema = schema.clone();
    let mut changed = false;
    let roots = schema
        .vertices
        .values_mut()
        .map(|v| &mut v.schema)
        .chain(schema.edges.values_mut().map(|e| &mut e.schema));
    for root in roots {
        for node in root.properties.values_mut() {
            let Some(vector) = node.vector.as_mut() else {
                continue;
            };
            if vector.from.is_none()
                || !vector
                    .embedder
                    .as_deref()
                    .is_some_and(|model| model.split('?').next() == Some(old))
            {
                continue;
            }
            vector.embedder = Some(new.to_string());
            if vector.dimension.is_some() {
                vector.dimension = u32::try_from(dimension).ok();
            }
            changed = true;
        }
    }
    changed.then_some(schema)
}

/// Whether `schema` declares any vector rule.
pub(crate) fn has_rules(schema: &GraphSchema) -> bool {
    schema
        .vertices
        .values()
        .map(|v| &v.schema)
        .chain(schema.edges.values().map(|e| &e.schema))
        .any(|root| {
            root.properties
                .values()
                .any(|node| node.vector.as_ref().is_some_and(|v| v.from.is_some()))
        })
}

// ---------------------------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// Test hook: embedders that serve rule models on this thread before anything else.
    static TEST_MODELS: RefCell<Vec<Arc<dyn Embedder>>> = const { RefCell::new(Vec::new()) };
}

/// Test hook: serve rule models with `embedders` on this thread while `run` runs.
#[cfg(test)]
pub(crate) fn with_test_models<R>(embedders: Vec<Arc<dyn Embedder>>, run: impl FnOnce() -> R) -> R {
    TEST_MODELS.with(|m| *m.borrow_mut() = embedders);
    let result = run();
    TEST_MODELS.with(|m| m.borrow_mut().clear());
    result
}

/// The embedder of a rule's model: the built-in `hash-ngram/1`, else the model of the
/// catalog of the graph whose lock this thread holds (the registry it attached).
fn rule_embedder(model: &str) -> Option<Arc<dyn Embedder>> {
    #[cfg(test)]
    {
        let found =
            TEST_MODELS.with(|m| m.borrow().iter().find(|e| e.info().model == model).cloned());
        if found.is_some() {
            return found;
        }
    }
    if model == HashNgram::NAME {
        static BUILT_IN: std::sync::OnceLock<Arc<dyn Embedder>> = std::sync::OnceLock::new();
        return Some(Arc::clone(
            BUILT_IN.get_or_init(|| Arc::new(HashNgram::new()) as Arc<dyn Embedder>),
        ));
    }
    crate::value::vector::embed::registry::attached().and_then(|e| e.by_model(model).cloned())
}

/// The texts per second a fill with `model` runs at, when known: the built-in's constant, or
/// what a loaded catalog model measured at its load.
pub(crate) fn texts_per_second(model: &str) -> Option<f64> {
    if model == HashNgram::NAME {
        return Some(HASH_NGRAM_TEXTS_PER_SECOND);
    }
    crate::value::vector::embed::registry::measured_texts_per_second(model)
}

/// The name a person knows a model by: the repository of a Model2Vec identity
/// (`model2vec/minishlab/potion-base-8M@3f2a9c41b7de` -> `minishlab/potion-base-8M`), else
/// the identity.
pub(crate) fn display_model(model: &str) -> &str {
    let base = model.split_once('?').map_or(model, |(base, _)| base);
    match base
        .strip_prefix("model2vec/")
        .and_then(|rest| rest.rsplit_once('@'))
    {
        Some((repo, _)) if !repo.is_empty() => repo,
        _ => model,
    }
}

// ---------------------------------------------------------------------------------------------
// The preview: planned fills and the one warning text
// ---------------------------------------------------------------------------------------------

/// Why a fill runs ([`PlannedFill::reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FillReason {
    /// A vector rule was added to the schema: its property is filled for every element.
    RuleAdded,
    /// A vector rule changed (`from`, `embedder`, `element` or `dimension`): refilled.
    RuleChanged,
    /// The rule's model was replaced by a new revision (`graphersal embed-model import
    /// --replace`): refilled.
    ModelReplaced,
}

impl FillReason {
    /// The snake_case name (`rule_added`, `rule_changed`, `model_replaced`).
    pub fn as_str(&self) -> &'static str {
        match self {
            FillReason::RuleAdded => "rule_added",
            FillReason::RuleChanged => "rule_changed",
            FillReason::ModelReplaced => "model_replaced",
        }
    }
}

/// One fill a schema change starts (book page "Vector Rules", section "First fill"): every
/// element of `label` gets its `to` embedded from `from` with `model`, in the schema-changing
/// unit, while the whole database is locked. Reported by
/// [`validate_schema`](crate::GraphTraversalSource::validate_schema) and
/// [`diff_schema`](crate::GraphTraversalSource::diff_schema) before anything is applied; the
/// one warning text of every front end is [`fill_warning`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PlannedFill {
    element: ElementKind,
    label: String,
    from: String,
    to: String,
    model: String,
    reason: FillReason,
    elements: u64,
    removals: u64,
    estimate_ms: Option<u64>,
    approximate: bool,
}

impl PlannedFill {
    /// Vertices or edges.
    pub fn element(&self) -> ElementKind {
        self.element
    }

    /// The label whose elements are filled.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The source property of the rule.
    pub fn from(&self) -> &str {
        &self.from
    }

    /// The vector property the fill writes.
    pub fn to(&self) -> &str {
        &self.to
    }

    /// The model identity the fill embeds with.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Why it runs.
    pub fn reason(&self) -> FillReason {
        self.reason
    }

    /// How many elements get a vector (those with a text in `from`); for a
    /// [`diff_schema`](crate::GraphTraversalSource::diff_schema) preview the label's element
    /// count ([`Self::approximate`]).
    pub fn elements(&self) -> u64 {
        self.elements
    }

    /// How many elements lose their `to` (it is there but `from` holds no text); `0` in an
    /// approximate preview.
    pub fn removals(&self) -> u64 {
        self.removals
    }

    /// The estimated time in milliseconds, when the model's speed is known.
    pub fn estimate_ms(&self) -> Option<u64> {
        self.estimate_ms
    }

    /// Whether the counts come from the label statistics only (no data read).
    pub fn approximate(&self) -> bool {
        self.approximate
    }

    /// Whether a front end asks before applying: an estimate of 1 s or more, or no estimate
    /// and 10,000 elements or more (book page "Vector Rules").
    pub fn needs_confirmation(&self) -> bool {
        match self.estimate_ms {
            Some(ms) => ms >= CONFIRM_ESTIMATE_MS,
            None => self.elements >= CONFIRM_ELEMENTS,
        }
    }

    /// The fill as data: `{element, label, from, to, model, reason, elements, removals,
    /// estimate_ms, approximate, needs_confirmation}` (`estimate_ms` `null` when unknown).
    pub fn to_value(&self) -> ElementProperty {
        let mut map = crate::value::PropertyMap::default();
        map.insert("element", ElementProperty::from(self.element.as_str()));
        map.insert("label", ElementProperty::from(self.label.as_str()));
        map.insert("from", ElementProperty::from(self.from.as_str()));
        map.insert("to", ElementProperty::from(self.to.as_str()));
        map.insert("model", ElementProperty::from(self.model.as_str()));
        map.insert("reason", ElementProperty::from(self.reason.as_str()));
        map.insert("elements", count(self.elements));
        map.insert("removals", count(self.removals));
        map.insert(
            "estimate_ms",
            self.estimate_ms.map_or(ElementProperty::Null, count),
        );
        map.insert("approximate", ElementProperty::Boolean(self.approximate));
        map.insert(
            "needs_confirmation",
            ElementProperty::Boolean(self.needs_confirmation()),
        );
        ElementProperty::Object(Box::new(map))
    }
}

fn count(n: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(n).unwrap_or(i64::MAX))
}

/// The fills as data: a list of [`PlannedFill::to_value`] maps.
pub(crate) fn fills_value(fills: &[PlannedFill]) -> ElementProperty {
    ElementProperty::Array(fills.iter().map(PlannedFill::to_value).collect())
}

/// The ONE warning text of a schema change that fills vectors (every front end shows this
/// string: the playground dialog, the CLI note, the Python warning, the MCP refusal), or
/// `None` when nothing is filled (no fill, or fills of labels without elements: an empty label
/// locks nothing worth a warning).
///
/// ```text
/// Applying this schema fills vectors: 1,204,331 elements of doc.emb from doc.text with the
/// model minishlab/potion-multilingual-128M (about 1 min).
/// The whole database is LOCKED while this runs: every write and every read waits until it ends.
/// Run it at a quiet time. If it fails, nothing is written.
/// ```
pub fn fill_warning(fills: &[PlannedFill]) -> Option<String> {
    let fills: Vec<&PlannedFill> = fills
        .iter()
        .filter(|f| f.elements > 0 || f.removals > 0)
        .collect();
    if fills.is_empty() {
        return None;
    }
    let parts: Vec<String> = fills
        .iter()
        .map(|fill| {
            format!(
                "{}{} {} of {}.{} from {}.{} with the model {}",
                if fill.approximate { "up to " } else { "" },
                thousands(fill.elements),
                if fill.elements == 1 {
                    "element"
                } else {
                    "elements"
                },
                fill.label,
                fill.to,
                fill.label,
                fill.from,
                display_model(&fill.model)
            )
        })
        .collect();
    let estimate = if fills.iter().all(|f| f.estimate_ms.is_some()) {
        let total: u64 = fills.iter().filter_map(|f| f.estimate_ms).sum();
        duration_text(total)
    } else {
        "time depends on the model".to_string()
    };
    Some(format!(
        "Applying this schema fills vectors: {} ({estimate}).\nThe whole database is LOCKED \
         while this runs: every write and every read waits until it ends.\nRun it at a quiet \
         time. If it fails, nothing is written.",
        parts.join("; ")
    ))
}

/// `1204331` -> `1,204,331`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `about 3 min`, `about 12 s`, `under a second`.
fn duration_text(ms: u64) -> String {
    if ms < 1_000 {
        "under a second".to_string()
    } else if ms < 60_000 {
        format!("about {} s", ms.div_ceil(1_000))
    } else {
        format!("about {} min", ms.div_ceil(60_000))
    }
}

/// The rules of `schema` with their models named by identity, resolved against the catalog of
/// `storage` (a repository name names the catalog's model of that repository).
fn compile_resolved<S: GraphStorage + ?Sized>(
    storage: &S,
    schema: &GraphSchema,
) -> Result<Option<Arc<VectorRules>>, GraphError> {
    let Some(rules) = VectorRules::compile(schema) else {
        return Ok(None);
    };
    let models: Vec<(String, String)> = storage
        .definitions()?
        .filter_map(|d| {
            d.as_model()
                .filter(|m| m.served())
                .map(|m| (m.repo().to_string(), d.name().to_string()))
        })
        .collect();
    if models.is_empty() {
        return Ok(Some(rules));
    }
    let mut resolved = VectorRules::compile(schema).map_or_else(VectorRules::default, |r| {
        Arc::try_unwrap(r).unwrap_or_default()
    });
    for rule in &mut resolved.rules {
        if let Some((_, identity)) = models.iter().find(|(repo, _)| repo == rule.model.as_str()) {
            rule.model = CompactString::from(identity.as_str());
        }
    }
    Ok(Some(Arc::new(resolved)))
}

/// The fills that storing `after` over `before` starts on `storage`. `exact`: count the
/// elements with a text (a data read, `validate_schema`); else the label statistics
/// (`diff_schema`, approximate).
pub(crate) fn plan_fills<S: GraphStorage + ?Sized>(
    storage: &S,
    before: Option<&GraphSchema>,
    after: &GraphSchema,
    exact: bool,
) -> Result<Vec<PlannedFill>, GraphError> {
    let Some(new) = compile_resolved(storage, after)? else {
        return Ok(Vec::new());
    };
    let old = match before {
        Some(before) => compile_resolved(storage, before)?,
        None => None,
    };
    let mut fills = Vec::new();
    for rule in new.all() {
        let previous = old
            .as_ref()
            .and_then(|old| old.all().iter().find(|o| o.same_target(rule)));
        let reason = match previous {
            Some(previous) if previous.equivalent(rule) => continue,
            Some(_) => FillReason::RuleChanged,
            None => FillReason::RuleAdded,
        };
        let (elements, removals) = if exact {
            count_texts(storage, &new, rule)?
        } else {
            let statistics = storage.statistics()?;
            let elements = match (statistics, rule.kind) {
                (Some(s), ElementKind::Vertex) => s.vertex_label_count(&rule.label) as u64,
                (Some(s), ElementKind::Edge) => s.edge_label_count(&rule.label) as u64,
                (None, _) => 0,
            };
            (elements, 0)
        };
        let estimate_ms = texts_per_second(&rule.model)
            .filter(|tps| *tps > 0.0)
            .map(|tps| ((elements as f64) / tps * 1000.0).ceil() as u64);
        fills.push(PlannedFill {
            element: rule.kind,
            label: rule.label.to_string(),
            from: rule.from.to_string(),
            to: rule.to.to_string(),
            model: rule.model.to_string(),
            reason,
            elements,
            removals,
            estimate_ms,
            approximate: !exact,
        });
    }
    Ok(fills)
}

/// The elements of `rule`'s label the rule applies to (first declaring label) with a text in
/// `from`, and those without a text that hold `to`.
fn count_texts<S: GraphStorage + ?Sized>(
    storage: &S,
    rules: &VectorRules,
    rule: &Rule,
) -> Result<(u64, u64), GraphError> {
    let (mut texts, mut removals) = (0u64, 0u64);
    let mut tally = |labels: &[CompactString], properties: Option<&crate::value::PropertyMap>| {
        if !rules
            .applicable(rule.kind, labels)
            .iter()
            .any(|&p| rules.all()[p] == *rule)
        {
            return;
        }
        let text = properties
            .and_then(|p| p.get(rule.from.as_str()))
            .and_then(ElementProperty::text)
            .is_some_and(|t| !t.is_empty());
        if text {
            texts += 1;
        } else if properties.is_some_and(|p| p.contains_key(rule.to.as_str())) {
            removals += 1;
        }
    };
    match rule.kind {
        ElementKind::Vertex => {
            for handle in storage.vertices_by_label(&rule.label)? {
                let labels = storage.vertex_labels(handle)?;
                tally(labels, storage.vertex_properties(handle)?);
            }
        }
        ElementKind::Edge => {
            let labels = [rule.label.clone()];
            for handle in storage.edges_by_label(&rule.label)? {
                tally(&labels, storage.edge_properties(handle)?);
            }
        }
    }
    Ok((texts, removals))
}

// ---------------------------------------------------------------------------------------------
// The writer's context: journal and progress of the graph whose write lock this thread holds
// ---------------------------------------------------------------------------------------------

/// The live progress of a running fill ([`crate::graph::Graph::vector_fill_progress`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FillProgress {
    /// Texts embedded so far.
    pub embedded: u64,
    /// Texts the fill embeds in all.
    pub total: u64,
    /// When the fill started, in microseconds since the Unix epoch.
    pub started_at: i64,
}

/// The progress cell of a graph: written by the fill under the write lock, read without it.
#[derive(Debug, Default)]
pub(crate) struct ProgressCell {
    running: AtomicBool,
    embedded: AtomicU64,
    total: AtomicU64,
    started_at: AtomicI64,
}

impl ProgressCell {
    pub(crate) fn get(&self) -> Option<FillProgress> {
        self.running.load(Ordering::Acquire).then(|| FillProgress {
            embedded: self.embedded.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            started_at: self.started_at.load(Ordering::Relaxed),
        })
    }

    fn start(&self, total: u64) {
        self.embedded.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.started_at
            .store(crate::graph::undo::now_micros(), Ordering::Relaxed);
        self.running.store(true, Ordering::Release);
    }

    fn add(&self, n: u64) {
        self.embedded.fetch_add(n, Ordering::Relaxed);
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}

/// What a write lock of a graph gives its thread for the rule pass.
#[derive(Clone)]
pub(crate) struct WriterContext {
    /// The graph's journal cell (an in-memory journal is made at the first fill).
    pub(crate) journal: Arc<std::sync::OnceLock<Arc<SystemJournal>>>,
    pub(crate) clock: crate::graph::CommitClock,
    pub(crate) progress: Arc<ProgressCell>,
}

impl WriterContext {
    fn journal(&self) -> Arc<SystemJournal> {
        Arc::clone(
            self.journal
                .get_or_init(|| SystemJournal::in_memory(self.clock.clone())),
        )
    }
}

thread_local! {
    static WRITER: RefCell<Option<WriterContext>> = const { RefCell::new(None) };
}

/// While alive, makes a graph's journal and progress cell the thread's (a write lock of a
/// [`crate::graph::Graph`] holds one); dropping restores the previous ones.
pub(crate) struct AttachedWriter(Option<WriterContext>);

impl AttachedWriter {
    pub(crate) fn new(context: WriterContext) -> Self {
        let previous = WRITER
            .try_with(|current| {
                current
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut current| current.replace(context))
            })
            .ok()
            .flatten();
        AttachedWriter(previous)
    }
}

impl Drop for AttachedWriter {
    fn drop(&mut self) {
        let previous = self.0.take();
        let _ = WRITER.try_with(|current| {
            if let Ok(mut current) = current.try_borrow_mut() {
                *current = previous;
            }
        });
    }
}

fn writer() -> Option<WriterContext> {
    WRITER
        .try_with(|current| current.try_borrow().ok().and_then(|c| c.clone()))
        .ok()
        .flatten()
}

// ---------------------------------------------------------------------------------------------
// The rule pass
// ---------------------------------------------------------------------------------------------

/// An element the pass looks at.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Target {
    Vertex(VertexHandle),
    Edge(EdgeHandle),
}

/// Which rule keys a touched element needs: all its rules, or those of these sources.
enum Touched {
    All,
    Sources(Vec<CompactString>),
}

/// One `to` to write or remove.
struct Item {
    target: Target,
    rule: usize,
    text: Option<String>,
}

/// A failed rule pass: the error and the fill it had opened (its failure record is written
/// after the rollback).
pub(crate) type PassFailure = (GraphError, Option<OpenFill>);

/// A fill that wrote `vector_fill.started` and still owes its end record.
pub(crate) struct OpenFill {
    journal: Option<Arc<SystemJournal>>,
    progress: Option<Arc<ProgressCell>>,
    op: Option<u64>,
    started: web_time::Instant,
    embedded: u64,
    removed: u64,
}

impl OpenFill {
    /// The end record after the unit's commit (`Ok(commit_seq)`) or its rollback.
    pub(crate) fn finish(self, outcome: Result<u64, &GraphError>) {
        if let Some(progress) = &self.progress {
            progress.stop();
        }
        let Some(journal) = &self.journal else {
            return;
        };
        let elapsed = self.started.elapsed().as_millis() as u64;
        match outcome {
            Ok(commit) => {
                let mut event = Event::new(format!(
                    "the vector fill embedded {} elements and removed {} vectors",
                    self.embedded, self.removed
                ))
                .commit(commit)
                .count("embedded", self.embedded)
                .count("removed", self.removed)
                .count("elapsed_ms", elapsed);
                if let Some(op) = self.op {
                    event = event.op(op);
                }
                journal.emit(&catalogue::VECTOR_FILL_DONE, event);
            }
            Err(error) => {
                let mut event = Event::new(format!(
                    "the vector fill failed, nothing was written: {error}"
                ))
                .field("cause", error.to_string())
                .count("elapsed_ms", elapsed)
                .count("embedded_before_failure", self.embedded);
                if let Some(op) = self.op {
                    event = event.op(op);
                }
                journal.emit(&catalogue::VECTOR_FILL_FAILED, event);
            }
        }
    }
}

impl TraversalGraph {
    /// Compiles the vector rules of the current schema (after every change of `self.schema`).
    pub(crate) fn refresh_vector_rules(&mut self) {
        self.vector_rules = VectorRules::compile(&self.schema.read());
    }

    /// The refusal of a direct write or removal of `key` of the vertex `node`: only inside a
    /// live unit with the rules on ([`GraphError::DerivedProperty`]).
    #[inline]
    pub(crate) fn check_derived_vertex(
        &self,
        node: VertexHandle,
        key: &str,
    ) -> Result<(), GraphError> {
        match &self.vector_rules {
            Some(rules) if rules.is_target(key) && self.rules_live() => {
                match self.vertex_weight(node) {
                    Some(vertex) => {
                        rules.refuse(ElementKind::Vertex, self.labels.set(vertex.1), key)
                    }
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// [`Self::check_derived_vertex`] of the edge `edge`.
    #[inline]
    pub(crate) fn check_derived_edge(&self, edge: EdgeHandle, key: &str) -> Result<(), GraphError> {
        match &self.vector_rules {
            Some(rules) if rules.is_target(key) && self.rules_live() => {
                match self.edge_weight(edge) {
                    Some(weight) => {
                        rules.refuse(ElementKind::Edge, self.labels.name_slice(weight.1), key)
                    }
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    /// The refusal for every key of an inserted map.
    #[inline]
    pub(crate) fn check_derived_map(
        &self,
        kind: ElementKind,
        labels: &[CompactString],
        properties: &crate::value::PropertyMap,
    ) -> Result<(), GraphError> {
        match &self.vector_rules {
            Some(rules) if self.rules_live() => properties
                .keys()
                .filter(|key| rules.is_target(key))
                .try_for_each(|key| rules.refuse(kind, labels, key)),
            _ => Ok(()),
        }
    }

    /// Whether the rules act now: inside a live unit, not in `apply_change_set` nor in the rule
    /// pass itself.
    #[inline]
    fn rules_live(&self) -> bool {
        self.rules_off == 0 && self.undo.is_recording() && !self.undo.loading
    }

    /// The vertex handle of `id`.
    fn handle_of_vertex(&self, id: &str) -> Option<VertexHandle> {
        GraphStorage::get_vertex_by_id(self, id).ok().flatten()
    }

    fn handle_of_edge(&self, id: &str) -> Option<EdgeHandle> {
        GraphStorage::get_edge_by_id(self, id).ok().flatten()
    }

    /// The labels of a live target (an unlabeled edge: none).
    fn target_labels(&self, target: Target) -> Vec<CompactString> {
        match target {
            Target::Vertex(handle) => GraphStorage::vertex_labels(self, handle)
                .map(<[CompactString]>::to_vec)
                .unwrap_or_default(),
            Target::Edge(handle) => GraphStorage::edge_label(self, handle)
                .ok()
                .flatten()
                .map(|label| vec![CompactString::from(label)])
                .unwrap_or_default(),
        }
    }

    fn target_properties(&self, target: Target) -> Option<&crate::value::PropertyMap> {
        match target {
            Target::Vertex(handle) => GraphStorage::vertex_properties(self, handle).ok().flatten(),
            Target::Edge(handle) => GraphStorage::edge_properties(self, handle).ok().flatten(),
        }
    }

    /// The rule pass of the outermost unit (module docs). `journal`: a commit (a dry run
    /// journals nothing). Returns the open fill whose end record the commit writes, if the unit
    /// ran a first fill or a model replacement.
    pub(crate) fn run_vector_rules(
        &mut self,
        journal: bool,
    ) -> Result<Option<OpenFill>, PassFailure> {
        let Some(rules) = self.vector_rules.clone() else {
            return Ok(None);
        };
        // 1. Full fills: rules added or changed by a schema change of the unit itself. Each
        // `Schema` entry holds the schema before its change; the schema after it is the next
        // entry's old one (or the current schema). A change written by an applied change set
        // (an exempt range) is skipped: the change set carries its vectors.
        let mut full: Vec<(usize, FillReason)> = Vec::new();
        let exempt = self.undo.rules_exempt().to_vec();
        let in_exempt = |position: usize| exempt.iter().any(|(a, b)| (*a..*b).contains(&position));
        let schemas: Vec<(usize, &GraphSchema)> = self
            .undo
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| match entry {
                UndoEntry::Schema(old) => Some((position, &**old)),
                _ => None,
            })
            .collect();
        if !schemas.is_empty() {
            let current = self.schema.read().clone();
            for (at, (position, before)) in schemas.iter().enumerate() {
                if in_exempt(*position) {
                    continue;
                }
                let after = schemas.get(at + 1).map_or(&current, |(_, next)| *next);
                let (Some(after_rules), old_rules) =
                    (VectorRules::compile(after), VectorRules::compile(before))
                else {
                    continue;
                };
                for rule in after_rules.all() {
                    let previous = old_rules
                        .as_ref()
                        .and_then(|o| o.all().iter().find(|p| p.same_target(rule)));
                    let reason = match previous {
                        Some(previous) if previous.equivalent(rule) => continue,
                        Some(previous)
                            if previous.from == rule.from
                                && previous.model != rule.model
                                && display_model(&previous.model) == display_model(&rule.model) =>
                        {
                            FillReason::ModelReplaced
                        }
                        Some(_) => FillReason::RuleChanged,
                        None => FillReason::RuleAdded,
                    };
                    // Only a rule still in force at the end of the unit is filled.
                    if let Some(index) = rules.all().iter().position(|r| r.equivalent(rule))
                        && !full.iter().any(|(i, _)| *i == index)
                    {
                        full.push((index, reason));
                    }
                }
            }
        }
        // 2. Touched elements.
        let mut touched: HashMap<(ElementKind, CompactString), Touched> = HashMap::new();
        let mut touch = |kind, id: &CompactString, key: Option<&CompactString>| {
            let entry = touched
                .entry((kind, id.clone()))
                .or_insert_with(|| Touched::Sources(Vec::new()));
            match (entry, key) {
                (Touched::All, _) => {}
                (entry @ Touched::Sources(_), None) => *entry = Touched::All,
                (Touched::Sources(keys), Some(key)) => {
                    if !keys.contains(key) {
                        keys.push(key.clone());
                    }
                }
            }
        };
        for (position, entry) in self.undo.entries().iter().enumerate() {
            if in_exempt(position) {
                continue;
            }
            match entry {
                UndoEntry::VertexProperty { id, key, .. } if rules.is_source(key) => {
                    touch(ElementKind::Vertex, id, Some(key));
                }
                UndoEntry::EdgeProperty { id, key, .. } if rules.is_source(key) => {
                    touch(ElementKind::Edge, id, Some(key));
                }
                UndoEntry::AddedVertex { id } | UndoEntry::VertexLabels { id, .. } => {
                    touch(ElementKind::Vertex, id, None);
                }
                UndoEntry::AddedEdge { id } | UndoEntry::EdgeLabel { id, .. } => {
                    touch(ElementKind::Edge, id, None);
                }
                UndoEntry::VertexProperty { .. }
                | UndoEntry::EdgeProperty { .. }
                | UndoEntry::DroppedVertex { .. }
                | UndoEntry::DroppedEdge(_)
                | UndoEntry::Schema(_)
                | UndoEntry::Definition { .. } => {}
            }
        }
        if full.is_empty() && touched.is_empty() {
            return Ok(None);
        }
        // 3. Work items, once per element and rule.
        let mut items: Vec<Item> = Vec::new();
        let mut seen: FnvHashSet<(Target, usize)> = FnvHashSet::default();
        let mut add = |this: &TraversalGraph, target: Target, rule: usize| -> Option<Item> {
            if !seen.insert((target, rule)) {
                return None;
            }
            let text = this
                .target_properties(target)
                .and_then(|p| p.get(rules.all()[rule].from.as_str()))
                .and_then(ElementProperty::text)
                .filter(|t| !t.is_empty())
                .map(|t| t.into_owned());
            Some(Item { target, rule, text })
        };
        let full_count;
        {
            for &(rule_index, _) in &full {
                let rule = &rules.all()[rule_index];
                let targets: Vec<Target> = match rule.kind {
                    ElementKind::Vertex => GraphStorage::vertices_by_label(self, &rule.label)
                        .map_err(|e| (e, None))?
                        .map(Target::Vertex)
                        .collect(),
                    ElementKind::Edge => GraphStorage::edges_by_label(self, &rule.label)
                        .map_err(|e| (e, None))?
                        .map(Target::Edge)
                        .collect(),
                };
                for target in targets {
                    let labels = self.target_labels(target);
                    if rules.applicable(rule.kind, &labels).contains(&rule_index) {
                        items.extend(add(self, target, rule_index));
                    }
                }
            }
            full_count = items.len();
            for ((kind, id), keys) in &touched {
                let target = match kind {
                    ElementKind::Vertex => self.handle_of_vertex(id).map(Target::Vertex),
                    ElementKind::Edge => self.handle_of_edge(id).map(Target::Edge),
                };
                let Some(target) = target else {
                    continue;
                };
                let labels = self.target_labels(target);
                for rule_index in rules.applicable(*kind, &labels) {
                    let wanted = match keys {
                        Touched::All => true,
                        Touched::Sources(sources) => {
                            sources.contains(&rules.all()[rule_index].from)
                        }
                    };
                    if wanted {
                        items.extend(add(self, target, rule_index));
                    }
                }
            }
        }
        if items.is_empty() {
            return Ok(None);
        }
        // 4. The journal record and progress of a first fill or a replacement.
        let context = (!full.is_empty()).then(writer).flatten();
        let mut open = None;
        if !full.is_empty() {
            let texts = items.iter().filter(|i| i.text.is_some()).count() as u64;
            let progress = context.as_ref().map(|c| Arc::clone(&c.progress));
            if let Some(progress) = &progress {
                progress.start(texts);
            }
            let journal_handle = context
                .as_ref()
                .filter(|_| journal)
                .map(WriterContext::journal);
            let op = journal_handle.as_ref().map(|journal| {
                let reason = full
                    .iter()
                    .map(|(_, r)| *r)
                    .max_by_key(|r| match r {
                        FillReason::RuleAdded => 0,
                        FillReason::RuleChanged => 1,
                        FillReason::ModelReplaced => 2,
                    })
                    .unwrap_or(FillReason::RuleAdded);
                let listed = ElementProperty::Array(
                    full.iter()
                        .map(|(index, _)| {
                            let rule = &rules.all()[*index];
                            let mut map = crate::value::PropertyMap::default();
                            map.insert("element", ElementProperty::from(rule.kind.as_str()));
                            map.insert("label", ElementProperty::from(rule.label.as_str()));
                            map.insert("from", ElementProperty::from(rule.from.as_str()));
                            map.insert("to", ElementProperty::from(rule.to.as_str()));
                            map.insert("model", ElementProperty::from(rule.model.as_str()));
                            ElementProperty::Object(Box::new(map))
                        })
                        .collect(),
                );
                let estimate = full
                    .iter()
                    .map(|(index, _)| {
                        let rule = &rules.all()[*index];
                        texts_per_second(&rule.model).map(|tps| {
                            let n = items[..full_count]
                                .iter()
                                .filter(|i| i.rule == *index && i.text.is_some())
                                .count();
                            ((n as f64) / tps * 1000.0).ceil() as u64
                        })
                    })
                    .try_fold(0u64, |sum, ms| ms.map(|ms| sum + ms));
                let mut event = Event::new(format!(
                    "a vector fill started: {texts} elements to embed; the database is locked \
                     until it ends"
                ))
                .field("reason", reason.as_str())
                .field("rules", listed)
                .count("elements", texts);
                if let Some(estimate) = estimate {
                    event = event.count("estimate_ms", estimate);
                }
                if let Some(principal) = self.undo.metadata.principal() {
                    event = event.principal(principal);
                }
                journal.emit_op(&catalogue::VECTOR_FILL_STARTED, event)
            });
            open = Some(OpenFill {
                journal: journal_handle,
                progress,
                op,
                started: web_time::Instant::now(),
                embedded: 0,
                removed: 0,
            });
        }
        match self.embed_items(&rules, items, open.as_mut()) {
            Ok(()) => Ok(open),
            // The caller rolls back and writes the failure record of the open fill.
            Err(error) => Err((error, open)),
        }
    }

    /// Embeds the items in one batch per model (chunks of the embedder's `max_batch`) and
    /// writes or removes every `to`.
    fn embed_items(
        &mut self,
        rules: &VectorRules,
        items: Vec<Item>,
        mut open: Option<&mut OpenFill>,
    ) -> Result<(), GraphError> {
        let mut by_model: BTreeMap<CompactString, Vec<usize>> = BTreeMap::new();
        let mut removals: Vec<usize> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match item.text {
                Some(_) => by_model
                    .entry(rules.all()[item.rule].model.clone())
                    .or_default()
                    .push(index),
                None => removals.push(index),
            }
        }
        let ctx = EmbedContext::of_current_thread();
        // The bytes of the vectors written so far (the memory estimate without a tracking
        // allocator counts elements only).
        let mut written: u64 = 0;
        for (model, indexes) in by_model {
            let first = &rules.all()[items[indexes[0]].rule];
            let fail = |cause: EmbedError| GraphError::VectorRuleFailed {
                label: first.label.to_string(),
                key: first.to.to_string(),
                model: model.to_string(),
                cause: Box::new(cause),
            };
            let Some(embedder) = rule_embedder(&model) else {
                return Err(fail(EmbedError::unavailable(format!(
                    "the model {} is not served here (it is not a model of this store, or its \
                     store is not open)",
                    display_model(&model)
                ))));
            };
            let batch = embedder.info().max_batch.max(1);
            for chunk in indexes.chunks(batch) {
                ctx.check().map_err(fail)?;
                let mut pending: Vec<usize> = chunk.to_vec();
                let vectors = loop {
                    let texts: Vec<&str> = pending
                        .iter()
                        .filter_map(|&i| items[i].text.as_deref())
                        .collect();
                    if texts.is_empty() {
                        break Vec::new();
                    }
                    match checked_embed(embedder.as_ref(), &texts, &ctx) {
                        Ok(vectors) => break vectors,
                        Err(EmbedError::NoFeatures { index }) if index < pending.len() => {
                            removals.push(pending.remove(index));
                        }
                        Err(cause) => return Err(fail(cause)),
                    }
                };
                for (&index, vector) in pending.iter().zip(vectors) {
                    let item = &items[index];
                    let rule = &rules.all()[item.rule];
                    let vector = vector.quantize(rule.element).map_err(|err| {
                        fail(EmbedError::failed(format!(
                            "its vector does not fit {}: {err}",
                            rule.element.name()
                        )))
                    })?;
                    written = written.saturating_add(vector.heap_bytes());
                    self.write_derived(
                        item.target,
                        &rule.to,
                        Some(ElementProperty::Vector(vector)),
                    )?;
                }
                if let Some(open) = open.as_deref_mut() {
                    open.embedded += pending.len() as u64;
                    if let Some(progress) = &open.progress {
                        progress.add(pending.len() as u64);
                    }
                }
                if let Some(budget) = self.undo.budget() {
                    budget.unit_check_adding(&*self, FILL_OPERATION, written)?;
                }
            }
        }
        for index in removals {
            let item = &items[index];
            let to = &rules.all()[item.rule].to;
            if self
                .target_properties(item.target)
                .is_some_and(|p| p.contains_key(to.as_str()))
            {
                self.write_derived(item.target, to, None)?;
                if let Some(open) = open.as_deref_mut() {
                    open.removed += 1;
                }
            }
        }
        Ok(())
    }

    /// Writes (`Some`) or removes (`None`) a rule's `to` with the refusal off.
    fn write_derived(
        &mut self,
        target: Target,
        key: &CompactString,
        value: Option<ElementProperty>,
    ) -> Result<(), GraphError> {
        self.rules_off += 1;
        let written = match (target, value) {
            (Target::Vertex(h), Some(v)) => {
                GraphStorage::set_vertex_property(self, h, key.clone(), v)
            }
            (Target::Edge(h), Some(v)) => GraphStorage::set_edge_property(self, h, key.clone(), v),
            (Target::Vertex(h), None) => GraphStorage::drop_vertex_property(self, key, h),
            (Target::Edge(h), None) => GraphStorage::drop_edge_property(self, key, h),
        };
        self.rules_off -= 1;
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_and_durations_read_like_the_design() {
        assert_eq!(thousands(1_204_331), "1,204,331");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(duration_text(400), "under a second");
        assert_eq!(duration_text(12_300), "about 13 s");
        assert_eq!(duration_text(61_000), "about 2 min");
        assert_eq!(
            display_model("model2vec/minishlab/potion-base-8M@3f2a9c41b7de"),
            "minishlab/potion-base-8M"
        );
        assert_eq!(display_model("hash-ngram/1"), "hash-ngram/1");
    }

    use crate::graph::{Graph, Transactional};
    use crate::value::vector::Vector;
    use crate::value::vector::embed::EmbedderInfo;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    /// `hash-ngram/1` with a call counter, or failing: serves the rule model `hash-ngram/1`
    /// through the test hook.
    struct Probe {
        inner: HashNgram,
        calls: AtomicUsize,
        texts: Mutex<Vec<String>>,
        fail: bool,
    }

    impl Probe {
        fn new(fail: bool) -> Arc<Probe> {
            Arc::new(Probe {
                inner: HashNgram::new(),
                calls: AtomicUsize::new(0),
                texts: Mutex::new(Vec::new()),
                fail,
            })
        }
    }

    impl Embedder for Probe {
        fn info(&self) -> &EmbedderInfo {
            self.inner.info()
        }
        fn embed(&self, texts: &[&str], ctx: &EmbedContext<'_>) -> Result<Vec<Vector>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut seen) = self.texts.lock() {
                seen.extend(texts.iter().map(|t| t.to_string()));
            }
            if self.fail {
                return Err(EmbedError::failed("the probe fails"));
            }
            self.inner.embed(texts, ctx)
        }
    }

    const SCHEMA: &str = r#"{"vertices": {"doc": {"schema": {"type": "object", "properties": {
        "text": {"type": "string"},
        "emb": {"type": "array", "format": "vector", "embedder": "hash-ngram/1", "from": "text"}}}}}}"#;

    fn ruled() -> TraversalGraph {
        let mut graph = TraversalGraph::new();
        graph
            .traversal_mut()
            .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
            .unwrap();
        graph
    }

    fn add_doc(graph: &mut TraversalGraph, text: &str) -> Result<(), crate::TraverserError> {
        graph
            .traversal_mut()
            .add_v("doc")
            .property("text", text)
            .to_list()
            .map(|_| ())
            .map_err(|e| *e)
    }

    fn vectors(graph: &TraversalGraph) -> usize {
        graph
            .traversal()
            .v(None)
            .values("emb")
            .to_list()
            .unwrap()
            .len()
    }

    #[test]
    fn one_batch_per_model_per_unit_and_an_element_written_twice_is_embedded_once() {
        let probe = Probe::new(false);
        let mut graph = ruled();
        with_test_models(vec![probe.clone() as Arc<dyn Embedder>], || {
            graph
                .transaction(|g| -> Result<(), crate::TraverserError> {
                    add_doc(g, "one text")?;
                    add_doc(g, "two text")?;
                    add_doc(g, "three text")?;
                    g.traversal_mut()
                        .v(None)
                        .has("text", "one text")
                        .property("text", "first")
                        .to_list()
                        .map_err(|e| *e)?;
                    g.traversal_mut()
                        .v(None)
                        .has("text", "first")
                        .property("text", "again")
                        .to_list()
                        .map_err(|e| *e)?;
                    Ok(())
                })
                .unwrap();
        });
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            1,
            "one batch for the unit"
        );
        let mut texts = probe.texts.lock().unwrap().clone();
        texts.sort();
        assert_eq!(texts, vec!["again", "three text", "two text"]);
        assert_eq!(vectors(&graph), 3);
    }

    #[test]
    fn a_failing_embedder_rolls_the_unit_back() {
        let mut graph = ruled();
        add_doc(&mut graph, "kept").unwrap();
        let err = with_test_models(vec![Probe::new(true) as Arc<dyn Embedder>], || {
            add_doc(&mut graph, "never stored")
        })
        .unwrap_err();
        assert!(err.to_string().contains("vector rule"), "{err}");
        assert_eq!(graph.vertex_count(), 1, "nothing of the unit was written");
        assert_eq!(vectors(&graph), 1);
    }

    #[test]
    fn replay_and_change_sets_never_call_the_embedder() {
        let mut source = ruled();
        let ((), changes) = source.dry_run(|g| add_doc(g, "replicated text")).unwrap();
        assert!(changes.mutations().iter().any(|m| matches!(
            m,
            crate::graph::Mutation::SetProperty { key, .. } if key == "emb"
        )));
        let probe = Probe::new(false);
        with_test_models(vec![probe.clone() as Arc<dyn Embedder>], || {
            let mut replica = ruled();
            replica.apply_change_set(&changes).unwrap();
            assert_eq!(vectors(&replica), 1);
            let replayed = ruled().replay(&changes).unwrap();
            assert_eq!(vectors(&replayed), 1);
        });
        assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_fill_is_journaled() {
        let mut data = TraversalGraph::new();
        add_doc(&mut data, "a text").unwrap();
        let graph = Graph::new(data);
        let journal = graph.system_journal().unwrap();
        let err = with_test_models(vec![Probe::new(true) as Arc<dyn Embedder>], || {
            graph
                .write()
                .traversal_mut()
                .set_schema(GraphSchema::from_json(SCHEMA).unwrap())
        })
        .unwrap_err();
        assert!(err.to_string().contains("vector rule"), "{err}");
        let records = journal
            .query(&crate::graph::system_journal::JournalQuery::new())
            .unwrap()
            .records;
        let started = records
            .iter()
            .find(|r| r.kind == "vector_fill.started")
            .expect("a start record");
        let failed = records
            .iter()
            .find(|r| r.kind == "vector_fill.failed")
            .expect("a failure record");
        assert_eq!(failed.op, Some(started.seq));
        assert!(
            graph.read().get_schema().unwrap().is_none(),
            "the schema was rolled back"
        );
        assert!(graph.vector_fill_progress().is_none());
    }

    fn fill(estimate_ms: Option<u64>, elements: u64) -> PlannedFill {
        PlannedFill {
            element: ElementKind::Vertex,
            label: "doc".to_string(),
            from: "text".to_string(),
            to: "emb".to_string(),
            model: "model2vec/minishlab/potion-base-8M@3f2a9c41b7de".to_string(),
            reason: FillReason::RuleAdded,
            elements,
            removals: 0,
            estimate_ms,
            approximate: false,
        }
    }

    #[test]
    fn the_threshold_asks_for_confirmation_and_the_warning_reads_like_the_design() {
        assert!(fill(Some(1_000), 5).needs_confirmation());
        assert!(!fill(Some(999), 1_000_000).needs_confirmation());
        assert!(fill(None, 10_000).needs_confirmation());
        assert!(!fill(None, 9_999).needs_confirmation());
        let text = fill_warning(&[fill(Some(61_000), 1_204_331)]).unwrap();
        assert_eq!(
            text,
            "Applying this schema fills vectors: 1,204,331 elements of doc.emb from doc.text \
             with the model minishlab/potion-base-8M (about 2 min).\nThe whole database is \
             LOCKED while this runs: every write and every read waits until it ends.\nRun it at \
             a quiet time. If it fails, nothing is written."
        );
        assert!(
            fill_warning(&[fill(None, 3)])
                .unwrap()
                .contains("(time depends on the model)")
        );
        assert!(fill_warning(&[]).is_none());
    }
}
