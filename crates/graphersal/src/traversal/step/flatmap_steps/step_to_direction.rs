//! TinkerPop 3.8's direction-parameterized vertex steps: `to(Direction, labels)`,
//! `toE(Direction, labels)`, `toV(Direction)`.
//!
//! Pure builder sugar: each pushes the existing `out`/`in`/`both`, `out_e`/`in_e`/`both_e` or
//! `out_v`/`in_v`/`both_v` step, so label pushdown and `.profile()` names stay those of the
//! underlying step.

use crate::graph::GraphStorage;
use crate::*;

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Gremlin `to(Direction, labels...)`: `Out` is [`out`](Self::out), `In` is
    /// [`in_`](Self::in_), `Both` is [`both`](Self::both).
    ///
    /// Named `to_direction` because `to()` is the `add_e()` target modulator.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let n = lock.traversal().v("1").to_direction(Direction::Out, "knows").count().next().unwrap();
    /// assert_eq!(n, Some(ElementProperty::Number(2.into())));
    /// ```
    pub fn to_direction<T: Into<ArgStrings>>(self, direction: Direction, labels: T) -> Self {
        match direction {
            Direction::Out => self.out(labels),
            Direction::In => self.in_(labels),
            Direction::Both => self.both(labels),
        }
    }

    /// Gremlin `toE(Direction, labels...)`: `Out` is [`out_e`](Self::out_e), `In` is
    /// [`in_e`](Self::in_e), `Both` is [`both_e`](Self::both_e).
    pub fn to_e<T: Into<ArgStrings>>(self, direction: Direction, labels: T) -> Self {
        match direction {
            Direction::Out => self.out_e(labels),
            Direction::In => self.in_e(labels),
            Direction::Both => self.both_e(labels),
        }
    }

    /// Gremlin `toV(Direction)`: `Out` is [`out_v`](Self::out_v), `In` is
    /// [`in_v`](Self::in_v), `Both` is [`both_v`](Self::both_v).
    pub fn to_v(self, direction: Direction) -> Self {
        match direction {
            Direction::Out => self.out_v(),
            Direction::In => self.in_v(),
            Direction::Both => self.both_v(),
        }
    }
}

impl __ {
    /// Anonymous `to(Direction, labels...)`; see [`GraphTraversalSource::to_direction`].
    pub fn to_direction<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        direction: Direction,
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_direction(direction, labels)
    }

    /// Anonymous `toE(Direction, labels...)`; see [`GraphTraversalSource::to_e`].
    pub fn to_e<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        direction: Direction,
        labels: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_e(direction, labels)
    }

    /// Anonymous `toV(Direction)`; see [`GraphTraversalSource::to_v`].
    pub fn to_v<'g, S: GraphStorage + 'g>(direction: Direction) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().to_v(direction)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Anonymous `to(Direction, labels...)`; see [`GraphTraversalSource::to_direction`].
    pub fn to_direction<T: Into<ArgStrings>>(self, direction: Direction, labels: T) -> Self {
        match direction {
            Direction::Out => self.out(labels),
            Direction::In => self.in_(labels),
            Direction::Both => self.both(labels),
        }
    }

    /// Anonymous `toE(Direction, labels...)`; see [`GraphTraversalSource::to_e`].
    pub fn to_e<T: Into<ArgStrings>>(self, direction: Direction, labels: T) -> Self {
        match direction {
            Direction::Out => self.out_e(labels),
            Direction::In => self.in_e(labels),
            Direction::Both => self.both_e(labels),
        }
    }

    /// Anonymous `toV(Direction)`; see [`GraphTraversalSource::to_v`].
    pub fn to_v(self, direction: Direction) -> Self {
        match direction {
            Direction::Out => self.out_v(),
            Direction::In => self.in_v(),
            Direction::Both => self.both_v(),
        }
    }
}

#[cfg(all(test, feature = "script"))]
mod tests {
    use super::*;

    fn names(res: &[ElementProperty]) -> Vec<&str> {
        let mut names: Vec<&str> = res.iter().filter_map(|v| v.as_str()).collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn anonymous_to_direction_in_a_child_traversal() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::to_direction(Direction::Out, "created"))
                .values("name")
                .to_list(),
            rhai: [
                r#"g.V().where(__.to(Direction.OUT, "created")).values("name")"#,
                r#"g.v().where(__.to(Direction::OUT, "created")).values("name")"#,
            ],
            assert: |res| assert_eq!(names(res), ["josh", "marko", "peter"]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::to_direction(Direction::In, "knows"))
                .values("name")
                .to_list(),
            rhai: [r#"g.V().where(__.to(Direction.IN, "knows")).values("name")"#],
            assert: |res| assert_eq!(names(res), ["josh", "vadas"]),
        );
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .where_t(__::to_direction(Direction::Both, None).count().is(1))
                .values("name")
                .to_list(),
            rhai: [r#"g.V().where(__.to(Direction.BOTH).count().is(1)).values("name")"#],
            assert: |res| assert_eq!(names(res), ["peter", "ripple", "vadas"]),
        );
    }

    #[test]
    fn anonymous_to_e_in_a_child_traversal() {
        for (direction, rhai, expected) in [
            (
                Direction::Out,
                r#"g.V().where(__.toE(Direction.OUT, "knows")).values("name")"#,
                vec!["marko"],
            ),
            (
                Direction::In,
                r#"g.V().where(__.toE(Direction.IN, "knows")).values("name")"#,
                vec!["josh", "vadas"],
            ),
            (
                Direction::Both,
                r#"g.V().where(__.to_e(Direction::BOTH, "knows")).values("name")"#,
                vec!["josh", "marko", "vadas"],
            ),
        ] {
            test_dual!(
                graph: GraphSource::tinkerpop_modern(),
                rust: |g| g
                    .v(None)
                    .where_t(__::to_e(direction, "knows"))
                    .values("name")
                    .to_list(),
                rhai: [rhai],
                assert: |res| assert_eq!(names(res), expected),
            );
        }
    }

    #[test]
    fn anonymous_to_v_in_a_child_traversal() {
        // marko's outgoing edges whose endpoint (in the given direction) is vadas
        for (direction, rhai, expected) in [
            (
                Direction::Out,
                r#"g.V("1").outE().where(__.toV(Direction.OUT).has("name", "vadas")).count()"#,
                0,
            ),
            (
                Direction::In,
                r#"g.V("1").outE().where(__.toV(Direction.IN).has("name", "vadas")).count()"#,
                1,
            ),
            (
                Direction::Both,
                r#"g.v("1").out_e().where(__.to_v(Direction::BOTH).has("name", "marko")).count()"#,
                3,
            ),
        ] {
            test_dual!(
                graph: GraphSource::tinkerpop_modern(),
                rust: |g| g
                    .v("1")
                    .out_e(None)
                    .where_t(__::to_v(direction).has(
                        "name",
                        if direction == Direction::Both { "marko" } else { "vadas" },
                    ))
                    .count()
                    .to_list(),
                rhai: [rhai],
                assert: |res| assert_eq!(res[0].as_i64(), Some(expected)),
            );
        }
    }
}
