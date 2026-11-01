//! Issue #19: the notes of upgrades and loads, and the refusal of a store whose model is
//! missing, are a short headline with indented details (one `key: value` per line) and every
//! command on a line of its own under a one-line `# comment`; the import of the only model the
//! schema names, in a store without a default, takes `--default`; a model records the names of
//! its repository's licence files.

#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use graphersal::persist::{ImportModelOptions, ModelRole, ModelSource, Store, StoreOptions};

use crate::model_fixture::*;

/// The 0.4.0 fixture directory `name` copied into `temp` (the fixtures are never written).
fn copy_fixture(temp: &Temp, name: &str) -> PathBuf {
    fn walk(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/v0.4.0")
        .join(name);
    let to = temp.join(name);
    walk(&from, &to);
    to
}

/// Every line after the headline is indented by two spaces, and every command line follows
/// its `# comment` (a command never shares a line with another one or with prose).
fn assert_layout(note: &str) {
    let mut lines = note.lines();
    let headline = lines.next().unwrap();
    assert!(!headline.starts_with(' '), "{note}");
    let rest: Vec<&str> = lines.collect();
    for (i, line) in rest.iter().enumerate() {
        assert!(line.starts_with("  "), "not indented: {line:?}\n{note}");
        let text = line.trim_start();
        if text.starts_with("graphersal ") {
            assert!(
                i > 0 && rest[i - 1].trim_start().starts_with("# "),
                "a command without its comment: {line:?}\n{note}"
            );
            assert_eq!(
                text.matches("graphersal ").count(),
                1,
                "two commands on one line: {line:?}\n{note}"
            );
        }
    }
}

#[test]
fn the_refusal_of_a_missing_model_has_one_commented_command_per_line_and_suggests_default() {
    let temp = Temp::new("note-refusal");
    let dir = copy_fixture(&temp, "missing");
    let report = Store::upgrade_in_place(&dir).unwrap();
    // The only model the schema names, and the store has no default: --default.
    let missing = &report.models_missing[0];
    assert!(missing.suggests_default());
    let store = dir.to_string_lossy().to_string();
    let note = missing.note(&store);
    assert_layout(&note);
    assert!(
        note.starts_with(
            "the schema needs the embedding model acme/gone, which the store does not keep\n"
        ),
        "{note}"
    );
    assert!(
        note.contains(&format!(
            "\n  # import it into the store as its default model (embed() without a model name \
             uses it)\n  graphersal embed-model import acme/gone --into {store} --default"
        )),
        "{note}"
    );
    // The vector note of that model says its patch runs after the import.
    let vector = &report.vector_notes(&store)[0];
    assert_layout(vector);
    assert!(
        vector.contains("# after the import of acme/gone (the store does not open before)"),
        "{vector}"
    );
    // The open is refused with the same layout.
    let refused = match Store::open(&dir, StoreOptions::new()) {
        Ok(_) => panic!("the model is missing"),
        Err(err) => err.to_string(),
    };
    let reason = refused
        .split_once(": ")
        .map(|(_, reason)| reason)
        .unwrap_or(&refused);
    assert_layout(reason);
    assert!(
        reason.starts_with(
            "the model acme/gone is not in the database at this position\n  needed by: doc.emb\n"
        ),
        "{refused}"
    );
    assert!(
        reason.contains(
            "\n  # download the model into the local models directory\n  graphersal embed-model \
             download acme/gone\n"
        ),
        "{refused}"
    );
    assert!(
        reason.contains(&format!(
            "\n  graphersal embed-model import acme/gone --into {store} --default\n"
        )),
        "{refused}"
    );
}

#[test]
fn every_note_of_a_040_upgrade_and_gsnap_has_the_layout() {
    let temp = Temp::new("note-upgrade");
    let dir = copy_fixture(&temp, "store");
    let report = Store::upgrade_in_place(&dir).unwrap();
    let notes = report.vector_notes("st");
    assert_eq!(notes.len(), 3, "{notes:#?}");
    for note in &notes {
        assert_layout(note);
    }
    let packed = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.4.0/snapshot.gsnap");
    let (_, report) = graphersal::persist::read_snapshot_file(
        &packed,
        &graphersal::persist::ReadOptions::default(),
    )
    .unwrap();
    let notes = report.notes("snapshot.gsnap");
    assert!(notes.len() >= 5, "{notes:#?}");
    for note in &notes {
        assert_layout(note);
    }
    let default = notes
        .iter()
        .find(|n| n.contains("none is the default"))
        .unwrap();
    assert!(
        default.ends_with(
            "\n  # or make one the default, in a store\n  graphersal store models default <dir> \
             <repo>"
        ),
        "{default}"
    );
}

#[test]
fn a_model_records_the_names_of_its_repository_licence_files() {
    let temp = Temp::new("note-licence-files");
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let with_files = tiny_model(1, "acme/a")
        .with_file(
            "LICENSE",
            ModelRole::License,
            Cursor::new(b"own terms".to_vec()),
        )
        .with_file(
            "COPYING",
            ModelRole::License,
            Cursor::new(b"more terms".to_vec()),
        );
    let model = store
        .import_model(ModelSource::Files(with_files), ImportModelOptions::new())
        .unwrap()
        .model;
    assert_eq!(model.license_text, "file");
    assert_eq!(model.license_files, ["COPYING", "LICENSE"]);
    // A standard text the store keeps is no file of the repository.
    let template = store
        .import_model(
            ModelSource::Files(tiny_model(2, "acme/b")),
            ImportModelOptions::new(),
        )
        .unwrap()
        .model;
    assert_eq!(template.license_text, "template");
    assert!(template.license_files.is_empty());
    store.close().unwrap();
    // The names are part of the definition: they survive a reopen.
    let store = Store::open(&path, options()).unwrap();
    let models = store.models().unwrap();
    let a = models.iter().find(|m| m.repo == "acme/a").unwrap();
    assert_eq!(a.license_files, ["COPYING", "LICENSE"]);
    store.close().unwrap();
}
