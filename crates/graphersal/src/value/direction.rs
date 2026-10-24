use std::fmt;

/// The Apache TinkerPop `Direction` token: which end(s) of an edge a step walks to.
///
/// Used by `to_direction()`/`to_e()`/`to_v()` (Gremlin `to(Direction, ..)`, `toE(..)`,
/// `toV(..)`): `to_direction(Direction::Out, ..)` is `out(..)`, `Direction::In` is `in_(..)`,
/// `Direction::Both` is `both(..)`. Gremlin's aliases `Direction.from` and `Direction.to` are
/// `Direction::Out` and `Direction::In`.
///
/// In the Rhai DSL: `Direction.OUT` / `Direction.IN` / `Direction.BOTH` / `Direction.from` /
/// `Direction.to` (or `Direction::OUT` ...).
///
/// ```rust
/// use graphersal::prelude::*;
///
/// let graph = GraphSource::tinkerpop_modern();
/// let lock = graph.read();
/// let names = lock
///     .traversal()
///     .v("1")
///     .to_direction(Direction::Out, "knows")
///     .values("name")
///     .to_list()
///     .unwrap();
/// assert_eq!(names.len(), 2);
/// assert_eq!(Direction::Out.to_string(), "OUT");
/// ```
///
/// Closed set, so deliberately not `#[non_exhaustive]`: a directed edge has exactly two ends, plus
/// "either".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// `Direction.OUT` (alias `Direction.from`): the outgoing side / the edge's out-vertex.
    Out,
    /// `Direction.IN` (alias `Direction.to`): the incoming side / the edge's in-vertex.
    In,
    /// `Direction.BOTH`: either side.
    Both,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Out => f.write_str("OUT"),
            Direction::In => f.write_str("IN"),
            Direction::Both => f.write_str("BOTH"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Direction;

    #[test]
    fn display() {
        assert_eq!(Direction::Out.to_string(), "OUT");
        assert_eq!(Direction::In.to_string(), "IN");
        assert_eq!(Direction::Both.to_string(), "BOTH");
    }
}
