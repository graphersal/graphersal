use crate::graph::GraphStorage;
use crate::*;

struct StepBy<'g, S: GraphStorage + 'g> {
    args: Option<By<'g, S>>,
}

impl<'g, S: GraphStorage + 'g> fmt::Display for StepBy<'g, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.args {
            Some(by) => write!(f, "by({by})"),
            None => f.write_str("by()"),
        }
    }
}

impl<'g, S: GraphStorage + 'g> Clone for StepBy<'g, S> {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
        }
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Modulates the step right before it (TinkerPop's `ByModulating`): what to read from each
    /// traverser, or how to sort/group it, depending on the step. Accepts a property key,
    /// `"label"`/`"id"`, a [`JsonPath`] (`by(jpath("meta.k"))`; a missing path is an
    /// unproductive `by()` like a missing property), an [`Order`], a child traversal (`__.values("name")`), or (for the few
    /// steps that take it at a specific position) a list of vertex labels.
    ///
    /// The steps that take `by()` are `order()`, `dedup()`, `group()`, `group_count()`,
    /// `select()` with step labels, `project()`, `path()`, `tree()`, `math()`, `value_map()`,
    /// `aggregate()`, `store()` and `glob_path()`. A `by()` after any other step, or with no step
    /// before it (`g.by(..)`), fails with [`TraverserError::InvalidModulator`] when
    /// the traversal executes, or earlier still when the plan-time validation pass finds it in a
    /// branch that never runs. See `book/src/optimizer/index.md` for how `by()` composes with
    /// the optimizer's pushdown rules.
    pub fn by<T: Into<By<'g, S>>>(self, args: T) -> Self {
        self.push_step(GraphStep::Modulator(
            "by",
            Box::new(StepBy {
                args: Some(args.into()),
            }),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// Modulates the step right before it, inside a child traversal (`__.by(...)`); see
    /// [`GraphTraversalSource::by`] for the accepted arguments and the steps that take `by()`.
    pub fn by<T: Into<By<'g, S>>>(self, args: T) -> Self {
        let step = GraphStep::Modulator(
            "by",
            Box::new(StepBy {
                args: Some(args.into()),
            }),
        );
        self.push_step(step)
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepModulator<'g, S> for StepBy<'g, S> {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        // Sorting or grouping by default; the plan walk reads it as a read after a step that
        // projects (`project`, `select`, `value_map`, ...).
        if let Some(crate::traversal::step::By::Property(key)) = &self.args {
            visit(Some(key), crate::graph::workload::AccessKind::OrderGroup);
        }
    }
    fn get_by(&mut self) -> Option<By<'g, S>> {
        self.args.take()
    }

    fn clone_box(&self) -> Box<dyn GraphStepModulator<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

impl<'g, S: GraphStorage + 'g> From<&str> for By<'g, S> {
    fn from(value: &str) -> Self {
        By::Property(CompactString::new(value))
    }
}

impl<'g, S: GraphStorage + 'g> From<&String> for By<'g, S> {
    fn from(value: &String) -> Self {
        By::Property(CompactString::new(value))
    }
}

impl<'g, S: GraphStorage + 'g> From<String> for By<'g, S> {
    fn from(value: String) -> Self {
        By::Property(CompactString::new(value))
    }
}

impl<'g, S: GraphStorage + 'g> From<&CompactString> for By<'g, S> {
    fn from(value: &CompactString) -> Self {
        By::Property(value.clone())
    }
}

impl<'g, S: GraphStorage + 'g> From<CompactString> for By<'g, S> {
    fn from(value: CompactString) -> Self {
        By::Property(value)
    }
}

/// `by(jpath("a.b[1]"))` reads the value at the path ([`By::JsonPath`]); a missing path is an
/// unproductive `by()`, exactly like a missing property.
impl<'g, S: GraphStorage + 'g> From<crate::value::JsonPath> for By<'g, S> {
    fn from(value: crate::value::JsonPath) -> Self {
        By::JsonPath(value)
    }
}

impl<'g, S: GraphStorage + 'g> From<&crate::value::JsonPath> for By<'g, S> {
    fn from(value: &crate::value::JsonPath) -> Self {
        By::JsonPath(value.clone())
    }
}

/// A [`PropKey`]: a name behaves as the string conversion (`"label"`/`"id"` are the tokens), a path
/// as [`By::JsonPath`].
impl<'g, S: GraphStorage + 'g> From<crate::value::PropKey> for By<'g, S> {
    fn from(value: crate::value::PropKey) -> Self {
        match value {
            crate::value::PropKey::Name(name) => By::from(name),
            crate::value::PropKey::Path(path) => By::JsonPath(path),
        }
    }
}

impl<'g, S: GraphStorage + 'g> From<AnonymousTraversal<'g, S>> for By<'g, S> {
    fn from(value: AnonymousTraversal<'g, S>) -> Self {
        By::Traversal(value)
    }
}

impl<'g, S: GraphStorage + 'g> From<Order> for By<'g, S> {
    fn from(value: Order) -> Self {
        match value {
            Order::Asc => By::Order(Order::Asc),
            Order::Desc => By::Order(Order::Desc),
            Order::Shuffle => By::Order(Order::Shuffle),
        }
    }
}

/// `by(Column::Keys)` / `by(Column::Values)` becomes [`By::Column`], an `order()` sort key.
impl<'g, S: GraphStorage + 'g> From<crate::value::Column> for By<'g, S> {
    fn from(value: crate::value::Column) -> Self {
        By::Column(value)
    }
}

/// A list of vertex labels, such as `vec!["directory", "file"]`, becomes [`By::Labels`]. An empty
/// list converts fine; the consuming step validates it.
impl<'g, S: GraphStorage + 'g> From<Vec<&str>> for By<'g, S> {
    fn from(value: Vec<&str>) -> Self {
        By::Labels(value.into_iter().map(CompactString::new).collect())
    }
}

/// A slice of vertex labels becomes [`By::Labels`]; see `From<Vec<&str>>`.
impl<'g, S: GraphStorage + 'g> From<&[&str]> for By<'g, S> {
    fn from(value: &[&str]) -> Self {
        By::Labels(
            value
                .iter()
                .map(|label| CompactString::new(label))
                .collect(),
        )
    }
}

/// An array of vertex labels, such as `["directory", "file"]`, becomes [`By::Labels`]; see
/// `From<Vec<&str>>`.
impl<'g, S: GraphStorage + 'g, const N: usize> From<[&str; N]> for By<'g, S> {
    fn from(value: [&str; N]) -> Self {
        By::Labels(
            value
                .iter()
                .map(|label| CompactString::new(label))
                .collect(),
        )
    }
}

/// A list of owned vertex labels becomes [`By::Labels`]; see `From<Vec<&str>>`.
impl<'g, S: GraphStorage + 'g> From<Vec<String>> for By<'g, S> {
    fn from(value: Vec<String>) -> Self {
        By::Labels(value.into_iter().map(CompactString::from).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::TraversalGraph;

    type TestBy = By<'static, TraversalGraph>;

    fn labels(by: TestBy) -> Option<Vec<CompactString>> {
        match by {
            By::Labels(labels) => Some(labels),
            By::Property(_)
            | By::Traversal(_)
            | By::Order(_)
            | By::Label
            | By::Id
            | By::Value
            | By::Identity
            | By::Key
            | By::Count
            | By::JsonPath(_)
            | By::Column(_) => None,
        }
    }

    #[test]
    fn label_lists_convert_to_by_labels() {
        let expected = Some(vec![
            CompactString::new("directory"),
            CompactString::new("file"),
        ]);
        assert_eq!(labels(TestBy::from(vec!["directory", "file"])), expected);
        let slice: &[&str] = &["directory", "file"];
        assert_eq!(labels(TestBy::from(slice)), expected);
        assert_eq!(labels(TestBy::from(["directory", "file"])), expected);
        assert_eq!(
            labels(TestBy::from(vec![
                "directory".to_string(),
                "file".to_string()
            ])),
            expected
        );
    }

    #[test]
    fn empty_label_list_converts() {
        assert_eq!(labels(TestBy::from(Vec::<&str>::new())), Some(Vec::new()));
        assert_eq!(labels(TestBy::from([] as [&str; 0])), Some(Vec::new()));
    }

    #[test]
    fn label_named_strings_stay_scalar() {
        // A single string is still a property key (or `label`/`id`), never a label list.
        assert_eq!(labels(TestBy::from("directory")), None);
    }

    /// `T.value` reads a property element, a jpath property and a map entry.
    #[cfg(feature = "script")]
    #[test]
    fn t_value_reads_property_elements_and_entries() {
        test_dual!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .properties("age")
                .order()
                .by(By::Value)
                .value()
                .to_list(),
            rhai: [
                r#"g.V().properties("age").order().by(T.value).value()"#,
                r#"g.v().properties("age").order().by(T::value).value()"#,
            ],
            assert: |res| assert_eq!(res, &[27.into(), 29.into(), 32.into(), 35.into()]),
        );
        test_dual!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g
                .v(None)
                .group_count()
                .by(By::Label)
                .unfold()
                .order()
                .by(By::Value)
                .select(crate::value::Column::Keys)
                .to_list(),
            rhai: [
                r#"g.V().groupCount().by(T.label).unfold().order().by(T.value).select(Column.keys)"#,
                r#"g.v().group_count().by(T::label).unfold().order().by(T::value).select(Column::keys)"#,
            ],
            assert: |res| assert_eq!(res, &["software".into(), "person".into()]),
        );
    }

    /// `T.value` of a value that is not a property element or an entry is a cast error, as in
    /// TinkerPop: a vertex, a computed number and a lazily read `values()` result alike.
    #[cfg(feature = "script")]
    #[test]
    fn t_value_of_a_plain_value_is_an_error() {
        test_dual_err!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).order().by(By::Value).to_list(),
            rhai: [
                r#"g.V().order().by(T.value)"#,
                r#"g.v().order().by(T::value)"#,
            ],
            assert: |err| {
                assert!(err.contains("got type 'vertex'"), "{err}");
                assert!(err.contains("T.key and T.value read a property element"), "{err}");
            },
        );
        test_dual_err!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").order().by(By::Value).to_list(),
            rhai: [
                r#"g.V().values("age").order().by(T.value)"#,
                r#"g.v().values("age").order().by(T::value)"#,
            ],
            assert: |err| {
                assert!(err.contains("got type 'int64'"), "{err}");
                assert!(err.contains("g.v().values(\"age\").order().by()"), "{err}");
                assert!(!err.contains("typeOf"), "no type search for a token misuse: {err}");
            },
        );
        test_dual_err!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g.inject(vec![1]).project(vec!["x"]).by(By::Value).to_list(),
            rhai: [
                r#"g.inject(1).project("x").by(T.value)"#,
                r#"g.inject(1).project("x").by(T::value)"#,
            ],
            assert: |err| assert!(err.contains("got type 'int64'"), "{err}"),
        );
        // `T.key` is just as strict.
        test_dual_err!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("name").dedup().by(By::Key).to_list(),
            rhai: [
                r#"g.V().values("name").dedup().by(T.key)"#,
                r#"g.v().values("name").dedup().by(T::key)"#,
            ],
            assert: |err| {
                assert!(err.contains("got type 'string'"), "{err}");
                assert!(err.contains("T.key and T.value read a property element"), "{err}");
            },
        );
    }

    /// `group()` without a `by()` keys by the incoming value itself ([`By::Identity`]), which
    /// is not the strict `T.value`.
    #[cfg(feature = "script")]
    #[test]
    fn group_without_by_keys_by_the_value_itself() {
        test_dual!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("lang").group_count().unfold().to_list(),
            rhai: [
                r#"g.V().values("lang").groupCount().unfold()"#,
                r#"g.v().values("lang").group_count().unfold()"#,
            ],
            assert: |res| assert_eq!(res.len(), 1, "{res:?}"),
        );
    }
}
