//! The playground UI the dev server serves: a file of the UI directory (`--ui-dir`, or
//! `./playground` when it exists) wins, else the copy embedded at build time (`build.rs`), so the
//! binary serves the UI on its own.

use std::path::{Path, PathBuf};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/ui_files.rs"));
}

/// Where the UI comes from: an optional directory over the embedded copy.
pub(super) struct Ui {
    /// The canonical UI directory whose files win over the embedded ones; `None` = embedded only.
    dir: Option<PathBuf>,
}

/// The embedded copy of `relative` (a `/`-separated path below the UI root).
fn embedded(relative: &str) -> Option<&'static [u8]> {
    embedded::UI_FILES
        .binary_search_by(|(name, _)| (*name).cmp(relative))
        .ok()
        .map(|index| embedded::UI_FILES[index].1)
}

impl Ui {
    /// The UI of `--ui-dir`: an explicit directory must exist (a usage error otherwise); the
    /// default `./playground` is used when it exists, else the embedded copy alone.
    pub(super) fn resolve(given: Option<&str>, default: &str) -> Result<Self, String> {
        let Some(given) = given else {
            let dir = std::fs::canonicalize(default)
                .ok()
                .filter(|dir| dir.is_dir());
            return Ok(Self { dir });
        };
        let hint = "Help: --ui-dir names a directory whose files override the embedded \
                    playground UI (for working on the UI); leave it out to serve the embedded UI.";
        let path = std::fs::canonicalize(given).map_err(|err| {
            format!("Error: the UI directory '{given}' (--ui-dir) is not usable: {err}\n{hint}")
        })?;
        if !path.is_dir() {
            return Err(format!(
                "Error: the UI directory '{}' (--ui-dir) is not a directory\n{hint}",
                path.display()
            ));
        }
        Ok(Self { dir: Some(path) })
    }

    /// The start banner's `UI` row: where the UI comes from.
    pub(super) fn banner(&self) -> String {
        match &self.dir {
            None => "embedded".to_string(),
            Some(dir) => format!("{} (embedded fallback)", dir.display()),
        }
    }

    /// The bytes of `relative` (already decoded, without a leading `/`; `""` or a directory
    /// means its `index.html`) and its file name for the content type; `None` when the path is
    /// not a UI file or leaves the UI root.
    pub(super) fn read(&self, relative: &str) -> Option<(Vec<u8>, String)> {
        if relative.contains('\\')
            || relative.contains('\0')
            || relative
                .split('/')
                .any(|segment| segment == ".." || segment == ".")
        {
            return None;
        }
        let relative = relative.trim_end_matches('/');
        if let Some(dir) = &self.dir
            && let Some(found) = read_from_dir(dir, relative)
        {
            return Some(found);
        }
        let name = if relative.is_empty() {
            "index.html".to_string()
        } else if embedded(relative).is_some() {
            relative.to_string()
        } else {
            format!("{relative}/index.html")
        };
        embedded(&name).map(|bytes| (bytes.to_vec(), name))
    }
}

/// `relative` from the directory `root` (a directory means its `index.html`), only when the
/// resolved file stays below `root`.
fn read_from_dir(root: &Path, relative: &str) -> Option<(Vec<u8>, String)> {
    let mut file = root.join(relative);
    if file.is_dir() {
        file = file.join("index.html");
    }
    let file = std::fs::canonicalize(&file).ok()?;
    if !file.starts_with(root) || !file.is_file() {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    let name = file.file_name()?.to_str()?.to_string();
    Some((bytes, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files of the repository's `playground/` the embedded copy must hold: everything but
    /// the wasm build output, the tests, the scripts, README, CLAUDE.md and hidden files.
    fn playground_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            if name.starts_with('.')
                || [
                    "pkg",
                    "tests",
                    "build.sh",
                    "serve.py",
                    "README.md",
                    "CLAUDE.md",
                ]
                .contains(&relative.as_str())
            {
                continue;
            }
            if path.is_dir() {
                playground_files(root, &path, out);
            } else {
                out.push(relative);
            }
        }
    }

    #[test]
    fn the_embedded_ui_equals_the_playground_directory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../playground");
        let root = root.canonicalize().expect("the repository's playground/");
        let mut files = Vec::new();
        playground_files(&root, &root, &mut files);
        files.sort();
        let names: Vec<String> = embedded::UI_FILES
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        assert_eq!(
            names, files,
            "the embedded UI differs from playground/: a new top-level entry belongs into \
             ENTRIES of build.rs and `include` of Cargo.toml"
        );
        for (name, bytes) in embedded::UI_FILES {
            assert_eq!(
                *bytes,
                std::fs::read(root.join(name)).unwrap().as_slice(),
                "{name} is stale in the embedded UI"
            );
        }
        assert!(names.contains(&"js/examples.json".to_string()));
    }

    #[test]
    fn embedded_lookup_and_traversal() {
        let ui = Ui { dir: None };
        let (bytes, name) = ui.read("").unwrap();
        assert_eq!(name, "index.html");
        assert_eq!(bytes, embedded("index.html").unwrap());
        assert_eq!(ui.read("reset").unwrap().1, "reset/index.html");
        assert_eq!(ui.read("reset/").unwrap().1, "reset/index.html");
        assert!(ui.read("js/main.js").is_some());
        assert!(ui.read("missing.js").is_none());
        assert!(ui.read("../playground/index.html").is_none());
        assert!(ui.read("js/../index.html").is_none());
        assert!(ui.read("./index.html").is_none());
        assert!(ui.read("pkg/graphersal_wasm.js").is_none());
        assert!(ui.read("README.md").is_none());
    }
}
