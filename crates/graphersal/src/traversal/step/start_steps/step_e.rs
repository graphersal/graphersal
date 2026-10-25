use crate::graph::GraphStorage;
use crate::traversal::step::PropertyFilter;
use crate::*;

#[derive(Default, Clone)]
struct StepE {
    arg: Option<Vec<String>>,
    labels: Option<Vec<String>>,
    /// `has(key, value)` conditions folded in by `source_filter_pushdown`.
    has: Vec<PropertyFilter>,
    count_only: bool,
    group_count_by_label: bool,
    /// The property index the source reads (set by the `property_index` rule).
    index: Option<crate::traversal::step::IndexAccess>,
}

impl fmt::Display for StepE {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&super::render_source(
            "e",
            &self.arg,
            &self.labels,
            &self.has,
            self.count_only,
            self.group_count_by_label,
            self.index.as_ref(),
        ))
    }
}

impl<'g, S: GraphStorage + 'g> GraphTraversalSource<'g, S> {
    /// Starts at the edges of the graph, all or the ones with the given ids.
    pub fn e<T: Into<ArgStrings>>(self, arg: T) -> Self {
        let arg = match arg.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };

        let step = StepE {
            arg,
            labels: None,
            has: Vec::new(),
            count_only: false,
            group_count_by_label: false,
            index: None,
        };
        self.push_step(GraphStep::Init("e", Box::new(step)))
    }
}

impl __ {
    /// Anonymous `E(ids)`: inside a child traversal each incoming traverser maps to every
    /// matching element, as TinkerPop's mid-traversal `E()`; the path restarts from the traverser's.
    pub fn e<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(arg: T) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().e(arg)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `E(ids)` inside a child traversal; see [`__::e`].
    pub fn e<T: Into<ArgStrings>>(self, arg: T) -> Self {
        let arg = match arg.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = StepE {
            arg,
            labels: None,
            has: Vec::new(),
            count_only: false,
            group_count_by_label: false,
            index: None,
        };
        self.push_step(GraphStep::Init("e", Box::new(step)))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepInit<'g, S> for StepE {
    fn property_access(
        &self,
        visit: &mut dyn FnMut(Option<&str>, crate::graph::workload::AccessKind),
    ) {
        // The index the source reads, then the folded `has()`s (scanned unless the index
        // serves them).
        let indexed = self.index.as_ref().map(|index| index.key.as_str());
        if let Some(key) = indexed {
            visit(Some(key), crate::graph::workload::AccessKind::FilterIndex);
        }
        for filter in &self.has {
            if Some(filter.key.as_str()) != indexed {
                visit(
                    Some(&filter.key),
                    crate::graph::workload::AccessKind::FilterScan,
                );
            }
        }
    }
    #[inline]
    fn process(
        &self,
        _context: &mut TraverserContext<'_, S>,
    ) -> (GraphNodeType, &Option<Vec<String>>) {
        (GraphNodeType::Edge, &self.arg)
    }

    fn labels(&self) -> Option<&[String]> {
        self.labels.as_deref()
    }

    fn set_labels(&mut self, labels: Vec<String>) {
        self.labels = Some(labels);
    }

    fn ids(&self) -> Option<&[String]> {
        self.arg.as_deref()
    }

    fn set_ids(&mut self, ids: Vec<String>) {
        self.arg = Some(ids);
    }

    fn property_filters(&self) -> &[PropertyFilter] {
        &self.has
    }

    fn push_property_filter(&mut self, filter: PropertyFilter) -> bool {
        self.has.push(filter);
        true
    }

    fn index_access(&self) -> Option<&crate::traversal::step::IndexAccess> {
        self.index.as_ref()
    }

    fn set_index_access(&mut self, access: Option<crate::traversal::step::IndexAccess>) -> bool {
        self.index = access;
        true
    }

    fn is_count_only(&self) -> bool {
        self.count_only
    }

    fn set_count_only(&mut self, count_only: bool) {
        self.count_only = count_only;
    }

    fn is_group_count_by_label(&self) -> bool {
        self.group_count_by_label
    }

    fn set_group_count_by_label(&mut self, group_count_by_label: bool) {
        self.group_count_by_label = group_count_by_label;
    }

    fn clone_box(&self) -> Box<dyn GraphStepInit<'g, S> + 'g> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).count().next().unwrap().unwrap();
        assert_eq!(result.as_i64().unwrap(), 6);
    }

    #[test]
    fn single() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e("0").id().next().unwrap().unwrap();
        assert_eq!(result, ElementProperty::String("0".into()));
    }

    #[test]
    fn list() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(vec!["0", "1"]).id().to_list().unwrap();
        assert_eq!(result[0], ElementProperty::String("0".into()));
        assert_eq!(result[1], ElementProperty::String("1".into()));
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.e(None).count().profile().unwrap();
        assert!(result.to_string().contains("e("));
    }

    /// Every fused form of `e()` (ids, labels, a pushed-down `count()` or `groupCount().by(label)`)
    /// renders its real shape in `.profile()` and returns what the unfused plan would.
    #[test]
    fn fused_forms_render_and_count_correctly() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let g = || lock.traversal();
        let shown = |metrics: TraversalMetrics| metrics.to_string();
        let count = |res: Vec<ElementProperty>| res[0].as_i64();

        assert!(shown(g().e(None).count().profile().unwrap()).contains("e().count()"));
        assert_eq!(count(g().e(None).count().to_list().unwrap()), Some(6));

        let p = shown(g().e(None).has_label("knows").count().profile().unwrap());
        assert!(p.contains(r#"e(labels: ["knows"]).count()"#), "{p}");
        assert_eq!(
            count(g().e(None).has_label("knows").count().to_list().unwrap()),
            Some(2)
        );

        let p = shown(g().e(vec!["0", "1"]).count().profile().unwrap());
        assert!(p.contains(r#"e("0", "1").count()"#), "{p}");

        // "0" is a knows edge, "1" a created one
        let p = shown(
            g().e(vec!["0", "1"])
                .has_label("knows")
                .count()
                .profile()
                .unwrap(),
        );
        assert!(
            p.contains(r#"e(ids: ["0", "1"], labels: ["knows"]).count()"#),
            "{p}"
        );
        assert_eq!(
            count(
                g().e(vec!["0", "1"])
                    .has_label("knows")
                    .count()
                    .to_list()
                    .unwrap()
            ),
            Some(1)
        );

        let p = shown(g().e("0").has_label("knows").profile().unwrap());
        assert!(p.contains(r#"e(ids: ["0"], labels: ["knows"])"#), "{p}");
    }

    #[test]
    fn fused_group_count_by_label() {
        let graph = GraphSource::tinkerpop_modern();
        let lock = graph.read();
        let g = || lock.traversal();
        let shown = |metrics: TraversalMetrics| metrics.to_string();
        let group = |res: Vec<ElementProperty>| -> Vec<(String, i64)> {
            let ElementProperty::Object(map) = &res[0] else {
                panic!("a map: {res:?}");
            };
            let mut pairs: Vec<(String, i64)> = map
                .iter()
                .map(|(k, v)| (k.to_string(), v.as_i64().unwrap()))
                .collect();
            pairs.sort();
            pairs
        };

        let p = shown(g().e(None).group_count().by(By::Label).profile().unwrap());
        assert!(p.contains("e().group_count().by(T.label)"), "{p}");
        assert_eq!(
            group(g().e(None).group_count().by(By::Label).to_list().unwrap()),
            vec![("created".to_string(), 4), ("knows".to_string(), 2)]
        );

        let p = shown(
            g().e(None)
                .has_label("knows")
                .group_count()
                .by(By::Label)
                .profile()
                .unwrap(),
        );
        assert!(
            p.contains(r#"e(labels: ["knows"]).group_count().by(T.label)"#),
            "{p}"
        );

        let p = shown(g().e("0").group_count().by(By::Label).profile().unwrap());
        assert!(p.contains(r#"e("0").group_count().by(T.label)"#), "{p}");

        let p = shown(
            g().e(vec!["0", "1"])
                .has_label("knows")
                .group_count()
                .by(By::Label)
                .profile()
                .unwrap(),
        );
        assert!(
            p.contains(r#"e(ids: ["0", "1"], labels: ["knows"]).group_count().by(T."#),
            "{p}"
        );
        assert_eq!(
            group(
                g().e(vec!["0", "1"])
                    .has_label("knows")
                    .group_count()
                    .by(By::Label)
                    .to_list()
                    .unwrap()
            ),
            vec![("knows".to_string(), 1)]
        );
    }
}
