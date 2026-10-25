use crate::graph::GraphStorage;
use crate::traversal::step::PropertyFilter;
use crate::*;

#[derive(Default, Clone)]
struct StepV {
    arg: Option<Vec<String>>,
    labels: Option<Vec<String>>,
    /// `has(key, value)` conditions folded in by `source_filter_pushdown`.
    has: Vec<PropertyFilter>,
    count_only: bool,
    group_count_by_label: bool,
    /// The property index the source reads (set by the `property_index` rule).
    index: Option<crate::traversal::step::IndexAccess>,
}

impl fmt::Display for StepV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&super::render_source(
            "v",
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
    /// Starts at the vertices of the graph (all, or the ones with the given ids); after steps it
    /// starts a new scan per traverser.
    pub fn v<T: Into<ArgStrings>>(self, arg: T) -> Self {
        let arg = match arg.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };

        let step = StepV {
            arg,
            labels: None,
            has: Vec::new(),
            count_only: false,
            group_count_by_label: false,
            index: None,
        };
        self.push_step(GraphStep::Init("v", Box::new(step)))
    }
}

impl __ {
    /// Anonymous `V(ids)`: inside a child traversal each incoming traverser maps to every
    /// matching element, as TinkerPop's mid-traversal `V()`; the path restarts from the traverser's.
    pub fn v<'g, S: GraphStorage + 'g, T: Into<ArgStrings>>(arg: T) -> AnonymousTraversal<'g, S> {
        AnonymousTraversal::new().v(arg)
    }
}

impl<'g, S: GraphStorage + 'g> AnonymousTraversal<'g, S> {
    /// `V(ids)` inside a child traversal; see [`__::v`].
    pub fn v<T: Into<ArgStrings>>(self, arg: T) -> Self {
        let arg = match arg.into() {
            ArgStrings::None => None,
            ArgStrings::String(v) => Some(vec![v]),
            ArgStrings::List(v) => Some(v),
        };
        let step = StepV {
            arg,
            labels: None,
            has: Vec::new(),
            count_only: false,
            group_count_by_label: false,
            index: None,
        };
        self.push_step(GraphStep::Init("v", Box::new(step)))
    }
}

impl<'g, S: GraphStorage + 'g> GraphStepInit<'g, S> for StepV {
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
        (GraphNodeType::Vertex, &self.arg)
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
        let result = g.v(None).count().next().unwrap().unwrap();
        assert_eq!(result.as_i64().unwrap(), 6);
    }

    #[test]
    fn single() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v("1").id().next().unwrap().unwrap();
        assert_eq!(result, ElementProperty::String("1".into()));
    }

    #[test]
    fn list() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(vec!["1", "2"]).id().to_list().unwrap();
        assert_eq!(result[0], ElementProperty::String("1".into()));
        assert_eq!(result[1], ElementProperty::String("2".into()));
    }

    #[test]
    fn profile() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let result = g.v(None).count().profile().unwrap();
        assert!(result.to_string().contains("v("));
    }
}
