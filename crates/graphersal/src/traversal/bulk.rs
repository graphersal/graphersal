//! Traverser bulk: the multiplicity a [`Traverser`] carries and the helpers that keep every
//! count and expansion boundary exact.
//!
//! Equal traversers can be merged into one that stands for all of them (TinkerPop's bulk). The
//! invariant, enforced by `TraverserContext::bulk_active`: no traverser has a bulk above 1
//! unless a merge actually merged something, so the fast paths below cost one comparison when
//! nothing merged.

use crate::error::{TraverserError, TraverserResult};
use crate::graph::GraphStorage;
use crate::traversal::context::{Deadline, ExecutionOptions, tick_deadline};
use crate::traversal::run_memory::RunMemory;
use crate::traversal::{Traverser, TraverserContext};
use crate::value::{ElementProperty, PathRef, SackRef, TraverserValue};
use fnv::FnvBuildHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

fn overflow(step: &str) -> Box<TraverserError> {
    Box::new(TraverserError::BulkOverflow {
        step: step.to_string(),
    })
}

/// Sum of the bulks of `items`, i.e. how many logical traversers they stand for.
pub(crate) fn total_bulk<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    items: &[Traverser<S>],
    step: &str,
) -> TraverserResult<u64> {
    if !context.bulk_active {
        return Ok(items.len() as u64);
    }
    sum_bulks(items, step)
}

fn sum_bulks<S: GraphStorage>(items: &[Traverser<S>], step: &str) -> TraverserResult<u64> {
    items
        .iter()
        .try_fold(0u64, |total, item| total.checked_add(item.bulk))
        .ok_or_else(|| overflow(step))
}

/// Multiplies two bulks, failing with `BulkOverflow` instead of wrapping.
pub(crate) fn mul_bulk(a: u64, b: u64, step: &str) -> TraverserResult<u64> {
    a.checked_mul(b).ok_or_else(|| overflow(step))
}

/// A count result for `total`; a total above `i64::MAX` is `BulkOverflow`.
pub(crate) fn count_property(total: u64, step: &str) -> TraverserResult<ElementProperty> {
    let count = i64::try_from(total).map_err(|_| overflow(step))?;
    Ok(ElementProperty::Number(count.into()))
}

/// `count` times `factor`, or [`TraverserError::BulkOverflow`] for `step`.
pub(crate) fn scale_count(count: u64, factor: u64, step: &str) -> TraverserResult<u64> {
    count.checked_mul(factor).ok_or_else(|| overflow(step))
}

/// The units `[skip, skip + take)` of the stream, counting bulk (TinkerPop `RangeGlobalStep`):
/// a traverser that straddles a bound keeps only the units inside it, so the result is the
/// same multiset as slicing the unmerged stream. Order is kept; `take` may be `u64::MAX`.
pub(crate) fn slice_units<S: GraphStorage>(
    items: Vec<Traverser<S>>,
    skip: u64,
    take: u64,
) -> Vec<Traverser<S>> {
    let upper = skip.saturating_add(take);
    let mut position: u64 = 0;
    let mut kept = Vec::new();
    for mut item in items {
        if position >= upper {
            break;
        }
        let end = position.saturating_add(item.bulk);
        let low = position.max(skip);
        let high = end.min(upper);
        position = end;
        if low < high {
            item.bulk = high - low;
            kept.push(item);
        }
    }
    kept
}

/// The last `n` units of the stream, counting bulk (TinkerPop `TailGlobalStep`).
pub(crate) fn tail_units<S: GraphStorage>(items: Vec<Traverser<S>>, n: u64) -> Vec<Traverser<S>> {
    let total = items
        .iter()
        .fold(0u64, |total, item| total.saturating_add(item.bulk));
    slice_units(items, total.saturating_sub(n), n)
}

/// The value bound of one bulk expansion: `bulk.max_expansion`. A plain number, copied once per
/// expansion; nothing here is read per item of a stream that has no bulk. The byte bounds
/// (`traversal.max_materialized_bytes`, `memory.limit`) are the run's
/// [`RunMemory`](crate::traversal::run_memory::RunMemory), passed beside it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExpansionLimits {
    /// `bulk.max_expansion`: the most values one expansion may produce.
    pub(crate) max_values: u64,
}

impl ExpansionLimits {
    /// The limits of an execution governed by `options`.
    #[inline]
    pub(crate) fn of(options: &ExecutionOptions) -> Self {
        Self {
            max_values: options.bulk_max_expansion,
        }
    }

    /// No bound at all (a run that failed before its options were parsed expands nothing).
    pub(crate) const UNBOUNDED: Self = Self {
        max_values: u64::MAX,
    };

    /// The `BulkExpansionLimit` error of `requested` values at `step`.
    #[cold]
    fn too_many(&self, requested: u64, step: &str) -> Box<TraverserError> {
        Box::new(TraverserError::BulkExpansionLimit {
            step: step.to_string(),
            requested,
            limit: self.max_values,
        })
    }

    /// Checks `copies` copies of `value` (one bucket push of a bulk, `aggregate()`, `group()`
    /// value lists) against `bulk.max_expansion` and the run's memory (what it holds plus the
    /// copies), then charges the copies to the run's estimated memory. Called only for
    /// `copies > 1`, so a stream without bulk never pays for the size estimate.
    pub(crate) fn check_copies<S: GraphStorage>(
        &self,
        memory: &RunMemory,
        value: &TraverserValue<S>,
        copies: u64,
        step: &str,
    ) -> TraverserResult<()> {
        if copies > self.max_values {
            return Err(self.too_many(copies, step));
        }
        let each = std::mem::size_of::<TraverserValue<S>>() as u64
            + crate::traversal::step::profile::estimate_traverser_value_bytes(value);
        let bytes = each.saturating_mul(copies);
        memory.check(bytes)?;
        memory.charge(bytes);
        Ok(())
    }
}

/// The error of a materialization whose estimated size (`observed` bytes) exceeds
/// `traversal.max_materialized_bytes` (`max`).
#[cold]
pub(crate) fn materialized_bytes_error(max: u64, observed: u64) -> Box<TraverserError> {
    Box::new(TraverserError::ResourceLimitExceeded {
        limit: crate::traversal::context::MAX_MATERIALIZED_BYTES.to_string(),
        max: Some(max),
        observed: Some(observed),
    })
}

/// Materialization boundary: one traverser per unit of bulk, in place (copies adjacent).
///
/// Returns `items` untouched (one scan, no allocation) when no bulk is above 1. Otherwise the
/// total must not exceed `limits.max_values` (else `BulkExpansionLimit`; a failed reservation of
/// the output maps to the same error), and what the run holds plus the expansion's estimated
/// size (`size_of::<Traverser>()` plus the value estimate per copy) must fit the run's
/// `memory` (else `ResourceLimitExceeded` of `traversal.max_materialized_bytes` or
/// `memory.limit`), both checked before anything is allocated; the expansion is then charged to
/// the run's estimated memory. The copy loop counts every copy against the
/// `evaluationTimeout` `deadline` (a clock read every 1024 copies), so a long expansion stops
/// in time and the timeout is reported at the expanding step.
pub(crate) fn expand<S: GraphStorage>(
    items: Vec<Traverser<S>>,
    limits: ExpansionLimits,
    memory: &RunMemory,
    deadline: &mut Option<Deadline>,
    step: &str,
) -> TraverserResult<Vec<Traverser<S>>> {
    if items.iter().all(|item| item.bulk == 1) {
        return Ok(items);
    }
    let total = sum_bulks(&items, step)?;
    if total > limits.max_values {
        return Err(limits.too_many(total, step));
    }
    // The size estimate walks each distinct value once (not once per copy): the expansion clones
    // every value `bulk` times anyway, so this costs a fraction of the copying it guards.
    let each_traverser = std::mem::size_of::<Traverser<S>>() as u64;
    let mut bytes: u64 = 0;
    for item in &items {
        let each = each_traverser
            + crate::traversal::step::profile::estimate_traverser_value_bytes(&item.value);
        bytes = bytes.saturating_add(each.saturating_mul(item.bulk));
        if bytes > memory.limit() {
            // Over the limit on its own: no need to size the rest.
            break;
        }
    }
    memory.check(bytes)?;
    let capacity = usize::try_from(total).map_err(|_| limits.too_many(total, step))?;
    let mut expanded = Vec::new();
    expanded
        .try_reserve_exact(capacity)
        .map_err(|_| limits.too_many(total, step))?;
    memory.charge(bytes);
    for item in items {
        let copies = item.bulk;
        for _ in 1..copies {
            tick_deadline(deadline)?;
            expanded.push(Traverser::with_parts(
                item.value.clone(),
                item.path,
                1,
                item.sack,
            ));
        }
        tick_deadline(deadline)?;
        expanded.push(Traverser::with_parts(item.value, item.path, 1, item.sack));
    }
    Ok(expanded)
}

/// [`expand`] under the limits, the memory and the deadline of `context` (a step inside an
/// execution).
#[inline]
pub(crate) fn expand_in<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    items: Vec<Traverser<S>>,
    step: &str,
) -> TraverserResult<Vec<Traverser<S>>> {
    context.memory.set_arena(context.paths.value_bytes());
    expand(
        items,
        ExpansionLimits::of(&context.options),
        &context.memory,
        &mut context.deadline,
        step,
    )
}

/// Like [`expand_in`], for the values of `items` only.
pub(crate) fn expand_values_in<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    items: Vec<Traverser<S>>,
    step: &str,
) -> TraverserResult<Vec<crate::value::TraverserValue<S>>> {
    let expands = items.iter().any(|item| item.bulk > 1);
    let expanded = expand_in(context, items, step)?;
    let traversers = if expands {
        expanded.capacity() as u64
    } else {
        0
    };
    let values: Vec<_> = expanded
        .into_iter()
        .map(|traverser| traverser.value)
        .collect();
    // The list replaces the traversers an expansion built (and charged) for it.
    let memory = &context.memory;
    memory.release(traversers.saturating_mul(std::mem::size_of::<Traverser<S>>() as u64));
    memory.charge(
        (values.capacity() as u64)
            .saturating_mul(std::mem::size_of::<crate::value::TraverserValue<S>>() as u64),
    );
    Ok(values)
}

/// Pushes `copies` copies of `value` onto `bucket` (one unit of bulk each), counting every copy
/// against the `evaluationTimeout` `deadline`. The caller checked the copies against the
/// expansion limits ([`ExpansionLimits::check_copies`]) when `copies > 1`.
pub(crate) fn push_copies<T: Clone>(
    bucket: &mut Vec<T>,
    value: T,
    copies: u64,
    deadline: &mut Option<Deadline>,
) -> TraverserResult<()> {
    for _ in 1..copies {
        tick_deadline(deadline)?;
        bucket.push(value.clone());
    }
    bucket.push(value);
    Ok(())
}

/// What makes two traversers interchangeable at a merge point: the same object (by identity),
/// the same path handle, and, once sacks exist, the same sack.
///
/// The key borrows from the traverser, so building one allocates nothing. Containers (`Array`,
/// `Object`, `Record`, `Entry`, `Path`, `Schema`) have no key and never merge, and neither do
/// `Vertex`/`Edge` values materialized as `Computed` (they are not identities).
///
/// Equality is strict, unlike `TraverserValue::eq`, which ignores the vertex tag of a
/// `Computed` value: two computed values merge only when value and tag are both equal, and
/// `Number` equality already tells `1` from `1.0`.
pub(crate) struct MergeKey<'a, S: GraphStorage> {
    value: &'a TraverserValue<S>,
    path: PathRef,
    sack: SackRef,
}

impl<'a, S: GraphStorage> MergeKey<'a, S> {
    /// The key of `traverser`, or `None` for a value that never merges.
    #[inline]
    pub(crate) fn of(traverser: &'a Traverser<S>) -> Option<MergeKey<'a, S>> {
        let mergeable = match &traverser.value {
            TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::VertexProperty(_, _, _)
            | TraverserValue::EdgeProperty(_, _, _) => true,
            TraverserValue::Computed(scalar, _) => matches!(
                scalar,
                ElementProperty::Null
                    | ElementProperty::Boolean(_)
                    | ElementProperty::Number(_)
                    | ElementProperty::String(_)
                    | ElementProperty::Uuid(_)
            ),
            TraverserValue::Path(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Object(_)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => false,
        };
        mergeable.then_some(MergeKey {
            value: &traverser.value,
            path: traverser.path,
            sack: traverser.sack,
        })
    }
}

/// The merge key of `traverser` under the execution's sack rules, chosen at compile time so a
/// plan without a sack merge operator and without `bulk.one` pays nothing for them.
///
/// With a sack merge operator (`OPERATOR`) the key ignores the sack (such traversers merge
/// whatever their sacks are, TinkerPop `equals`); without one and under `bulk.one` (`ONE`) a
/// traverser that carries a sack has no key (TinkerPop `carriesUnmergeableSack`); otherwise the
/// sack handle is part of the key.
#[inline(always)]
fn key_for<'a, S: GraphStorage, const OPERATOR: bool, const ONE: bool>(
    traverser: &'a Traverser<S>,
) -> Option<MergeKey<'a, S>> {
    let mut key = MergeKey::of(traverser)?;
    if OPERATOR {
        key.sack = SackRef::NONE;
    } else if ONE && !traverser.sack.is_none() {
        return None;
    }
    Some(key)
}

impl<S: GraphStorage> PartialEq for MergeKey<'_, S> {
    fn eq(&self, other: &Self) -> bool {
        // Equal sack handle or no merge. Equal scalar sacks share a handle (they are interned),
        // so this is value equality for them; a container sack has a handle of its own and
        // merges only with copies of the same stored value. Without `withSack` every handle is
        // `SackRef::NONE`.
        if self.path != other.path || self.sack != other.sack {
            return false;
        }
        match (self.value, other.value) {
            (TraverserValue::Vertex(a), TraverserValue::Vertex(b)) => a == b,
            (TraverserValue::Edge(a), TraverserValue::Edge(b)) => a == b,
            (
                TraverserValue::VertexProperty(ka, a, _),
                TraverserValue::VertexProperty(kb, b, _),
            ) => a == b && ka == kb,
            (TraverserValue::EdgeProperty(ka, a, _), TraverserValue::EdgeProperty(kb, b, _)) => {
                a == b && ka == kb
            }
            (TraverserValue::Computed(a, ta), TraverserValue::Computed(b, tb)) => {
                ta == tb && a == b
            }
            _ => false,
        }
    }
}

impl<S: GraphStorage> Eq for MergeKey<'_, S> {}

impl<S: GraphStorage> Hash for MergeKey<'_, S> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
        // Only a sacked traverser mixes its handle in: equality still compares it, so a collision
        // between different sacks is merely a longer probe, and the no-sack path hashes as before.
        if !self.sack.is_none() {
            self.sack.hash(state);
        }
        match self.value {
            TraverserValue::Vertex(id) => {
                0u8.hash(state);
                id.hash(state);
            }
            TraverserValue::Edge(id) => {
                1u8.hash(state);
                id.hash(state);
            }
            TraverserValue::VertexProperty(key, id, _) => {
                2u8.hash(state);
                key.hash(state);
                id.hash(state);
            }
            TraverserValue::EdgeProperty(key, id, _) => {
                3u8.hash(state);
                key.hash(state);
                id.hash(state);
            }
            TraverserValue::Computed(scalar, tag) => {
                4u8.hash(state);
                scalar.hash(state);
                tag.hash(state);
            }
            // Never keyed (see `MergeKey::of`).
            TraverserValue::Path(_)
            | TraverserValue::Record { .. }
            | TraverserValue::Object(_)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Array(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_) => 5u8.hash(state),
        }
    }
}

/// How many leading traversers a merge pass looks at before it decides whether the whole pass is
/// worth finishing (see [`merge_traversers_adaptive`]).
const MERGE_PROBE: usize = 4096;

/// The probe gives up when fewer than one in `MERGE_PROBE_RATIO` of the probed traversers merged.
const MERGE_PROBE_RATIO: usize = 16;

/// Whether merging `input` right before the adjacency step `next` saves more than it costs: the
/// decision of an optimizer-inserted merge point (`lazy_barrier()`).
///
/// A merge costs a hash insert per traverser; what it saves is `next`'s work on the duplicates,
/// which is their fan-out on `next`. A high merge ratio alone says nothing: on an org chart the
/// 1.6M traversers after `out()` merge into 180k, but the merged vertices have no out-edges, so
/// the following `out()` produces exactly as much as without the merge (measured: the merge cost
/// 39 ms and saved 12 ms). The estimate therefore looks at the first [`MERGE_PROBE`] traversers and
/// adds, for every one that would merge into an earlier one, its fan-out on `next` (`count()`,
/// which materializes nothing; once per group). Fewer than one saved output per two probed
/// traversers means "do not merge" (the savings repeat in every later step, so half an output
/// already pays: a star whose 6000 sources meet in one hub of fan-out 1 merges). Streams below [`MERGE_PROBE`] are always merged (cheap, and duplicates in
/// a small stream are where an exponential blow-up starts). Never changes results.
pub(crate) fn merge_saves_work<'g, S: GraphStorage + 'g>(
    context: &mut TraverserContext<'_, S>,
    input: &[Traverser<S>],
    next: &dyn crate::traversal::step::GraphStepFlatMap<'g, S>,
) -> bool {
    if input.len() < MERGE_PROBE {
        return true;
    }
    let sample = &input[..MERGE_PROBE];
    // Per group: the fan-out of its vertex on `next`, computed when the first duplicate shows.
    #[allow(clippy::mutable_key_type)]
    let mut groups: HashMap<MergeKey<'_, S>, Option<u64>, FnvBuildHasher> =
        HashMap::with_capacity_and_hasher(sample.len(), FnvBuildHasher::default());
    let mut saved: u64 = 0;
    let needed = sample.len() as u64 / 2;
    for traverser in sample {
        let Some(key) = MergeKey::of(traverser) else {
            continue;
        };
        let fan_out = match groups.entry(key) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(None);
                continue;
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => match *slot.get() {
                Some(fan_out) => fan_out,
                None => {
                    // An error here is `next`'s own error on this input: merge, and let `next`
                    // report it on the merged stream.
                    let fan_out = match next.count(context, traverser.clone()) {
                        Ok(total) => total / traverser.bulk().max(1),
                        Err(_) => return true,
                    };
                    slot.insert(Some(fan_out));
                    fan_out
                }
            },
        };
        saved = saved.saturating_add(fan_out.saturating_mul(traverser.bulk()));
        if saved >= needed {
            return true;
        }
    }
    false
}

/// Merges equal traversers into one that carries the sum of their bulks, in first-occurrence
/// order: duplicates move to the position of their first occurrence.
///
/// Returns `input` unchanged when merging is off (`context.bulk_merge`), there are fewer than
/// two traversers, the step records full paths (every path is distinct then), or nothing merged.
/// Sets `context.bulk_active` when something merged. Costs one hash map and two vectors per call
/// and no allocation per item. The whole stream is always merged (an explicit `barrier()`);
/// optimizer-inserted merge points use [`merge_traversers_adaptive`] instead.
pub(crate) fn merge_traversers<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    input: Vec<Traverser<S>>,
    step: &'static str,
) -> TraverserResult<Vec<Traverser<S>>> {
    Ok(merge_dispatch(context, input, step, false)?.0)
}

/// [`merge_traversers`] that also says whether merging is worth trying again on the next
/// stream of the same step.
///
/// A merge pass on a stream where nothing merges is pure overhead (a hash insert per traverser,
/// e.g. on a tree-shaped graph). The pass therefore probes: once it has looked at
/// [`MERGE_PROBE`] traversers and fewer than one in sixteen of them merged, it stops, returns the
/// input untouched and answers `false`. Smaller streams are always merged completely (the cost is
/// small and their duplicates are what an exponential blow-up starts from) and answer `true`.
/// A repeat loop uses the answer to stop merging for its remaining iterations. The heuristic
/// never changes results, only whether duplicates are combined.
pub(crate) fn merge_traversers_adaptive<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    input: Vec<Traverser<S>>,
    step: &'static str,
) -> TraverserResult<(Vec<Traverser<S>>, bool)> {
    merge_dispatch(context, input, step, true)
}

/// Picks the merge pass once per call: a plan without a sack merge operator runs the pass that
/// has no sack bookkeeping at all (the hot path of every merging query).
fn merge_dispatch<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    input: Vec<Traverser<S>>,
    step: &'static str,
    probe: bool,
) -> TraverserResult<(Vec<Traverser<S>>, bool)> {
    match (context.sack_merge.is_some(), context.bulk_one) {
        (true, true) => merge_inner::<S, true, true>(context, input, step, probe),
        (true, false) => merge_inner::<S, true, false>(context, input, step, probe),
        (false, true) => merge_inner::<S, false, true>(context, input, step, probe),
        (false, false) => merge_inner::<S, false, false>(context, input, step, probe),
    }
}

/// Combines two sacks with the merge operator; kept out of line so the merge loop stays small.
#[cold]
#[inline(never)]
fn combine_sacks<S: GraphStorage>(
    context: &mut TraverserContext<'_, S>,
    operator: crate::value::Operator,
    left: SackRef,
    right: SackRef,
) -> TraverserResult<SackRef> {
    let sacks = &mut context.sacks;
    // Numbers (the common sack) are copied field by field instead of through the generic
    // `ElementProperty::clone`, an out-of-line call over every variant.
    let operands = match (sacks.get(left), sacks.get(right)) {
        (ElementProperty::Number(a), ElementProperty::Number(b)) => (
            ElementProperty::Number(a.clone()),
            ElementProperty::Number(b.clone()),
        ),
        (a, b) => (a.clone(), b.clone()),
    };
    let combined = operator
        .apply_owned(operands.0, operands.1)
        .map_err(|error| Box::new(TraverserError::from(error)))?;
    sacks.intern(combined)
}

fn merge_inner<S: GraphStorage, const OPERATOR: bool, const ONE: bool>(
    context: &mut TraverserContext<'_, S>,
    input: Vec<Traverser<S>>,
    step: &'static str,
    probe: bool,
) -> TraverserResult<(Vec<Traverser<S>>, bool)> {
    if !context.bulk_merge || input.len() < 2 || context.path_mode.is_full() {
        return Ok((input, true));
    }
    // `target[i]`: index of the first occurrence `i` merges into (itself for a first occurrence).
    // A probing pass may give up after `MERGE_PROBE` items, so it does not reserve the whole stream.
    let reserve = if probe {
        input.len().min(MERGE_PROBE)
    } else {
        input.len()
    };
    let mut target: Vec<usize> = Vec::with_capacity(reserve);
    let mut bulks: Vec<u64> = Vec::with_capacity(reserve);
    // Under a sack merge operator `acc[i]` is the combined sack of the group whose first
    // member is `i`; without one it stays empty and costs nothing (`OPERATOR` is a const).
    let sack_merge = context.sack_merge;
    let mut acc: Vec<SackRef> = Vec::new();
    if OPERATOR {
        acc.reserve(reserve);
    }
    let mut merged = false;
    {
        // False positive: `MergeKey` hashes and compares only element identities, scalars and the
        // path/sack handles (containers never get a key); clippy flags it only because some
        // `TraverserValue` variant can reach interior mutability.
        #[allow(clippy::mutable_key_type)]
        let mut first: HashMap<MergeKey<'_, S>, usize, FnvBuildHasher> =
            HashMap::with_capacity_and_hasher(reserve, FnvBuildHasher::default());
        let mut merges = 0usize;
        for (index, traverser) in input.iter().enumerate() {
            context.tick()?;
            if probe && index == MERGE_PROBE && merges * MERGE_PROBE_RATIO < MERGE_PROBE {
                return Ok((input, false));
            }
            bulks.push(traverser.bulk);
            if OPERATOR {
                acc.push(traverser.sack);
            }
            let Some(key) = key_for::<S, OPERATOR, ONE>(traverser) else {
                target.push(index);
                continue;
            };
            match first.entry(key) {
                std::collections::hash_map::Entry::Occupied(slot) => {
                    let first_index = *slot.get();
                    if !ONE {
                        bulks[first_index] = bulks[first_index]
                            .checked_add(traverser.bulk)
                            .ok_or_else(|| overflow(step))?;
                    }
                    if OPERATOR && let Some(operator) = sack_merge {
                        acc[first_index] =
                            combine_sacks(context, operator, acc[first_index], traverser.sack)?;
                    }
                    target.push(first_index);
                    merged = true;
                    merges += 1;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(index);
                    target.push(index);
                }
            }
        }
    }
    if !merged {
        return Ok((input, true));
    }
    // Under `bulk.one` a merge keeps bulk 1, so nothing above 1 appeared: the fast paths of
    // `bulk_active` stay valid.
    if !ONE {
        context.bulk_active = true;
    }
    let mut output = Vec::with_capacity(input.len());
    for (index, mut traverser) in input.into_iter().enumerate() {
        if target[index] == index {
            traverser.bulk = bulks[index];
            if OPERATOR {
                traverser.sack = acc[index];
            }
            output.push(traverser);
        }
    }
    Ok((output, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TraversalGraph;
    use crate::value::{PathRef, TraverserValue};

    type T = Traverser<TraversalGraph>;

    fn limits(max_values: u64) -> ExpansionLimits {
        ExpansionLimits { max_values }
    }

    /// A run memory without a byte limit or a process ceiling.
    fn unbounded() -> RunMemory {
        RunMemory::new(u64::MAX, None)
    }

    fn item(n: i64, bulk: u64) -> T {
        Traverser::with_parts(
            TraverserValue::Computed(ElementProperty::Number(n.into()), None),
            PathRef::NONE,
            bulk,
            SackRef::NONE,
        )
    }

    fn number(t: &T) -> Option<i64> {
        match &t.value {
            TraverserValue::Computed(ElementProperty::Number(n), _) => n.as_i64(),
            _ => None,
        }
    }

    #[test]
    fn mul_bulk_multiplies_and_reports_overflow() {
        assert_eq!(mul_bulk(3, 4, "out").unwrap(), 12);
        let err = mul_bulk(u64::MAX, 2, "out").unwrap_err();
        assert!(matches!(*err, TraverserError::BulkOverflow { ref step } if step == "out"));
    }

    #[test]
    fn count_property_rejects_totals_above_i64_max() {
        assert_eq!(
            count_property(7, "count").unwrap(),
            ElementProperty::Number(7i64.into())
        );
        assert!(count_property(i64::MAX as u64, "count").is_ok());
        let err = count_property(i64::MAX as u64 + 1, "count").unwrap_err();
        assert!(matches!(*err, TraverserError::BulkOverflow { .. }));
    }

    #[test]
    fn sums_add_bulks_and_detect_overflow() {
        let items = vec![item(1, 2), item(2, 5)];
        assert_eq!(sum_bulks(&items, "count").unwrap(), 7);
        let items = vec![item(1, u64::MAX), item(2, 1)];
        let err = sum_bulks(&items, "count").unwrap_err();
        assert!(matches!(*err, TraverserError::BulkOverflow { .. }));
    }

    #[test]
    fn expand_repeats_values_in_place_and_resets_bulk() {
        let expanded = expand(
            vec![item(1, 1), item(2, 3), item(3, 2)],
            limits(100),
            &unbounded(),
            &mut None,
            "fold",
        )
        .unwrap();
        let values: Vec<_> = expanded.iter().map(|t| number(t).unwrap()).collect();
        assert_eq!(values, vec![1, 2, 2, 2, 3, 3]);
        assert!(expanded.iter().all(|t| t.bulk() == 1));
    }

    #[test]
    fn expand_without_bulk_returns_the_input() {
        let expanded = expand(
            vec![item(1, 1), item(2, 1)],
            limits(1),
            &unbounded(),
            &mut None,
            "fold",
        )
        .unwrap();
        assert_eq!(expanded.len(), 2);
    }

    #[test]
    fn expand_enforces_the_limit() {
        let err = expand(
            vec![item(1, 6), item(2, 5)],
            limits(10),
            &unbounded(),
            &mut None,
            "fold",
        )
        .unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::BulkExpansionLimit { ref step, requested: 11, limit: 10 } if step == "fold"
        ));
        assert_eq!(
            expand(
                vec![item(1, 6), item(2, 4)],
                limits(10),
                &unbounded(),
                &mut None,
                "fold"
            )
            .unwrap()
            .len(),
            10
        );
    }

    #[test]
    fn expand_enforces_the_byte_budget_before_it_allocates() {
        let each = std::mem::size_of::<T>() as u64;
        // A run that holds nothing yet, limited to `max_bytes` (`traversal.max_materialized_bytes`).
        let bounded = |max_bytes| RunMemory::new(max_bytes, None);
        // Exactly the budget: 5 traversers of a number (no heap) fit into 5 x 80 bytes.
        let expanded = expand(
            vec![item(1, 5)],
            limits(u64::MAX),
            &bounded(5 * each),
            &mut None,
            "fold",
        )
        .unwrap();
        assert_eq!(expanded.len(), 5);
        let err = expand(
            vec![item(1, 6)],
            limits(u64::MAX),
            &bounded(5 * each),
            &mut None,
            "fold",
        )
        .unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::ResourceLimitExceeded { ref limit, max: Some(_), observed: Some(o) }
                if limit == "traversal.max_materialized_bytes" && o == 6 * each
        ));
        // Without bulk nothing is expanded, so nothing is checked.
        assert_eq!(
            expand(
                vec![item(1, 1), item(2, 1)],
                limits(u64::MAX),
                &bounded(1),
                &mut None,
                "fold"
            )
            .unwrap()
            .len(),
            2
        );
        // The copies of one bucket push count the value's own heap bytes too.
        let text = TraverserValue::<TraversalGraph>::Computed(
            ElementProperty::String("x".repeat(1000).into()),
            None,
        );
        assert!(
            limits(u64::MAX)
                .check_copies(&bounded(10_000), &text, 9, "aggregate")
                .is_ok()
        );
        assert!(
            limits(u64::MAX)
                .check_copies(&bounded(10_000), &text, 10, "aggregate")
                .is_err()
        );
        let err = limits(3)
            .check_copies(&unbounded(), &text, 4, "aggregate")
            .unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::BulkExpansionLimit { requested: 4, .. }
        ));
    }

    #[test]
    fn expand_enforces_the_memory_budget_before_it_allocates() {
        let each = std::mem::size_of::<T>() as u64;
        // A budget without a starting point: nothing in use, so the expansion alone counts.
        let budgeted = |limit| {
            RunMemory::new(
                u64::MAX,
                Some(crate::traversal::memory_budget::MemoryBudget::new(limit)),
            )
        };
        assert_eq!(
            expand(
                vec![item(1, 5)],
                limits(u64::MAX),
                &budgeted(5 * each),
                &mut None,
                "fold"
            )
            .unwrap()
            .len(),
            5
        );
        let err = expand(
            vec![item(1, 6)],
            limits(u64::MAX),
            &budgeted(5 * each),
            &mut None,
            "fold",
        )
        .unwrap_err();
        assert!(matches!(
            *err,
            TraverserError::ResourceLimitExceeded { ref limit, max: Some(max), observed: Some(o) }
                if limit == "memory.limit" && max == 5 * each && o == 6 * each
        ));
        let text = TraverserValue::<TraversalGraph>::Computed(
            ElementProperty::String("x".repeat(1000).into()),
            None,
        );
        assert!(
            limits(u64::MAX)
                .check_copies(&budgeted(10_000), &text, 10, "aggregate")
                .is_err()
        );
    }

    #[test]
    fn expand_checks_the_deadline_while_it_copies() {
        let mut deadline =
            crate::traversal::context::Deadline::new(std::time::Duration::from_nanos(1));
        std::thread::sleep(std::time::Duration::from_millis(2));
        let err = expand(
            vec![item(1, 5000)],
            ExpansionLimits::UNBOUNDED,
            &unbounded(),
            &mut deadline,
            "fold",
        )
        .unwrap_err();
        assert!(matches!(*err, TraverserError::Timeout { .. }));
    }

    #[test]
    fn expand_reports_overflow_of_the_total() {
        let err = expand(
            vec![item(1, u64::MAX), item(2, 2)],
            ExpansionLimits::UNBOUNDED,
            &unbounded(),
            &mut None,
            "fold",
        )
        .unwrap_err();
        assert!(matches!(*err, TraverserError::BulkOverflow { .. }));
    }

    #[test]
    fn expand_maps_an_unreservable_total_to_the_limit_error() {
        let err = expand(
            vec![item(1, u64::MAX)],
            ExpansionLimits::UNBOUNDED,
            &unbounded(),
            &mut None,
            "fold",
        )
        .unwrap_err();
        assert!(matches!(*err, TraverserError::BulkExpansionLimit { .. }));
    }

    mod merging {
        use super::*;
        use crate::graph::VertexHandle;
        use crate::traversal::context::TraversalStrategy;
        use std::collections::HashMap;

        fn vertex(id: usize) -> T {
            Traverser::from_value(TraverserValue::Vertex(VertexHandle::for_test(id as u32)))
        }

        fn computed(value: ElementProperty, tag: Option<usize>) -> T {
            Traverser::from_value(TraverserValue::Computed(
                value,
                tag.map(|id| VertexHandle::for_test(id as u32)),
            ))
        }

        fn with_context<R>(
            merge: bool,
            run: impl FnOnce(&mut TraverserContext<'_, TraversalGraph>) -> R,
        ) -> R {
            let mut graph = TraversalGraph::new();
            let mut context =
                TraverserContext::new(&mut graph, TraversalStrategy::none(), HashMap::new());
            context.bulk_merge = merge;
            run(&mut context)
        }

        fn bulks(items: &[T]) -> Vec<u64> {
            items.iter().map(Traverser::bulk).collect()
        }

        #[test]
        fn identical_vertices_merge_and_sum_in_first_occurrence_order() {
            let merged = with_context(true, |context| {
                let input = vec![vertex(2), vertex(1), vertex(2), vertex(2), vertex(1)];
                let merged = merge_traversers(context, input, "barrier").unwrap();
                assert!(context.bulk_active);
                merged
            });
            let ids: Vec<_> = merged
                .iter()
                .map(|t| match t.value {
                    TraverserValue::Vertex(id) => id.index().index(),
                    _ => unreachable!(),
                })
                .collect();
            assert_eq!(ids, vec![2, 1]);
            assert_eq!(bulks(&merged), vec![3, 2]);
        }

        #[test]
        fn different_sacks_never_merge_and_equal_sacks_do() {
            let mut arena = crate::value::SackArena::new();
            let one = arena.intern(ElementProperty::Number(1.into())).unwrap();
            let two = arena.intern(ElementProperty::Number(2.into())).unwrap();
            let with = |id: usize, sack: SackRef| {
                let mut t = vertex(id);
                t.sack = sack;
                t
            };
            let merged = with_context(true, |context| {
                let input = vec![
                    with(1, one),
                    with(1, two),
                    with(1, one),
                    with(1, SackRef::NONE),
                    with(1, two),
                ];
                merge_traversers(context, input, "barrier").unwrap()
            });
            assert_eq!(bulks(&merged), vec![2, 2, 1]);
            let sacks: Vec<_> = merged.iter().map(|t| t.sack).collect();
            assert_eq!(sacks, vec![one, two, SackRef::NONE]);
        }

        #[test]
        fn the_merge_key_follows_the_sack_rules() {
            let mut arena = crate::value::SackArena::new();
            let one = arena.intern(ElementProperty::Number(1.into())).unwrap();
            let two = arena.intern(ElementProperty::Number(2.into())).unwrap();
            let mut a = vertex(1);
            a.sack = one;
            let mut b = vertex(1);
            b.sack = two;
            fn default(t: &T) -> Option<MergeKey<'_, TraversalGraph>> {
                key_for::<TraversalGraph, false, false>(t)
            }
            fn operator(t: &T) -> Option<MergeKey<'_, TraversalGraph>> {
                key_for::<TraversalGraph, true, false>(t)
            }
            fn one_bulk(t: &T) -> Option<MergeKey<'_, TraversalGraph>> {
                key_for::<TraversalGraph, false, true>(t)
            }
            // Default: the handle is part of the key.
            assert!(default(&a).unwrap() != default(&b).unwrap());
            assert!(default(&a).unwrap() == default(&a).unwrap());
            // A merge operator ignores the sack.
            assert!(operator(&a).unwrap() == operator(&b).unwrap());
            // bulk.one: a sacked traverser never merges, an unsacked one still does.
            assert!(one_bulk(&a).is_none());
            assert!(one_bulk(&vertex(1)).is_some());
            // The operator wins over bulk.one.
            assert!(key_for::<TraversalGraph, true, true>(&a).is_some());
        }

        #[test]
        fn a_merge_operator_combines_sacks_and_bulk_one_keeps_bulk_one() {
            for bulk_one in [false, true] {
                let mut arena = crate::value::SackArena::new();
                let handles: Vec<_> = (1..=3)
                    .map(|n| arena.intern(ElementProperty::Number(n.into())).unwrap())
                    .collect();
                let merged = with_context(true, |context| {
                    context.sacks = arena;
                    context.sack_merge = Some(crate::value::Operator::Sum);
                    context.bulk_one = bulk_one;
                    let input: Vec<_> = handles
                        .iter()
                        .map(|h| {
                            let mut t = vertex(1);
                            t.sack = *h;
                            t
                        })
                        .collect();
                    let merged = merge_traversers(context, input, "barrier").unwrap();
                    assert_eq!(context.bulk_active, !bulk_one);
                    let sack = context.sacks.get(merged[0].sack).clone();
                    assert_eq!(sack, ElementProperty::Number(6.into()));
                    merged
                });
                assert_eq!(bulks(&merged), vec![if bulk_one { 1 } else { 3 }]);
            }
        }

        #[test]
        fn existing_bulks_are_summed() {
            let merged = with_context(true, |context| {
                let mut a = vertex(1);
                a.bulk = 5;
                let mut b = vertex(1);
                b.bulk = 7;
                merge_traversers(context, vec![a, b], "barrier").unwrap()
            });
            assert_eq!(bulks(&merged), vec![12]);
        }

        #[test]
        fn different_paths_do_not_merge() {
            let merged = with_context(true, |context| {
                let path = context
                    .paths
                    .push(
                        PathRef::NONE,
                        TraverserValue::Vertex(VertexHandle::for_test(0)),
                        None,
                    )
                    .unwrap();
                let a = vertex(1);
                let b = Traverser::with_parts(
                    TraverserValue::Vertex(VertexHandle::for_test(1)),
                    path,
                    1,
                    SackRef::NONE,
                );
                let result = merge_traversers(context, vec![a, b], "barrier").unwrap();
                assert!(!context.bulk_active);
                result
            });
            assert_eq!(bulks(&merged), vec![1, 1]);
        }

        #[test]
        fn computed_values_of_different_number_types_do_not_merge() {
            let merged = with_context(true, |context| {
                let input = vec![
                    computed(ElementProperty::Number(1i64.into()), None),
                    computed(ElementProperty::Number(1.0f64.into()), None),
                    computed(ElementProperty::Number(1i64.into()), None),
                ];
                merge_traversers(context, input, "barrier").unwrap()
            });
            assert_eq!(bulks(&merged), vec![2, 1]);
        }

        #[test]
        fn computed_values_with_different_tags_do_not_merge() {
            let merged = with_context(true, |context| {
                let input = vec![
                    computed(ElementProperty::String("x".into()), Some(1)),
                    computed(ElementProperty::String("x".into()), Some(2)),
                    computed(ElementProperty::String("x".into()), None),
                    computed(ElementProperty::String("x".into()), Some(1)),
                ];
                merge_traversers(context, input, "barrier").unwrap()
            });
            assert_eq!(bulks(&merged), vec![2, 1, 1]);
        }

        #[test]
        fn containers_pass_through() {
            let merged = with_context(true, |context| {
                let array = || Traverser::from_value(TraverserValue::Array(vec![]));
                let result = merge_traversers(context, vec![array(), array()], "barrier").unwrap();
                assert!(!context.bulk_active);
                result
            });
            assert_eq!(bulks(&merged), vec![1, 1]);
        }

        #[test]
        fn nothing_merges_when_the_switch_is_off() {
            let merged = with_context(false, |context| {
                merge_traversers(context, vec![vertex(1), vertex(1)], "barrier").unwrap()
            });
            assert_eq!(bulks(&merged), vec![1, 1]);
        }

        #[test]
        fn full_path_mode_skips_merging() {
            let merged = with_context(true, |context| {
                context.path_mode = crate::traversal::optimizer::path_analysis::PathMode::Full;
                merge_traversers(context, vec![vertex(1), vertex(1)], "barrier").unwrap()
            });
            assert_eq!(bulks(&merged), vec![1, 1]);
        }

        #[test]
        fn overflowing_bulks_fail_with_bulk_overflow() {
            let err = with_context(true, |context| {
                let mut a = vertex(1);
                a.bulk = u64::MAX;
                merge_traversers(context, vec![a, vertex(1)], "barrier").unwrap_err()
            });
            assert!(matches!(*err, TraverserError::BulkOverflow { ref step } if step == "barrier"));
        }
    }

    #[test]
    fn slicing_counts_units_and_cuts_straddling_traversers() {
        let stream = || vec![item(1, 3), item(2, 1), item(3, 4)];
        let sliced = slice_units(stream(), 2, 4);
        let shape: Vec<_> = sliced
            .iter()
            .map(|t| (number(t).unwrap(), t.bulk()))
            .collect();
        assert_eq!(shape, vec![(1, 1), (2, 1), (3, 2)]);
        assert!(slice_units(stream(), 8, 5).is_empty());
        assert_eq!(slice_units(stream(), 0, u64::MAX).len(), 3);
        let tail = tail_units(stream(), 5);
        let shape: Vec<_> = tail
            .iter()
            .map(|t| (number(t).unwrap(), t.bulk()))
            .collect();
        assert_eq!(shape, vec![(2, 1), (3, 4)]);
    }
}
