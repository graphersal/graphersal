//! Embeds the playground UI into the crate (feature `ui-assets`).
//!
//! The UI is read from `ui/` next to this file: in the repository it is a symbolic link to
//! `../../playground`, in a packaged crate (`cargo package` follows the link) a real copy. When
//! `ui/` is not usable (a checkout without symbolic links turns the link into a text file), the
//! repository's `../../playground` is used. Only the entries of [`ENTRIES`] are embedded (the
//! same list as `include` in `Cargo.toml`): never `pkg/`, `tests/`, the scripts or the README.
//! Hidden files (`.DS_Store`, ...) are skipped. The generated `ui_files.rs` in `OUT_DIR` is one
//! sorted table of `(relative path, include_bytes!(..))`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The top-level entries of the playground directory that make up the served UI.
const ENTRIES: &[&str] = &["index.html", "css", "js", "assets", "reset"];

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let target = out.join("ui_files.rs");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_UI_ASSETS").is_none() {
        std::fs::write(&target, "pub static UI_FILES: &[(&str, &[u8])] = &[];\n")
            .expect("ui_files.rs is writable");
        return;
    }
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let root = [manifest.join("ui"), manifest.join("../../playground")]
        .into_iter()
        .find(|dir| dir.join("index.html").is_file())
        .unwrap_or_else(|| {
            panic!(
                "the playground UI is missing: neither {}/ui nor the repository's playground/ \
                 holds index.html",
                manifest.display()
            )
        });
    let root = root.canonicalize().expect("the UI directory resolves");
    let mut files = Vec::new();
    for entry in ENTRIES {
        let path = root.join(entry);
        println!("cargo::rerun-if-changed={}", path.display());
        collect(&root, &path, &mut files);
    }
    files.sort();
    let mut code = String::from("pub static UI_FILES: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in &files {
        let absolute = absolute.to_str().expect("UI file paths are UTF-8");
        writeln!(code, "    ({relative:?}, include_bytes!({absolute:?})),").expect("String");
    }
    code.push_str("];\n");
    std::fs::write(&target, code).expect("ui_files.rs is writable");
}

/// Adds `path` (a file, or every file below a directory) as `(relative name, absolute path)`.
fn collect(root: &Path, path: &Path, files: &mut Vec<(String, PathBuf)>) {
    if path.is_dir() {
        let mut children: Vec<PathBuf> = std::fs::read_dir(path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
            .map(|entry| entry.expect("a directory entry").path())
            .collect();
        children.sort();
        for child in children {
            let hidden = child
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| name.starts_with('.'));
            if !hidden {
                collect(root, &child, files);
            }
        }
    } else if path.is_file() {
        let relative = path
            .strip_prefix(root)
            .expect("below the UI root")
            .components()
            .map(|c| c.as_os_str().to_str().expect("UI file names are UTF-8"))
            .collect::<Vec<_>>()
            .join("/");
        files.push((relative, path.to_path_buf()));
    } else {
        panic!("the playground UI entry {} is missing", path.display());
    }
}
