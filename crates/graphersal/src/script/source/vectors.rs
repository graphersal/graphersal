//! Builder methods of the vector steps (`as_vector`, `quantize`, `similarity`, `nearest`,
//! `vector_mean`; book page "Vectors").

use crate::script::tokens::ScriptVector;
use crate::script::*;
use crate::value::{Vector, VectorError, VectorMetric};

/// The script error of a vector argument in `step`.
fn vector_failure(step: &'static str, error: VectorError) -> Box<rhai::EvalAltResult> {
    script_error(crate::value::vector::vector_error(step, error))
}

/// The query vector argument: a traversal becomes a child traversal, a vector or a list of
/// numbers a constant vector, a string a text query (embedded with the model the schema declares
/// for the property); anything else fails when the step is built.
fn query_arg(
    ctx: &NativeCallContext,
    step: &'static str,
    arg: Dynamic,
) -> Result<ScriptVectorQuery, Box<rhai::EvalAltResult>> {
    if arg.is::<ScriptGraphTraversalSource>() {
        return Ok(ScriptVectorQuery::Traversal(try_dynamic_to_gts(arg)?));
    }
    if let Some(vector) = arg.read_lock::<ScriptVector>() {
        return Ok(ScriptVectorQuery::Value(vector.0.clone()));
    }
    if let Some(text) = arg.read_lock::<rhai::ImmutableString>() {
        return Ok(ScriptVectorQuery::Text(text.to_string()));
    }
    if arg.is_array() {
        let ElementProperty::Array(items) = value_arg(ctx, arg)? else {
            return Err(vector_failure(
                step,
                VectorError::NotAVector {
                    found: "a list".to_string(),
                },
            ));
        };
        return Vector::from_values(&items)
            .map(ScriptVectorQuery::Value)
            .map_err(|error| vector_failure(step, error));
    }
    Err(vector_failure(
        step,
        VectorError::NotAVector {
            found: type_name_of(&arg),
        },
    ))
}

/// A metric name, or the script error naming the metrics.
fn metric_arg(step: &'static str, name: &str) -> Result<VectorMetric, Box<rhai::EvalAltResult>> {
    VectorMetric::from_name(name)
        .ok_or_else(|| vector_failure(step, VectorError::unknown_metric(name)))
}

/// Whether a script value names a property (a string or a `jpath(..)`).
fn is_key(arg: &Dynamic) -> bool {
    arg.is_string() || arg.is::<crate::value::JsonPath>()
}

impl ScriptGraphTraversalSource {
    /// Converts a list of numbers to an f32 vector (a vector passes). Rhai binding of
    /// `as_vector()`.
    pub fn as_vector(&mut self) -> Self {
        self.add_step(Step::AsVector)
    }

    /// Converts a vector to the element kind `kind` (`"f32"`, `"f16"`, `"i8"`, `"bit"`); an
    /// unknown kind fails when the step is built. Rhai binding of `quantize(kind)`.
    pub fn quantize(&mut self, kind: &str) -> Result<Self, Box<rhai::EvalAltResult>> {
        let element = crate::script::tokens::vector_element_of("quantize", kind)?;
        Ok(self.add_step(Step::Quantize(element)))
    }

    /// The score of the incoming vector value against `q`. Rhai binding of `similarity(q)`.
    pub fn similarity(
        ctx: NativeCallContext,
        this: &mut Self,
        q: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let query = query_arg(&ctx, "similarity", q)?;
        Ok(this.add_step(Step::Similarity(None, query, None)))
    }

    /// `similarity(key, q)` (a string or `jpath(..)` first) or `similarity(q, metric)`. Rhai
    /// binding of the two-argument overload.
    pub fn similarity_2(
        ctx: NativeCallContext,
        this: &mut Self,
        first: Dynamic,
        second: Dynamic,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        if is_key(&first) {
            let query = query_arg(&ctx, "similarity", second)?;
            let key = dynamic_to_prop_key("similarity", first)?;
            return Ok(this.add_step(Step::Similarity(Some(key), query, None)));
        }
        let query = query_arg(&ctx, "similarity", first)?;
        let Some(name) = second
            .read_lock::<rhai::ImmutableString>()
            .map(|s| s.to_string())
        else {
            return Err(script_error_from_script(
                crate::error::ScriptError::ArgumentMismatch {
                    step_name: "similarity".to_string(),
                    expected: "a metric name (\"cosine\", \"dot\", \"l2\", \"hamming\")"
                        .to_string(),
                    actual: type_name_of(&second),
                },
            ));
        };
        let metric = metric_arg("similarity", &name)?;
        Ok(this.add_step(Step::Similarity(None, query, Some(metric))))
    }

    /// The score of the element's vector property `key` against `q` with `metric`. Rhai
    /// binding of `similarity(key, q, metric)`.
    pub fn similarity_3(
        ctx: NativeCallContext,
        this: &mut Self,
        key: Dynamic,
        q: Dynamic,
        metric: &str,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let query = query_arg(&ctx, "similarity", q)?;
        let metric = metric_arg("similarity", metric)?;
        let key = dynamic_to_prop_key("similarity", key)?;
        Ok(this.add_step(Step::Similarity(Some(key), query, Some(metric))))
    }

    /// The `k` incoming elements closest to `q`. Rhai binding of `nearest(key, q, k)`.
    pub fn nearest(
        ctx: NativeCallContext,
        this: &mut Self,
        key: Dynamic,
        q: Dynamic,
        k: i64,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        Self::nearest_with(ctx, this, key, q, k, rhai::Map::new())
    }

    /// The `k` incoming elements closest to `q` with options (`#{metric: "l2"}`). Rhai binding
    /// of `nearest(key, q, k, options)`.
    pub fn nearest_with(
        ctx: NativeCallContext,
        this: &mut Self,
        key: Dynamic,
        q: Dynamic,
        k: i64,
        options: rhai::Map,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        let k = usize::try_from(k).map_err(|_| {
            script_error_from_script(crate::error::ScriptError::ArgumentMismatch {
                step_name: "nearest".to_string(),
                expected: "a number of results k >= 0".to_string(),
                actual: k.to_string(),
            })
        })?;
        let mut metric = None;
        for (name, value) in options {
            match name.as_str() {
                "metric" => {
                    let Some(text) = value
                        .read_lock::<rhai::ImmutableString>()
                        .map(|s| s.to_string())
                    else {
                        return Err(script_error_from_script(
                            crate::error::ScriptError::ArgumentMismatch {
                                step_name: "nearest".to_string(),
                                expected:
                                    "a metric name (\"cosine\", \"dot\", \"l2\", \"hamming\")"
                                        .to_string(),
                                actual: type_name_of(&value),
                            },
                        ));
                    };
                    metric = Some(metric_arg("nearest", &text)?);
                }
                other => {
                    return Err(script_error_from_script(
                        crate::error::ScriptError::ArgumentMismatch {
                            step_name: "nearest".to_string(),
                            expected: "an option map with the key metric".to_string(),
                            actual: format!(
                                "the key {}",
                                crate::value::literal::rhai_string(other)
                            ),
                        },
                    ));
                }
            }
        }
        let query = query_arg(&ctx, "nearest", q)?;
        let key = dynamic_to_prop_key("nearest", key)?;
        Ok(this.add_step(Step::Nearest(key, query, k, metric)))
    }

    /// The component-wise mean of the incoming vectors. Rhai binding of `vector_mean()`.
    pub fn vector_mean(&mut self) -> Self {
        self.add_step(Step::VectorMean)
    }

    /// The incoming string embedded by the registry's default embedder. Rhai binding of
    /// `embed()`.
    pub fn embed(&mut self) -> Self {
        self.add_step(Step::Embed(None, None))
    }

    /// `embed(key)` (a string or `jpath(..)`: the element's property) or `embed(#{embedder})`
    /// (the incoming string). Rhai binding of the one-argument overload.
    pub fn embed_1(&mut self, arg: Dynamic) -> Result<Self, Box<rhai::EvalAltResult>> {
        if let Some(options) = arg.read_lock::<rhai::Map>() {
            let embedder = embed_options(&options)?;
            return Ok(self.add_step(Step::Embed(None, embedder)));
        }
        if is_key(&arg) {
            return Ok(self.add_step(Step::Embed(Some(dynamic_to_prop_key("embed", arg)?), None)));
        }
        Err(script_error_from_script(
            crate::error::ScriptError::ArgumentMismatch {
                step_name: "embed".to_string(),
                expected: "a property key (a string or jpath(..)) or an option map \
                           #{embedder: \"name\"}"
                    .to_string(),
                actual: type_name_of(&arg),
            },
        ))
    }

    /// The element's property `key` embedded by the embedder the options name. Rhai binding of
    /// `embed(key, #{embedder})`.
    pub fn embed_2(
        &mut self,
        key: Dynamic,
        options: rhai::Map,
    ) -> Result<Self, Box<rhai::EvalAltResult>> {
        if !is_key(&key) {
            return Err(script_error_from_script(
                crate::error::ScriptError::ArgumentMismatch {
                    step_name: "embed".to_string(),
                    expected: "a property key (a string or jpath(..))".to_string(),
                    actual: type_name_of(&key),
                },
            ));
        }
        let embedder = embed_options(&options)?;
        Ok(self.add_step(Step::Embed(
            Some(dynamic_to_prop_key("embed", key)?),
            embedder,
        )))
    }

    /// `g.embedders()`: the embedders of the session, one map each (`name`, `model`,
    /// `dimension`, `element`, `metric`, `max_batch`, `deterministic`, `local`), the default
    /// first. Asks nothing: it names what the host registered, it runs none of them.
    pub fn embedders(&mut self) -> rhai::Array {
        embedder_list(&self.access.embedders)
    }

    /// `g.embed_property(#{label, from, to, element?, embedder?, only_missing?, batch?})` /
    /// `g.embed_property(..)`: the job that embeds the property `from` of every element with
    /// `label` into its vector property `to`, page by page (book page "Vectors"). Returns the
    /// report map. Asks `Read` and `Update` on the label's data, `Execute` on the embedder and,
    /// when the job declares the property in the schema, `Update` on `Schema`.
    pub fn embed_property(
        &mut self,
        options: rhai::Map,
    ) -> Result<rhai::Map, Box<rhai::EvalAltResult>> {
        let mut job_options = EmbedPropertyOptions::parse(&options)?;
        let embedder = self
            .access
            .embedders
            .resolve(job_options.embedder.as_deref())
            .map_err(|err| script_error(Box::new(crate::TraverserError::Embed(err))))?
            .clone();
        let element = Some(job_options.element);
        let label = job_options.label.clone();
        for action in [crate::graph::Action::Read, crate::graph::Action::Update] {
            self.require(
                "embed_property",
                crate::graph::AccessRequest::new(
                    action,
                    crate::graph::Resource::data(element, Some(label.as_str())),
                ),
            )?;
        }
        self.require(
            "embed_property",
            crate::graph::AccessRequest::new(
                crate::graph::Action::Execute,
                crate::graph::Resource::Embedder {
                    name: embedder.info().name.as_str(),
                },
            ),
        )?;
        let restrictions = self.restrictions();
        let mut job = crate::value::EmbedJob::new(
            job_options.label.clone(),
            job_options.from.clone(),
            job_options.to.clone(),
        )
        .element(job_options.element)
        .embedder(embedder.info().name.clone())
        .only_missing(job_options.only_missing);
        if let Some(batch) = job_options.batch.take() {
            job = job.batch(batch);
        }
        job.schema_update = Some(std::sync::Arc::new(move || {
            restrictions.authorize(
                "embed_property",
                &crate::graph::AccessRequest::new(
                    crate::graph::Action::Update,
                    crate::graph::Resource::Schema,
                ),
            )
        }));
        let graph = self.schema_graph("embed_property")?;
        let cancel = crate::traversal::cancel::CancelToken::current().unwrap_or_default();
        let report = graph
            .embed_property(&job, &self.access.embedders, &cancel, &mut |_| {})
            .map_err(|failed| script_error(failed.error))?;
        let count = |n: u64| Dynamic::from_int(i64::try_from(n).unwrap_or(i64::MAX));
        let mut map = rhai::Map::new();
        map.insert("embedded".into(), count(report.embedded));
        map.insert("skipped_no_text".into(), count(report.skipped_no_text));
        map.insert("not_text".into(), count(report.not_text));
        map.insert("skipped_changed".into(), count(report.skipped_changed));
        map.insert("skipped_removed".into(), count(report.skipped_removed));
        map.insert("units".into(), count(report.units));
        map.insert(
            "schema_changed".into(),
            Dynamic::from_bool(report.schema_changed),
        );
        map.insert(
            "elapsed_ms".into(),
            count(u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX)),
        );
        map.insert(
            "embedder".into(),
            Dynamic::from(embedder.info().name.to_string()),
        );
        map.insert(
            "model".into(),
            Dynamic::from(embedder.info().model.to_string()),
        );
        Ok(map)
    }
}

/// The option map of `g.embed_property(..)`.
struct EmbedPropertyOptions {
    label: CompactString,
    from: PropKey,
    to: CompactString,
    element: crate::graph::schema::ElementKind,
    embedder: Option<CompactString>,
    only_missing: bool,
    batch: Option<usize>,
}

impl EmbedPropertyOptions {
    fn parse(options: &rhai::Map) -> Result<Self, Box<rhai::EvalAltResult>> {
        let mismatch = |expected: &str, actual: String| {
            script_error_from_script(crate::error::ScriptError::ArgumentMismatch {
                step_name: "embed_property".to_string(),
                expected: expected.to_string(),
                actual,
            })
        };
        let text = |key: &str, value: &Dynamic| {
            value
                .read_lock::<rhai::ImmutableString>()
                .map(|s| CompactString::new(s.as_str()))
                .ok_or_else(|| mismatch(&format!("{key}: a string"), type_name_of(value)))
        };
        let (mut label, mut from, mut to) = (None, None, None);
        let mut parsed = EmbedPropertyOptions {
            label: CompactString::default(),
            from: PropKey::Name(CompactString::default()),
            to: CompactString::default(),
            element: crate::graph::schema::ElementKind::Vertex,
            embedder: None,
            only_missing: true,
            batch: None,
        };
        for (name, value) in options {
            match name.as_str() {
                "label" => label = Some(text("label", value)?),
                "to" => to = Some(text("to", value)?),
                "from" => {
                    if !is_key(value) {
                        return Err(mismatch(
                            "from: a property key (a string or jpath(..))",
                            type_name_of(value),
                        ));
                    }
                    from = Some(dynamic_to_prop_key("embed_property", value.clone())?);
                }
                "element" => {
                    parsed.element = match text("element", value)?.as_str() {
                        "vertex" => crate::graph::schema::ElementKind::Vertex,
                        "edge" => crate::graph::schema::ElementKind::Edge,
                        other => {
                            return Err(mismatch(
                                "element: \"vertex\" or \"edge\"",
                                crate::value::literal::rhai_string(other),
                            ));
                        }
                    }
                }
                "embedder" => parsed.embedder = Some(text("embedder", value)?),
                "only_missing" => {
                    parsed.only_missing = value
                        .as_bool()
                        .map_err(|_| mismatch("only_missing: a bool", type_name_of(value)))?;
                }
                "batch" => {
                    let n = value
                        .as_int()
                        .ok()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n >= 1)
                        .ok_or_else(|| {
                            mismatch("batch: a number of elements >= 1", value.to_string())
                        })?;
                    parsed.batch = Some(n);
                }
                other => {
                    return Err(mismatch(
                        "an option map with the keys label, from, to, element, embedder, \
                         only_missing, batch",
                        format!("the key {}", crate::value::literal::rhai_string(other)),
                    ));
                }
            }
        }
        match (label, from, to) {
            (Some(label), Some(from), Some(to)) if !label.is_empty() && !to.is_empty() => {
                parsed.label = label;
                parsed.from = from;
                parsed.to = to;
                Ok(parsed)
            }
            _ => Err(mismatch(
                "the keys label, from and to: #{label: \"doc\", from: \"text\", to: \"emb\"}",
                "an option map without them".to_string(),
            )),
        }
    }
}

/// The embedders of `registry` as script maps, the default first.
fn embedder_list(registry: &crate::value::Embedders) -> rhai::Array {
    let default = registry.default_embedder().info().name.clone();
    let mut infos: Vec<&crate::value::EmbedderInfo> = registry.infos().collect();
    infos.sort_by_key(|info| info.name != default);
    infos
        .into_iter()
        .map(|info| {
            let mut map = rhai::Map::new();
            map.insert("name".into(), Dynamic::from(info.name.to_string()));
            map.insert("model".into(), Dynamic::from(info.model.to_string()));
            map.insert(
                "dimension".into(),
                Dynamic::from_int(i64::try_from(info.dimension).unwrap_or(i64::MAX)),
            );
            map.insert(
                "element".into(),
                Dynamic::from(info.element.name().to_string()),
            );
            map.insert(
                "metric".into(),
                Dynamic::from(info.metric.name().to_string()),
            );
            map.insert(
                "max_batch".into(),
                Dynamic::from_int(i64::try_from(info.max_batch).unwrap_or(i64::MAX)),
            );
            map.insert(
                "deterministic".into(),
                Dynamic::from_bool(info.deterministic),
            );
            map.insert("local".into(), Dynamic::from_bool(info.local));
            map.insert("default".into(), Dynamic::from_bool(info.name == default));
            Dynamic::from_map(map)
        })
        .collect()
}

/// The embedder an option map of `embed` names (`#{embedder: "name"}`; `None`: the default);
/// any other key, or a name that is not a string, fails.
pub(crate) fn embed_options(
    options: &rhai::Map,
) -> Result<Option<CompactString>, Box<rhai::EvalAltResult>> {
    let mut embedder = None;
    for (name, value) in options {
        match name.as_str() {
            "embedder" => {
                let Some(text) = value
                    .read_lock::<rhai::ImmutableString>()
                    .map(|s| CompactString::new(s.as_str()))
                else {
                    return Err(script_error_from_script(
                        crate::error::ScriptError::ArgumentMismatch {
                            step_name: "embed".to_string(),
                            expected: "an embedder name (g.embedders() lists them)".to_string(),
                            actual: type_name_of(value),
                        },
                    ));
                };
                embedder = Some(text);
            }
            other => {
                return Err(script_error_from_script(
                    crate::error::ScriptError::ArgumentMismatch {
                        step_name: "embed".to_string(),
                        expected: "an option map with the key embedder".to_string(),
                        actual: format!("the key {}", crate::value::literal::rhai_string(other)),
                    },
                ));
            }
        }
    }
    Ok(embedder)
}

/// The DSL function `embed(text[, #{embedder}])`: a vector for a query, computed by Rhai before
/// any traversal runs (no graph lock). Asks `Execute` on the embedder through the session's
/// access, like the `embed` step.
pub(crate) fn script_embed(
    ctx: &NativeCallContext,
    text: &str,
    options: Option<&rhai::Map>,
) -> Result<ScriptVector, Box<rhai::EvalAltResult>> {
    let requested = match options {
        Some(options) => embed_options(options)?,
        None => None,
    };
    let access = crate::script::access::ScriptAccess::of_context(ctx);
    let embedder = access
        .embedders
        .resolve(requested.as_deref())
        .map_err(|err| script_error(Box::new(crate::TraverserError::Embed(err))))?
        .clone();
    access
        .restrictions(false)
        .authorize(
            "embed",
            &crate::graph::AccessRequest::new(
                crate::graph::Action::Execute,
                crate::graph::Resource::Embedder {
                    name: embedder.info().name.as_str(),
                },
            ),
        )
        .map_err(script_error)?;
    let context = crate::value::EmbedContext::of_current_thread();
    let mut vectors = crate::value::vector::embed::checked_embed(&*embedder, &[text], &context)
        .map_err(|err| {
            script_error(Box::new(match err {
                crate::value::EmbedError::Cancelled => {
                    crate::TraverserError::Cancelled { elapsed_ms: 0 }
                }
                other => crate::TraverserError::Embed(other),
            }))
        })?;
    match vectors.pop() {
        Some(vector) => Ok(ScriptVector(vector)),
        None => Err(script_error(Box::new(crate::TraverserError::Embed(
            crate::value::EmbedError::bad_output("no vector for the text"),
        )))),
    }
}
