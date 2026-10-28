use crate::graph::GraphStorage;
use crate::value::JsonPath;
use crate::*;

/// `.json_path(path)`: direct-access JSONPath navigation, without needing `.by()`/`select()`/
/// `.as()`. A `FlatMap` step, not `Map` (per AGENTS.md's step classification): `GraphStepMap`
/// must return exactly one `TraverserValue` per input traverser with no way to signal "emit
/// nothing", but a JSONPath lookup that misses (a missing key, an out-of-range index, or a
/// container-shape mismatch) must drop the traverser from the stream instead of erroring - the
/// same "not found" semantics `values(missingKey)` already has (step 3's decision). Only
/// `FlatMap`'s `TraverserResult<Option<Vec<Traverser<S>>>>` can express that.
#[derive(Clone)]
struct StepJsonPath {
    /// The original argument, kept only for `Debug`/`.profile()` rendering.
    raw: CompactString,
    /// Parsed eagerly at construction time (step builders in this codebase are conventionally
    /// infallible - no `-> TraverserResult<Self>` precedent), so a syntactically invalid
    /// path is captured here and surfaced as a normal `TraverserError` on first `process()` call
    /// rather than panicking or being silently ignored. The parse failure reason is stored as a
    /// plain `String` (rather than the already-built `TraverserError`) because `TraverserError`
    /// is not `Clone` and this struct is cloned via `clone_box()`; the full error is rebuilt from
    /// `raw` + `reason` on demand in `process()`.
    path: Result<JsonPath, String>,
}

impl fmt::Display for StepJsonPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "json_path({})",
            crate::value::literal::rhai_string(&self.raw)
        )
    }
}

/// Parses eagerly, reducing a parse failure to just its `reason` string (see [`StepJsonPath::path`]
/// doc comment for why the full, non-`Clone` `TraverserError` isn't stored directly).
fn parse_eager(path: &str) -> Result<JsonPath, String> {
    path.parse::<JsonPath>().map_err(|err| match err {
        TraverserError::Value(ValueError::InvalidJsonPath { reason, .. }) => reason,
        other => other.to_string(),
    })
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Navigates a JSON-shaped vertex/edge property (or an already-materialized stream value)
    /// with a dot-path + array-index JSONPath expression (see [`JsonPath`] for the supported
    /// subset). A path that resolves to nothing - a missing key, an out-of-range index, or a
    /// container-shape mismatch - drops the traverser from the stream, matching `values(key)`'s
    /// "not found" semantics; a syntactically invalid `path` surfaces as
    /// `ValueError::InvalidJsonPath` on first execution.
    ///
    /// **`json_path()` vs `jpath` keys:** `json_path(text)` is a standalone step: the path is a
    /// string argument and the step navigates whatever the stream carries. A [`JsonPath`] *key*
    /// (`values(jpath("a.b[1]"))`, `has(jpath(..))`, `by(jpath(..))`, `properties(jpath(..))`)
    /// is the same grammar used as the name of a property inside the step that reads it. Both
    /// resolve exactly one target (a missing target produces nothing) and share one parser; this
    /// step is unchanged by the jpath keys and does not accept a `JsonPath` object. See the user's
    /// guide page "Path keys (jpath)".
    pub fn json_path(self, path: &str) -> Self {
        self.push_step(GraphStep::FlatMap(
            "json_path",
            Box::new(StepJsonPath {
                raw: CompactString::new(path),
                path: parse_eager(path),
            }),
        ))
    }
}

impl __ {
    /// Reads a nested value from each element with a JSONPath (Graphersal extension, see the book
    /// page Path keys (jpath)). Starts an anonymous traversal (`__.json_path()`).
    pub fn json_path<'g, S: GraphStorage + 'g>(path: &str) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().json_path(path)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Reads a nested value from each element with a JSONPath (Graphersal extension, see the book
    /// page Path keys (jpath)).
    pub fn json_path(self, path: &str) -> Self {
        let step = GraphStep::FlatMap(
            "json_path",
            Box::new(StepJsonPath {
                raw: CompactString::new(path),
                path: parse_eager(path),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepJsonPath {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        let path = match &self.path {
            Ok(path) => path,
            Err(reason) => {
                return Err(Box::new(TraverserError::Value(
                    ValueError::InvalidJsonPath {
                        path: self.raw.to_string(),
                        reason: reason.clone(),
                    },
                )));
            }
        };

        let resolved = TraverserValue::resolve_json_path(context, &source.value, path.segments())?;
        if let Some(value) = resolved {
            output.push(Traverser::with_parts(
                value,
                source.path,
                source.bulk,
                source.sack,
            ));
        }
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn GraphStepFlatMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "serde")]
    fn negative_index_reads_from_the_end_and_out_of_range_is_empty() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"t": [1, 2, 3]}))
            .next()
            .unwrap();
        let last = graph
            .traversal()
            .v("1")
            .json_path("t[-1]")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(last.as_i64(), Some(3));
        let first = graph
            .traversal()
            .v("1")
            .json_path("t[-3]")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(first.as_i64(), Some(1));
        assert!(
            graph
                .traversal()
                .v("1")
                .json_path("t[-4]")
                .next()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[cfg(feature = "serde")]
    fn direct_off_vertex() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .json_path("meta.k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    #[test]
    #[cfg(feature = "serde")]
    fn relative_off_values() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .values("meta")
            .json_path("k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    #[test]
    #[cfg(feature = "serde")]
    fn off_element_map() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .element_map(None)
            .json_path("meta.k")
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(result.as_i64(), Some(1));
    }

    #[test]
    #[cfg(feature = "serde")]
    fn missing_key_is_empty_not_error() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .json_path("meta.missing")
            .to_list()
            .unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    #[cfg(feature = "serde")]
    fn index_into_non_array_is_empty_not_error() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"tags": {"not": "an array"}}))
            .json_path("tags[0]")
            .to_list()
            .unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    #[cfg(feature = "serde")]
    fn profile_shows_json_path_in_plan() {
        use serde_json::json;

        let mut graph = TraversalGraph::tinkerpop_modern();
        let result = graph
            .traversal_mut()
            .v("1")
            .property_json(json!({"meta": {"k": 1}}))
            .json_path("meta.k")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("json_path(\"meta.k\")"));
    }

    /// A syntactically rejected path (recursive descent, unsupported by this engine's JSONPath
    /// subset) must surface as a normal `ValueError::InvalidJsonPath` on first execution, not
    /// panic - the parse failure is captured eagerly at construction but only returned once the
    /// step actually runs.
    #[test]
    fn rejected_syntax_is_invalid_json_path_error_not_panic() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let err = g
            .v("1")
            .json_path("$..k")
            .next()
            .expect_err("recursive descent is not supported by this engine's JSONPath subset");
        assert!(matches!(
            err.root_cause(),
            TraverserError::Value(ValueError::InvalidJsonPath { .. })
        ));
    }
}
