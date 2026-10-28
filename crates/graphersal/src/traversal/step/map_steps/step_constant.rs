use crate::graph::GraphStorage;
use crate::*;

#[derive(Clone)]
struct StepConstant {
    arg: ElementProperty,
}

impl fmt::Display for StepConstant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "constant({})",
            crate::value::literal::rhai_literal(&self.arg)
        )
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Replaces every incoming value with the given constant.
    pub fn constant<T: Into<ElementProperty>>(self, arg: T) -> Self {
        self.push_step(GraphStep::Map(
            "constant",
            Box::new(StepConstant { arg: arg.into() }),
        ))
    }
}

impl __ {
    /// Replaces every incoming value with the given constant. Starts an anonymous traversal
    /// (`__.constant()`).
    pub fn constant<'g, S: GraphStorage + 'g, T: Into<ElementProperty>>(
        arg: T,
    ) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().constant(arg)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Replaces every incoming value with the given constant.
    pub fn constant<T: Into<ElementProperty>>(self, arg: T) -> Self {
        let step = GraphStep::Map("constant", Box::new(StepConstant { arg: arg.into() }));
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepMap<'g, S> for StepConstant {
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
        source: Traverser<S>,
    ) -> TraverserResult<Traverser<S>> {
        Ok(Traverser::with_parts(
            (&self.arg).into(),
            source.path,
            source.bulk,
            source.sack,
        ))
    }

    fn clone_box(&self) -> Box<dyn GraphStepMap<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn v_string_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").constant("hello").to_list(),
            rhai: [
                r#"g.V("1").constant("hello")"#,
                r#"g.v("1").constant("hello")"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("hello".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::constant("hello").is("hello")).id().to_list(),
            rhai: [
                r#"g.V("1").where(__.constant("hello").is("hello")).id()"#,
                r#"g.v("1").where(__.constant("hello").is("hello")).id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn where_t_inner() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").where_t(__::element().constant("hello").is("hello")).id().to_list(),
            rhai: [
                r#"g.V("1").where(__.element().constant("hello").is("hello")).id()"#,
                r#"g.v("1").where(__.element().constant("hello").is("hello")).id()"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::String("1".into())),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_string_double() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(vec!["1", "2"]).constant("hello").to_list(),
            rhai: [
                r#"g.V(["1", "2"]).constant("hello")"#,
                r#"g.v(["1", "2"]).constant("hello")"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0], ElementProperty::String("hello".into()));
                assert_eq!(res[1], ElementProperty::String("hello".into()));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_string_array() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").constant(vec!["hello", "world"]).to_list(),
            rhai: [
                r#"g.V("1").constant(["hello", "world"])"#,
                r#"g.v("1").constant(["hello", "world"])"#,
            ],
            assert: |res| assert_eq!(res[0], ElementProperty::Array(vec![ElementProperty::String("hello".into()), ElementProperty::String("world".into())])),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_integer_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").constant(10).to_list(),
            rhai: [
                r#"g.V("1").constant(10)"#,
                r#"g.v("1").constant(10)"#,
            ],
            assert: |res| assert_eq!(res[0].as_i64(), Some(10)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_integer_double() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(vec!["1", "2"]).constant(42).to_list(),
            rhai: [
                r#"g.V(["1", "2"]).constant(42)"#,
                r#"g.v(["1", "2"]).constant(42)"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_i64(), Some(42));
                assert_eq!(res[1].as_i64(), Some(42));
            },
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_float_simple() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v("1").constant(10.42).to_list(),
            rhai: [
                r#"g.V("1").constant(10.42)"#,
                r#"g.v("1").constant(10.42)"#,
            ],
            assert: |res| assert_eq!(res[0].as_f64(), Some(10.42)),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn v_float_double() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(vec!["1", "2"]).constant(42.1).to_list(),
            rhai: [
                r#"g.V(["1", "2"]).constant(42.1)"#,
                r#"g.v(["1", "2"]).constant(42.1)"#,
            ],
            assert: |res| {
                assert_eq!(res.len(), 2);
                assert_eq!(res[0].as_f64(), Some(42.1));
                assert_eq!(res[1].as_f64(), Some(42.1));
            },
        );
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").constant(10.42).profile().unwrap();
        assert!(result.to_string().contains("constant("));
    }
}
