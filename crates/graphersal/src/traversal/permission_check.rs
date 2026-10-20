//! The authorization check of one execution ([`Restrictions::check`]): asks the host's
//! [`Authorizer`] for every request of a plan (the step -> request mapping is
//! [`GraphStep::for_each_access`]), run by `GraphTraversalSource::run` before the optimizer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use compact_str::CompactString;

use crate::error::{TraverserError, TraverserResult};
use crate::graph::{
    AccessRequest, AccessRequestBuf, Action, AllowAll, Authorizer, GraphStorage, Resource,
};
use crate::traversal::context::is_admin_option;
use crate::traversal::step::{AccessFlow, GraphStep};
use crate::value::ElementProperty;

/// What a restricted traversal source may do: the host's authorizer, the options the host
/// preset (trusted, never checked) and requests the front end found outside the step list.
#[derive(Clone)]
pub(crate) struct Restrictions {
    /// The host's authorizer.
    pub(crate) authorizer: Arc<dyn Authorizer>,
    /// Whether the graph is one the script built itself: a request that writes data or the
    /// schema ([`AccessRequest::writes_graph`]) is not a write to the host graph then and is
    /// allowed without asking the authorizer.
    pub(crate) private_graph: bool,
    /// Options present when the restriction was attached (the host's presets): an
    /// administrative key holding exactly this value is not the query's own setting.
    pub(crate) preset_options: Arc<HashMap<CompactString, ElementProperty>>,
    /// Requests the plan makes besides its steps, with the operation that makes them (the
    /// script DSL adds `("with_side_effect", Create Data)` for a `subgraph()` into a host
    /// graph).
    pub(crate) required: Vec<(&'static str, AccessRequest<'static>)>,
}

impl Default for Restrictions {
    fn default() -> Self {
        Self {
            authorizer: Arc::new(AllowAll),
            private_graph: false,
            preset_options: Arc::default(),
            required: Vec::new(),
        }
    }
}

impl std::fmt::Debug for Restrictions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restrictions")
            .field("private_graph", &self.private_graph)
            .field("preset_options", &self.preset_options)
            .field("required", &self.required)
            .finish_non_exhaustive()
    }
}

/// The [`TraverserError::PermissionDenied`] of `operation` for `request`.
pub(crate) fn permission_denied(
    operation: &str,
    request: &AccessRequest<'_>,
    reason: impl Into<String>,
) -> Box<TraverserError> {
    Box::new(TraverserError::PermissionDenied {
        step: operation.to_string(),
        request: Box::new(request.to_owned_request()),
        reason: reason.into(),
    })
}

impl Restrictions {
    /// Asks the authorizer for `request`, made by `operation` (a step or source method name);
    /// a denial is [`TraverserError::PermissionDenied`]. A write on a private graph is allowed
    /// without asking.
    pub(crate) fn authorize(
        &self,
        operation: &str,
        request: &AccessRequest<'_>,
    ) -> TraverserResult<()> {
        if self.private_graph && request.writes_graph() {
            return Ok(());
        }
        self.authorizer
            .authorize(request)
            .map_err(|denied| permission_denied(operation, request, denied.reason()))
    }

    /// The file `operation` may `action` for `path`: asks the authorizer for `action` on
    /// [`Resource::File`] with the path as written, then resolves it
    /// ([`Authorizer::resolve_file`], e.g. under a file root).
    #[cfg_attr(not(feature = "script"), allow(dead_code))]
    pub(crate) fn authorize_file(
        &self,
        operation: &str,
        action: Action<'_>,
        path: &Path,
    ) -> TraverserResult<PathBuf> {
        let request = AccessRequest::new(action, Resource::File { path });
        self.authorize(operation, &request)?;
        self.authorizer
            .resolve_file(path)
            .map_err(|denied| permission_denied(operation, &request, denied.reason()))
    }

    /// Asks for `request` like [`Self::authorize`] unless an equal request was already allowed
    /// in this check (`seen`), so each distinct request is asked once per execution.
    fn authorize_once(
        &self,
        seen: &mut Vec<AccessRequestBuf>,
        operation: &str,
        request: &AccessRequest<'_>,
    ) -> TraverserResult<()> {
        if seen.iter().any(|asked| asked.as_request() == *request) {
            return Ok(());
        }
        self.authorize(operation, request)?;
        seen.push(request.to_owned_request());
        Ok(())
    }

    /// Checks the plan AS WRITTEN (before optimization): the extra requests, then every
    /// administrative option the query set itself (in key order), then every step in plan
    /// order, child traversals depth first. Each distinct request is asked once
    /// ([`Self::authorize_once`]). O(steps + options); fails with the first denial, a denied
    /// step with its location.
    pub(crate) fn check<'g, S: GraphStorage + 'g>(
        &self,
        steps: &mut [GraphStep<'g, S>],
        options: &HashMap<CompactString, ElementProperty>,
    ) -> TraverserResult<()> {
        let mut seen = Vec::new();
        for (operation, request) in &self.required {
            self.authorize_once(&mut seen, operation, request)?;
        }
        let mut set_by_query: Vec<&CompactString> = options
            .iter()
            .filter(|(key, value)| {
                is_admin_option(key) && self.preset_options.get(*key) != Some(*value)
            })
            .map(|(key, _)| key)
            .collect();
        set_by_query.sort();
        for key in set_by_query {
            self.authorize_once(
                &mut seen,
                &format!("with(\"{key}\")"),
                &AccessRequest::new(Action::Update, Resource::Option { key }),
            )?;
        }
        let mut denied: *const GraphStep<'g, S> = std::ptr::null();
        self.check_steps(steps, &mut seen, &mut denied)
            .map_err(|err| {
                // A denied step is located like a failing one (`at #N`); ids are positions of
                // the plan as written, the only plan there is before the optimizer runs. The
                // denied step is known by its address: its requests depend on the steps before
                // it (`AccessFlow`), so it cannot be re-checked on its own.
                GraphStep::number_children(steps);
                crate::traversal::source::locate_pre_run_error(steps, None, err, &|step| {
                    std::ptr::eq(step, denied)
                })
            })
    }

    /// Checks `steps` (one level, children depth first), each step with the
    /// [`AccessFlow`] the steps before it state; on a denial `denied` is the denied step.
    fn check_steps<'g, S: GraphStorage + 'g>(
        &self,
        steps: &mut [GraphStep<'g, S>],
        seen: &mut Vec<AccessRequestBuf>,
        denied: &mut *const GraphStep<'g, S>,
    ) -> TraverserResult<()> {
        let mut flow = AccessFlow::default();
        for step in steps {
            if let Err(err) = step.for_each_access_in(&flow, |request| {
                self.authorize_once(seen, step.kind_name(), &request)
            }) {
                *denied = step;
                return Err(err);
            }
            for child in step.child_traversals_mut() {
                self.check_steps(&mut child.steps, seen, denied)?;
            }
            flow = step.access_flow_after(flow);
        }
        Ok(())
    }
}

/// Adds every write (`Create`, `Update`, `Delete`) that [`Restrictions::check`] asks for
/// `steps` to `writes`, each once, in the same order and with the same [`AccessFlow`]s (child
/// traversals depth first); returns the flow after the last step. The effect inference of a
/// saved query's `g.gql(..)` call reads a lowered plan through it.
#[cfg(all(feature = "gql", feature = "script"))]
pub(crate) fn plan_write_requests<'g, S: GraphStorage + 'g>(
    steps: &mut [GraphStep<'g, S>],
    writes: &mut Vec<AccessRequestBuf>,
) -> AccessFlow {
    let mut flow = AccessFlow::default();
    for step in steps {
        let _ = step.for_each_access_in(&flow, |request| {
            if matches!(
                request.action,
                Action::Create | Action::Update | Action::Delete
            ) {
                let request = request.to_owned_request();
                if !writes.contains(&request) {
                    writes.push(request);
                }
            }
            Ok::<(), std::convert::Infallible>(())
        });
        for child in step.child_traversals_mut() {
            plan_write_requests(&mut child.steps, writes);
        }
        flow = step.access_flow_after(flow);
    }
    flow
}

/// Whether some step of `steps`, child traversals included, changes the graph
/// ([`GraphStep::mutates`]).
#[cfg_attr(not(feature = "script"), allow(dead_code))]
pub(crate) fn plan_mutates<'g, S: GraphStorage + 'g>(steps: &mut [GraphStep<'g, S>]) -> bool {
    steps.iter_mut().any(|step| {
        step.mutates()
            || step
                .child_traversals_mut()
                .into_iter()
                .any(|child| plan_mutates(&mut child.steps))
    })
}
