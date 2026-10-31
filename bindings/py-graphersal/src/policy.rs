//! The `graphersal.AccessPolicy` class: the Python face of the engine's ready-made
//! `graphersal::auth::AccessPolicy` (which actions a script may perform on which kinds of
//! resource, and an optional file root), passed to `Graph.execute`/`Graph.query`/
//! `Graph.dry_run` as `policy=`.

use std::path::PathBuf;
use std::sync::Arc;

use graphersal::auth::{AccessPolicy as EnginePolicy, ActionKind, Authorizer, ResourceKind};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyFrozenSet, PyString, PyTuple};

/// The Python names of the action kinds, in the engine's declaration order.
const ACTIONS: [(&str, ActionKind); 6] = [
    ("read", ActionKind::Read),
    ("create", ActionKind::Create),
    ("update", ActionKind::Update),
    ("delete", ActionKind::Delete),
    ("execute", ActionKind::Execute),
    ("custom", ActionKind::Custom),
];

/// The Python names of the resource kinds, in the engine's declaration order.
const RESOURCES: [(&str, ResourceKind); 8] = [
    ("data", ResourceKind::Data),
    ("schema", ResourceKind::Schema),
    ("file", ResourceKind::File),
    ("option", ResourceKind::Option),
    ("definition", ResourceKind::Definition),
    ("custom", ResourceKind::Custom),
    ("embedder", ResourceKind::Embedder),
    ("journal", ResourceKind::Journal),
];

/// Which actions a script may perform on which kinds of resource, plus an optional file root
/// that confines file access to one directory.
///
/// Actions: ``"read"``, ``"create"``, ``"update"``, ``"delete"``, ``"execute"``, ``"custom"``.
/// Resources: ``"data"`` (the graph's elements), ``"schema"``, ``"file"``, ``"option"`` (the
/// administrative ``g.with()`` options such as ``evaluationTimeout``), ``"definition"`` (the
/// catalog of definitions: saved queries), ``"custom"``, ``"embedder"`` (``execute`` on it runs
/// an embedder: the ``embed`` step, ``embed(text)``, a text query),
/// ``"journal"`` (``read`` on it reads the system journal). ``"*"`` stands for every action or every resource.
///
/// An ``AccessPolicy`` is immutable: ``allow``, ``deny`` and ``with_file_root`` return a new
/// value. ``AccessPolicy(("read", "data"), ("read", "schema"))`` allows exactly the given pairs;
/// ``AccessPolicy.allow_all()``, ``AccessPolicy.read_only()`` and ``AccessPolicy.deny_all()``
/// are the common presets. A script whose request is not allowed raises
/// ``PermissionDeniedError`` before the denied traversal runs.
#[pyclass(module = "graphersal", frozen, eq, hash)]
#[derive(PartialEq, Eq, Hash)]
pub struct AccessPolicy {
    /// Bit `action * RESOURCES.len() + resource` set = allowed.
    allowed: u64,
    /// The file root, if any.
    file_root: Option<PathBuf>,
}

fn bit(action: usize, resource: usize) -> u64 {
    1 << (action * RESOURCES.len() + resource)
}

impl AccessPolicy {
    /// The engine authorizer for this policy.
    pub(crate) fn to_engine(&self) -> EnginePolicy {
        let mut policy = EnginePolicy::deny_all();
        for (a, (_, action)) in ACTIONS.iter().enumerate() {
            for (r, (_, resource)) in RESOURCES.iter().enumerate() {
                if self.allowed & bit(a, r) != 0 {
                    policy = policy.allow(*action, *resource);
                }
            }
        }
        match &self.file_root {
            Some(root) => policy.with_file_root(root.clone()),
            None => policy,
        }
    }

    /// What a script run from Python may do without `policy=`: everything (see
    /// `Graph.execute`).
    pub(crate) fn default_authorizer() -> Arc<dyn Authorizer> {
        Arc::new(graphersal::auth::AllowAll)
    }

    fn all_bits() -> u64 {
        (1u64 << (ACTIONS.len() * RESOURCES.len())) - 1
    }

    /// The bits a `(action, resource)` rule covers (`"*"` = all).
    fn bits_of_rule(action: &Bound<'_, PyAny>, resource: &Bound<'_, PyAny>) -> PyResult<u64> {
        let actions = indices(action, &ACTIONS.map(|(name, _)| name), "action")?;
        let resources = indices(resource, &RESOURCES.map(|(name, _)| name), "resource")?;
        Ok(actions.iter().fold(0, |bits, a| {
            resources.iter().fold(bits, |bits, r| bits | bit(*a, *r))
        }))
    }

    fn bits_of_rules(rules: &Bound<'_, PyTuple>) -> PyResult<u64> {
        let mut bits = 0;
        for rule in rules.iter() {
            let (action, resource): (Bound<'_, PyAny>, Bound<'_, PyAny>) =
                rule.extract().map_err(|_| {
                    PyValueError::new_err(
                        "a rule is an (action, resource) tuple, for example (\"read\", \"data\")",
                    )
                })?;
            bits |= Self::bits_of_rule(&action, &resource)?;
        }
        Ok(bits)
    }

    fn pairs(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        ACTIONS
            .iter()
            .enumerate()
            .flat_map(move |(a, (action, _))| {
                RESOURCES
                    .iter()
                    .enumerate()
                    .filter(move |(r, _)| self.allowed & bit(a, *r) != 0)
                    .map(move |(_, (resource, _))| (*action, *resource))
            })
    }
}

/// The indices `name` (a `str`, `"*"` for all) selects in `names`, or `ValueError` naming the
/// valid names.
fn indices(name: &Bound<'_, PyAny>, names: &[&str], what: &str) -> PyResult<Vec<usize>> {
    let valid = || {
        names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let name: String = name.extract().map_err(|_| {
        PyValueError::new_err(format!("an {what} is a str, '*' or one of {}", valid()))
    })?;
    if name == "*" {
        return Ok((0..names.len()).collect());
    }
    names
        .iter()
        .position(|known| *known == name)
        .map(|index| vec![index])
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown {what} '{name}'; expected '*' or one of {}",
                valid()
            ))
        })
}

#[pymethods]
impl AccessPolicy {
    /// ``AccessPolicy(*rules, file_root=None)``: allows exactly ``rules``, each an
    /// ``(action, resource)`` tuple (``"*"`` for all); ``file_root`` confines file access to
    /// that directory (it does not allow file access by itself). An unknown name raises
    /// ``ValueError``.
    #[new]
    #[pyo3(signature = (*rules, file_root=None))]
    fn new(rules: &Bound<'_, PyTuple>, file_root: Option<PathBuf>) -> PyResult<Self> {
        Ok(AccessPolicy {
            allowed: AccessPolicy::bits_of_rules(rules)?,
            file_root,
        })
    }

    /// ``AccessPolicy.allow_all()``: every request, no file root; what ``Graph.execute`` uses
    /// when no ``policy`` is given.
    #[staticmethod]
    fn allow_all() -> Self {
        AccessPolicy {
            allowed: AccessPolicy::all_bits(),
            file_root: None,
        }
    }

    /// ``AccessPolicy.deny_all()``: nothing; a script can only compute with its own values.
    #[staticmethod]
    fn deny_all() -> Self {
        AccessPolicy {
            allowed: 0,
            file_root: None,
        }
    }

    /// ``AccessPolicy.read_only()``: ``read`` on ``data``, ``schema`` and ``definition``, and
    /// ``execute`` on ``definition`` (calling a saved query, which is read-only itself) and on
    /// ``embedder`` (embedding text changes nothing), and ``read`` on ``journal``, as the
    /// engine's ``read_only``.
    #[staticmethod]
    fn read_only() -> Self {
        AccessPolicy {
            allowed: bit(0, 0) | bit(0, 1) | bit(0, 4) | bit(4, 4) | bit(4, 6) | bit(0, 7),
            file_root: None,
        }
    }

    /// ``p.allow(action, resource)``: a copy that also allows ``action`` on ``resource``
    /// (``"*"`` for all).
    fn allow(&self, action: &Bound<'_, PyAny>, resource: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(AccessPolicy {
            allowed: self.allowed | AccessPolicy::bits_of_rule(action, resource)?,
            file_root: self.file_root.clone(),
        })
    }

    /// ``p.deny(action, resource)``: a copy that no longer allows ``action`` on ``resource``
    /// (``"*"`` for all).
    fn deny(&self, action: &Bound<'_, PyAny>, resource: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(AccessPolicy {
            allowed: self.allowed & !AccessPolicy::bits_of_rule(action, resource)?,
            file_root: self.file_root.clone(),
        })
    }

    /// ``p.with_file_root(path)``: a copy whose file access is confined to the directory
    /// ``path`` (``None`` removes the root). A relative path a script passes is resolved under
    /// the root; a path that leaves it (``..``, an absolute path elsewhere, a symbolic link
    /// pointing out) is denied. The root must exist when a file is accessed.
    fn with_file_root(&self, path: Option<PathBuf>) -> Self {
        AccessPolicy {
            allowed: self.allowed,
            file_root: path,
        }
    }

    /// ``p.allows(action, resource)``: whether ``action`` on ``resource`` is allowed (with
    /// ``"*"``: for every one it stands for).
    fn allows(&self, action: &Bound<'_, PyAny>, resource: &Bound<'_, PyAny>) -> PyResult<bool> {
        let bits = AccessPolicy::bits_of_rule(action, resource)?;
        Ok(self.allowed & bits == bits)
    }

    /// The allowed ``(action, resource)`` pairs, as a ``frozenset``.
    #[getter]
    fn rules<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyFrozenSet>> {
        PyFrozenSet::new(py, self.pairs())
    }

    /// The file root as a ``str``, or ``None``.
    #[getter]
    fn file_root(&self) -> Option<String> {
        self.file_root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned())
    }

    /// ``AccessPolicy(('read', 'data'), ('read', 'schema'), file_root='/srv/graphs')``:
    /// evaluates back to an equal value.
    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let mut parts: Vec<String> = self
            .pairs()
            .map(|(action, resource)| format!("('{action}', '{resource}')"))
            .collect();
        if let Some(root) = &self.file_root {
            let root = PyString::new(py, &root.to_string_lossy()).repr()?;
            parts.push(format!("file_root={root}"));
        }
        Ok(format!("AccessPolicy({})", parts.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cover_every_engine_kind_in_order() {
        let actions: Vec<ActionKind> = ACTIONS.iter().map(|(_, kind)| *kind).collect();
        assert_eq!(actions, ActionKind::ALL.to_vec());
        let resources: Vec<ResourceKind> = RESOURCES.iter().map(|(_, kind)| *kind).collect();
        assert_eq!(resources, ResourceKind::ALL.to_vec());
    }

    #[test]
    fn presets_match_the_engine_presets() {
        assert_eq!(
            AccessPolicy::allow_all().to_engine(),
            EnginePolicy::allow_all()
        );
        assert_eq!(
            AccessPolicy::read_only().to_engine(),
            EnginePolicy::read_only()
        );
        assert_eq!(
            AccessPolicy::deny_all().to_engine(),
            EnginePolicy::deny_all()
        );
    }

    #[test]
    fn file_root_is_carried_over() {
        let policy = AccessPolicy::read_only().with_file_root(Some(PathBuf::from("/srv")));
        assert_eq!(
            policy.to_engine(),
            EnginePolicy::read_only().with_file_root("/srv")
        );
    }
}
