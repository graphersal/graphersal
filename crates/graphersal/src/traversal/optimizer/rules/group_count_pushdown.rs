use crate::graph::GraphStorage;
use crate::traversal::optimizer::plan::Plan;
use crate::traversal::optimizer::rule::OptimizationRule;
use crate::traversal::step::GraphStep;

/// Optimizer rule that pushes `group_count().by(Label)` and `group().by(Label).by(Count)`
/// down into `Init` (`v` or `e`) step.
///
/// When a grouping step aggregating count by label immediately follows an `Init` step
/// (after consecutive filter steps have already been folded by `SourceFilterPushdownRule`),
/// this rule marks the `Init` step with `group_count_by_label: true` and removes the redundant
/// `Barrier` and attached `Modulator` steps.
#[derive(Default)]
pub(crate) struct GroupCountPushdownRule;

impl GroupCountPushdownRule {
    pub fn new() -> Self {
        Self
    }
}

impl<'g, S: GraphStorage + 'g> OptimizationRule<'g, S> for GroupCountPushdownRule {
    fn name(&self) -> &'static str {
        "group_count_pushdown"
    }

    fn apply(&self, steps: &mut Plan<'g, S>) -> bool {
        if steps.len() < 2 {
            return false;
        }

        // Check if the first step is an Init step ("v" or "e") that is not count_only or already group_count
        let is_valid_init = match &steps[0] {
            GraphStep::Init(name, init_step) => {
                // A source with folded `has(key, value)` conditions or an index access (whose
                // answered condition may have left the plan) is not counted per label from the
                // statistics: those count every element of the label.
                (*name == "v" || *name == "e")
                    && !init_step.is_count_only()
                    && !init_step.is_group_count_by_label()
                    && init_step.property_filters().is_empty()
                    && init_step.index_access().is_none()
            }
            _ => false,
        };
        if !is_valid_init {
            return false;
        }

        // Check if the step immediately following Init is a Barrier step grouping by label count
        let is_group_by_label_count = match &steps[1] {
            GraphStep::Barrier(_, barrier_step) => barrier_step.is_group_by_label_count(),
            _ => false,
        };

        if is_group_by_label_count {
            if let GraphStep::Init(_, init_step) = steps.step_mut(0) {
                init_step.set_group_count_by_label(true);
            }
            steps.absorb(1, 0);

            // Remove any trailing Modulator steps attached to the removed group step
            while steps.len() > 1 && matches!(&steps[1], GraphStep::Modulator(_, _)) {
                steps.absorb(1, 0);
            }

            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::By::{Count, Label};
    use crate::traversal::optimizer::rules::filter_pushdown::SourceFilterPushdownRule;
    use crate::{__, GraphSource};

    #[test]
    fn test_group_count_pushdown_v_by_label() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.v(None).group_count().by(Label);

        let rule = GroupCountPushdownRule::new();
        let modified = rule.apply_to_steps(&mut traversal.steps);

        assert!(modified);
        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("v", step) = &traversal.steps[0] {
            assert!(step.is_group_count_by_label());
        } else {
            panic!("Expected Init('v') step");
        }
    }

    #[test]
    fn test_group_by_label_by_count_pushdown_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.v(None).group().by(Label).by(Count);

        let rule = GroupCountPushdownRule::new();
        let modified = rule.apply_to_steps(&mut traversal.steps);

        assert!(modified);
        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("v", step) = &traversal.steps[0] {
            assert!(step.is_group_count_by_label());
        } else {
            panic!("Expected Init('v') step");
        }
    }

    #[test]
    fn test_group_by_label_by_anonymous_count_pushdown_v() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.v(None).group().by(Label).by(__::count());

        let rule = GroupCountPushdownRule::new();
        let modified = rule.apply_to_steps(&mut traversal.steps);

        assert!(modified);
        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("v", step) = &traversal.steps[0] {
            assert!(step.is_group_count_by_label());
        } else {
            panic!("Expected Init('v') step");
        }
    }

    #[test]
    fn test_group_count_pushdown_e_by_label() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.e(None).group_count().by(Label);

        let rule = GroupCountPushdownRule::new();
        let modified = rule.apply_to_steps(&mut traversal.steps);

        assert!(modified);
        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("e", step) = &traversal.steps[0] {
            assert!(step.is_group_count_by_label());
        } else {
            panic!("Expected Init('e') step");
        }
    }

    #[test]
    fn test_group_count_pushdown_after_filter_pushdown() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();
        let g = g_lock.traversal();
        let mut traversal = g.v(None).has_label("person").group_count().by(Label);

        let filter_rule = SourceFilterPushdownRule::new();
        let group_rule = GroupCountPushdownRule::new();

        let filter_applied = filter_rule.apply_to_steps(&mut traversal.steps);
        assert!(filter_applied);

        let group_applied = group_rule.apply_to_steps(&mut traversal.steps);
        assert!(group_applied);

        assert_eq!(traversal.steps.len(), 1);
        if let GraphStep::Init("v", step) = &traversal.steps[0] {
            assert_eq!(step.labels(), Some(&["person".to_string()][..]));
            assert!(step.is_group_count_by_label());
        } else {
            panic!("Expected Init('v') step");
        }
    }

    #[test]
    fn test_non_matching_not_pushed_down() {
        let graph = GraphSource::tinkerpop_modern();
        let g_lock = graph.read();

        // 1. group().by(Label) without count
        let g1 = g_lock.traversal();
        let mut traversal1 = g1.v(None).group().by(Label);
        let rule = GroupCountPushdownRule::new();
        assert!(!rule.apply_to_steps(&mut traversal1.steps));

        // 2. group_count().by("age")
        let g2 = g_lock.traversal();
        let mut traversal2 = g2.v(None).group_count().by("age");
        assert!(!rule.apply_to_steps(&mut traversal2.steps));
    }
}
