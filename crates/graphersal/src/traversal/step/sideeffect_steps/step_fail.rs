//! `fail()` / `fail(message)`: stops the traversal with an error when a traverser reaches it.

use compact_str::CompactString;

use crate::graph::GraphStorage;
use crate::*;

/// The message of `fail()` without an argument (TinkerPop's default).
pub(crate) const DEFAULT_FAIL_MESSAGE: &str = "fail() step triggered";

/// `fail()` (TinkerPop `FailStep`); see [`GraphTraversalSource::fail`].
#[derive(Clone)]
struct StepFail {
    message: Option<CompactString>,
}

impl fmt::Display for StepFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            Some(message) => write!(f, "fail({})", crate::value::literal::rhai_string(message)),
            None => write!(f, "fail()"),
        }
    }
}

fn fail_step<'g, S: GraphStorage + 'g>(message: Option<&str>) -> GraphStep<'g, S> {
    GraphStep::SideEffect(
        "fail",
        Box::new(StepFail {
            message: message.map(CompactString::new),
        }),
    )
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// `fail()` / `fail(message)` (TinkerPop `FailStep`): the first traverser that reaches this
    /// step stops the whole traversal with [`TraverserError::Fail`], carrying `message` (or
    /// `"fail() step triggered"`) and the traverser's value. Like any failing traversal, a
    /// mutating one is rolled back. Use it to assert that a branch is never taken:
    /// `g.V().coalesce(__.out("knows"), __.fail("no friends"))`.
    /// Details: `book/src/traversal/steps/fail.md`.
    ///
    /// ```rust
    /// use graphersal::prelude::*;
    ///
    /// let graph = GraphSource::tinkerpop_modern();
    /// let lock = graph.read();
    /// let err = lock.traversal().v("2").fail(Some("vadas reached")).to_list().unwrap_err();
    /// assert!(err.to_string().contains("vadas reached"));
    /// ```
    pub fn fail(self, message: Option<&str>) -> Self {
        self.push_step(fail_step(message))
    }
}

impl __ {
    /// Anonymous `fail()` / `fail(message)`; see [`GraphTraversalSource::fail`].
    pub fn fail<'g, S: GraphStorage + 'g>(message: Option<&str>) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().fail(message)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `fail()` / `fail(message)`; see [`GraphTraversalSource::fail`].
    pub fn fail(self, message: Option<&str>) -> Self {
        self.push_step(fail_step(message))
    }
}

/// A short rendering of the failing traverser's value for the error: `v[id]`/`e[id]` for an
/// element, the JSON text of anything else, cut after 80 characters.
fn render_value<S: GraphStorage>(
    context: &TraverserContext<'_, S>,
    value: &TraverserValue<S>,
) -> String {
    match value {
        TraverserValue::Vertex(_) | TraverserValue::Edge(_) => {
            let prefix = if matches!(value, TraverserValue::Vertex(_)) {
                "v"
            } else {
                "e"
            };
            match TraverserValue::fetch_id(context, value) {
                Ok(Some(id)) => format!("{prefix}[{id}]"),
                Ok(None) | Err(_) => format!("{prefix}[?]"),
            }
        }
        other => {
            let text = other
                .into_node_property(context.storage)
                .ok()
                .and_then(|property| crate::value::physical::to_json_text(&property).ok())
                .unwrap_or_else(|| other.type_name().to_string());
            if text.chars().count() > 80 {
                let cut: String = text.chars().take(80).collect();
                format!("{cut}...")
            } else {
                text
            }
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepSideEffect<'g, S> for StepFail {
    fn process(
        &self,
        context: &mut TraverserContext<'_, S>,
        source: &Traverser<S>,
    ) -> TraverserResult<()> {
        Err(Box::new(TraverserError::Fail {
            message: self
                .message
                .as_deref()
                .unwrap_or(DEFAULT_FAIL_MESSAGE)
                .to_string(),
            value: render_value(context, &source.value),
        }))
    }

    fn clone_box(&self) -> Box<dyn GraphStepSideEffect<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "script")]
    #[test]
    fn fails_with_the_default_message() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fail(None).to_list(),
            rhai: [r#"g.V().fail()"#, r#"g.v().fail()"#],
            assert: |err| assert!(err.to_string().contains("fail() step triggered"), "{err}"),
        );
    }

    #[cfg(feature = "script")]
    #[test]
    fn fails_with_the_message() {
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).fail(Some("msg")).to_list(),
            rhai: [r#"g.V().fail("msg")"#],
            assert: |err| assert!(err.to_string().contains("msg"), "{err}"),
        );
    }

    /// Only a traverser that reaches the step fails the traversal.
    #[cfg(feature = "script")]
    #[test]
    fn only_when_reached() {
        test_dual!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).coalesce(vec![__::values("name"), __::fail(None)]).count().to_list(),
            rhai: [r#"g.V().coalesce(__.values("name"), __.fail()).count()"#],
            assert: |res| assert_eq!(res[0].as_i64(), Some(6)),
        );
        test_dual_err!(
            graph: GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).union(vec![__::out(None), __::fail(None)]).to_list(),
            rhai: [r#"g.V().union(__.out(), __.fail())"#],
            assert: |err| assert!(err.to_string().contains("fail()"), "{err}"),
        );
    }

    #[test]
    fn profile_names_the_step() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let result = lock.traversal().v("99").fail(Some("x")).profile().unwrap();
        assert!(result.to_string().contains("fail(\"x\")"), "{result}");
    }
}
