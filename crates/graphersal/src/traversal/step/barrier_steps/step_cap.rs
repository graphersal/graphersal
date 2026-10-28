use crate::graph::GraphStorage;
use crate::*;

#[derive(Default, Clone)]
struct StepCap {
    args: Option<Vec<String>>,
}

impl fmt::Display for StepCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cap({})", display_option_vec_string(&self.args))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Emits the value of the named side effect(s) (one map for several keys) after the stream is
    /// exhausted.
    pub fn cap<T: Into<ArgStrings>>(self, args: T) -> Self {
        let args = match args.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        self.push_step(GraphStep::Barrier("cap", Box::new(StepCap { args })))
    }
}

impl __ {
    /// Emits the value of the named side effect(s) (one map for several keys) after the stream is
    /// exhausted. Starts an anonymous traversal (`__.cap()`).
    pub fn cap<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(
        args: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().cap(args)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Emits the value of the named side effect(s) (one map for several keys) after the stream is
    /// exhausted.
    pub fn cap<T: Into<ArgStrings>>(self, args: T) -> Self {
        let args = match args.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = GraphStep::Barrier("cap", Box::new(StepCap { args }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepBarrier<'g, S> for StepCap {
    /// `cap()`'s output depends only on `context.side_effect`, never on the incoming stream, so
    /// it always emits exactly one traverser regardless of how many traversers reach it —
    /// including zero (e.g. `aggregate("x").has("nope").cap("x")` must still see the bucket
    /// `aggregate()` filled before the filter ran).
    fn always_emits(&self) -> bool {
        true
    }

    #[inline]
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        _source: Vec<Traverser<S>>,
    ) -> TraverserResult<Option<Traverser<S>>> {
        if self.args.is_none() {
            return Err(Box::new(TraverserError::QueryExecution {
                query: "cap".to_string(),
                reason: "args can't be empty for cap() step".to_string(),
            }));
        }

        let args = &self.args.as_ref().unwrap();
        if args.len() == 1 {
            let res = context.side_effect.get(args[0].as_str());
            match res {
                Some(value) => {
                    let value = value.read(&*context.storage)?;
                    Ok(Some(Traverser::start(context, value)))
                }
                None => Err(Box::new(TraverserError::Value(
                    ValueError::PropertyNotFound {
                        key: args[0].clone(),
                        element_type: "side_effect",
                    },
                ))),
            }
        } else {
            let mut res: StreamMap<S> = StreamMap::default();
            for r in args.iter() {
                let e_p = context.side_effect.get(r.as_str());
                match e_p {
                    Some(value) => {
                        res.insert(
                            TraverserValue::Computed(
                                ElementProperty::String(r.clone().into()),
                                None,
                            ),
                            value.read(&*context.storage)?,
                        );
                    }
                    None => {
                        return Err(Box::new(TraverserError::Value(
                            ValueError::PropertyNotFound {
                                key: r.clone(),
                                element_type: "side_effect",
                            },
                        )));
                    }
                }
            }
            Ok(Some(Traverser::start(context, TraverserValue::Object(res))))
        }
    }

    fn clone_box(&self) -> Box<dyn GraphStepBarrier<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cap()`'s output depends only on side effects collected so far, never on the incoming
    /// stream's size: it must still emit its side-effect value when zero traversers reach it at
    /// this point in the pipeline — here, `aggregate("x")` fills the bucket from all 6 vertices
    /// before `hasLabel("nope")` filters everything out, so `cap("x")` must still see the whole
    /// bucket (confirmed via `graphersal`: before this task's fix, this returned nothing).
    #[cfg(feature = "script")]
    #[test]
    fn cap_ignores_the_upstream_stream_size() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).aggregate("x").has_label("nope").cap("x").to_list(),
            rhai: [
                r#"g.V().aggregate("x").hasLabel("nope").cap("x")"#,
                r#"g.v().aggregate("x").has_label("nope").cap("x")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Array(items) => assert_eq!(items.len(), 6),
                    other => panic!("expected the whole aggregated bucket, got {other:?}"),
                }
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn string() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with_side_effect("$a", "hello").v(None).cap("$a").to_list(),
            rhai: [
                r#"g.withSideEffect("$a", "hello").V().cap("$a")"#,
                r#"g.with_side_effect("$a", "hello").v().cap("$a")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_string(), Some("hello"));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with_side_effect("$a", "hello").v(None).where_t(__::cap("$a")).to_list(),
            rhai: [
                r#"g.withSideEffect("$a", "hello").V().where(__.cap("$a"))"#,
                r#"g.with_side_effect("$a", "hello").v().where(__.cap("$a"))"#,
            ],
            assert: |res| assert_eq!(res.len(), 6),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with_side_effect("$a", "hello").v(None).where_t(__::id().cap("$a")).to_list(),
            rhai: [
                r#"g.withSideEffect("$a", "hello").V().where(__.id().cap("$a"))"#,
                r#"g.with_side_effect("$a", "hello").v().where(__.id().cap("$a"))"#,
            ],
            assert: |res| assert_eq!(res.len(), 6),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn number() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with_side_effect("$a", 42).v(None).cap("$a").to_list(),
            rhai: [
                r#"g.withSideEffect("$a", 42).V().cap("$a")"#,
                r#"g.with_side_effect("$a", 42).v().cap("$a")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                assert_eq!(res[0].as_i64(), Some(42));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn array() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.with_side_effect("$a", "hello").with_side_effect("$b", "world")
                .v(None).cap(vec!["$a", "$b"]).to_list(),
            rhai: [
                r#"g.withSideEffect("$a", "hello").withSideEffect("$b", "world").V().cap(["$a", "$b"])"#,
                r#"g.with_side_effect("$a", "hello").with_side_effect("$b", "world").v().cap(["$a", "$b"])"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 1);
                match &res[0] {
                    ElementProperty::Object(r) => {
                        assert_eq!(r.get("$a").unwrap().as_string(), Some("hello"));
                        assert_eq!(r.get("$b").unwrap().as_string(), Some("world"));
                    }
                    _ => panic!(),
                }
            },
        );
    }

    #[test]
    fn wrong() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.with_side_effect("$a", 42).v(None).cap("$b").to_list();
        assert!(result.is_err());
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g
            .with_side_effect("$a", 42)
            .v(None)
            .cap("$a")
            .profile()
            .unwrap();
        assert!(result.to_string().contains("cap("));
    }
}
