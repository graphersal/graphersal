//! `limit/skip/tail/range(Scope.local)`: slice each traverser's own list or map.

use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalSlice, slice_local};
use crate::value::Scope;
use crate::*;

/// Which slice a [`StepRangeLocal`] takes, in the spelling the user wrote (so the profile shows
/// `limit(local, 2)` rather than `range(local, 0, 2)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RangeMode {
    /// `limit(local, n)`: the first `n` elements, `[0, n)`.
    Limit(i64),
    /// `skip(local, n)`: everything after the first `n` elements, `[n, -1)`.
    Skip(i64),
    /// `tail(local, n)`: the last `n` elements.
    Tail(i64),
    /// `range(local, low, high)`: `[low, high)`, `high == -1` meaning "no upper bound".
    Range { low: i64, high: i64 },
}

impl RangeMode {
    /// The Gremlin step name, used for the `GraphStep` and in the diagnostics.
    fn name(self) -> &'static str {
        match self {
            RangeMode::Limit(_) => "limit",
            RangeMode::Skip(_) => "skip",
            RangeMode::Tail(_) => "tail",
            RangeMode::Range { .. } => "range",
        }
    }

    /// Rejects bounds TinkerPop rejects, as [`TraverserError::InvalidRange`]. A negative count is
    /// reported rather than wrapped into a huge `usize`.
    fn validate(self) -> TraverserResult<()> {
        let invalid = |low: i64, high: i64, reason: String| {
            Err(Box::new(TraverserError::InvalidRange { low, high, reason }))
        };
        match self {
            RangeMode::Limit(n) if n < 0 => invalid(
                0,
                n,
                format!("limit(local, {n}) needs a count of zero or more"),
            ),
            RangeMode::Skip(n) if n < 0 => invalid(
                n,
                -1,
                format!("skip(local, {n}) needs a count of zero or more"),
            ),
            RangeMode::Tail(n) if n < 0 => invalid(
                n,
                -1,
                format!("tail(local, {n}) needs a count of zero or more"),
            ),
            RangeMode::Range { low, high } if low < 0 => invalid(
                low,
                high,
                "low must be zero or a positive integer".to_string(),
            ),
            RangeMode::Range { low, high } if high < -1 => invalid(
                low,
                high,
                "high must be -1 (unbounded) or zero or a positive integer".to_string(),
            ),
            RangeMode::Range { low, high } if high != -1 && high < low => {
                invalid(low, high, "high must be at least low".to_string())
            }
            RangeMode::Limit(_)
            | RangeMode::Skip(_)
            | RangeMode::Tail(_)
            | RangeMode::Range { .. } => Ok(()),
        }
    }

    /// The element range `[start, end)` this mode keeps of a collection of `len` elements
    /// (`start <= end <= len`). Only called after [`validate`](Self::validate), so every count
    /// is non-negative.
    fn bounds(self, len: usize) -> (usize, usize) {
        let clamp = |n: i64| usize::try_from(n).map_or(0, |n| n.min(len));
        match self {
            RangeMode::Limit(n) => (0, clamp(n)),
            RangeMode::Skip(n) => (clamp(n), len),
            RangeMode::Tail(n) => (len - clamp(n), len),
            RangeMode::Range { low, high } => {
                let start = clamp(low);
                let end = if high == -1 { len } else { clamp(high) };
                (start, end.max(start))
            }
        }
    }
}

/// `limit/skip/tail/range(local, …)` (TinkerPop `RangeLocalStep`/`TailLocalStep`): maps each
/// traverser holding a list to the sub-list and one holding a map to the sub-map (entry order)
/// its bounds select. A list stays a list even when one element is left (TinkerPop 3.8:
/// `limit(local, 1)` on `[1, 2, 3]` → `[1]`); any other value passes through unchanged, even out
/// of range. A path fails with [`TraverserError::NotImplemented`].
#[derive(Clone)]
struct StepRangeLocal {
    mode: RangeMode,
}

impl StepRangeLocal {
    fn step<'g, S: GraphStorage + 'g>(mode: RangeMode) -> GraphStep<'g, S> {
        GraphStep::Map(mode.name(), Box::new(StepRangeLocal { mode }))
    }
}

impl fmt::Display for StepRangeLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.mode {
            RangeMode::Limit(n) => write!(f, "limit(Scope.local, {n})"),
            RangeMode::Skip(n) => write!(f, "skip(Scope.local, {n})"),
            RangeMode::Tail(n) => write!(f, "tail(Scope.local, {n})"),
            RangeMode::Range { low, high } => write!(f, "range(Scope.local, {low}, {high})"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `limit(Scope, n)`: `Scope::Global` is [`limit(n)`](Self::limit) (the first `n`
    /// traversers of the stream); `Scope::Local` keeps the first `n` elements of the list or map
    /// each traverser holds (`g.V().values("name").fold().limit(Scope.local, 2)` →
    /// `["marko", "vadas"]`). A negative `n` fails with [`TraverserError::InvalidRange`] for the
    /// local form; for the global form a negative `n` becomes `range(0, n)`, so `-1` means no
    /// limit (TinkerPop's sentinel) and anything below fails with `InvalidRange`. A negative
    /// global `skip`/`tail` count fails with `InvalidRange` as well.
    pub fn limit_scoped(self, scope: Scope, n: i64) -> Self {
        match scope {
            Scope::Global => match usize::try_from(n) {
                Ok(n) => self.limit(n),
                Err(_) => self.range(0, n),
            },
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Limit(n))),
        }
    }

    /// `skip(Scope, n)`: `Scope::Global` is [`skip(n)`](Self::skip); `Scope::Local` drops the
    /// first `n` elements of the list or map each traverser holds
    /// (`g.V().values("name").fold().skip(Scope.local, 4)` → `["ripple", "peter"]`).
    pub fn skip_scoped(self, scope: Scope, n: i64) -> Self {
        match scope {
            Scope::Global => match usize::try_from(n) {
                Ok(n) => self.skip(n),
                Err(_) => self.range(n, -1),
            },
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Skip(n))),
        }
    }

    /// `tail(Scope, n)`: `Scope::Global` is [`tail(n)`](Self::tail); `Scope::Local` keeps the
    /// last `n` elements of the list or map each traverser holds. Gremlin's `tail(Scope.local)`
    /// is `tail_scoped(Scope::Local, 1)`.
    pub fn tail_scoped(self, scope: Scope, n: i64) -> Self {
        match scope {
            Scope::Global => match usize::try_from(n) {
                Ok(n) => self.tail(n),
                Err(_) => self.range(n, -1),
            },
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Tail(n))),
        }
    }

    /// `range(Scope, low, high)`: `Scope::Global` is [`range(low, high)`](Self::range);
    /// `Scope::Local` keeps the elements `[low, high)` of the list or map each traverser holds
    /// (`high == -1`: no upper bound). Invalid bounds fail with [`TraverserError::InvalidRange`].
    ///
    /// ```
    /// use graphersal::prelude::*;
    ///
    /// let graph = TraversalGraph::tinkerpop_modern();
    /// let g = GraphTraversalSource::new(&graph);
    /// let names = g.v(None).values("name").fold().range_scoped(Scope::Local, 1, 3).to_list();
    /// assert!(names.is_ok());
    /// ```
    pub fn range_scoped(self, scope: Scope, low: i64, high: i64) -> Self {
        match scope {
            Scope::Global => self.range(low, high),
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Range { low, high })),
        }
    }
}

impl __ {
    /// Anonymous `limit(Scope, n)`; see [`GraphTraversalSource::limit_scoped`].
    pub fn limit_scoped<'g, S: GraphStorage + 'g>(
        scope: Scope,
        n: i64,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().limit_scoped(scope, n)
    }

    /// Anonymous `skip(Scope, n)`; see [`GraphTraversalSource::skip_scoped`].
    pub fn skip_scoped<'g, S: GraphStorage + 'g>(
        scope: Scope,
        n: i64,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().skip_scoped(scope, n)
    }

    /// Anonymous `tail(Scope, n)`; see [`GraphTraversalSource::tail_scoped`].
    pub fn tail_scoped<'g, S: GraphStorage + 'g>(
        scope: Scope,
        n: i64,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().tail_scoped(scope, n)
    }

    /// Anonymous `range(Scope, low, high)`; see [`GraphTraversalSource::range_scoped`].
    pub fn range_scoped<'g, S: GraphStorage + 'g>(
        scope: Scope,
        low: i64,
        high: i64,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().range_scoped(scope, low, high)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `limit(Scope, n)`; see [`GraphTraversalSource::limit_scoped`].
    pub fn limit_scoped(self, scope: Scope, n: i64) -> Self {
        match scope {
            Scope::Global => match usize::try_from(n) {
                Ok(n) => self.limit(n),
                Err(_) => self.range(0, n),
            },
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Limit(n))),
        }
    }

    /// `skip(Scope, n)`; see [`GraphTraversalSource::skip_scoped`].
    pub fn skip_scoped(self, scope: Scope, n: i64) -> Self {
        match scope {
            Scope::Global => match usize::try_from(n) {
                Ok(n) => self.skip(n),
                Err(_) => self.range(n, -1),
            },
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Skip(n))),
        }
    }

    /// `tail(Scope, n)`; see [`GraphTraversalSource::tail_scoped`].
    pub fn tail_scoped(self, scope: Scope, n: i64) -> Self {
        match scope {
            Scope::Global => match usize::try_from(n) {
                Ok(n) => self.tail(n),
                Err(_) => self.range(n, -1),
            },
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Tail(n))),
        }
    }

    /// `range(Scope, low, high)`; see [`GraphTraversalSource::range_scoped`].
    pub fn range_scoped(self, scope: Scope, low: i64, high: i64) -> Self {
        match scope {
            Scope::Global => self.range(low, high),
            Scope::Local => self.push_step(StepRangeLocal::step(RangeMode::Range { low, high })),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepRangeLocal {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        self.mode.validate()?;
        let mode = self.mode;
        match slice_local(context, source.value, |len| mode.bounds(len))? {
            LocalSlice::Value(value) => Ok(Traverser::with_parts(
                value,
                source.path,
                source.bulk,
                source.sack,
            )),
            LocalSlice::Path => Err(Box::new(TraverserError::not_implemented(match mode {
                RangeMode::Limit(_) => "limit(local) on a path",
                RangeMode::Skip(_) => "skip(local) on a path",
                RangeMode::Tail(_) => "tail(local) on a path",
                RangeMode::Range { .. } => "range(local) on a path",
            }))),
        }
    }

    fn materializes(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> ElementProperty {
        ElementProperty::Array(values.iter().map(|v| ElementProperty::from(*v)).collect())
    }

    #[test]
    fn limit_list_stays_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").fold().limit_scoped(Scope::Local, 1).to_list(),
            rhai: [
                r#"g.V().values("name").fold().limit(Scope.local, 1)"#,
                r#"g.v().values("name").fold().limit(Scope::local, 1)"#,
            ],
            assert: |res| assert_eq!(res, vec![names(&["marko"])]),
        );
    }

    #[test]
    fn skip_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").fold().skip_scoped(Scope::Local, 4).to_list(),
            rhai: [
                r#"g.V().values("name").fold().skip(Scope.local, 4)"#,
                r#"g.v().values("name").fold().skip(Scope::Local, 4)"#,
            ],
            assert: |res| assert_eq!(res, vec![names(&["ripple", "peter"])]),
        );
    }

    #[test]
    fn tail_list() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").fold().tail_scoped(Scope::Local, 2).to_list(),
            rhai: [
                r#"g.V().values("name").fold().tail(Scope.local, 2)"#,
                r#"g.v().values("name").fold().tail(Scope::local, 2)"#,
            ],
            assert: |res| assert_eq!(res, vec![names(&["ripple", "peter"])]),
        );
    }

    #[test]
    fn range_list_out_of_bounds_is_empty() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().range_scoped(Scope::Local, 6, 7).to_list(),
            rhai: [
                r#"g.V().fold().range(Scope.local, 6, 7)"#,
                r#"g.v().fold().range(Scope::Local, 6, 7)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::Array(vec![])]),
        );
    }

    #[test]
    fn range_scalar_passes_through() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("age").range_scoped(Scope::Local, 20, 30).to_list(),
            rhai: [
                r#"g.V("1").values("age").range(Scope.local, 20, 30)"#,
                r#"g.v("1").values("age").range(Scope::local, 20, 30)"#,
            ],
            assert: |res| assert_eq!(res, vec![ElementProperty::from(29i64)]),
        );
    }

    #[test]
    fn bounds_of_every_mode() {
        assert_eq!(RangeMode::Limit(2).bounds(5), (0, 2));
        assert_eq!(RangeMode::Limit(9).bounds(5), (0, 5));
        assert_eq!(RangeMode::Skip(2).bounds(5), (2, 5));
        assert_eq!(RangeMode::Skip(9).bounds(5), (5, 5));
        assert_eq!(RangeMode::Tail(2).bounds(5), (3, 5));
        assert_eq!(RangeMode::Tail(9).bounds(5), (0, 5));
        assert_eq!(RangeMode::Range { low: 1, high: 3 }.bounds(5), (1, 3));
        assert_eq!(RangeMode::Range { low: 1, high: -1 }.bounds(5), (1, 5));
        assert_eq!(RangeMode::Range { low: 6, high: 7 }.bounds(5), (5, 5));
        assert!(RangeMode::Limit(-1).validate().is_err());
        assert!(RangeMode::Skip(-1).validate().is_err());
        assert!(RangeMode::Tail(-1).validate().is_err());
        assert!(RangeMode::Range { low: 3, high: 1 }.validate().is_err());
        assert!(RangeMode::Range { low: 0, high: -2 }.validate().is_err());
        assert!(RangeMode::Range { low: -1, high: 2 }.validate().is_err());
        assert!(RangeMode::Range { low: 0, high: -1 }.validate().is_ok());
    }

    #[test]
    fn profile_name() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .v(None)
            .fold()
            .limit_scoped(Scope::Local, 2)
            .profile()
            .unwrap();
        assert!(result.to_string().contains("limit(Scope.local, 2)"));
    }

    #[test]
    fn global_scope_with_a_negative_count() {
        // limit(-1) is TinkerPop's "no limit"
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).limit_scoped(Scope::Global, -1).count().to_list(),
            rhai: [
                r#"g.V().limit(Scope.global, -1).count()"#,
                r#"g.v().limit(Scope::global, -1).count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).limit_scoped(Scope::Global, -2).to_list(),
            rhai: [r#"g.V().limit(Scope.global, -2)"#],
            assert: |err| assert!(err.contains("Invalid range [0, -2)"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).skip_scoped(Scope::Global, -1).to_list(),
            rhai: [r#"g.V().skip(Scope.global, -1)"#],
            assert: |err| assert!(err.contains("low must be zero or a positive integer"), "{err}"),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).tail_scoped(Scope::Global, -1).to_list(),
            rhai: [r#"g.V().tail(Scope.global, -1)"#],
            assert: |err| assert!(err.contains("low must be zero or a positive integer"), "{err}"),
        );
    }

    #[test]
    fn anonymous_scoped_forms_in_child_traversals() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .local(__::out(None).limit_scoped(Scope::Global, 1))
                .count()
                .to_list(),
            rhai: [r#"g.V().local(__.out().limit(Scope.global, 1)).count()"#],
            // marko, josh, peter have out-neighbours
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .values("name")
                .fold()
                .local(__::tail_scoped(Scope::Local, 2))
                .to_list(),
            rhai: [r#"g.V().values("name").fold().local(__.tail(Scope.local, 2))"#],
            assert: |res| assert_eq!(res, &[names(&["ripple", "peter"])]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .values("name")
                .fold()
                .local(__::skip_scoped(Scope::Local, 4))
                .to_list(),
            rhai: [r#"g.V().values("name").fold().local(__.skip(Scope.local, 4))"#],
            assert: |res| assert_eq!(res, &[names(&["ripple", "peter"])]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .values("name")
                .fold()
                .local(__::range_scoped(Scope::Local, 1, 3))
                .to_list(),
            rhai: [r#"g.V().values("name").fold().local(__.range(Scope.local, 1, 3))"#],
            assert: |res| assert_eq!(res, &[names(&["vadas", "lop"])]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .local(__::out(None).skip_scoped(Scope::Global, 2))
                .count()
                .to_list(),
            rhai: [r#"g.V().local(__.out().skip(Scope.global, 2)).count()"#],
            // only marko has more than two out-neighbours
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .local(__::out(None).tail_scoped(Scope::Global, 1))
                .count()
                .to_list(),
            rhai: [r#"g.V().local(__.out().tail(Scope.global, 1)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .local(__::out(None).range_scoped(Scope::Global, 0, 1))
                .count()
                .to_list(),
            rhai: [r#"g.V().local(__.out().range(Scope.global, 0, 1)).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    #[test]
    fn a_path_is_not_sliced_yet() {
        for (mode, rhai) in [
            ("limit", r#"g.V("1").out().path().limit(Scope.local, 1)"#),
            ("skip", r#"g.V("1").out().path().skip(Scope.local, 1)"#),
            ("tail", r#"g.V("1").out().path().tail(Scope.local, 1)"#),
            ("range", r#"g.V("1").out().path().range(Scope.local, 0, 1)"#),
        ] {
            let rust = move |g: GraphTraversalSource<'_, &TraversalGraph>| {
                let t = g.v("1").out(None).path();
                match mode {
                    "limit" => t.limit_scoped(Scope::Local, 1),
                    "skip" => t.skip_scoped(Scope::Local, 1),
                    "tail" => t.tail_scoped(Scope::Local, 1),
                    _ => t.range_scoped(Scope::Local, 0, 1),
                }
                .to_list()
            };
            test_dual_err!(
                graph: GraphSource::tinkerpop_modern(),
                rust: rust,
                rhai: [rhai],
                assert: |err| assert!(err.contains(&format!("{mode}(local) on a path")), "{err}"),
            );
        }
    }
}
