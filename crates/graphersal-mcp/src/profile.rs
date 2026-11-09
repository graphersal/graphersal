//! What one connection may do ([`Profile`]): the operator's configuration (tools, descriptions,
//! instructions, saved-query tools, resources, answer limit), the read-only flag and the access
//! policy its queries run under. The HOST picks the profile of every connection (from its own
//! handshake, the graph it resolved, the identity its request filter attached); a client never
//! chooses one inside MCP.

use std::sync::Arc;

use graphersal::auth::{AccessPolicy, ActionKind, Authorizer, ResourceKind};
use graphersal_session::{playground_authorizer, read_only_authorizer};
use serde_json::{Map, Value};

use crate::config::McpConfig;
use crate::protocol::MAX_TEXT_CHARS;

/// What a connection (or an HTTP request) of the endpoint may do: an [`McpConfig`] (what an
/// agent sees), the read-only flag (only reading tools; a query that would write is refused),
/// the answer limit (the configuration's `answer_limit_chars`, else [`MAX_TEXT_CHARS`]) and an
/// optional [`AccessPolicy`].
///
/// The policy is enforced by the engine: every `query`, `gql` and saved-query run is
/// authorized against it once per execution, before the optimizer (a denied write is a tool
/// error naming `PermissionDenied`, and changes nothing). A saved query that writes asks the
/// policy for each of its inferred effects first, so a policy that allows exactly the effects of
/// some saved queries (`AccessPolicy::read_only()` plus their `Create`/`Update`/`Delete` on
/// `Data`) lets an agent run those queries but write nothing else. Without a policy the queries
/// run under the playground's authorizers (`graphersal_session::playground_authorizer`, or
/// `read_only_authorizer` when read-only), as the dev server always did. A read-only profile
/// with a policy denies the writes on top of it.
///
/// The built-in tools that write through the session rather than through a query
/// (`set_schema`, `patch_schema`, the compression and index tools, `apply_recommendations`) are
/// offered only when the policy allows what they write (the schema's `Update`, a catalog
/// definition's `Create`/`Update`/`Delete`); otherwise they are left out of `tools/list` and a
/// call of one is refused like a tool the configuration does not offer.
///
/// Bound saved-query parameters ([`Profile::with_bound_params`]) fix parameters for the
/// connection: a host that serves one project (a user, a tenant) per connection binds the
/// parameter that names it, and the agent can neither see nor change it.
///
/// [`Mcp`](crate::Mcp) has a default profile ([`Mcp::profile`](crate::Mcp::profile)); a host
/// gives a connection another with [`Connection::with_profile`](crate::Connection::with_profile)
/// or an HTTP request with [`Mcp::handle_with_profile`](crate::Mcp::handle_with_profile). A
/// connection keeps the profile it was built with: a host that changes its profiles later
/// affects only the connections it builds afterwards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub(crate) config: Arc<McpConfig>,
    pub(crate) read_only: bool,
    pub(crate) policy: Option<AccessPolicy>,
    pub(crate) bound_params: Map<String, Value>,
}

impl Profile {
    /// A profile with the default configuration (every tool of the mode with its built-in
    /// description), only reading tools when `read_only`, no access policy of its own.
    pub fn new(read_only: bool) -> Self {
        Self {
            config: Arc::new(McpConfig::default()),
            read_only,
            policy: None,
            bound_params: Map::new(),
        }
    }

    /// Serves the operator's configuration (the answer limit included).
    pub fn with_config(mut self, config: McpConfig) -> Self {
        self.config = Arc::new(config);
        self
    }

    /// Runs the queries under `policy` (and offers the session's writing tools only where it
    /// allows their writes).
    pub fn with_access_policy(mut self, policy: AccessPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Binds saved-query parameters for this profile: name -> value, added to the parameters
    /// bound before (a name bound again takes the new value).
    ///
    /// A bound parameter is left out of every saved-query tool's input schema in `tools/list`
    /// (its `properties` and `required`), and a `tools/call` of a saved-query tool or of
    /// `run_saved_query` gets it set to the bound value, whatever the client sent, for every
    /// saved query that declares a parameter of that name (a query that does not declare it is
    /// called unchanged). The answer's `call:` line shows the call without the bound
    /// parameters. `list_saved_queries` and the resource `graphersal://saved-queries` still list
    /// a query's declared parameters, bound ones included.
    ///
    /// ```
    /// use graphersal_mcp::Profile;
    /// use serde_json::json;
    ///
    /// let profile = Profile::new(false)
    ///     .with_bound_params([("project".to_string(), json!("/work/a"))]);
    /// assert_eq!(profile.bound_params()["project"], json!("/work/a"));
    /// ```
    pub fn with_bound_params(mut self, params: impl IntoIterator<Item = (String, Value)>) -> Self {
        self.bound_params.extend(params);
        self
    }

    /// The bound saved-query parameters ([`Profile::with_bound_params`]).
    pub fn bound_params(&self) -> &Map<String, Value> {
        &self.bound_params
    }

    /// Sets the largest tool answer in characters (`None`: [`MAX_TEXT_CHARS`]).
    pub fn with_max_chars(mut self, max_chars: Option<usize>) -> Self {
        Arc::make_mut(&mut self.config).answer_limit_chars = max_chars;
        self
    }

    /// Whether only reading tools are offered.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The configuration served.
    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    /// The access policy of the queries, if the profile has one of its own.
    pub fn access_policy(&self) -> Option<&AccessPolicy> {
        self.policy.as_ref()
    }

    /// The largest tool answer in characters.
    pub fn max_chars(&self) -> usize {
        self.config.answer_limit_chars.unwrap_or(MAX_TEXT_CHARS)
    }
}

/// The authorizer of a caller's query runs: its policy (read-only: without writes), else the
/// playground's authorizer of the mode.
pub(crate) fn authorizer(read_only: bool, policy: Option<&AccessPolicy>) -> Arc<dyn Authorizer> {
    match policy {
        None if read_only => read_only_authorizer(),
        None => playground_authorizer(),
        Some(policy) if read_only => {
            let mut policy = policy.clone();
            for resource in [
                ResourceKind::Data,
                ResourceKind::Schema,
                ResourceKind::Definition,
            ] {
                for action in [ActionKind::Create, ActionKind::Update, ActionKind::Delete] {
                    policy = policy.deny(action, resource);
                }
            }
            Arc::new(policy)
        }
        Some(policy) => Arc::new(policy.clone()),
    }
}

/// Whether `policy` allows what the built-in tool `name` writes through the session (not
/// through a query, which the engine authorizes itself). Every other tool: allowed.
pub(crate) fn policy_allows_tool(policy: Option<&AccessPolicy>, name: &str) -> bool {
    let Some(policy) = policy else {
        return true;
    };
    session_writes(name)
        .iter()
        .all(|(action, resource)| policy.allows(*action, *resource))
}

/// What a built-in tool writes through a session method directly.
fn session_writes(name: &str) -> &'static [(ActionKind, ResourceKind)] {
    use ActionKind::{Create, Delete, Update};
    use ResourceKind::{Definition, Schema};
    match name {
        "set_schema" | "patch_schema" => &[(Update, Schema)],
        "define_compression" | "create_index" => &[(Create, Definition)],
        "drop_compression" | "drop_index" => &[(Delete, Definition)],
        "recompress" => &[(Update, Definition)],
        "apply_recommendations" => &[(Create, Definition), (Delete, Definition)],
        _ => &[],
    }
}
