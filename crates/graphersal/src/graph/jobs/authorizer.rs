//! The authorizer a user's job runs under (book page "Scheduled Jobs", "Rights"): the job may
//! read, call saved queries and run embedders, and write exactly the data its stored effects
//! name (what its definer was allowed when the job was defined); everything else is denied.

use crate::graph::authorization::{
    AccessRequest, AccessRequestBuf, Action, ActionKind, Authorizer, Denied, Resource,
};
use crate::graph::definition::DefinitionKind;

/// The rights of one run of the job `job`: its stored `effects`.
#[derive(Debug, Clone)]
pub(crate) struct JobAuthorizer {
    job: String,
    effects: Vec<AccessRequestBuf>,
}

impl JobAuthorizer {
    pub(crate) fn new(job: impl Into<String>, effects: Vec<AccessRequestBuf>) -> Self {
        JobAuthorizer {
            job: job.into(),
            effects,
        }
    }

    /// Whether a stored data effect covers the data write `request`: the same action, and the
    /// effect's element kind and label are either open (`None`) or the request's.
    fn covers(&self, request: &AccessRequest<'_>) -> bool {
        let Resource::Data { element, label } = request.resource else {
            return false;
        };
        self.effects.iter().any(|effect| {
            let effect = effect.as_request();
            let Resource::Data {
                element: effect_element,
                label: effect_label,
            } = effect.resource
            else {
                return false;
            };
            effect.action.kind() == request.action.kind()
                && open_or_equal(effect_element, element)
                && open_or_equal(effect_label, label)
        })
    }
}

/// An effect's open (`None`) value covers every request; a stated one only the same stated
/// value (a request that does not state it might be any value).
fn open_or_equal<T: PartialEq>(effect: Option<T>, request: Option<T>) -> bool {
    match (effect, request) {
        (None, _) => true,
        (Some(effect), Some(request)) => effect == request,
        (Some(_), None) => false,
    }
}

impl Authorizer for JobAuthorizer {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        let allowed = match (request.action, request.resource) {
            (Action::Read, _) => true,
            (
                Action::Execute,
                Resource::Definition {
                    kind: Some(DefinitionKind::QUERY),
                    ..
                }
                | Resource::Embedder { .. },
            ) => true,
            (Action::Create | Action::Update | Action::Delete, Resource::Data { .. }) => {
                self.covers(request)
            }
            _ => false,
        };
        if allowed {
            return Ok(());
        }
        let what = match request.action.kind() {
            ActionKind::Create | ActionKind::Update | ActionKind::Delete
                if matches!(request.resource, Resource::Data { .. }) =>
            {
                "this write of data is not among the effects stored with the job (the saved query \
                 now writes more than when the job was defined): define the job again"
            }
            _ => "a job may read and write only the data its definer was allowed",
        };
        Err(Denied::new(format!("the job '{}': {what}", self.job)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ElementKind;

    fn data(
        action: Action<'static>,
        element: Option<ElementKind>,
        label: Option<&'static str>,
    ) -> AccessRequestBuf {
        AccessRequest::new(action, Resource::Data { element, label }).to_owned_request()
    }

    #[test]
    fn a_job_writes_only_its_effects() {
        let job = JobAuthorizer::new(
            "purge",
            vec![data(
                Action::Delete,
                Some(ElementKind::Vertex),
                Some("event"),
            )],
        );
        let allowed = |r: AccessRequestBuf| job.authorize(&r.as_request()).is_ok();
        assert!(allowed(data(Action::Read, None, None)));
        assert!(allowed(data(
            Action::Delete,
            Some(ElementKind::Vertex),
            Some("event")
        )));
        assert!(!allowed(data(
            Action::Delete,
            Some(ElementKind::Vertex),
            Some("person")
        )));
        assert!(!allowed(data(
            Action::Delete,
            Some(ElementKind::Vertex),
            None
        )));
        assert!(!allowed(data(
            Action::Create,
            Some(ElementKind::Vertex),
            Some("event")
        )));
        let open = JobAuthorizer::new("any", vec![data(Action::Create, None, None)]);
        assert!(
            open.authorize(
                &data(Action::Create, Some(ElementKind::Edge), Some("knows")).as_request()
            )
            .is_ok()
        );
        let schema = AccessRequest::new(Action::Update, Resource::Schema);
        let err = job.authorize(&schema).unwrap_err();
        assert!(err.reason().contains("purge"), "{}", err.reason());
    }
}
