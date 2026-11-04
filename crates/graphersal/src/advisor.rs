//! The storage advisor (book page "Optimize"): ranked recommendations (create or drop a property
//! index, add or drop a compression rule) computed from the memory report, the workload
//! statistics and the catalog. Each names the ordinary catalog command that applies it;
//! nothing is applied automatically.
//!
//! For hosts and front ends: [`Graph::advise`](crate::storage::Graph::advise) (or
//! `AnyGraph::advise`) computes an [`Advice`]; [`Advice::to_property`] is the one form every
//! front end shows. [`Graph::apply_recommendations`](crate::storage::Graph::apply_recommendations)
//! applies several recommendations (a human's pick, such as one [`AdviceGroup`]) in one unit.

pub use crate::graph::advisor::{
    Advice, AdviceGroup, AdviceRequest, AppliedOutcome, Confidence, DismissedRecommendation,
    Impact, Recommendation, RecommendedAction, compute, measured_bytes,
};
