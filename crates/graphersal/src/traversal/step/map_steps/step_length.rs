use crate::graph::GraphStorage;
use crate::traversal::step::local_scope::{LocalMapped, map_local_strings};
use crate::value::Scope;
use crate::*;

/// `length()`: the length of a string (`null` stays `null`; a list, map, vertex, edge or number is a
/// cast error, as in TinkerPop). With
/// `Scope::Local` it maps each string of the list a traverser holds to its length instead (a
/// `null` element stays `null`).
///
/// **Deviations from TinkerPop 3.8.2** (user's guide: "Type Conversion and length()"): the length
/// of a string counts Unicode scalar values (`chars().count()`), where TinkerPop counts Java
/// `char`s (UTF-16 code units). The two agree for every character in the Basic Multilingual Plane
/// (`"é"` and `"日本語"` count 1 and 3 in both); an astral-plane character such as an emoji counts
/// 1 here and 2 in TinkerPop. `length()` takes strings only: a list, path or map is
/// a cast error (use `count(local)` for those), so `fold().length()` does not work.
#[derive(Default, Clone)]
struct StepLength {
    scope: Scope,
}

impl fmt::Display for StepLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.scope {
            Scope::Global => write!(f, "length()"),
            Scope::Local => write!(f, "length(Scope.local)"),
        }
    }
}

/// The length of one string element of `length(local)`, measured like the single-value path.
/// The cast error for a non-string input (TinkerPop's `length()` accepts strings only).
fn not_a_string(actual: &str) -> Box<TraverserError> {
    Box::new(TraverserError::cast_exception_str(
        &["string"],
        actual.to_string(),
    ))
}

/// The length of a stored value: a string yields its length, `null` stays `null`, anything else is
/// a cast error.
fn string_length_of(value: &ElementProperty) -> TraverserResult<Option<usize>> {
    match value {
        ElementProperty::Null => Ok(None),
        ElementProperty::String(text) => Ok(Some(char_count(text))),
        ElementProperty::Compressed(text) => Ok(Some(text.with_text(char_count))),
        ElementProperty::Deduped(text) => Ok(Some(char_count(text.as_str()))),
        ElementProperty::Object(_) | ElementProperty::Map(_) => Err(not_a_string("object")),
        ElementProperty::Array(_) => Err(not_a_string("array")),
        ElementProperty::Vertex(_) => Err(not_a_string("vertex")),
        ElementProperty::Edge(_) => Err(not_a_string("edge")),
        ElementProperty::Number(_) => Err(not_a_string("number")),
        ElementProperty::Boolean(_) => Err(not_a_string("boolean")),
        ElementProperty::Uuid(_) => Err(not_a_string("uuid")),
        ElementProperty::Temporal(value) => Err(not_a_string(value.kind().name())),
        ElementProperty::Vector(_) => Err(not_a_string("vector")),
    }
}

/// The number of Unicode scalar values (not UTF-8 bytes) of `text`.
fn char_count(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    }
}

fn string_length(text: &str) -> ElementProperty {
    ElementProperty::Number(char_count(text).into())
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// The length of a string; `null` stays `null`, any other value is a cast error.
    pub fn length(self) -> Self {
        self.length_scoped(Scope::Global)
    }

    /// `length(Scope)`: `Scope::Global` is [`length()`](Self::length); `Scope::Local` maps each
    /// string of the list a traverser holds to its length
    /// (`g.V().values("name").order().fold().length(Scope.local)` → `[4, 3, 5, 5, 6, 5]`); a
    /// `null` element or input stays `null`, any other element is a cast error.
    pub fn length_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("length", Box::new(StepLength { scope })))
    }
}

impl __ {
    /// Anonymous `length()`.
    pub fn length<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().length()
    }

    /// Anonymous `length(Scope)`; see [`GraphTraversalSource::length_scoped`].
    pub fn length_scoped<'g, S: GraphStorage + 'g>(scope: Scope) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().length_scoped(scope)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `length()`; see [`GraphTraversalSource::length`].
    pub fn length(self) -> Self {
        self.length_scoped(Scope::Global)
    }

    /// `length(Scope)`; see [`GraphTraversalSource::length_scoped`].
    pub fn length_scoped(self, scope: Scope) -> Self {
        self.push_step(GraphStep::Map("length", Box::new(StepLength { scope })))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepLength {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        let path = source.path;
        let bulk = source.bulk;
        let sack = source.sack;
        let value = match self.scope {
            Scope::Global => source.value,
            Scope::Local => {
                match map_local_strings(context, source.value, "length", |text| {
                    Ok(string_length(text))
                })? {
                    LocalMapped::Done(value) => {
                        return Ok(Traverser::with_parts(value, path, bulk, sack));
                    }
                    LocalMapped::Single(value) => value,
                }
            }
        };
        let length = match &value {
            TraverserValue::Computed(value, _) => string_length_of(value)?,
            TraverserValue::VertexProperty(prop, vertex_node, kind) => {
                if *kind == crate::value::PropertyHandleKind::Label {
                    context.storage.vertex_label(*vertex_node)?.map(char_count)
                } else if *kind == crate::value::PropertyHandleKind::Id {
                    Some(char_count(crate::graph::storage::stored_vertex_id(
                        &*context.storage,
                        *vertex_node,
                    )?))
                } else if !kind.reads_property() {
                    None
                } else {
                    match context
                        .storage
                        .vertex_properties(*vertex_node)?
                        .and_then(|vertex| vertex.get(prop).map(string_length_of))
                    {
                        Some(length) => length?,
                        None => None,
                    }
                }
            }
            TraverserValue::EdgeProperty(prop, edge_index, kind) => {
                if *kind == crate::value::PropertyHandleKind::Label {
                    context.storage.edge_label(*edge_index)?.map(char_count)
                } else if *kind == crate::value::PropertyHandleKind::Id {
                    Some(char_count(crate::graph::storage::stored_edge_id(
                        &*context.storage,
                        *edge_index,
                    )?))
                } else if !kind.reads_property() {
                    None
                } else {
                    match context
                        .storage
                        .edge_properties(*edge_index)?
                        .and_then(|edge| edge.get(prop).map(string_length_of))
                    {
                        Some(length) => length?,
                        None => None,
                    }
                }
            }
            TraverserValue::Object(_) | TraverserValue::Record { .. } => {
                return Err(not_a_string("object"));
            }
            TraverserValue::Array(_) => return Err(not_a_string("array")),
            TraverserValue::Vertex(_) => return Err(not_a_string("vertex")),
            TraverserValue::Edge(_) => return Err(not_a_string("edge")),
            TraverserValue::Path(_) => return Err(not_a_string("path")),
            TraverserValue::PathProperty(_) => return Err(not_a_string("path property")),
            TraverserValue::Entry(_) => return Err(not_a_string("map entry")),
            TraverserValue::Schema(_) => return Err(not_a_string("schema")),
            TraverserValue::Graph(_) => return Err(not_a_string("graph")),
        };

        match length {
            Some(length) => Ok(Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::Number(length.into()), None),
                path,
                bulk,
                sack,
            )),
            None => Ok(Traverser::with_parts(
                TraverserValue::Computed(ElementProperty::Null, None),
                path,
                bulk,
                sack,
            )),
        }
    }

    fn materializes(&self) -> bool {
        self.scope == Scope::Local
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v_string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").length().to_list(),
            rhai: [
                r#"g.V("1").values("name").length()"#,
                r#"g.v("1").values("name").length()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(5)),
        );
    }

    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").where_t(__::length().is(5)).to_list(),
            rhai: [
                r#"g.V("1").values("name").where(__.length().is(5))"#,
                r#"g.v("1").values("name").where(__.length().is(5))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").values("name").where_t(__::to_lower().length().is(5)).to_list(),
            rhai: [
                r#"g.V("1").values("name").where(__.to_lower().length().is(5))"#,
                r#"g.v("1").values("name").where(__.to_lower().length().is(5))"#,
            ],
            assert: |res| assert_eq!(res[0].as_string(), Some("marko")),
        );
    }

    #[test]
    fn v_wrong_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).values("age").length().next();
        assert!(result.is_err());
    }

    #[test]
    fn v_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").label().length().to_list(),
            rhai: [
                r#"g.V("1").label().length()"#,
                r#"g.v("1").label().length()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[test]
    fn e_label() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("5").in_e(None).label().length().to_list(),
            rhai: [
                r#"g.V("5").inE().label().length()"#,
                r#"g.v("5").in_e().label().length()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(7)),
        );
    }

    #[test]
    fn e_wrong_number() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).values("weight").length().next();
        assert!(result.is_err());
    }

    /// TinkerPop's `length()` accepts strings only: a list, map, vertex, edge or path is a cast error.
    #[test]
    fn collections_and_elements_are_rejected() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        assert!(
            g_lock
                .traversal()
                .v(None)
                .values("name")
                .fold()
                .length()
                .next()
                .is_err()
        );
        assert!(g_lock.traversal().v("1").length().next().is_err());
        assert!(g_lock.traversal().e(None).limit(1).length().next().is_err());
        assert!(
            g_lock
                .traversal()
                .v("1")
                .out(None)
                .path()
                .length()
                .next()
                .is_err()
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").values("name").length().profile().unwrap();
        assert!(result.to_string().contains("length("));
    }
}
