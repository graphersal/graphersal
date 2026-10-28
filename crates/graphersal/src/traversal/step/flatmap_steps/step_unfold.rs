use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepUnfold {}

impl fmt::Display for StepUnfold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unfold()")
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Unrolls a list into one traverser per element, and a map (`group()`, `project()`, ...)
    /// into one traverser per entry, in the map's order. An entry materializes as a single-key
    /// map and is read with [`select(Column::Keys)`](Self::select) / `select(Column::Values)`.
    /// A path unrolls into its objects, and a property that holds a list (or a map) unrolls like
    /// the list (or map) itself. Any other value passes through unchanged.
    pub fn unfold(self) -> Self {
        self.push_step(GraphStep::FlatMap(
            "unfold",
            Box::new(StepUnfold::default()),
        ))
    }
}

impl __ {
    /// Splits a list (or map, into entries) into its elements, one traverser each. Starts an
    /// anonymous traversal (`__.unfold()`).
    pub fn unfold<'g, S: GraphStorage + 'g>() -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().unfold()
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Splits a list (or map, into entries) into its elements, one traverser each.
    pub fn unfold(self) -> Self {
        let step = GraphStep::FlatMap("unfold", Box::new(StepUnfold::default()));
        self.push_step(step)
    }
}

/// Flattens a stored list (one traverser per element) or map (one `Entry` per key).
fn unfold_stored<S: GraphStorage>(
    stored: ElementProperty,
    path: PathRef,
    bulk: u64,
    sack: SackRef,
    output: &mut Vec<Traverser<S>>,
) {
    match stored {
        ElementProperty::Array(array) => {
            output.extend(
                array
                    .into_iter()
                    .map(|v| Traverser::with_parts(v.into(), path, bulk, sack)),
            );
        }
        ElementProperty::Object(obj) => {
            output.extend(obj.into_iter().map(|(key, value)| {
                let entry = (TraverserValue::from(key), TraverserValue::from(value));
                Traverser::with_parts(TraverserValue::Entry(Box::new(entry)), path, bulk, sack)
            }));
        }
        ElementProperty::Map(map) => {
            output.extend(map.into_iter().map(|(key, value)| {
                let entry = (TraverserValue::from(key), TraverserValue::from(value));
                Traverser::with_parts(TraverserValue::Entry(Box::new(entry)), path, bulk, sack)
            }));
        }
        _ => {}
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepFlatMap<'g, S> for StepUnfold {
    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
        output: &mut Vec<Traverser<S>>,
    ) -> TraverserResult<()> {
        match source.value {
            TraverserValue::Array(array) => {
                output.extend(
                    array
                        .into_iter()
                        .map(|v| Traverser::with_parts(v, source.path, source.bulk, source.sack)),
                );
            }
            // A map unrolls into one `Entry` per key, in the map's own order (TinkerPop:
            // `unfold()` of a `Map` emits its `Map.Entry`s). An entry itself is not iterable and
            // passes through, like any other scalar.
            TraverserValue::Object(obj) => {
                output.extend(obj.into_iter().map(|entry| {
                    Traverser::with_parts(
                        TraverserValue::Entry(Box::new(entry)),
                        source.path,
                        source.bulk,
                        source.sack,
                    )
                }));
            }
            TraverserValue::Record { keys, values } => {
                output.extend(keys.iter().zip(values).map(|(key, value)| {
                    let key =
                        TraverserValue::Computed(ElementProperty::String(key.to_compact()), None);
                    Traverser::with_parts(
                        TraverserValue::Entry(Box::new((key, value))),
                        source.path,
                        source.bulk,
                        source.sack,
                    )
                }));
            }
            TraverserValue::Computed(ElementProperty::Object(obj), _) => {
                output.extend(obj.into_iter().map(|(key, value)| {
                    let entry = (TraverserValue::from(key), TraverserValue::from(value));
                    Traverser::with_parts(
                        TraverserValue::Entry(Box::new(entry)),
                        source.path,
                        source.bulk,
                        source.sack,
                    )
                }));
            }
            TraverserValue::Computed(ElementProperty::Map(map), _) => {
                output.extend(map.into_iter().map(|(key, value)| {
                    let entry = (TraverserValue::from(key), TraverserValue::from(value));
                    Traverser::with_parts(
                        TraverserValue::Entry(Box::new(entry)),
                        source.path,
                        source.bulk,
                        source.sack,
                    )
                }));
            }
            // A plain `ElementProperty` array wrapped as `Computed` (a value computed outside the
            // stream); each element converts back through `TraverserValue::from(ElementProperty)`.
            // `cap("x")` of an `aggregate("x")` bucket is a `TraverserValue::Array` of the stored
            // stream values (handles intact), see `traversal/side_effect.rs`.
            TraverserValue::Computed(ElementProperty::Array(array), _) => {
                output.extend(array.into_iter().map(|v| {
                    Traverser::with_parts(v.into(), source.path, source.bulk, source.sack)
                }));
            }
            // A path unrolls into its objects, first position first (a `by()`-modulated path
            // already holds the modulated values).
            TraverserValue::Path(path) => {
                output.extend(path.objects().map(|object| {
                    Traverser::with_parts(object.clone(), source.path, source.bulk, source.sack)
                }));
            }
            // `values("list")` yields a lazy handle: read the stored value now and flatten it when
            // it is a list or a map; a scalar (or a missing value) passes through unchanged.
            TraverserValue::VertexProperty(prop, node, kind) => {
                let stored = if !kind.reads_property() {
                    None
                } else {
                    context
                        .storage
                        .vertex_properties(node)?
                        .and_then(|p| p.get(prop.as_str()))
                        .map(ElementProperty::to_logical)
                };
                match stored {
                    Some(
                        stored @ (ElementProperty::Array(_)
                        | ElementProperty::Object(_)
                        | ElementProperty::Map(_)),
                    ) => unfold_stored(stored, source.path, source.bulk, source.sack, output),
                    _ => output.push(Traverser::with_parts(
                        TraverserValue::VertexProperty(prop, node, kind),
                        source.path,
                        source.bulk,
                        source.sack,
                    )),
                }
            }
            TraverserValue::EdgeProperty(prop, edge, kind) => {
                let stored = if !kind.reads_property() {
                    None
                } else {
                    context
                        .storage
                        .edge_properties(edge)?
                        .and_then(|p| p.get(prop.as_str()))
                        .map(ElementProperty::to_logical)
                };
                match stored {
                    Some(
                        stored @ (ElementProperty::Array(_)
                        | ElementProperty::Object(_)
                        | ElementProperty::Map(_)),
                    ) => unfold_stored(stored, source.path, source.bulk, source.sack, output),
                    _ => output.push(Traverser::with_parts(
                        TraverserValue::EdgeProperty(prop, edge, kind),
                        source.path,
                        source.bulk,
                        source.sack,
                    )),
                }
            }
            value @ (TraverserValue::Vertex(_)
            | TraverserValue::Edge(_)
            | TraverserValue::PathProperty(_)
            | TraverserValue::Entry(_)
            | TraverserValue::Schema(_)
            | TraverserValue::Graph(_)
            | TraverserValue::Computed(..)) => output.push(Traverser::with_parts(
                value,
                source.path,
                source.bulk,
                source.sack,
            )),
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

    #[cfg(feature = "script")]
    #[test]
    fn vertex() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().unfold().count().to_list(),
            rhai: [
                r#"g.V().fold().unfold().count()"#,
                r#"g.v().fold().unfold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_wrong() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").unfold().count().to_list(),
            rhai: [
                r#"g.V("1").unfold().count()"#,
                r#"g.v("1").unfold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(1)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fold().where_t(__::unfold().count().is(6)).to_list(),
            rhai: [
                r#"g.V().fold().where(__.unfold().count().is(6))"#,
                r#"g.v().fold().where(__.unfold().count().is(6))"#,
            ],
            assert: |res| assert_eq!(res.len(), 1),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![1, 2, 3]).fold().where_t(__::inject(vec![4]).unfold()).to_list(),
            rhai: [
                r#"g.inject(1, 2, 3).fold().where(__.inject(4).unfold())"#,
                r#"g.inject(1, 2, 3).fold().where(__.inject(4).unfold())"#,
            ],
            assert: |res| assert_eq!(res.len(), 1),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn id() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).id().fold().unfold().to_list(),
            rhai: [
                r#"g.V().id().fold().unfold()"#,
                r#"g.v().id().fold().unfold()"#,
            ],
            assert: |res| {
                assert_eq!(res[0], ElementProperty::String("1".into()));
                assert_eq!(res[1], ElementProperty::String("2".into()));
                assert_eq!(res[2], ElementProperty::String("3".into()));
                assert_eq!(res[3], ElementProperty::String("4".into()));
                assert_eq!(res[4], ElementProperty::String("5".into()));
                assert_eq!(res[5], ElementProperty::String("6".into()));
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).fold().unfold().count().profile().unwrap();
        assert!(result.to_string().contains("unfold("));
    }

    /// `cap("x")` hands back `context.side_effect["x"]` as a bare `ElementProperty`
    /// (`TraverserValue::Computed`), not the richer `TraverserValue::Array`/`Object` shape a live
    /// traversal builds. `aggregate("x")` grows `context.side_effect["x"]` into an
    /// `ElementProperty::Array` (`push_side_effect`, `sideeffect_steps/step_aggregate.rs`), so
    /// `unfold()` must flatten it into one traverser per collected vertex, exactly like TinkerPop
    /// (confirmed via `graphersal`: before this fix, `g.V().aggregate("x").cap("x").unfold().count()`
    /// returned `1`, the whole bucket as one scalar, instead of `6`).
    #[cfg(feature = "script")]
    #[test]
    fn cap_of_aggregate_unfolds_into_one_traverser_per_element() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).aggregate("x").cap("x").unfold().count().to_list(),
            rhai: [
                r#"g.V().aggregate("x").cap("x").unfold().count()"#,
                r#"g.v().aggregate("x").cap("x").unfold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    /// Same bucket shape as `aggregate()` (both share `push_side_effect`), so `store("x")` needs
    /// the same fix.
    #[cfg(feature = "script")]
    #[test]
    fn cap_of_store_unfolds_into_one_traverser_per_element() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).store("x").cap("x").unfold().count().to_list(),
            rhai: [
                r#"g.V().store("x").cap("x").unfold().count()"#,
                r#"g.v().store("x").cap("x").unfold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
    }

    /// `tree("x")` populates `context.side_effect["x"]` with an `ElementProperty::Object`
    /// (already handled by the `Computed(ElementProperty::Object(..))` arm above, unlike
    /// `aggregate()`/`store()`'s `Array`): `cap("x").unfold()` must yield one entry per root of
    /// the tree. `g.V().out()` from the modern graph has 3 distinct source vertices with an
    /// outgoing edge (`marko`, `josh`, `peter`), so the tree has 3 root branches (confirmed via
    /// `graphersal` against the same query).
    #[cfg(feature = "script")]
    #[test]
    fn cap_of_tree_unfolds_into_one_entry_per_root() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).out(None).tree("x").cap("x").unfold().count().to_list(),
            rhai: [
                r#"g.V().out().tree("x").cap("x").unfold().count()"#,
                r#"g.v().out().tree("x").cap("x").unfold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(3)),
        );
    }

    /// `cap("a", "b")` (several keys) already builds a `TraverserValue::Object` directly (not a
    /// `Computed` wrapper), so `unfold()`'s pre-existing `Object` arm already flattens it into one
    /// `Entry` per key — no change needed for this shape, asserted here to lock it in alongside
    /// the `Array`/`tree` fixes above.
    #[cfg(feature = "script")]
    #[test]
    fn cap_of_two_keys_unfolds_into_one_entry_per_key() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with_side_effect("$a", "hello").with_side_effect("$b", "world")
                .v(None).cap(vec!["$a", "$b"]).unfold().count().to_list(),
            rhai: [
                r#"g.withSideEffect("$a", "hello").withSideEffect("$b", "world").V().cap(["$a", "$b"]).unfold().count()"#,
                r#"g.with_side_effect("$a", "hello").with_side_effect("$b", "world").v().cap(["$a", "$b"]).unfold().count()"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(2)),
        );
    }
}
