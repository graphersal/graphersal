use crate::graph::GraphStorage;
use crate::traversal::optimizer::plan::Plan;
use crate::traversal::optimizer::rule::OptimizationRule;
use crate::traversal::step::GraphStep;

/// Optimizer rule that pushes `count()` step down into `Init` (`v` or `e`) step.
///
/// When an aggregation step like `Count` immediately follows an `Init` step
/// (after consecutive filter steps have already been folded by `SourceFilterPushdownRule`),
/// this rule marks the `Init` step with `count_only: true` and removes the redundant `Count` step.
#[derive(Default)]
pub(crate) struct CountPushdownRule;

impl CountPushdownRule {
    pub fn new() -> Self {
        Self
    }
}

impl<'g, S: GraphStorage + 'g> OptimizationRule<'g, S> for CountPushdownRule {
    fn name(&self) -> &'static str {
        "count_pushdown"
    }

    fn apply(&self, steps: &mut Plan<'g, S>) -> bool {
        if steps.len() < 2 {
            return false;
        }

        // Check if the first step is an Init step ("v" or "e") that is not group_count. A source
        // with folded `has(key, value)` conditions or an index access is not counted from
        // storage: its filtered scan feeds the `count()` (which the executor collapses without
        // materializing more than the matches), unless the `property_index` rule marked its
        // index access countable (the index answers the only condition exactly, no label to
        // check): the count is then the number of entries in the index slice, answered from
        // the postings (book page "Property Indexes").
        let is_init = matches!(
            &steps[0],
            GraphStep::Init(name, init) if (*name == "v" || *name == "e")
                && !init.is_group_count_by_label()
                && match init.index_access() {
                    Some(access) => access.countable,
                    None => init.property_filters().is_empty(),
                }
        );
        if !is_init {
            return false;
        }

        // Check if the step immediately following Init is Count
        if matches!(&steps[1], GraphStep::Count(_, _)) {
            if let GraphStep::Init(_, init_step) = steps.step_mut(0) {
                init_step.set_count_only(true);
            }
            steps.absorb(1, 0);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphSource;
    use crate::traversal::optimizer::rules::filter_pushdown::SourceFilterPushdownRule;

    #[test]
    fn test_count_pushdown_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.v(None).count();

        let rule = CountPushdownRule::new();
        let modified = rule.apply_to_steps(&mut traversal.steps);

        assert!(modified);
        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("v", step) = &traversal.steps[0] {
            assert!(step.is_count_only());
        } else {
            panic!("Expected Init('v') step");
        }
    }

    #[test]
    fn test_count_pushdown_e() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.e(None).count();

        let rule = CountPushdownRule::new();
        let modified = rule.apply_to_steps(&mut traversal.steps);

        assert!(modified);
        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("e", step) = &traversal.steps[0] {
            assert!(step.is_count_only());
        } else {
            panic!("Expected Init('e') step");
        }
    }

    #[test]
    fn test_count_pushdown_after_filter_pushdown() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.v(None).has_label("person").count();

        let filter_rule = SourceFilterPushdownRule::new();
        let count_rule = CountPushdownRule::new();

        let filter_applied = filter_rule.apply_to_steps(&mut traversal.steps);
        assert!(filter_applied);

        let count_applied = count_rule.apply_to_steps(&mut traversal.steps);
        assert!(count_applied);

        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("v", step) = &traversal.steps[0] {
            assert_eq!(step.labels(), Some(&["person".to_string()][..]));
            assert!(step.is_count_only());
        } else {
            panic!("Expected Init('v') step");
        }
    }
}
