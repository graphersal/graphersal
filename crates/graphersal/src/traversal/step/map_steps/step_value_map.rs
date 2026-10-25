use std::sync::Arc;

use crate::graph::GraphStorage;
use crate::traversal::step::prop_path::PathKeys;
use crate::*;

/// The `reason` texts of `TraverserError::InvalidModulator` for `valueMap()`; its `help()` keys
/// on them.
pub(crate) mod reason {
    /// A `by()` kind other than a traversal: `valueMap()` applies `by()` to each property's value
    /// list, and only a child traversal (such as `__.unfold()`) can take a list.
    pub(crate) const KIND: &str =
        "value_map() applies by() to each property's value list; only a traversal can take it";
    /// A second `by()`: `valueMap()` takes one traversal, applied to every property's list.
    pub(crate) const TOO_MANY: &str = "value_map() takes a single by()";
}

/// `valueMap()`: builds a key -> value-list dictionary from a vertex/edge's properties (all, or
/// the named subset when args are given) — **excluding** `id`/`label`, unless the step was built
/// with tokens (`valueMap(true, ..)`, [`value_map_with_tokens`](GraphTraversalSource::value_map_with_tokens)).
/// Without tokens that exclusion is what distinguishes it from [`element_map()`](GraphTraversalSource::element_map), which prepends both
/// and maps each key to its value directly.
///
/// **Cardinality (TinkerPop shape):** every value is wrapped in a list, as in TinkerPop, where a
/// property key can carry several values (`Cardinality.list`/`.set`). This engine stores one value
/// per key, so each list has exactly one element: `{name: [marko], age: [29]}`.
///
/// **`by()` (TinkerPop 3.4+):** each `by()` is a child traversal applied to one property's value
/// list; its first result replaces the list, and a key whose traversal yields nothing is dropped.
/// `valueMap().by(__.unfold())` therefore yields the unwrapped values. Only one `by()` is allowed
/// (a second is `InvalidModulator`, as in TinkerPop), and it applies to every key. Any other `by()` kind (a property
/// key, `T.id`/`T.label`, an order, a JSON path, `Value`/`Count`, a label list) is rejected with
/// `InvalidModulator`.
///
/// **Deviation from TinkerPop 3.8.2:** earlier versions of this step cycled through several
/// `by()` modulators (one per key, in turn); that ring is gone, a second `by()` is rejected
/// (user's guide: "The by() Modulator: Single-by Steps").
struct StepValueMap<'g, S: GraphStorage + 'g> {
    args: Option<Vec<String>>,
    /// The element tokens prepended to the map, a bit set of [`TOKEN_IDS`] and [`TOKEN_LABELS`]:
    /// both for `valueMap(true, ..)` / `with(WithOptions.tokens)`, one for
    /// `with(WithOptions.tokens, WithOptions.ids | labels)`, none by default.
    tokens: u8,
    precomputed_keys: Option<Arc<Vec<RecordKey>>>,
    /// The `jpath(..)` key arguments, if any; `None` for a plain-name call.
    path_keys: Option<PathKeys>,
    /// Every `by()` received, in order. A rejected one is kept (and `by_check` fails the step),
    /// so the plan in an error or `.profile()` shows what was written.
    by: Vec<By<'g, S>>,
    by_check: ByModulatorCheck,
}

impl<'g, S: GraphStorage + 'g> Clone for StepValueMap<'g, S> {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
            tokens: self.tokens,
            precomputed_keys: self.precomputed_keys.clone(),
            path_keys: self.path_keys.clone(),
            by: self.by.clone(),
            by_check: self.by_check.clone(),
        }
    }
}

/// The id token of [`StepValueMap::tokens`].
const TOKEN_IDS: u8 = 1;
/// The label token of [`StepValueMap::tokens`].
const TOKEN_LABELS: u8 = 2;
/// Both tokens.
const TOKENS_ALL: u8 = TOKEN_IDS | TOKEN_LABELS;

impl<'g, S: GraphStorage + 'g> StepValueMap<'g, S> {
    fn new(keys: PropKeys, tokens: bool) -> Self {
        let (args, path_keys) = PathKeys::split(keys);
        let mut step = Self {
            args,
            tokens: if tokens { TOKENS_ALL } else { 0 },
            precomputed_keys: None,
            path_keys,
            by: Vec::new(),
            by_check: ByModulatorCheck::default(),
        };
        step.precompute_keys();
        step
    }

    /// The key list shared by every record of a plain-name call, built once.
    fn precompute_keys(&mut self) {
        let tokens = self.tokens;
        self.precomputed_keys = self
            .args
            .as_ref()
            .filter(|_| self.path_keys.is_none())
            .map(|a| {
                let mut k = Vec::with_capacity(a.len() + 2);
                if tokens & TOKEN_IDS != 0 {
                    k.push(RecordKey::Id);
                }
                if tokens & TOKEN_LABELS != 0 {
                    k.push(RecordKey::Label);
                }
                k.extend(a.iter().map(|s| RecordKey::from(s.as_str())));
                Arc::new(k)
            });
    }

    /// Whether the property `key` is selected by the step's key arguments (all keys without any).
    #[inline]
    fn selects(&self, key: &str) -> bool {
        match &self.args {
            Some(args) => args.iter().any(|arg| arg == key),
            None => true,
        }
    }

    /// Collects the selected keys of one element's properties, each mapped to its value wrapped
    /// in a one-element list (TinkerPop's `valueMap()` shape).
    ///
    /// With `tokens`, `id` and `label` entries come first (`label` only for a labeled element,
    /// like `element_map()`); they are plain values, not lists, and never pass through `by()`.
    fn collect(
        &self,
        id_and_label: Option<(String, Option<&str>)>,
        props: Option<&PropertyMap>,
    ) -> (Vec<RecordKey>, Vec<TraverserValue<S>>, usize) {
        let capacity = props.map(|p| p.len()).unwrap_or(0) + 2;
        let mut keys: Vec<RecordKey> = Vec::with_capacity(capacity);
        let mut values: Vec<TraverserValue<S>> = Vec::with_capacity(capacity);
        if let Some((id, label)) = id_and_label {
            if self.tokens & TOKEN_IDS != 0 {
                keys.push(RecordKey::Id);
                values.push(TraverserValue::Computed(
                    ElementProperty::String(CompactString::new(id)),
                    None,
                ));
            }
            if let Some(label) = label.filter(|_| self.tokens & TOKEN_LABELS != 0) {
                keys.push(RecordKey::Label);
                values.push(TraverserValue::Computed(
                    ElementProperty::String(CompactString::new(label)),
                    None,
                ));
            }
        }
        let token_count = keys.len();
        if let Some(props) = props {
            for (k, v) in props {
                if self.selects(k.as_str()) {
                    keys.push(RecordKey::Name(k.clone()));
                    values.push(TraverserValue::Array(vec![v.to_logical().into()]));
                }
            }
        }
        if let Some(path_keys) = &self.path_keys {
            path_keys.append(props, &mut keys, &mut values, |leaf| {
                TraverserValue::Array(vec![leaf.into()])
            });
        }
        (keys, values, token_count)
    }

    /// Builds the record from the collected keys and value lists: with `by()`, each list is
    /// replaced by the first result of the key's `by()` traversal applied to it, and a key
    /// without a result is dropped.
    fn build(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
        (mut keys, mut values, token_count): (Vec<RecordKey>, Vec<TraverserValue<S>>, usize),
    ) -> TraverserResult<TraverserValue<S>> {
        if !self.by.is_empty() {
            let mut modulated_keys = Vec::with_capacity(keys.len());
            let mut modulated_values = Vec::with_capacity(values.len());
            // The id/label tokens stay as they are: `by()` modulates property values only.
            let mut entries = keys.into_iter().zip(values);
            for (key, value) in entries.by_ref().take(token_count) {
                modulated_keys.push(key);
                modulated_values.push(value);
            }
            for (i, (key, list)) in entries.enumerate() {
                let by = &self.by[i % self.by.len()];
                let By::Traversal(child) = by else {
                    // Unreachable: every other kind is rejected by `by_check` before `build`.
                    return Err(Box::new(TraverserError::InvalidModulator {
                        step: "value_map".to_string(),
                        position: i % self.by.len() + 1,
                        modulator: crate::traversal::by_reason::MODULATOR,
                        reason: reason::KIND.to_string(),
                    }));
                };
                let start = Traverser::with_parts(list, source.path, source.bulk, source.sack);
                if let Some(first) = child.first(&mut *context, &start)? {
                    modulated_keys.push(key);
                    modulated_values.push(first.value);
                }
            }
            keys = modulated_keys;
            values = modulated_values;
        }

        let keys_arc = match &self.precomputed_keys {
            Some(precomputed) if keys == **precomputed => Arc::clone(precomputed),
            _ => Arc::new(keys),
        };
        Ok(TraverserValue::Record {
            keys: keys_arc,
            values,
        })
    }
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepValueMap<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(path_keys) = &self.path_keys {
            if self.tokens == TOKENS_ALL {
                write!(f, "value_map(true, {})", path_keys.render())?;
            } else {
                write!(f, "value_map({})", path_keys.render())?;
            }
        } else if self.tokens == TOKENS_ALL {
            match &self.args {
                Some(args) => write!(
                    f,
                    "value_map(true, {})",
                    display_option_vec_string(&Some(args.clone()))
                )?,
                None => write!(f, "value_map(true)")?,
            }
        } else {
            write!(f, "value_map({})", display_option_vec_string(&self.args))?;
        }
        match self.tokens {
            TOKEN_IDS => write!(f, ".with(WithOptions.tokens, WithOptions.ids)")?,
            TOKEN_LABELS => write!(f, ".with(WithOptions.tokens, WithOptions.labels)")?,
            _ => {}
        }
        for by in &self.by {
            f.write_str(&crate::traversal::render_by(by))?;
        }
        Ok(())
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// A vertex/edge's properties as a key -> value-list dictionary (all, or only the named keys),
    /// excluding `id`/`label`. As in TinkerPop, each value is wrapped in a list
    /// (`{name: [marko], age: [29]}`); `.by(__::unfold())` unwraps them. See this module's own
    /// doc comment for the `by()` rules.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let maps = lock
    ///     .traversal()
    ///     .v("1")
    ///     .value_map(vec!["name", "age"])
    ///     .to_list()
    ///     .unwrap();
    /// match &maps[0] {
    ///     ElementProperty::Object(obj) => {
    ///         assert_eq!(
    ///             obj.get("name"),
    ///             Some(&ElementProperty::Array(vec![ElementProperty::from("marko")]))
    ///         );
    ///         assert!(obj.get("id").is_none());
    ///     }
    ///     _ => panic!("expected an object"),
    /// }
    /// ```
    ///
    /// A key may also be a [`JsonPath`] (`jpath("meta.k")` in the DSL): the
    /// entry's key is the canonical path text (`$.meta.k`), its value the list-wrapped value at the
    /// path; a missing path adds no entry. Plain keys come in element order, then the paths in the
    /// order given.
    pub fn value_map<T: Into<PropKeys>>(self, args: T) -> Self {
        self.value_map_impl(args.into(), false)
    }

    /// `valueMap(true, keys..)`: like [`value_map`](Self::value_map), but each map starts with the
    /// element's `id` and `label` entries (plain values, not lists; `label` is absent for an
    /// unlabeled vertex), exactly as [`element_map`](Self::element_map) builds them. `by()` still
    /// modulates only the property lists, never `id` and `label`.
    ///
    /// **Deviations from TinkerPop:** the token keys are the strings `"id"` and `"label"` (TinkerPop
    /// uses the `T.id`/`T.label` tokens), so a property named `id` or `label` yields a second
    /// entry with the same key, the same limit `element_map()` has. In the script DSL
    /// `valueMap(null)` alone means all keys. See the user's guide page "Property elements".
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let maps = lock.traversal().v("1").value_map_with_tokens(vec!["name"]).to_list().unwrap();
    /// match &maps[0] {
    ///     ElementProperty::Object(obj) => {
    ///         assert_eq!(obj.get("label"), Some(&ElementProperty::from("person")));
    ///         assert!(obj.get("id").is_some());
    ///         assert!(obj.get("age").is_none());
    ///     }
    ///     _ => panic!("expected an object"),
    /// }
    /// ```
    pub fn value_map_with_tokens<T: Into<PropKeys>>(self, args: T) -> Self {
        self.value_map_impl(args.into(), true)
    }

    fn value_map_impl(self, args: PropKeys, tokens: bool) -> Self {
        self.push_step(GraphStep::Map(
            "value_map",
            Box::new(StepValueMap::new(args, tokens)),
        ))
    }
}

impl __ {
    /// Anonymous `value_map()`; see [`GraphTraversalSource::value_map`].
    pub fn value_map<'g, S: GraphStorage + 'g, T: Into<PropKeys>>(
        args: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().value_map(args)
    }

    /// Anonymous `value_map_with_tokens()`; see [`GraphTraversalSource::value_map_with_tokens`].
    pub fn value_map_with_tokens<'g, S: GraphStorage + 'g, T: Into<PropKeys>>(
        args: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().value_map_with_tokens(args)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `value_map()`; see [`GraphTraversalSource::value_map`].
    pub fn value_map<T: Into<PropKeys>>(self, args: T) -> Self {
        self.value_map_impl(args.into(), false)
    }

    /// Anonymous `value_map_with_tokens()`; see [`GraphTraversalSource::value_map_with_tokens`].
    pub fn value_map_with_tokens<T: Into<PropKeys>>(self, args: T) -> Self {
        self.value_map_impl(args.into(), true)
    }

    fn value_map_impl(self, args: PropKeys, tokens: bool) -> Self {
        let step = GraphStep::Map("value_map", Box::new(StepValueMap::new(args, tokens)));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepValueMap<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        match (&self.args, &self.path_keys) {
            (None, None) => visit(None, crate::graph::workload::AccessKind::Read),
            (args, paths) => {
                args.iter()
                    .flatten()
                    .for_each(|key| visit(Some(key), crate::graph::workload::AccessKind::Read));
                for (path, _) in paths.iter().flat_map(|p| p.paths.iter()) {
                    if let Some(crate::value::json_path::JsonPathSegment::Key(key)) =
                        path.segments().first()
                    {
                        visit(Some(key), crate::graph::workload::AccessKind::Read);
                    }
                }
            }
        }
        for key in self.by.iter().filter_map(By::property_key) {
            visit(Some(key), crate::graph::workload::AccessKind::Read);
        }
    }
    /// Builds an aggregation/materialization buffer, like `element_map()` — reported to
    /// `.profile()` as a non-zero `bytes_materialized`.
    fn materializes(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.by_check.check("value_map")?;
        let value = match &source.value {
            TraverserValue::Vertex(node_index) => {
                let head = if self.tokens != 0 {
                    let id =
                        crate::graph::storage::stored_vertex_id(&*context.storage, *node_index)?
                            .to_string();
                    Some((id, context.storage.vertex_label(*node_index)))
                } else {
                    None
                };
                let collected = self.collect(head, context.storage.vertex_properties(*node_index));
                self.build(context, &source, collected)?
            }
            TraverserValue::Edge(edge_index) => {
                let head = if self.tokens != 0 {
                    let id = crate::graph::storage::stored_edge_id(&*context.storage, *edge_index)?
                        .to_string();
                    Some((id, context.storage.edge_label(*edge_index)))
                } else {
                    None
                };
                let collected = self.collect(head, context.storage.edge_properties(*edge_index));
                self.build(context, &source, collected)?
            }
            v => {
                return Err(Box::new(TraverserError::cast_exception(
                    &["vertex", "edge"],
                    v,
                )));
            }
        };
        Ok(Traverser::with_parts(
            value,
            source.path,
            source.bulk,
            source.sack,
        ))
    }

    /// `with(WithOptions.tokens[, ids | labels | all | none | keys | values])`: which element
    /// tokens the map includes (all without a value). `keys`/`values` name the tokens of a
    /// vertex-property map in TinkerPop; this step reads vertices and edges, so they add none.
    fn push_with_option(&mut self, key: WithOptions, value: Option<WithOptions>) -> Option<String> {
        if key != WithOptions::Tokens {
            return Some(format!(
                "{key} is not an option of value_map(), which takes WithOptions.tokens"
            ));
        }
        self.tokens = match value {
            None | Some(WithOptions::All) => TOKENS_ALL,
            Some(WithOptions::Ids) => TOKEN_IDS,
            Some(WithOptions::Labels) => TOKEN_LABELS,
            Some(WithOptions::None | WithOptions::Keys | WithOptions::Values) => 0,
            Some(
                other @ (WithOptions::Tokens
                | WithOptions::Indexer
                | WithOptions::List
                | WithOptions::Map),
            ) => {
                return Some(format!(
                    "{other} is not a value of WithOptions.tokens (ids, labels, all, none)"
                ));
            }
        };
        self.precompute_keys();
        None
    }

    fn push_modulator_by(&mut self, args: By<'g, S>, _modulator_name: &str) -> ByModulation {
        self.by_check.observe(&args);
        match &args {
            By::Traversal(_) | By::Labels(_) => {}
            By::Property(_)
            | By::Order(_)
            | By::Label
            | By::Id
            | By::Value
            | By::Identity
            | By::Key
            | By::Count
            | By::JsonPath(_)
            | By::Column(_) => self.by_check.reject_last(reason::KIND),
        }
        if !self.by.is_empty() {
            self.by_check.reject_last(reason::TOO_MANY);
        }
        self.by.push(args);
        ByModulation::Accepted
    }

    fn child_traversals_mut(&mut self) -> Vec<&mut crate::traversal::AnonymousTraversal<'g, S>> {
        self.by.iter_mut().filter_map(By::traversal_mut).collect()
    }

    fn analyze_nested_paths(
        &mut self,
        analysis: &crate::traversal::optimizer::path_analysis::PathAnalysis,
        _downstream: &crate::traversal::optimizer::path_analysis::PathRequirement,
    ) -> crate::traversal::optimizer::path_analysis::PathRequirement {
        // `by(__...)` children run on a list that continues the incoming traverser's path.
        By::analyze_all_paths(&mut self.by, analysis)
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    /// A one-element list holding `value`, the shape `valueMap()` gives every property.
    fn list(value: impl Into<ElementProperty>) -> ElementProperty {
        ElementProperty::Array(vec![value.into()])
    }

    /// `g.V("1").valueMap("name", "age")`: the correct key -> value-list dict, with `id`/`label`
    /// excluded and each value wrapped in a one-element list (TinkerPop's cardinality shape).
    #[test]
    fn person_value_map_excludes_id_label_list_wrapped() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(vec!["name", "age"]).to_list(),
            rhai: [
                r#"g.V("1").valueMap(["name", "age"])"#,
                r#"g.v("1").value_map(["name", "age"])"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.len(), 2);
                        assert_eq!(obj.get("name"), Some(&list("marko")));
                        assert_eq!(obj.get("age"), Some(&list(29i64)));
                        assert!(obj.get("id").is_none());
                        assert!(obj.get("label").is_none());
                    }
                    _ => panic!("expected an object"),
                }
            },
        );
    }

    #[test]
    fn value_map_no_args_returns_all_properties() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("3").value_map(None).to_list(),
            rhai: [
                r#"g.V("3").valueMap()"#,
                r#"g.v("3").value_map()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.len(), 2);
                        assert_eq!(obj.get("name"), Some(&list("lop")));
                        assert_eq!(obj.get("lang"), Some(&list("java")));
                    }
                    _ => panic!("expected an object"),
                }
            },
        );
    }

    #[test]
    fn edge_value_map() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.e("1").value_map(None).to_list(),
            rhai: [
                r#"g.E("1").valueMap()"#,
                r#"g.e("1").value_map()"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.len(), 1);
                        assert_eq!(obj.get("weight"), Some(&list(0.4f64)));
                    }
                    _ => panic!("expected an object"),
                }
            },
        );
    }

    /// `valueMap().by(__.unfold())` (TinkerPop 3.4+): the `by()` traversal runs on each value
    /// list, so `unfold()` yields the plain values.
    #[test]
    fn by_unfold_unwraps_values() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").value_map(None).by(__::unfold()).to_list(),
            rhai: [
                r#"g.V("1").valueMap().by(__.unfold())"#,
                r#"g.v("1").value_map().by(__.unfold())"#,
            ],
            assert: |res| {
                match &res[0] {
                    ElementProperty::Object(obj) => {
                        assert_eq!(obj.len(), 2);
                        assert_eq!(obj.get("name"), Some(&ElementProperty::from("marko")));
                        assert_eq!(obj.get("age").and_then(|v| v.as_i64()), Some(29));
                    }
                    _ => panic!("expected an object"),
                }
            },
        );
    }

    #[test]
    fn profile_no_args_renders_empty_parens() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").value_map(None).profile().unwrap();
        assert!(result.to_string().contains("value_map()"));
        assert!(!result.to_string().contains("(None)"));
    }

    /// `valueMap()` materializes (`materializes() == true`), like `element_map()`: it must report
    /// a non-zero `bytes_materialized` under `.profile_with(ProfileType::Memory)`.
    #[test]
    fn profile_memory_reports_materialization() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let g = lock.traversal();
        let stats = g
            .v("1")
            .value_map(None)
            .profile_with(ProfileType::Memory)
            .unwrap();
        #[allow(unused_variables)]
        let node = stats
            .metrics()
            .iter()
            .find(|n| n.name().contains("value_map"))
            .expect("a value_map() node is present");
        #[cfg(not(feature = "alloc-tracking"))]
        {
            assert!(node.memory().unwrap().bytes_materialized() > 0);
        }
    }
}
