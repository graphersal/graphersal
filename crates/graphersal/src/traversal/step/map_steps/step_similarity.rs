//! `similarity(..)`: the vector similarity map step, and the parts `nearest()` shares with it
//! (the query vector, the property key, the metric resolution). Graphersal extensions, book page
//! "Vectors".

use std::borrow::Cow;

use crate::graph::GraphStorage;
use crate::traversal::optimizer::path_analysis::{PathAnalysis, PathRequirement};
use crate::traversal::step::map_steps::step_embed::embed_failure;
use crate::traversal::step::map_steps::temporal::{describe, held_value, replaced};
use crate::traversal::step::prop_path::peek_path;
use crate::value::json_path::JsonPathSegment;
use crate::value::vector::embed::provenance::{TextCandidate, text_query_candidates};
use crate::value::vector::vector_error;
use crate::value::{Vector, VectorError, VectorMetric};
use crate::*;

/// The query vector of `similarity()` / `nearest()`: a constant, a child traversal whose first
/// result (a vector, or a list of numbers made an f32 vector) is the query, or a text embedded
/// with the embedder of the model the attached schema declares for the step's property.
///
/// Built from a [`Vector`], an anonymous traversal (`__::select("q")`) or a string through
/// `From`, so the step methods take `impl Into<VectorQuery>`.
#[non_exhaustive]
pub enum VectorQuery<'g, S: GraphStorage + 'g> {
    /// A constant query vector.
    Value(Vector),
    /// A child traversal run on the incoming traverser (path included); its first result is
    /// the query vector.
    Traversal(AnonymousTraversal<'g, S>),
    /// A text, embedded once per execution with the registered embedder whose model the
    /// attached schema declares (`embedder`) for the step's property on the elements met.
    Text(TextQuery),
}

impl<'g, S: GraphStorage + 'g> Clone for VectorQuery<'g, S> {
    fn clone(&self) -> Self {
        match self {
            VectorQuery::Value(vector) => VectorQuery::Value(vector.clone()),
            VectorQuery::Traversal(traversal) => VectorQuery::Traversal(traversal.clone()),
            VectorQuery::Text(text) => VectorQuery::Text(text.clone()),
        }
    }
}

impl<'g, S: GraphStorage + 'g> From<&str> for VectorQuery<'g, S> {
    fn from(text: &str) -> Self {
        VectorQuery::Text(TextQuery::new(text))
    }
}

impl<'g, S: GraphStorage + 'g> From<String> for VectorQuery<'g, S> {
    fn from(text: String) -> Self {
        VectorQuery::Text(TextQuery::new(text))
    }
}

/// A text query of `similarity()` / `nearest()` (book page "Vectors", section "Text
/// queries"): before the run (`bind`) the candidate embedders come from the attached schema's
/// `embedder` declarations of the step's property; during it the first element met picks the
/// model, the text is embedded once and every later element must declare the same model.
pub struct TextQuery {
    text: String,
    /// Bound by `run()` before the permission check.
    candidates: Vec<TextCandidate>,
    /// The candidate chosen, the label that chose it and the embedded text (per execution).
    resolved: parking_lot::Mutex<Option<Resolved>>,
    /// Set when `bind` refused it, so the error is located at this step.
    bind_failed: bool,
}

/// A resolved text query: the candidate, the label that chose it, the embedded text and its
/// bit quantization (made when a `bit` property needs it).
type Resolved = (usize, CompactString, Vector, Option<Vector>);

impl Clone for TextQuery {
    fn clone(&self) -> Self {
        TextQuery {
            text: self.text.clone(),
            candidates: self.candidates.clone(),
            resolved: parking_lot::Mutex::new(None),
            bind_failed: false,
        }
    }
}

impl TextQuery {
    fn new(text: impl Into<String>) -> Self {
        TextQuery {
            text: text.into(),
            candidates: Vec::new(),
            resolved: parking_lot::Mutex::new(None),
            bind_failed: false,
        }
    }

    /// Binds the candidate embedders for the property `key` of `step` from the attached
    /// `schema` and the `registry` (refused without an attached schema, a top-level name or a
    /// declared model).
    pub(crate) fn bind(
        &mut self,
        step: &str,
        key: Option<&PropKey>,
        schema: Option<&crate::graph::schema::GraphSchema>,
        registry: &crate::value::Embedders,
    ) -> Result<(), crate::value::EmbedError> {
        let bound = self.bind_inner(step, key, schema, registry);
        self.bind_failed = bound.is_err();
        bound
    }

    /// Whether `bind` refused this query.
    pub(crate) fn bind_failed(&self) -> bool {
        self.bind_failed
    }

    fn bind_inner(
        &mut self,
        step: &str,
        key: Option<&PropKey>,
        schema: Option<&crate::graph::schema::GraphSchema>,
        registry: &crate::value::Embedders,
    ) -> Result<(), crate::value::EmbedError> {
        let name = match key {
            Some(PropKey::Name(name)) => name,
            Some(path @ PropKey::Path(_)) => {
                return Err(crate::value::EmbedError::TextQueryKey {
                    found: crate::value::literal::rhai_prop_key(path),
                });
            }
            None => {
                return Err(crate::value::EmbedError::TextQueryKey {
                    found: "the incoming value (no key)".to_string(),
                });
            }
        };
        self.candidates = text_query_candidates(schema, step, name, registry)?;
        Ok(())
    }

    /// The registry names of the bound embedders (the permission check asks `Execute` on each).
    pub(crate) fn embedder_names(&self) -> impl Iterator<Item = &str> {
        self.candidates
            .iter()
            .filter_map(|c| c.embedder.as_ref().map(|e| e.info().name.as_str()))
    }

    /// The query vector for the element `value` whose property `key` is searched: the text
    /// embedded with the embedder of the model the schema declares there (embedded once; a
    /// later element declaring another model fails).
    fn vector_for<S: GraphStorage>(
        &self,
        context: &TraverserContext<'_, S>,
        key: &VectorKey,
        value: &TraverserValue<S>,
    ) -> TraverserResult<Vector> {
        use crate::value::EmbedError;
        let quoted = |text: &str| crate::value::literal::rhai_string(text);
        let name = match &key.key {
            PropKey::Name(name) => name.as_str(),
            PropKey::Path(_) => "",
        };
        let (declared, label) = match value {
            TraverserValue::Vertex(node) => {
                let labels = context.storage.vertex_labels(*node);
                (
                    context
                        .storage
                        .declared_property_schema(GraphNodeType::Vertex, labels, name),
                    labels.first().cloned().unwrap_or_default(),
                )
            }
            TraverserValue::Edge(edge) => {
                let label = CompactString::new(context.storage.edge_label(*edge).unwrap_or(""));
                (
                    context.storage.declared_property_schema(
                        GraphNodeType::Edge,
                        std::slice::from_ref(&label),
                        name,
                    ),
                    label,
                )
            }
            _ => (None, CompactString::default()),
        };
        let declared = declared.and_then(|node| node.vector);
        // A bit property compares only with bit vectors: the query is quantized like a write.
        let bit = declared
            .as_ref()
            .is_some_and(|vector| vector.element == crate::value::VectorElement::Bit);
        let Some(model) = declared.and_then(|vector| vector.embedder) else {
            return Err(Box::new(TraverserError::Embed(
                EmbedError::NoDeclaredEmbedder {
                    key: quoted(name),
                    on: if label.is_empty() {
                        " on a value without a label".to_string()
                    } else {
                        format!(" of {}", quoted(&label))
                    },
                },
            )));
        };
        let Some(index) = self.candidates.iter().position(|c| c.model == model) else {
            return Err(Box::new(TraverserError::Embed(
                EmbedError::ModelNotRegistered {
                    model: quoted(&model),
                    key: quoted(name),
                },
            )));
        };
        let mut resolved = self.resolved.lock();
        if let Some((first, first_label, vector, bits)) = resolved.as_mut() {
            if *first == index {
                if !bit {
                    return Ok(vector.clone());
                }
                if bits.is_none() {
                    *bits = Some(Self::to_bits(vector)?);
                }
                return Ok(bits.clone().unwrap_or_else(|| vector.clone()));
            }
            return Err(Box::new(TraverserError::Embed(EmbedError::ModelConflict {
                key: quoted(name),
                first: format!(
                    "{} on {}",
                    quoted(&self.candidates[*first].model),
                    quoted(first_label)
                ),
                second: format!("{} on {}", quoted(&model), quoted(&label)),
            })));
        }
        let Some(embedder) = self.candidates[index].embedder.as_ref() else {
            return Err(Box::new(TraverserError::Embed(
                EmbedError::ModelNotRegistered {
                    model: quoted(&model),
                    key: quoted(name),
                },
            )));
        };
        let ctx = crate::value::EmbedContext::of_deadline(context.deadline.as_ref());
        let vector = crate::value::vector::embed::checked_embed(&**embedder, &[&self.text], &ctx)
            .map_err(|error| embed_failure(context, error))?
            .pop()
            .ok_or_else(|| {
                Box::new(TraverserError::Embed(EmbedError::bad_output(
                    "no vector for the query text",
                )))
            })?;
        let bits = if bit {
            Some(Self::to_bits(&vector)?)
        } else {
            None
        };
        let answer = bits.clone().unwrap_or_else(|| vector.clone());
        *resolved = Some((index, label, vector, bits));
        Ok(answer)
    }

    /// The query quantized to bits for a property declared `element: bit` (the quantizer a
    /// schema write uses).
    fn to_bits(vector: &Vector) -> TraverserResult<Vector> {
        vector
            .quantize(crate::value::VectorElement::Bit)
            .map_err(|error| vector_error("nearest", error))
    }
}

impl fmt::Display for TextQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::value::literal::rhai_string(&self.text))
    }
}

impl<'g, S: GraphStorage + 'g> VectorQuery<'g, S> {
    /// The rendering suffix naming the embedder(s) a text query resolved (`, embedder:
    /// "hash-ngram/1"`), empty for any other query: the profile shows what runs.
    pub(crate) fn embedder_suffix(&self) -> String {
        let VectorQuery::Text(text) = self else {
            return String::new();
        };
        let names: Vec<String> = text
            .embedder_names()
            .map(crate::value::literal::rhai_string)
            .collect();
        match names.len() {
            0 => String::new(),
            1 => format!(", embedder: {}", names[0]),
            _ => format!(", embedder: [{}]", names.join(", ")),
        }
    }
}

impl<'g, S: GraphStorage + 'g> From<Vector> for VectorQuery<'g, S> {
    fn from(vector: Vector) -> Self {
        VectorQuery::Value(vector)
    }
}

impl<'g, S: GraphStorage + 'g> From<&Vector> for VectorQuery<'g, S> {
    fn from(vector: &Vector) -> Self {
        VectorQuery::Value(vector.clone())
    }
}

impl<'g, S: GraphStorage + 'g> From<AnonymousTraversal<'g, S>> for VectorQuery<'g, S> {
    fn from(traversal: AnonymousTraversal<'g, S>) -> Self {
        VectorQuery::Traversal(traversal)
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for VectorQuery<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VectorQuery::Value(vector) => f.write_str(&vector.literal(Some(16))),
            VectorQuery::Traversal(traversal) => write!(f, "{traversal}"),
            VectorQuery::Text(text) => write!(f, "{text}"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> VectorQuery<'g, S> {
    /// The query vector for `source`: the constant, the first result of the traversal run on
    /// `source`, or the text embedded for the element's property `key` (`None`: the step has
    /// no key, which `bind` refused already).
    pub(crate) fn evaluate(
        &self,
        step: &'static str,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        key: Option<&VectorKey>,
    ) -> TraverserResult<Vector> {
        match self {
            VectorQuery::Value(vector) => Ok(vector.clone()),
            VectorQuery::Text(text) => match key {
                Some(key) => text.vector_for(context, key, &source.value),
                None => Err(Box::new(TraverserError::Embed(
                    crate::value::EmbedError::TextQueryKey {
                        found: "the incoming value (no key)".to_string(),
                    },
                ))),
            },
            VectorQuery::Traversal(traversal) => {
                let Some(first) = traversal.first(context, source)? else {
                    return Err(vector_error(
                        step,
                        VectorError::NotAVector {
                            found: "no result (the query traversal yielded nothing)".to_string(),
                        },
                    ));
                };
                vector_of(step, context, &first.value)
            }
        }
    }

    pub(crate) fn traversal_mut(&mut self) -> Option<&mut AnonymousTraversal<'g, S>> {
        match self {
            VectorQuery::Value(_) | VectorQuery::Text(_) => None,
            VectorQuery::Traversal(traversal) => Some(traversal),
        }
    }

    /// What the query traversal reads from the incoming path (it runs on the incoming
    /// traverser; its output does not continue the path).
    pub(crate) fn analyze_paths(&mut self, analysis: &PathAnalysis) -> PathRequirement {
        match self {
            VectorQuery::Value(_) | VectorQuery::Text(_) => PathRequirement::None,
            VectorQuery::Traversal(traversal) => {
                traversal.analyze_paths(analysis, PathRequirement::None)
            }
        }
    }
}

/// The vector a traverser value holds (a list of numbers becomes an f32 vector); anything else
/// fails.
pub(crate) fn vector_of<S: GraphStorage>(
    step: &'static str,
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> TraverserResult<Vector> {
    let held = match value {
        TraverserValue::Array(_) => Ok(Cow::Owned(value.into_node_property(context.storage)?)),
        _ => held_value(context, value)?,
    };
    match held {
        Ok(held) => match held.as_ref() {
            ElementProperty::Vector(vector) => Ok(vector.clone()),
            ElementProperty::Array(items) => {
                Vector::from_values(items).map_err(|error| vector_error(step, error))
            }
            other => Err(vector_error(
                step,
                VectorError::NotAVector {
                    found: describe(other),
                },
            )),
        },
        Err(kind) => Err(vector_error(
            step,
            VectorError::NotAVector {
                found: format!("a {kind}"),
            },
        )),
    }
}

/// The property key of a vector search: a name or a jpath, with its path segments.
#[derive(Clone)]
pub(crate) struct VectorKey {
    key: PropKey,
    segments: Vec<JsonPathSegment>,
}

impl VectorKey {
    /// The top-level property the vector lives in.
    pub(crate) fn top_level(&self) -> Option<&str> {
        self.key.top_level()
    }

    pub(crate) fn new(key: PropKey) -> Self {
        let segments = match &key {
            PropKey::Name(name) => vec![JsonPathSegment::Key(name.clone())],
            PropKey::Path(path) => path.segments().to_vec(),
        };
        VectorKey { key, segments }
    }

    /// The property key.
    pub(crate) fn prop_key(&self) -> &PropKey {
        &self.key
    }

    /// The key as a Rhai literal (`"emb"`, `jpath("$.a.emb")`).
    pub(crate) fn literal(&self) -> String {
        crate::value::literal::rhai_prop_key(&self.key)
    }

    /// Applies `f` to the stored value of the key on `value` (`None`: the element has no such
    /// property); a value that holds no properties at all is a cast error.
    pub(crate) fn peek<S: GraphStorage, R>(
        &self,
        context: &TraverserContext<'_, S>,
        value: &TraverserValue<S>,
        f: impl FnOnce(&ElementProperty) -> R,
    ) -> TraverserResult<Option<R>> {
        peek_path(context, value, &self.segments, f)
    }

    /// The metric the stored schema declares for this key on the element `value` (the
    /// `metric` annotation of a vector declaration), for a top-level property name only.
    pub(crate) fn declared_metric<S: GraphStorage>(
        &self,
        storage: &S,
        value: &TraverserValue<S>,
    ) -> Option<VectorMetric> {
        match &self.key {
            PropKey::Name(name) => element_metric(storage, value, name),
            PropKey::Path(_) => None,
        }
    }
}

/// The metric the stored schema declares for the property `name` of the element `value`
/// (vertex or edge), by the element's labels (the first label declaring it, as the write path
/// coerces).
fn element_metric<S: GraphStorage>(
    storage: &S,
    value: &TraverserValue<S>,
    name: &str,
) -> Option<VectorMetric> {
    let declared = match value {
        TraverserValue::Vertex(node) => storage.declared_property_schema(
            GraphNodeType::Vertex,
            storage.vertex_labels(*node),
            name,
        ),
        TraverserValue::Edge(edge) => {
            let label = CompactString::new(storage.edge_label(*edge)?);
            storage.declared_property_schema(GraphNodeType::Edge, &[label], name)
        }
        _ => None,
    }?;
    declared.vector.and_then(|vector| vector.metric)
}

/// The metric the stored schema declares for the property a property reference
/// (`values("emb")`) points to.
fn reference_metric<S: GraphStorage>(
    storage: &S,
    value: &TraverserValue<S>,
) -> Option<VectorMetric> {
    match value {
        TraverserValue::VertexProperty(name, node, _) => {
            element_metric(storage, &TraverserValue::Vertex(*node), name)
        }
        TraverserValue::EdgeProperty(name, edge, _) => {
            element_metric(storage, &TraverserValue::Edge(*edge), name)
        }
        _ => None,
    }
}

/// The metric argument as a Rhai literal (`"l2"`).
fn metric_literal(metric: VectorMetric) -> String {
    crate::value::literal::rhai_string(metric.name())
}

/// `similarity(..)`: the score of the incoming element's vector property (or of the incoming
/// vector value) against the query vector, as a float64 (module docs of
/// [`crate::value::vector::similarity`]: cosine and dot are similarities, l2 and hamming
/// distances).
struct StepSimilarity<'g, S: GraphStorage + 'g> {
    /// `None`: the incoming value is the vector (`values("emb").similarity(q)`).
    key: Option<VectorKey>,
    query: VectorQuery<'g, S>,
    /// `None`: the schema's `metric` annotation of the property, else cosine.
    metric: Option<VectorMetric>,
}

impl<'g, S: GraphStorage + 'g> Clone for StepSimilarity<'g, S> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            query: self.query.clone(),
            metric: self.metric,
        }
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepSimilarity<'g, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("similarity(")?;
        if let Some(key) = &self.key {
            write!(f, "{}, ", key.literal())?;
        }
        write!(f, "{}", self.query)?;
        if let Some(metric) = self.metric {
            write!(f, ", {}", metric_literal(metric))?;
        }
        f.write_str(&self.query.embedder_suffix())?;
        f.write_str(")")
    }
}

fn similarity_step<'g, S: GraphStorage + 'g>(
    key: Option<PropKey>,
    query: VectorQuery<'g, S>,
    metric: Option<VectorMetric>,
) -> GraphStep<'g, S> {
    GraphStep::Map(
        "similarity",
        Box::new(StepSimilarity {
            key: key.map(VectorKey::new),
            query,
            metric,
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepSimilarity<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        if let Some(key) = self.key.as_ref().and_then(VectorKey::top_level) {
            visit(Some(key), crate::graph::workload::AccessKind::Similarity);
        }
    }
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.process_filter(context, source)?.ok_or_else(|| {
            Box::new(TraverserError::QueryExecution {
                query: self.to_string(),
                reason: "similarity() dropped a traverser in a context that cannot filter"
                    .to_string(),
            })
        })
    }

    fn process_filter(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        // The stored vector first: an element without it is dropped before the query runs.
        let stored = match &self.key {
            Some(key) => {
                match key.peek(context, &source.value, |stored| match stored {
                    ElementProperty::Vector(vector) => Some(vector.clone()),
                    _ => None,
                })? {
                    Some(Some(vector)) => vector,
                    // No such property, or a value that is no vector: dropped (as nearest()
                    // skips it).
                    Some(None) | None => return Ok(None),
                }
            }
            None => match held_value(context, &source.value)? {
                Ok(held) => match held.as_ref() {
                    ElementProperty::Null => return Ok(None),
                    ElementProperty::Vector(vector) => vector.clone(),
                    other => {
                        return Err(vector_error(
                            "similarity",
                            VectorError::NotAVector {
                                found: describe(other),
                            },
                        ));
                    }
                },
                Err(kind) => {
                    return Err(vector_error(
                        "similarity",
                        VectorError::NotAVector {
                            found: format!("a {kind}"),
                        },
                    ));
                }
            },
        };
        let query = self
            .query
            .evaluate("similarity", context, &source, self.key.as_ref())?;
        let metric = match self.metric {
            Some(metric) => metric,
            None => match &self.key {
                Some(key) => key.declared_metric(context.storage, &source.value),
                None => reference_metric(context.storage, &source.value),
            }
            .unwrap_or(VectorMetric::Cosine),
        };
        context.tick_n(1 + stored.dimension() / 1024)?;
        let score = stored
            .similarity(&query, metric)
            .map_err(|error| vector_error("similarity", error))?;
        Ok(Some(replaced(
            source,
            TraverserValue::Computed(ElementProperty::from(score), None),
        )))
    }

    fn may_filter(&self) -> bool {
        true
    }

    fn text_query_mut(&mut self) -> Option<(Option<&PropKey>, &mut TextQuery)> {
        match &mut self.query {
            VectorQuery::Text(text) => Some((self.key.as_ref().map(|key| &key.key), text)),
            _ => None,
        }
    }

    fn text_query(&self) -> Option<&TextQuery> {
        match &self.query {
            VectorQuery::Text(text) => Some(text),
            _ => None,
        }
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &PathAnalysis,
        _downstream: &PathRequirement,
    ) -> PathRequirement {
        self.query.analyze_paths(analysis)
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut AnonymousTraversal<'g, S>> {
        self.query.traversal_mut().into_iter().collect()
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `similarity(key, q[, metric])`: the score of the incoming element's vector property
    /// `key` (a name or a [`JsonPath`](crate::value::JsonPath)) against the query vector `q`, as
    /// a float64. A Graphersal extension (book page "Vectors").
    ///
    /// `q` is a [`Vector`] or an anonymous traversal run on each incoming traverser (its first
    /// result is the query; a list of numbers becomes an f32 vector). `metric` `None` takes the
    /// `metric` annotation the stored schema declares for the property (a top-level name on a
    /// vertex or edge), else cosine. Cosine and dot are similarities (higher is closer), l2
    /// and hamming distances (smaller is closer). An element without the property, or whose
    /// value is not a vector, is dropped; a dimension mismatch, a bit vector against another
    /// kind and hamming on non-bit vectors fail.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::empty();
    /// let mut lock = graph.write();
    /// let g = lock.traversal_mut();
    /// let emb = Vector::from_f32(vec![1.0, 0.0]).unwrap();
    /// g.add_v("doc").property("emb", emb).iterate().unwrap();
    /// let g = lock.traversal();
    /// let q = Vector::from_f32(vec![1.0, 0.0]).unwrap();
    /// let score = g.v(None).similarity("emb", q, None).next().unwrap().unwrap();
    /// assert_eq!(score.as_f64(), Some(1.0));
    /// ```
    pub fn similarity(
        self,
        key: impl Into<PropKey>,
        q: impl Into<VectorQuery<'g, S>>,
        metric: Option<VectorMetric>,
    ) -> Self {
        self.push_step(similarity_step(Some(key.into()), q.into(), metric))
    }

    /// `similarity(q[, metric])`: the score of the incoming vector value (`values("emb")`, a
    /// computed vector) against `q`, as in [`similarity`](Self::similarity); a property
    /// reference takes the schema's `metric` annotation of its property. `null` is dropped,
    /// any other value that is not a vector fails.
    pub fn similarity_value(
        self,
        q: impl Into<VectorQuery<'g, S>>,
        metric: Option<VectorMetric>,
    ) -> Self {
        self.push_step(similarity_step(None, q.into(), metric))
    }
}

impl __ {
    /// Anonymous `similarity(key, q[, metric])`; see [`GraphTraversalSource::similarity`].
    pub fn similarity<'g, S: GraphStorage + 'g>(
        key: impl Into<PropKey>,
        q: impl Into<VectorQuery<'g, S>>,
        metric: Option<VectorMetric>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().similarity(key, q, metric)
    }

    /// Anonymous `similarity(q[, metric])`; see [`GraphTraversalSource::similarity_value`].
    pub fn similarity_value<'g, S: GraphStorage + 'g>(
        q: impl Into<VectorQuery<'g, S>>,
        metric: Option<VectorMetric>,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().similarity_value(q, metric)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `similarity(key, q[, metric])`; see [`GraphTraversalSource::similarity`].
    pub fn similarity(
        self,
        key: impl Into<PropKey>,
        q: impl Into<VectorQuery<'g, S>>,
        metric: Option<VectorMetric>,
    ) -> Self {
        self.push_step(similarity_step(Some(key.into()), q.into(), metric))
    }

    /// `similarity(q[, metric])`; see [`GraphTraversalSource::similarity_value`].
    pub fn similarity_value(
        self,
        q: impl Into<VectorQuery<'g, S>>,
        metric: Option<VectorMetric>,
    ) -> Self {
        self.push_step(similarity_step(None, q.into(), metric))
    }
}
