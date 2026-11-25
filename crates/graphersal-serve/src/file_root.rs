//! File confinement ([`crate::ContextBuilder::file_root`]): the directory every file path a
//! client names must lie in. Without a root (the default, the CLI dev server) a client may name
//! any path the process can write; with one:
//!
//! - `POST /api/jobs/define` runs under `AccessPolicy::allow_all().with_file_root(root)`, so the
//!   engine resolves a backup job's path under the root (`..` and symbolic links resolved) and
//!   refuses one outside it;
//! - `POST /api/catalog/load` refuses a catalog whose jobs write outside the root
//!   ([`jobs_outside_file_root`]), nothing loaded;
//! - the `dir` of `POST /api/store/fork`, `store/attic/fork` and `store/backup` must resolve
//!   inside the root (a relative `dir` is resolved under the root, as a job's path is), else
//!   `403`; the default directory next to the store is unchanged.
//!
//! The check is made when the request arrives: a symbolic link created inside the root between
//! the check and the write is not seen (the root is the host's directory; its other writers are
//! the host's to trust).

use std::path::{Component, Path, PathBuf};

use graphersal::auth::{AccessPolicy, Authorizer, Resource};
use graphersal::catalog::{Definition, JobTask};

use crate::http::HttpError;

/// The help of every refusal of a path outside the file root.
pub(crate) const FILE_ROOT_HELP: &str =
    "paths must lie inside the server's file root (a relative path is resolved under it)";

/// A stored job that writes outside a file root ([`jobs_outside_file_root`]).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutsideFileRoot {
    /// The job's name.
    pub job: String,
    /// The path it writes, as stored (a file effect, or a backup job's path without one).
    pub path: PathBuf,
    /// Why the path is refused.
    pub reason: String,
}

/// The jobs among `definitions` that write outside `root`: every file effect of a job (the
/// directory a backup job writes, resolved when it was defined), and a backup job's path when
/// it has no file effect (the Store's runner then uses the path as written). A path is accepted
/// when it is absolute, holds no `..` and resolves inside `root` with symbolic links followed
/// (the check of [`AccessPolicy::with_file_root`]); a relative stored path is refused, since
/// the runner resolves it against the process's working directory, not the root. Definitions
/// of other kinds are skipped.
///
/// A host calls it on the catalog of a store it loads (an imported, restored or cloned store
/// may carry backup jobs pointing anywhere) and disables or refuses what it returns;
/// `POST /api/catalog/load` calls it on the uploaded catalog. A root that cannot be resolved
/// (missing) makes every such path a violation. Never panics.
pub fn jobs_outside_file_root<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
    root: &Path,
) -> Vec<JobOutsideFileRoot> {
    let policy = AccessPolicy::allow_all().with_file_root(root);
    let mut outside = Vec::new();
    for definition in definitions {
        let Some(job) = definition.as_job() else {
            continue;
        };
        let mut paths: Vec<PathBuf> = job
            .effects()
            .iter()
            .filter_map(|effect| match effect.as_request().resource {
                Resource::File { path } => Some(path.to_path_buf()),
                _ => None,
            })
            .collect();
        if paths.is_empty()
            && let JobTask::Backup { path } = job.action()
        {
            paths.push(PathBuf::from(path));
        }
        for path in paths {
            let refused = if !path.is_absolute() {
                Some(
                    "a relative path in a stored job is resolved against the working directory"
                        .to_string(),
                )
            } else if path.components().any(|c| c == Component::ParentDir) {
                Some("the path holds '..'".to_string())
            } else {
                policy
                    .resolve_file(&path)
                    .err()
                    .map(|_| "it lies outside the server's file root".to_string())
            };
            if let Some(reason) = refused {
                outside.push(JobOutsideFileRoot {
                    job: definition.name().to_string(),
                    path,
                    reason,
                });
            }
        }
    }
    outside
}

/// `requested` (a client's `dir`) resolved under `root`: a relative path is relative to the
/// root, `..` and symbolic links are resolved; `403` when the result leaves the root, `500`
/// when the root itself cannot be resolved (the host's configuration).
pub(crate) fn resolve_in_root(root: &Path, requested: &str) -> Result<PathBuf, HttpError> {
    if std::fs::canonicalize(root).is_err() {
        return Err(HttpError(
            500,
            "the server's file root is not an existing directory: ask the host's operator"
                .to_string(),
        ));
    }
    AccessPolicy::allow_all()
        .with_file_root(root)
        .resolve_file(Path::new(requested))
        .map_err(|_| {
            HttpError(
                403,
                format!("'{requested}' is outside the server's file root: {FILE_ROOT_HELP}"),
            )
        })
}

/// `403` when the catalog `text` holds jobs that write outside `root`; `Ok` when it holds none
/// or does not parse (the load then reports the parse error itself).
pub(crate) fn check_catalog(root: &Path, text: &str) -> Result<(), HttpError> {
    let Ok(definitions) = graphersal::catalog::catalog_from_json(text) else {
        return Ok(());
    };
    let outside = jobs_outside_file_root(&definitions, root);
    if outside.is_empty() {
        return Ok(());
    }
    let list = outside
        .iter()
        .map(|job| {
            format!(
                "job '{}' writes '{}' ({})",
                job.job,
                job.path.display(),
                job.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(HttpError(
        403,
        format!(
            "nothing was loaded: {list}; stored job paths must be absolute, and paths must \
             lie inside the server's file root"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphersal::auth::{AccessRequest, Action};
    use graphersal::catalog::JobDefinition;

    fn backup_job(name: &str, path: &str, effect: Option<&Path>) -> Definition {
        let mut job = JobDefinition::new(
            "0 3 * * *",
            JobTask::Backup {
                path: path.to_string(),
            },
        );
        if let Some(effect) = effect {
            job = job.with_effects(vec![
                AccessRequest::new(Action::Create, Resource::File { path: effect })
                    .to_owned_request(),
            ]);
        }
        Definition::job(name, job)
    }

    #[test]
    fn stored_jobs_are_checked_against_the_root() {
        let base = std::env::temp_dir().join(format!("graphersal_fileroot_{}", std::process::id()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let inside = root.join("b1");
        let escape = root.join("..").join("x");
        let definitions = vec![
            backup_job("in_effect", "ignored", Some(&inside)),
            backup_job("in_path", inside.to_str().unwrap(), None),
            backup_job("out_effect", "b", Some(&base.join("elsewhere"))),
            backup_job("relative", "b1", None),
            backup_job("dotdot", escape.to_str().unwrap(), None),
        ];
        let names: Vec<String> = jobs_outside_file_root(&definitions, &root)
            .into_iter()
            .map(|job| job.job)
            .collect();
        assert_eq!(names, ["out_effect", "relative", "dotdot"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
