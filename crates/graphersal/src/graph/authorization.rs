//! Authorization: the [`Authorizer`] hook that decides which [`AccessRequest`]s (an [`Action`]
//! on a [`Resource`]) an execution may perform.
//!
//! **Everything is allowed by default**, in the Rust API and in the Rhai script entry alike
//! ([`AllowAll`]). A host that runs input it does not trust plugs in its own rules: an
//! implementation of [`Authorizer`] (for example built per connection from the roles in a JWT;
//! the subject lives inside the authorizer instance, never in the request), or the ready-made,
//! configurable [`AccessPolicy`] (allow/deny by action and resource kind, plus a file root).
//!
//! The check runs once per execution, before anything runs, on the plan AS WRITTEN (before the
//! optimizer, so no rule can hide a step by fusing it), recursing into every child traversal;
//! the source methods that are not steps (file I/O, schema loading, graph sources) ask the same
//! authorizer. There are no per-element checks. A denial is
//! [`TraverserError::PermissionDenied`](crate::TraverserError::PermissionDenied) naming the
//! first step whose request was denied, the request and the reason. Functions are never hidden:
//! a denied step is still registered, documented and listed by `help()`.
//!
//! Which requests a step makes is decided in one place (the step -> request mapping of the
//! engine): reads are `Read` on [`Resource::Data`] (`v()` names the vertex kind, `e()` the edge
//! kind), `add_v("person")` is `Create` on `Data { Vertex, Some("person") }`, `property(..)` and
//! `remove_property(..)` are `Update`, `add_label`/`drop_label` are `Update` on
//! `Data { Vertex, None }` and `set_label` on `Data { Edge, None }`, `drop()` is `Delete`, `merge_v`/`merge_e` are `Create` and
//! `Update`, `schema()` is `Read` on [`Resource::Schema`]; an administrative `g.with()` option
//! is `Update` on [`Resource::Option`]. See `book/src/users_guide/permissions.md` for the table.
//!
//! [`ExecutionPolicy`](crate::graph::ExecutionPolicy) is a different, complementary host
//! control: it fixes the VALUE of individual `g.with()` options (locks and defaults) for every
//! entry, Rust included, while the authorizer decides whether a query may set an
//! administrative option at all.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::graph::ElementKind;
use crate::graph::definition::DefinitionKind;

/// Decides whether an execution may perform an [`AccessRequest`].
///
/// The subject (who asks) is not part of the request: it lives inside the authorizer instance,
/// so a server builds one authorizer per connection or session (from a JWT, an API key, ...)
/// and maps its roles to actions and resources. Roles are not a library concept.
///
/// The library calls [`Self::authorize`] once per request of a plan, before the plan runs, and
/// for every source method that is not a step. It never produces [`Resource::Custom`]: an
/// embedding server calls `authorize` with its own resources (a database, a tenant, ...) at its
/// own boundary, so one policy covers library and server resources.
///
/// ```rust
/// use graphersal::auth::{AccessRequest, Action, Authorizer, Denied, Resource};
///
/// /// A role from a token: analysts read, editors also write, nobody touches files.
/// struct Role { editor: bool }
///
/// impl Authorizer for Role {
///     fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
///         match (request.action, request.resource) {
///             (_, Resource::File { .. }) => Err(Denied::new("no file access")),
///             (Action::Read, _) => Ok(()),
///             (Action::Create | Action::Update | Action::Delete, Resource::Data { .. })
///                 if self.editor => Ok(()),
///             _ => Err(Denied::new("analysts only read")),
///         }
///     }
/// }
///
/// let analyst = Role { editor: false };
/// assert!(analyst.authorize(&AccessRequest::new(Action::Read, Resource::Schema)).is_ok());
/// assert!(analyst
///     .authorize(&AccessRequest::new(Action::Delete, Resource::data(None, None)))
///     .is_err());
/// ```
pub trait Authorizer: Send + Sync {
    /// Allows `request` (`Ok`) or denies it with a reason.
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied>;

    /// The file a request for `path` actually accesses, called after [`Self::authorize`]
    /// allowed a [`Resource::File`] request. The default returns `path` unchanged;
    /// [`AccessPolicy`] resolves it under its [file root](AccessPolicy::with_file_root) and
    /// denies a path outside it.
    fn resolve_file(&self, path: &Path) -> Result<PathBuf, Denied> {
        Ok(path.to_path_buf())
    }
}

/// A shared authorizer authorizes like the one it points to (one instance can serve several
/// sessions, or be kept by the host to inspect later).
impl<T: Authorizer + ?Sized> Authorizer for Arc<T> {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        (**self).authorize(request)
    }

    fn resolve_file(&self, path: &Path) -> Result<PathBuf, Denied> {
        (**self).resolve_file(path)
    }
}

/// The default [`Authorizer`]: allows every request. What the Rust API and the script entry
/// use unless the host passes another one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllowAll;

impl Authorizer for AllowAll {
    fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Denied> {
        Ok(())
    }
}

/// Why an [`Authorizer`] denied a request; it becomes the `reason` of
/// [`TraverserError::PermissionDenied`](crate::TraverserError::PermissionDenied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    reason: String,
}

impl Denied {
    /// A denial with `reason`, shown to the user as is.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The reason given to [`Self::new`].
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Denied {}

/// What an execution wants to do: an [`Action`] on a [`Resource`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccessRequest<'a> {
    /// What is done.
    pub action: Action<'a>,
    /// What it is done to.
    pub resource: Resource<'a>,
}

impl<'a> AccessRequest<'a> {
    /// The request to perform `action` on `resource` (an embedding server builds its own
    /// requests, typically on a [`Resource::Custom`]).
    pub fn new(action: Action<'a>, resource: Resource<'a>) -> Self {
        Self { action, resource }
    }

    /// Whether the request changes data, the schema or the catalog (`Create`, `Update`,
    /// `Delete` on [`Resource::Data`], [`Resource::Schema`] or [`Resource::Definition`]).
    pub fn writes_graph(&self) -> bool {
        matches!(
            self.action,
            Action::Create | Action::Update | Action::Delete
        ) && matches!(
            self.resource,
            Resource::Data { .. } | Resource::Schema | Resource::Definition { .. }
        )
    }

    /// An owned copy, e.g. to keep it in an error.
    pub fn to_owned_request(&self) -> AccessRequestBuf {
        AccessRequestBuf::from(self)
    }
}

impl fmt::Display for AccessRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.action, self.resource)
    }
}

/// What a request does. `#[non_exhaustive]`: a later action is an additive change.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action<'a> {
    /// Read data, the schema or a file.
    Read,
    /// Create elements or write a file.
    Create,
    /// Change existing elements (properties, labels), the schema or an option.
    Update,
    /// Drop elements or properties.
    Delete,
    /// Run code: Rhai's `import "module"` (on a [`Resource::File`]).
    Execute,
    /// An action of an embedding server; never produced by the library.
    Custom(&'a str),
}

impl Action<'_> {
    /// The kind of this action (the custom name dropped).
    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Read => ActionKind::Read,
            Action::Create => ActionKind::Create,
            Action::Update => ActionKind::Update,
            Action::Delete => ActionKind::Delete,
            Action::Execute => ActionKind::Execute,
            Action::Custom(_) => ActionKind::Custom,
        }
    }
}

impl fmt::Display for Action<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Custom(name) => f.write_str(name),
            other => f.write_str(other.kind().name()),
        }
    }
}

/// What a request is about. `#[non_exhaustive]`: a later resource is an additive change.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource<'a> {
    /// Graph data. `element` and `label` are set when the step knows them statically from the
    /// plan (`v()` reads vertices, `add_v("person")` creates a `person` vertex) and `None`
    /// otherwise: a policy that restricts one kind or label must treat `None` as "possibly
    /// that one".
    Data {
        /// The element kind, when statically known.
        element: Option<ElementKind>,
        /// The label, when statically known.
        label: Option<&'a str>,
    },
    /// The graph's schema.
    Schema,
    /// A definition of the graph's catalog (a saved query, ...; see
    /// [`crate::catalog`]): `Read` to list or read, `Create` / `Update` / `Delete`
    /// to manage, `Execute` to call a saved query.
    Definition {
        /// The kind; `None` for a request about every kind (listing them all).
        kind: Option<DefinitionKind>,
        /// The name; `None` for a request about every definition of the kind (listing).
        name: Option<&'a str>,
    },
    /// A file, with the path as the script wrote it.
    File {
        /// The path as given (relative paths are resolved by [`Authorizer::resolve_file`]).
        path: &'a Path,
    },
    /// An administrative `g.with()` option (the resource ceilings and the engine switches).
    Option {
        /// The option key, such as `evaluationTimeout`.
        key: &'a str,
    },
    /// An embedder of the registry the host gave its traversals (book page "Vectors"): the
    /// `embed` step and the DSL function `embed(text)` ask `Execute` on it, for every embedder
    /// including the built-in `hash-ngram/1`.
    Embedder {
        /// The registry name (`"hash-ngram/1"`).
        name: &'a str,
    },
    /// The system journal (book page "System Journal"): `Read` to query or follow it. Asked by
    /// the front ends (session, dev server, MCP) before they answer; the Rust API itself is
    /// host-level and asks nothing.
    Journal,
    /// A resource of an embedding server (a database, a tenant, ...); never produced by the
    /// library.
    Custom {
        /// The kind of resource, such as `"database"`.
        kind: &'a str,
        /// Its name, when the request is about one instance.
        name: Option<&'a str>,
    },
}

impl<'a> Resource<'a> {
    /// [`Resource::Data`] with `element` and `label`.
    pub fn data(element: Option<ElementKind>, label: Option<&'a str>) -> Self {
        Resource::Data { element, label }
    }

    /// [`Resource::Definition`] with `kind` and `name`.
    pub fn definition(kind: Option<DefinitionKind>, name: Option<&'a str>) -> Self {
        Resource::Definition { kind, name }
    }

    /// The kind of this resource (its details dropped).
    pub fn kind(&self) -> ResourceKind {
        match self {
            Resource::Data { .. } => ResourceKind::Data,
            Resource::Schema => ResourceKind::Schema,
            Resource::Definition { .. } => ResourceKind::Definition,
            Resource::File { .. } => ResourceKind::File,
            Resource::Option { .. } => ResourceKind::Option,
            Resource::Embedder { .. } => ResourceKind::Embedder,
            Resource::Journal => ResourceKind::Journal,
            Resource::Custom { .. } => ResourceKind::Custom,
        }
    }
}

impl fmt::Display for Resource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::value::literal::rhai_string;
        match self {
            Resource::Data { element, label } => {
                f.write_str("Data")?;
                match (element, label) {
                    (None, None) => Ok(()),
                    (Some(element), None) => write!(f, "({element})"),
                    (None, Some(label)) => write!(f, "({})", rhai_string(label)),
                    (Some(element), Some(label)) => {
                        write!(f, "({element} {})", rhai_string(label))
                    }
                }
            }
            Resource::Schema => f.write_str("Schema"),
            Resource::Definition { kind, name } => {
                f.write_str("Definition")?;
                match (kind, name) {
                    (None, None) => Ok(()),
                    (Some(kind), None) => write!(f, "({kind})"),
                    (None, Some(name)) => write!(f, "({})", rhai_string(name)),
                    (Some(kind), Some(name)) => write!(f, "({kind} {})", rhai_string(name)),
                }
            }
            Resource::File { path } => {
                write!(f, "File({})", rhai_string(&path.to_string_lossy()))
            }
            Resource::Option { key } => write!(f, "Option({})", rhai_string(key)),
            Resource::Embedder { name } => write!(f, "Embedder({})", rhai_string(name)),
            Resource::Journal => f.write_str("Journal"),
            Resource::Custom { kind, name } => match name {
                Some(name) => write!(f, "{kind}({})", rhai_string(name)),
                None => f.write_str(kind),
            },
        }
    }
}

/// The kind of an [`Action`], what [`AccessPolicy`] rules match on.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// [`Action::Read`].
    Read,
    /// [`Action::Create`].
    Create,
    /// [`Action::Update`].
    Update,
    /// [`Action::Delete`].
    Delete,
    /// [`Action::Execute`].
    Execute,
    /// Every [`Action::Custom`].
    Custom,
}

impl ActionKind {
    /// Every action kind, in declaration order.
    pub const ALL: [ActionKind; 6] = [
        ActionKind::Read,
        ActionKind::Create,
        ActionKind::Update,
        ActionKind::Delete,
        ActionKind::Execute,
        ActionKind::Custom,
    ];

    /// The name as the Rust API spells it (`"Read"`, ..., `"Custom"`).
    pub fn name(self) -> &'static str {
        match self {
            ActionKind::Read => "Read",
            ActionKind::Create => "Create",
            ActionKind::Update => "Update",
            ActionKind::Delete => "Delete",
            ActionKind::Execute => "Execute",
            ActionKind::Custom => "Custom",
        }
    }

    fn index(self) -> u32 {
        match self {
            ActionKind::Read => 0,
            ActionKind::Create => 1,
            ActionKind::Update => 2,
            ActionKind::Delete => 3,
            ActionKind::Execute => 4,
            ActionKind::Custom => 5,
        }
    }
}

impl fmt::Display for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The kind of a [`Resource`], what [`AccessPolicy`] rules match on.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// [`Resource::Data`].
    Data,
    /// [`Resource::Schema`].
    Schema,
    /// [`Resource::File`].
    File,
    /// [`Resource::Option`].
    Option,
    /// [`Resource::Definition`].
    Definition,
    /// [`Resource::Custom`].
    Custom,
    /// [`Resource::Embedder`].
    Embedder,
    /// [`Resource::Journal`].
    Journal,
}

impl ResourceKind {
    /// Every resource kind, in declaration order.
    pub const ALL: [ResourceKind; 8] = [
        ResourceKind::Data,
        ResourceKind::Schema,
        ResourceKind::File,
        ResourceKind::Option,
        ResourceKind::Definition,
        ResourceKind::Custom,
        ResourceKind::Embedder,
        ResourceKind::Journal,
    ];

    /// The name as the Rust API spells it (`"Data"`, ..., `"Custom"`).
    pub fn name(self) -> &'static str {
        match self {
            ResourceKind::Data => "Data",
            ResourceKind::Schema => "Schema",
            ResourceKind::File => "File",
            ResourceKind::Option => "Option",
            ResourceKind::Definition => "Definition",
            ResourceKind::Custom => "Custom",
            ResourceKind::Embedder => "Embedder",
            ResourceKind::Journal => "Journal",
        }
    }

    fn index(self) -> u32 {
        match self {
            ResourceKind::Data => 0,
            ResourceKind::Schema => 1,
            ResourceKind::File => 2,
            ResourceKind::Option => 3,
            ResourceKind::Definition => 5,
            ResourceKind::Custom => 4,
            ResourceKind::Embedder => 6,
            ResourceKind::Journal => 7,
        }
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An owned [`AccessRequest`], kept by
/// [`TraverserError::PermissionDenied`](crate::TraverserError::PermissionDenied).
/// [`Self::as_request`] gives the borrowed form back.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccessRequestBuf {
    action: ActionKind,
    custom_action: String,
    resource: ResourceBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ResourceBuf {
    Data {
        element: Option<ElementKind>,
        label: Option<String>,
    },
    Schema,
    Definition {
        kind: Option<DefinitionKind>,
        name: Option<String>,
    },
    File(PathBuf),
    Option(String),
    Embedder(String),
    Journal,
    Custom {
        kind: String,
        name: Option<String>,
    },
}

impl AccessRequestBuf {
    /// The borrowed request.
    pub fn as_request(&self) -> AccessRequest<'_> {
        let action = match self.action {
            ActionKind::Read => Action::Read,
            ActionKind::Create => Action::Create,
            ActionKind::Update => Action::Update,
            ActionKind::Delete => Action::Delete,
            ActionKind::Execute => Action::Execute,
            ActionKind::Custom => Action::Custom(&self.custom_action),
        };
        let resource = match &self.resource {
            ResourceBuf::Data { element, label } => Resource::Data {
                element: *element,
                label: label.as_deref(),
            },
            ResourceBuf::Schema => Resource::Schema,
            ResourceBuf::Definition { kind, name } => Resource::Definition {
                kind: *kind,
                name: name.as_deref(),
            },
            ResourceBuf::File(path) => Resource::File { path },
            ResourceBuf::Option(key) => Resource::Option { key },
            ResourceBuf::Embedder(name) => Resource::Embedder { name },
            ResourceBuf::Journal => Resource::Journal,
            ResourceBuf::Custom { kind, name } => Resource::Custom {
                kind,
                name: name.as_deref(),
            },
        };
        AccessRequest { action, resource }
    }
}

impl From<&AccessRequest<'_>> for AccessRequestBuf {
    fn from(request: &AccessRequest<'_>) -> Self {
        let custom_action = match request.action {
            Action::Custom(name) => name.to_string(),
            _ => String::new(),
        };
        let resource = match request.resource {
            Resource::Data { element, label } => ResourceBuf::Data {
                element,
                label: label.map(str::to_string),
            },
            Resource::Schema => ResourceBuf::Schema,
            Resource::Definition { kind, name } => ResourceBuf::Definition {
                kind,
                name: name.map(str::to_string),
            },
            Resource::File { path } => ResourceBuf::File(path.to_path_buf()),
            Resource::Option { key } => ResourceBuf::Option(key.to_string()),
            Resource::Embedder { name } => ResourceBuf::Embedder(name.to_string()),
            Resource::Journal => ResourceBuf::Journal,
            Resource::Custom { kind, name } => ResourceBuf::Custom {
                kind: kind.to_string(),
                name: name.map(str::to_string),
            },
        };
        Self {
            action: request.action.kind(),
            custom_action,
            resource,
        }
    }
}

impl fmt::Display for AccessRequestBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_request().fmt(f)
    }
}

/// The ready-made, configurable [`Authorizer`]: a table of allowed (action kind, resource
/// kind) pairs plus an optional file root, enough for a front end that needs no trait impl of
/// its own. [`Default`] is [`Self::allow_all`]. Rules apply in call order; a later
/// [`Self::allow`]/[`Self::deny`] overrides an earlier one for the same pair.
///
/// ```rust
/// use graphersal::auth::{AccessPolicy, ActionKind, ResourceKind};
///
/// // A query service: reads data and the schema, nothing else.
/// let query_service = AccessPolicy::read_only();
/// assert!(query_service.allows(ActionKind::Read, ResourceKind::Data));
/// assert!(!query_service.allows(ActionKind::Create, ResourceKind::Data));
///
/// // An editor: reads and writes data, no files, no administrative options.
/// let editor = AccessPolicy::read_only()
///     .allow(ActionKind::Create, ResourceKind::Data)
///     .allow(ActionKind::Update, ResourceKind::Data)
///     .allow(ActionKind::Delete, ResourceKind::Data);
/// assert!(editor.allows(ActionKind::Delete, ResourceKind::Data));
///
/// // Everything but the file system (the web playground).
/// let server = AccessPolicy::allow_all().deny_resource(ResourceKind::File);
/// assert!(!server.allows(ActionKind::Read, ResourceKind::File));
///
/// // File access limited to one directory.
/// let rooted = AccessPolicy::allow_all().with_file_root("/srv/graphs");
/// assert_eq!(rooted.file_root(), Some(std::path::Path::new("/srv/graphs")));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPolicy {
    /// Bit `action * RESOURCES + resource` set = allowed.
    allowed: u64,
    file_root: Option<Arc<Path>>,
}

impl Default for AccessPolicy {
    fn default() -> Self {
        Self::allow_all()
    }
}

const RESOURCES: u32 = ResourceKind::ALL.len() as u32;

fn bit(action: ActionKind, resource: ResourceKind) -> u64 {
    1u64 << (action.index() * RESOURCES + resource.index())
}

// The bit table must fit its word.
const _: () = assert!(ActionKind::ALL.len() * ResourceKind::ALL.len() <= u64::BITS as usize);

impl AccessPolicy {
    /// Every request allowed, no file root (the default).
    pub fn allow_all() -> Self {
        let allowed = ActionKind::ALL.iter().fold(0, |bits, action| {
            ResourceKind::ALL
                .iter()
                .fold(bits, |bits, resource| bits | bit(*action, *resource))
        });
        Self {
            allowed,
            file_root: None,
        }
    }

    /// Nothing allowed: a script can only compute with its own values.
    pub fn deny_all() -> Self {
        Self {
            allowed: 0,
            file_root: None,
        }
    }

    /// A query service: `Read` on [`ResourceKind::Data`], [`ResourceKind::Schema`] and
    /// [`ResourceKind::Definition`], plus `Execute` on [`ResourceKind::Definition`] (calling a
    /// saved query, which is read-only itself) and on [`ResourceKind::Embedder`] (an embedder
    /// computes, it writes nothing), and `Read` on [`ResourceKind::Journal`].
    pub fn read_only() -> Self {
        Self::deny_all()
            .allow(ActionKind::Read, ResourceKind::Data)
            .allow(ActionKind::Read, ResourceKind::Schema)
            .allow(ActionKind::Read, ResourceKind::Definition)
            .allow(ActionKind::Execute, ResourceKind::Definition)
            .allow(ActionKind::Execute, ResourceKind::Embedder)
            .allow(ActionKind::Read, ResourceKind::Journal)
    }

    /// Builder: also allows `action` on `resource`.
    pub fn allow(mut self, action: ActionKind, resource: ResourceKind) -> Self {
        self.allowed |= bit(action, resource);
        self
    }

    /// Builder: no longer allows `action` on `resource`.
    pub fn deny(mut self, action: ActionKind, resource: ResourceKind) -> Self {
        self.allowed &= !bit(action, resource);
        self
    }

    /// Builder: allows every action on `resource`.
    pub fn allow_resource(self, resource: ResourceKind) -> Self {
        ActionKind::ALL
            .iter()
            .fold(self, |policy, action| policy.allow(*action, resource))
    }

    /// Builder: denies every action on `resource`.
    pub fn deny_resource(self, resource: ResourceKind) -> Self {
        ActionKind::ALL
            .iter()
            .fold(self, |policy, action| policy.deny(*action, resource))
    }

    /// Whether `action` on `resource` is allowed.
    pub fn allows(&self, action: ActionKind, resource: ResourceKind) -> bool {
        self.allowed & bit(action, resource) != 0
    }

    /// Builder: limits file access to the directory `root`. A relative path a script passes is
    /// resolved under it; a path that leaves it (`..`, an absolute path elsewhere, a symbolic
    /// link pointing out) is denied. The root must exist when a file is accessed. It does not
    /// allow file access by itself.
    pub fn with_file_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.file_root = Some(Arc::from(root.into().into_boxed_path()));
        self
    }

    /// The file root set with [`Self::with_file_root`], if any.
    pub fn file_root(&self) -> Option<&Path> {
        self.file_root.as_deref()
    }
}

impl Authorizer for AccessPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        let (action, resource) = (request.action.kind(), request.resource.kind());
        if self.allows(action, resource) {
            Ok(())
        } else {
            Err(Denied::new(format!(
                "the host's access policy does not allow {action} on {resource}"
            )))
        }
    }

    /// With a [file root](Self::with_file_root), resolves `path` under it (a relative path is
    /// relative to the root) and denies a path outside it, symbolic links resolved. Without a
    /// root the path is returned unchanged. Never panics; on a target without a file system
    /// (wasm32) a rooted check is denied because the root cannot be resolved.
    fn resolve_file(&self, path: &Path) -> Result<PathBuf, Denied> {
        let Some(root) = self.file_root.as_deref() else {
            return Ok(path.to_path_buf());
        };
        let root = canonical(root).map_err(|err| {
            Denied::new(format!(
                "the file root '{}' cannot be resolved: {err}",
                root.display()
            ))
        })?;
        let candidate = normalize(&root.join(path));
        let resolved = canonicalize_existing_prefix(&candidate);
        if resolved.starts_with(&root) {
            Ok(resolved)
        } else {
            Err(Denied::new(format!(
                "'{}' is outside the file root '{}'",
                path.display(),
                root.display()
            )))
        }
    }
}

/// The canonical form of `path`. On wasm32-unknown-unknown there is no file system: always an
/// error there (the file root check then denies), never a panic.
fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no file system on this target",
        ))
    }
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    {
        std::fs::canonicalize(path)
    }
}

/// `path` with `.` dropped and `..` applied lexically (no file system access).
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// `path` (already normalized) with its longest existing ancestor replaced by its canonical form,
/// so a symbolic link inside the root that points out of it is seen for what it is. The missing
/// rest (a file to be created) is appended as written; it holds no `..` after [`normalize`].
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = canonical(&existing) {
            let mut resolved = canonical;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        match (
            existing.file_name().map(|n| n.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            // Nothing of it exists (or no file system): keep the lexical form, which the root
            // check then compares as written.
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_all_allows_nothing_and_allow_all_everything() {
        for action in ActionKind::ALL {
            for resource in ResourceKind::ALL {
                assert!(!AccessPolicy::deny_all().allows(action, resource));
                assert!(AccessPolicy::allow_all().allows(action, resource));
            }
        }
        assert_eq!(AccessPolicy::default(), AccessPolicy::allow_all());
    }

    #[test]
    fn allow_and_deny_toggle_one_pair() {
        let policy = AccessPolicy::read_only().allow(ActionKind::Create, ResourceKind::Data);
        assert!(policy.allows(ActionKind::Create, ResourceKind::Data));
        assert!(!policy.allows(ActionKind::Update, ResourceKind::Option));
        let policy = policy.deny(ActionKind::Read, ResourceKind::Data);
        assert!(!policy.allows(ActionKind::Read, ResourceKind::Data));
        assert!(policy.allows(ActionKind::Read, ResourceKind::Schema));
        let policy = AccessPolicy::allow_all().deny_resource(ResourceKind::File);
        assert!(!policy.allows(ActionKind::Execute, ResourceKind::File));
        assert!(policy.allows(ActionKind::Execute, ResourceKind::Custom));
    }

    #[test]
    fn normalize_applies_parent_components_lexically() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize(Path::new("/a/../../b")), PathBuf::from("/b"));
    }

    #[test]
    fn without_root_a_path_passes_unchanged() {
        let path = AccessPolicy::allow_all()
            .resolve_file(Path::new("../x.xml"))
            .unwrap();
        assert_eq!(path, PathBuf::from("../x.xml"));
    }

    #[test]
    fn an_owned_request_round_trips_and_renders() {
        let path = Path::new("a.xml");
        for request in [
            AccessRequest::new(
                Action::Create,
                Resource::data(Some(ElementKind::Vertex), Some("p")),
            ),
            AccessRequest::new(Action::Read, Resource::data(None, None)),
            AccessRequest::new(Action::Update, Resource::Schema),
            AccessRequest::new(Action::Execute, Resource::File { path }),
            AccessRequest::new(
                Action::Update,
                Resource::Option {
                    key: "evaluationTimeout",
                },
            ),
            AccessRequest::new(
                Action::Custom("open"),
                Resource::Custom {
                    kind: "database",
                    name: Some("sales"),
                },
            ),
        ] {
            assert_eq!(request.to_owned_request().as_request(), request);
        }
        assert_eq!(
            AccessRequest::new(
                Action::Create,
                Resource::data(Some(ElementKind::Vertex), Some("p"))
            )
            .to_string(),
            "Create Data(vertex \"p\")"
        );
        assert_eq!(
            AccessRequest::new(
                Action::Custom("open"),
                Resource::Custom {
                    kind: "database",
                    name: Some("sales")
                }
            )
            .to_string(),
            "open database(\"sales\")"
        );
    }
}
