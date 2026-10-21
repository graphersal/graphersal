//! Lowering of the script step AST to engine traversals: `ast_convert_to_gts` (a rooted
//! `GraphTraversalSource`) and `ast_convert_to_anonymous` (a child `AnonymousTraversal`).
//!
//! Both go through the one lowering in [`ScriptGraphTraversalSource::lower`], which builds the
//! steps with the `AnonymousTraversal` builders; a new step is lowered there and nowhere else.
//! The root and a child differ only in three places: the root takes over the source steps
//! (`withSack`, `withSideEffect`; in a child they become
//! [`crate::error::TraverserError::SourceStepInChild`]), a `mergeV()`/`mergeE()` that starts the
//! root runs once, and a modulator with no step before it names `g` instead of `__`.

use super::*;
use crate::traversal::step::sideeffect_steps::merge_map::MergeInput;

/// A label/id list argument: empty is no argument (`out()`), otherwise the list.
fn strings(arg: &[CompactString]) -> ArgStrings {
    if arg.is_empty() {
        ArgStrings::None
    } else {
        ArgStrings::List(arg.iter().map(|x| x.to_string()).collect())
    }
}

impl ScriptMergeArg {
    /// The engine input of a `mergeV(..)`/`mergeE(..)` argument.
    fn lower<'g, S: GraphStorage + 'g>(&self) -> MergeInput<'g, S> {
        match self {
            ScriptMergeArg::Entries(entries) => MergeInput::Entries(entries.clone()),
            ScriptMergeArg::Traversal(gts) => MergeInput::Traversal(gts.ast_convert_to_anonymous()),
            ScriptMergeArg::Value(value) => MergeInput::Value(value.clone()),
        }
    }
}

impl ScriptGraphTraversalSource {
    /// Converts this script-level step pipeline into the real Rust [`GraphTraversalSource`] over
    /// `storage`, ready to execute. The result carries the session's authorizer: its execution
    /// asks it for every request of the plan as written before anything runs (see
    /// [`Authorizer`](crate::graph::Authorizer)).
    pub fn ast_convert_to_gts<'g, S: GraphStorage + 'g>(
        &self,
        storage: S,
    ) -> GraphTraversalSource<'g, S> {
        let mut graph = GraphTraversalSource::new(storage)
            .restricted(self.restrictions())
            .with_embedders(self.access.embedders.clone());
        // The host's presets reach every source of the session (`g`, `_g`, `__`, the
        // `GraphSource::x()` constructors), whichever way the source was created; the query's own
        // `with(..)` of a key replaces the preset.
        let mut options = (*self.access.presets).clone();
        options.extend(self.options.iter().map(|(k, v)| (k.clone(), v.clone())));
        graph.options = options;
        for step in &self.steps {
            if let Step::Source(source) = step {
                graph = source.apply(graph);
            }
        }
        #[cfg(feature = "gql")]
        let (graph, start) = self.gql_start(graph);
        #[cfg(not(feature = "gql"))]
        let start = AnonymousTraversal::new();
        graph.adopt_lowered(self.lower_onto(true, start))
    }

    /// The root hook of `gql()`: a leading `Step::Gql` (source steps may come before it) is
    /// lowered by [`GraphTraversalSource::gql_with`], which needs the storage (the planner reads
    /// its statistics and index catalog); its steps start the body [`Self::lower_onto`]
    /// continues, so the Gremlin steps written after `gql()` follow them. Every creating step
    /// of a GQL write (`gql:insert_node`, `gql:insert_edge`, in `CALL` bodies too) carries the
    /// evaluation's `now()` like `add_v` (design C8 of stage 3a), and the source is pinned to
    /// it (`with_now`), so the GQL clocks read the same instant (3g.T10).
    #[cfg(feature = "gql")]
    pub(super) fn gql_start<'g, S: GraphStorage + 'g>(
        &self,
        mut graph: GraphTraversalSource<'g, S>,
    ) -> (GraphTraversalSource<'g, S>, AnonymousTraversal<'g, S>) {
        let mut start = AnonymousTraversal::new();
        if let Some((query, params)) = self.leading_gql() {
            // The GQL clocks (`datetime()`, `date()`, `X.statement()`) read the execution's
            // pinned time: the evaluation's `now()`, so a query's `datetime()` equals the
            // script's `now()` (3g.T10).
            if let Some(now) = self.now {
                graph = graph.with_now(now);
            }
            graph = graph.gql_with(query.as_str(), params.iter().cloned());
            start.steps = std::mem::take(&mut graph.steps);
            if let Some(now) = self.now {
                stamp_create_times(&mut start.steps, now);
            }
        }
        (graph, start)
    }

    /// The query and parameters of a `gql()` that starts the root traversal (source steps may
    /// come before it).
    #[cfg(feature = "gql")]
    pub(super) fn leading_gql(
        &self,
    ) -> Option<(&CompactString, &[(CompactString, ElementProperty)])> {
        let leading = self
            .steps
            .iter()
            .find(|step| !matches!(step, Step::Source(_)));
        match leading {
            Some(Step::Gql(query, params)) => Some((query, params.as_slice())),
            _ => None,
        }
    }

    /// Converts this script-level step pipeline into an [`AnonymousTraversal`], for use as a
    /// child traversal (e.g. inside `.by(...)`). A source step in it (`__.withSack(1)`) fails
    /// with [`crate::error::TraverserError::SourceStepInChild`] before execution.
    pub fn ast_convert_to_anonymous<'g, S: GraphStorage + 'g>(&self) -> AnonymousTraversal<'g, S> {
        self.lower(false)
    }

    /// The one lowering of the step AST, for the root traversal (`root`; its source steps are
    /// applied by [`Self::ast_convert_to_gts`]) and for a child traversal alike.
    pub(super) fn lower<'g, S: GraphStorage + 'g>(&self, root: bool) -> AnonymousTraversal<'g, S> {
        self.lower_onto(root, AnonymousTraversal::new())
    }

    /// [`Self::lower`] continuing `graph`, the steps a root hook already built (`gql()`).
    pub(super) fn lower_onto<'g, S: GraphStorage + 'g>(
        &self,
        root: bool,
        mut graph: AnonymousTraversal<'g, S>,
    ) -> AnonymousTraversal<'g, S> {
        let steps = self.fold_connectives(root);
        // The position of the root's start step (source steps come first; folding connectives
        // keeps it in place): a `gql()` there was lowered by `gql_start`.
        #[cfg(feature = "gql")]
        let leading = steps
            .iter()
            .find(|step| !matches!(step, Step::Source(_)))
            .filter(|_| root);
        for step in steps.iter() {
            graph = match step {
                Step::Source(source) => {
                    if root {
                        graph
                    } else {
                        graph.source_step_in_child(source.name())
                    }
                }
                Step::V(arg) => graph.v(strings(arg)),
                Step::E(arg) => graph.e(strings(arg)),
                Step::Count => graph.count(),
                Step::Limit(arg) => graph.limit(*arg),
                Step::Label => graph.label(),
                Step::Labels => graph.labels(),
                Step::Id => graph.id(),
                Step::Identity => graph.identity(),
                Step::Sack => graph.sack(),
                Step::SackWith(operator) => graph.sack_with(*operator),
                Step::InferSchema => graph.infer_schema(),
                Step::Inject(vec) => graph.inject(vec.clone()),
                Step::Cap(vec) => graph.cap(vec.clone()),
                Step::Aggregate(key) => graph.aggregate(key.as_str()),
                Step::Store(key) => graph.store(key.as_str()),
                Step::Tree(key) => match key {
                    Some(k) => graph.tree(k.as_str()),
                    None => graph.tree(ArgStrings::None),
                },
                Step::Fold => graph.fold(),
                Step::FoldWith(seed, operator) => graph.fold_with(seed.clone(), *operator),
                Step::Unfold => graph.unfold(),
                Step::Group => graph.group(),
                Step::GroupCount => graph.group_count(),
                Step::GroupSideEffect(key) => graph.group_side_effect(key),
                Step::GroupCountSideEffect(key) => graph.group_count_side_effect(key),
                Step::Order => graph.order(),
                Step::By(arg_by) => match arg_by {
                    ArgBy::Property(prop) => graph.by(prop),
                    ArgBy::Traversal(gts) => graph.by(gts.ast_convert_to_anonymous()),
                    ArgBy::Label => graph.by(By::Label),
                    ArgBy::Id => graph.by(By::Id),
                    ArgBy::Value => graph.by(By::Value),
                    ArgBy::Key => graph.by(By::Key),
                    ArgBy::Count => graph.by(By::Count),
                    ArgBy::JsonPath(path) => graph.by(By::JsonPath(path.clone())),
                    ArgBy::Order(order) => graph.by(order.clone()),
                    ArgBy::Labels(labels) => graph.by(By::Labels(labels.clone())),
                    ArgBy::Column(column) => graph.by(By::Column(*column)),
                },
                Step::Max => graph.max(),
                Step::Mean => graph.mean(),
                Step::Median => graph.median(),
                Step::Min => graph.min(),
                Step::Sum => graph.sum(),
                Step::CountScoped(scope) => graph.count_scoped(*scope),
                Step::SumScoped(scope) => graph.sum_scoped(*scope),
                Step::MinScoped(scope) => graph.min_scoped(*scope),
                Step::MaxScoped(scope) => graph.max_scoped(*scope),
                Step::MeanScoped(scope) => graph.mean_scoped(*scope),
                Step::Coin(val) => graph.coin(*val),
                Step::Fail(message) => graph.fail(message.as_deref()),
                Step::Sample(scope, n) => graph.sample_scoped(*scope, *n),
                Step::HasId(vec) => graph.has_id(vec),
                Step::HasKey(vec) => graph.has_key(vec),
                Step::HasLabel(vec) => graph.has_label(vec),
                Step::HasAnyLabel => graph.has_label(None),
                Step::HasIdP(arg_p) => graph.has_id_p(argp_to_p(arg_p)),
                Step::HasKeyP(arg_p) => graph.has_key_p(argp_to_p(arg_p)),
                Step::HasLabelP(arg_p) => graph.has_label_p(argp_to_p(arg_p)),
                Step::HasValue(element_property) => graph.has_value(element_property.clone()),
                Step::HasValueAny(values) => graph.has_value_any(values.clone()),
                Step::HasValueP(arg_p) => graph.has_value_p(argp_to_p(arg_p)),
                Step::HasP(prop, arg_p) => graph.has_p(prop.clone(), argp_to_p(arg_p)),
                Step::Has(prop, element_property) => {
                    graph.has(prop.clone(), element_property.clone())
                }
                Step::HasProperty(key) => graph.has_property(key.clone()),
                Step::HasTraversal(key, arg) => {
                    graph.has_traversal(key.clone(), arg.ast_convert_to_anonymous())
                }
                Step::HasNot(key) => graph.has_not(key.clone()),
                Step::Discard => graph.discard(),
                Step::Dedup => graph.dedup(),
                Step::Barrier(None) => graph.barrier(),
                Step::Barrier(Some(n)) => graph.barrier_with_size(*n),
                Step::BarrierWith(barrier) => graph.barrier_with(*barrier),
                Step::Skip(arg) => graph.skip(*arg),
                Step::Tail(arg) => graph.tail(*arg),
                Step::Range(low, high) => graph.range(*low, *high),
                Step::LimitScoped(scope, n) => graph.limit_scoped(*scope, *n),
                Step::SkipScoped(scope, n) => graph.skip_scoped(*scope, *n),
                Step::TailScoped(scope, n) => graph.tail_scoped(*scope, *n),
                Step::RangeScoped(scope, low, high) => graph.range_scoped(*scope, *low, *high),
                Step::OrderScoped(scope) => graph.order_scoped(*scope),
                Step::DedupScoped(scope, labels) => {
                    graph.dedup_scoped_labels(*scope, labels.iter().cloned())
                }
                Step::Values(vec) => {
                    if vec.is_empty() {
                        graph.values(None)
                    } else {
                        graph.values(vec.clone())
                    }
                }
                Step::Project(vec) => graph.project(vec),
                Step::ElementMap(vec) => {
                    if vec.is_empty() {
                        graph.element_map(None)
                    } else {
                        graph.element_map(vec.clone())
                    }
                }
                Step::Properties(vec) => {
                    if vec.is_empty() {
                        graph.properties(None)
                    } else {
                        graph.properties(vec.clone())
                    }
                }
                Step::Value => graph.value(),
                Step::Key => graph.key(),
                Step::ValueMap(vec, tokens) => match (vec.is_empty(), *tokens) {
                    (true, false) => graph.value_map(None),
                    (false, false) => graph.value_map(vec.clone()),
                    (true, true) => graph.value_map_with_tokens(None),
                    (false, true) => graph.value_map_with_tokens(vec.clone()),
                },
                Step::Math(arg) => graph.math(arg),
                Step::Out(arg) => graph.out(strings(arg)),
                Step::In(arg) => graph.in_(strings(arg)),
                Step::Element => graph.element(),
                Step::Both(arg) => graph.both(strings(arg)),
                Step::BothE(arg) => graph.both_e(strings(arg)),
                Step::InE(arg) => graph.in_e(strings(arg)),
                Step::InV => graph.in_v(),
                Step::OutE(arg) => graph.out_e(strings(arg)),
                Step::OutV => graph.out_v(),
                Step::BothV => graph.both_v(),
                Step::OtherV => graph.other_v(),
                Step::Constant(element_property) => graph.constant(element_property.clone()),
                Step::Length => graph.length(),
                Step::LTrim => graph.l_trim(),
                Step::RTrim => graph.r_trim(),
                Step::Trim => graph.trim(),
                Step::ToLower => graph.to_lower(),
                Step::ToUpper => graph.to_upper(),
                Step::AsString(policy) => graph.as_string_with(policy.clone()),
                Step::AsBool(policy) => graph.as_bool_with(policy.clone()),
                Step::AsNumber(target, policy) => {
                    graph.as_number_with(target.clone(), policy.clone())
                }
                Step::Cast(target, policy) => graph.cast_with(target.clone(), policy.clone()),
                Step::AsDate(None) => graph.as_date(),
                Step::AsDate(Some(target)) => graph.as_date_to(target.clone()),
                Step::AsDuration => graph.as_duration(),
                Step::AsVector => graph.as_vector(),
                Step::Quantize(element) => graph.quantize(*element),
                Step::Similarity(key, query, metric) => {
                    let query = lower_vector_query(query);
                    match key {
                        Some(key) => graph.similarity(key.clone(), query, *metric),
                        None => graph.similarity_value(query, *metric),
                    }
                }
                Step::Nearest(key, query, k, metric) => {
                    graph.nearest(key.clone(), lower_vector_query(query), *k, *metric)
                }
                Step::VectorMean => graph.vector_mean(),
                Step::Embed(Some(key), embedder) => graph.embed(key.clone(), embedder.as_deref()),
                Step::Embed(None, embedder) => graph.embed_value(embedder.as_deref()),
                Step::DateAdd(unit, n, None) => graph.date_add(*unit, *n),
                Step::DateAdd(unit, n, Some(zone)) => graph.date_add_in(*unit, *n, zone),
                Step::DateAddDuration(duration, None) => graph.date_add_duration(*duration),
                Step::DateAddDuration(duration, Some(zone)) => {
                    graph.date_add_duration_in(*duration, zone)
                }
                Step::DateDiff(other, unit, zone) => match (other, unit) {
                    (ScriptListArg::Value(value), None) => graph.date_diff(value.clone()),
                    (ScriptListArg::Traversal(gts), None) => {
                        graph.date_diff_traversal(gts.ast_convert_to_anonymous())
                    }
                    (ScriptListArg::Value(value), Some(unit)) => {
                        graph.date_diff_in(value.clone(), *unit, zone.as_deref())
                    }
                    (ScriptListArg::Traversal(gts), Some(unit)) => graph.date_diff_traversal_in(
                        gts.ast_convert_to_anonymous(),
                        *unit,
                        zone.as_deref(),
                    ),
                },
                Step::Age(other, zone) => match other {
                    ScriptListArg::Value(value) => graph.age(value.clone(), zone.as_deref()),
                    ScriptListArg::Traversal(gts) => {
                        graph.age_traversal(gts.ast_convert_to_anonymous(), zone.as_deref())
                    }
                },
                Step::DateTrunc(unit, None) => graph.date_trunc(*unit),
                Step::DateTrunc(unit, Some(zone)) => graph.date_trunc_in(*unit, zone),
                Step::DatePart(part, None) => graph.date_part(*part),
                Step::DatePart(part, Some(zone)) => graph.date_part_in(*part, zone),
                Step::AtZone(zone) => graph.at_zone(zone),
                Step::AsLocal(None) => graph.as_local(),
                Step::AsLocal(Some(zone)) => graph.as_local_in(zone),
                Step::Substring(a_from, a_to) => graph.substring(*a_from, *a_to),
                Step::ToUpperScoped(scope) => graph.to_upper_scoped(*scope),
                Step::ToLowerScoped(scope) => graph.to_lower_scoped(*scope),
                Step::TrimScoped(scope) => graph.trim_scoped(*scope),
                Step::LTrimScoped(scope) => graph.l_trim_scoped(*scope),
                Step::RTrimScoped(scope) => graph.r_trim_scoped(*scope),
                Step::LengthScoped(scope) => graph.length_scoped(*scope),
                Step::AsStringScoped(scope) => graph.as_string_scoped(*scope),
                Step::ReplaceScoped(scope, str_from, str_to) => {
                    graph.replace_scoped(*scope, str_from.as_deref(), str_to.as_deref())
                }
                Step::ConcatStrings(parts) => graph.concat(parts.iter().cloned()),
                Step::ConcatTraversals(parts) => graph.concat_traversal(lower_all(parts)),
                Step::Format(pattern) => graph.format(pattern.clone()),
                Step::Split(scope, separator) => graph.split_scoped(*scope, separator.as_deref()),
                Step::Reverse => graph.reverse(),
                Step::Index => graph.index(),
                Step::SubstringScoped(scope, a_from, a_to) => {
                    graph.substring_scoped(*scope, *a_from, *a_to)
                }
                Step::As(arg) => graph.as_(strings(arg)),
                Step::Select(arg) => {
                    if arg.is_empty() {
                        graph.select(None)
                    } else {
                        graph.select(arg)
                    }
                }
                Step::SelectColumn(column) => graph.select(*column),
                Step::SelectToken(key) => graph.select(key.clone()),
                Step::SelectPop(pop, arg) => graph.select_pop(*pop, strings(arg)),
                Step::SelectTraversal(pop, key) => {
                    graph.select_traversal(*pop, key.ast_convert_to_anonymous())
                }
                Step::Path => graph.path(),
                Step::SimplePath => graph.simple_path(),
                Step::CyclicPath => graph.cyclic_path(),
                Step::Is(arg) => graph.is(argp_to_p(arg)),
                Step::WhereP(arg) => graph.where_p(argp_to_p(arg)),
                Step::WhereLabelP(start, arg) => graph.where_label_p(start, argp_to_p(arg)),
                Step::WhereT(arg) => graph.where_t(arg.ast_convert_to_anonymous()),
                Step::And(vec) => graph.and(lower_all(vec)),
                // `fold_connectives` has replaced every infix marker by an `And`/`Or` step.
                Step::AndInfix | Step::OrInfix => graph,
                Step::Not(arg) => graph.not(arg.ast_convert_to_anonymous()),
                Step::FilterT(arg) => graph.filter_t(arg.ast_convert_to_anonymous()),
                Step::Or(vec) => graph.or(lower_all(vec)),
                Step::All(arg) => graph.all(argp_to_p(arg)),
                Step::Any(arg) => graph.any(argp_to_p(arg)),
                Step::NoneP(arg) => graph.none(argp_to_p(arg)),
                Step::Coalesce(vec) => graph.coalesce(lower_all(vec)),
                Step::Union(vec) => graph.union(lower_all(vec)),
                Step::Optional(arg) => graph.optional(arg.ast_convert_to_anonymous()),
                Step::Local(arg) => graph.local(arg.ast_convert_to_anonymous()),
                Step::MapT(arg) => graph.map(arg.ast_convert_to_anonymous()),
                Step::FlatMapT(arg) => graph.flat_map(arg.ast_convert_to_anonymous()),
                Step::ListFunction(kind, arg) => graph.list_function(
                    *kind,
                    match arg {
                        ScriptListArg::Value(value) => ListArg::Value(value.clone()),
                        ScriptListArg::Traversal(gts) => {
                            ListArg::Traversal(gts.ast_convert_to_anonymous())
                        }
                    },
                ),
                Step::Conjoin(delimiter) => graph.conjoin(delimiter.clone()),
                Step::SideEffect(arg) => graph.side_effect_t(arg.ast_convert_to_anonymous()),
                Step::Choose3(predicate, on_true, on_false) => graph.choose(
                    predicate.ast_convert_to_anonymous(),
                    on_true.ast_convert_to_anonymous(),
                    on_false.ast_convert_to_anonymous(),
                ),
                Step::ChooseSwitch(selector) => {
                    graph.choose_switch(selector.ast_convert_to_anonymous())
                }
                Step::Branch(selector) => graph.branch(selector.ast_convert_to_anonymous()),
                Step::Option(token, branch) => {
                    graph.option_token(token.clone(), branch.ast_convert_to_anonymous())
                }
                Step::OptionTraversal(key, branch) => graph.option_traversal(
                    key.ast_convert_to_anonymous(),
                    branch.ast_convert_to_anonymous(),
                ),
                Step::Repeat(name, body) => {
                    let body: AnonymousTraversal<'g, S> = body.ast_convert_to_anonymous();
                    match name {
                        Some(name) => graph.repeat_named(name, body),
                        None => graph.repeat(body),
                    }
                }
                Step::Times(n) => graph.times(*n),
                Step::Until(arg) => graph.until(arg.ast_convert_to_anonymous()),
                Step::Emit(arg) => match arg {
                    Some(arg) => graph.emit_with(arg.ast_convert_to_anonymous()),
                    None => graph.emit(),
                },
                Step::Loops(name) => match name {
                    Some(name) => graph.loops_named(name),
                    None => graph.loops(),
                },
                Step::AddV(labels) => match labels {
                    Some(labels) => graph.add_v(labels.clone()),
                    None => graph.add_v(None),
                }
                .stamp_create_time(self.now),
                Step::Property(prop, val) => graph.property(prop.clone(), val.clone()),
                Step::PropertyTraversal(prop, gts) => {
                    graph.property_by(prop.clone(), gts.ast_convert_to_anonymous())
                }
                Step::PropertyKeyTraversal(key, val) => {
                    graph.property_key_by(key.ast_convert_to_anonymous(), val.clone())
                }
                Step::PropertyKeyTraversalBy(key, val) => graph.property_key_by_traversal(
                    key.ast_convert_to_anonymous(),
                    val.ast_convert_to_anonymous(),
                ),
                Step::AddVTraversal(gts) => graph
                    .add_v_by(gts.ast_convert_to_anonymous())
                    .stamp_create_time(self.now),
                Step::PropertyCardinality(cardinality, prop, val) => {
                    graph.property_with(*cardinality, prop.clone(), val.clone())
                }
                Step::PropertyCardinalityTraversal(cardinality, prop, gts) => graph
                    .property_by_with(*cardinality, prop.clone(), gts.ast_convert_to_anonymous()),
                Step::PropertyMap(cardinality, entries) => {
                    graph.property_entries(*cardinality, entries.clone())
                }
                Step::PropertyToken(PropertyTokenKey::Id, val) => graph.property_id(val.clone()),
                Step::PropertyToken(PropertyTokenKey::Label, val) => {
                    graph.property_label(val.clone())
                }
                // At the root, a merge step with no step before it starts the traversal and runs
                // once, as `GraphTraversalSource::merge_v` decides.
                Step::MergeV(arg) => {
                    let start = root && graph.is_empty();
                    graph
                        .merge_v_lowered(arg.lower(), start)
                        .stamp_create_time(self.now)
                }
                Step::MergeE(arg) => {
                    let start = root && graph.is_empty();
                    graph
                        .merge_e_lowered(arg.lower(), start)
                        .stamp_create_time(self.now)
                }
                Step::MergeOption(token, ScriptMergeArg::Entries(entries), cardinality) => {
                    graph.option_merge_entries(*token, entries.clone(), *cardinality)
                }
                Step::MergeOption(token, ScriptMergeArg::Traversal(gts), cardinality) => {
                    graph.option_merge_by_with(*token, gts.ast_convert_to_anonymous(), *cardinality)
                }
                Step::MergeOption(token, ScriptMergeArg::Value(value), _) => {
                    graph.option_merge(*token, value.clone())
                }
                #[cfg(feature = "serde")]
                Step::PropertyJson(json) => graph.property_json(json.clone()),
                // The root's leading `gql()` is already lowered (`gql_start`); anywhere else
                // (the builder refuses it there already) it fails before the run.
                #[cfg(feature = "gql")]
                Step::Gql(query, _) => {
                    if leading.is_some_and(|leading| std::ptr::eq(leading, step)) {
                        graph
                    } else {
                        graph.gql_error(super::source::gql_not_at_start_error(
                            query,
                            "after other steps or inside __",
                        ))
                    }
                }
                Step::JsonPath(path) => graph.json_path(path.as_str()),
                Step::GlobPath(pattern, options) => {
                    graph.glob_path_with(pattern.as_str(), options.convert_to_anonymous())
                }
                Step::From(arg) => graph.from(strings(arg)),
                Step::To(arg) => graph.to(strings(arg)),
                Step::AddE(label) => match label {
                    Some(label) => graph.add_e(label.clone()),
                    None => graph.add_e(None),
                }
                .stamp_create_time(self.now),
                Step::AddETraversal(gts) => graph
                    .add_e_by(gts.ast_convert_to_anonymous())
                    .stamp_create_time(self.now),
                Step::FromTraversal(gts) => graph.from_by(gts.ast_convert_to_anonymous()),
                Step::ToTraversal(gts) => graph.to_by(gts.ast_convert_to_anonymous()),
                Step::Drop => graph.drop(),
                Step::RemoveProperty(keys) => {
                    if keys.is_empty() {
                        graph.remove_property(None::<()>)
                    } else {
                        graph.remove_property(keys.clone())
                    }
                }
                Step::AddLabel(label) => graph.add_label(label.clone()),
                Step::DropLabel(label) => graph.drop_label(label.clone()),
                Step::SetLabel(label) => graph.set_label(label.clone()),
                Step::Subgraph(arg) => graph.subgraph(arg.as_str()),
                Step::WithOption(key, value) => graph.with_option(*key, *value),
            };
        }
        graph
    }
}

/// Lowers the query vector of `similarity()` / `nearest()`.
fn lower_vector_query<'g, S: GraphStorage + 'g>(
    query: &ScriptVectorQuery,
) -> crate::traversal::step::map_steps::step_similarity::VectorQuery<'g, S> {
    use crate::traversal::step::map_steps::step_similarity::VectorQuery;
    match query {
        ScriptVectorQuery::Value(vector) => VectorQuery::Value(vector.clone()),
        ScriptVectorQuery::Traversal(gts) => VectorQuery::Traversal(gts.ast_convert_to_anonymous()),
        ScriptVectorQuery::Text(text) => VectorQuery::from(text.clone()),
    }
}

/// Lowers every traversal of a multi-child step (`and()`, `or()`, `coalesce()`, `union()`).
fn lower_all<'g, S: GraphStorage + 'g>(
    traversals: &[ScriptGraphTraversalSource],
) -> Vec<AnonymousTraversal<'g, S>> {
    traversals
        .iter()
        .map(ScriptGraphTraversalSource::ast_convert_to_anonymous)
        .collect()
}

/// Pins `now` on every creating step of `steps`, child traversals included (the steps a GQL
/// write lowers to; a step that creates nothing ignores it).
#[cfg(feature = "gql")]
fn stamp_create_times<'g, S: GraphStorage + 'g>(
    steps: &mut [crate::GraphStep<'g, S>],
    now: crate::value::temporal::DateTime,
) {
    for step in steps {
        if let crate::GraphStep::SideEffectMut(_, step) = step {
            step.set_create_time(now);
        }
        for child in step.child_traversals_mut() {
            stamp_create_times(&mut child.steps, now);
        }
    }
}
