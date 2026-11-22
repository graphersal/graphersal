//! `Store::convert_with` and `ConvertOptions::with_fill_missing_blobs`: a closed store whose
//! model blob is missing from its `blobs/` converts when a fill source (a host's shared model
//! directory laid out as `blobs/<2 hex>/<hash>`) holds the blob; a blob with other bytes in the
//! source, or missing from both, is refused with the named error and no copy is left; nothing of
//! the source store or the fill directory changes; `Store::convert` without options behaves as
//! before. `ConvertOptions::with_skip_missing_blobs`: the conversion completes without a blob
//! missing from both, lists it in `ConvertReport::blobs_missing`, and `Store::restore_blobs_from`
//! fills it later; a damaged blob is still refused.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use graphersal::error::DiagnosticError;
use graphersal::persist::{
    BlobSource, ConvertOptions, ModelState, PersistError, Store, StoreFailure,
};
use graphersal::prelude::*;
use graphersal::system_journal::JournalQuery;

use crate::model_fixture::*;

/// Every file under `root` with its bytes (relative names, `/` separated).
fn files_of(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, out);
            } else {
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A closed directory store at `temp/s` whose schema needs the tiny model, with the model's
/// blob moved out of its `blobs/` into `temp/models/blobs/<2 hex>/<hash>` (a host's shared
/// model directory). Returns the store path, the models directory, the blob's relative name
/// (`blobs/..`) and the blob's path in the models directory.
fn store_without_its_blob(temp: &Temp) -> (PathBuf, PathBuf, String, PathBuf) {
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &model.identity);
    let blob = blob_path(&path, &store, &model.identity);
    store.close().unwrap();
    let relative = blob.strip_prefix(&path).unwrap().to_path_buf();
    let name = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let models = temp.join("models");
    let mut shared = models.clone();
    for part in relative.components() {
        shared = shared.join(part.as_os_str());
    }
    fs::create_dir_all(shared.parent().unwrap()).unwrap();
    fs::rename(&blob, &shared).unwrap();
    (path, models, name, shared)
}

#[test]
fn a_missing_model_blob_is_filled_from_the_models_directory() {
    let temp = Temp::new("convert-fill");
    let (path, models, name, _) = store_without_its_blob(&temp);
    let source_before = files_of(&path);
    let models_before = files_of(&models);

    let to = temp.join("s.gstore");
    let report = Store::convert_with(
        &path,
        &to,
        ConvertOptions::new().with_fill_missing_blobs(BlobSource::store(&models)),
    )
    .unwrap();
    assert_eq!(report.blobs_filled, vec![name.clone()]);
    assert!(report.bytes_filled > 0);
    assert!(report.fill_source.is_some());
    assert!(report.files_copied > 0 && report.bytes_copied > 0);

    // Nothing of the source store or the fill directory changed.
    assert_eq!(files_of(&path), source_before);
    assert_eq!(files_of(&models), models_before);

    // The copy lists the model with its blob, verifies and opens.
    let listed = Store::models_dir(&to).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].repo, "acme/tiny");
    assert_eq!(listed[0].state, ModelState::NotLoaded);
    let verified = Store::verify_dir(&to).unwrap();
    assert!(verified.is_ok(), "{:?}", verified.problems);
    assert_eq!(verified.models_checked, 1);
    let store = Store::open(&to, options()).unwrap();
    #[cfg(feature = "model2vec")]
    {
        assert_eq!(store.models().unwrap()[0].state, ModelState::Loaded);
        assert!(store.read_only_reason().unwrap().is_none());
    }
    store.close().unwrap();
    // The filled blob is recorded in the copy's system journal.
    let journal = Store::journal_dir(&to, &JournalQuery::new()).unwrap();
    let hash = name.rsplit('/').next().unwrap();
    assert!(
        journal.records.iter().any(|r| r.kind == "blob.repaired"
            && r.field("how") == Some(&ElementProperty::from("restored"))
            && r.field("hash") == Some(&ElementProperty::from(hash))),
        "{:?}",
        journal.records
    );
}

#[test]
fn a_blob_with_other_bytes_in_the_models_directory_is_refused() {
    let temp = Temp::new("convert-fill-damaged");
    let (path, models, name, shared) = store_without_its_blob(&temp);
    flip_byte(&shared);
    let models_before = files_of(&models);
    let to = temp.join("copy");
    let err = Store::convert_with(
        &path,
        &to,
        ConvertOptions::new().with_fill_missing_blobs(BlobSource::store(&models)),
    )
    .unwrap_err();
    let PersistError::Store { cause, reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*cause, StoreFailure::BlobDamaged);
    assert!(reason.contains(&name), "{reason}");
    assert!(err.help().unwrap().contains("restore_blobs_from"));
    assert!(!to.exists() || fs::read_dir(&to).unwrap().next().is_none());
    assert_eq!(files_of(&models), models_before);
}

#[test]
fn a_blob_missing_from_both_is_refused_by_name() {
    let temp = Temp::new("convert-fill-missing");
    let (path, _, name, _) = store_without_its_blob(&temp);
    let empty = temp.join("empty-models");
    fs::create_dir_all(&empty).unwrap();
    let to = temp.join("copy.gstore");
    let err = Store::convert_with(
        &path,
        &to,
        ConvertOptions::new().with_fill_missing_blobs(BlobSource::store(&empty)),
    )
    .unwrap_err();
    let PersistError::Store { cause, reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*cause, StoreFailure::BlobUnavailable);
    assert!(reason.contains(&name), "{reason}");
    assert!(err.help().unwrap().contains("restore-blobs"));
    assert!(!to.exists());
}

#[test]
fn convert_without_options_is_unchanged() {
    let temp = Temp::new("convert-plain");
    // A healthy store: convert and convert_with without options copy the same.
    let path = temp.join("healthy");
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    let model = import(&store, 1, "acme/tiny").model;
    set_schema(&store, &model.identity);
    store.close().unwrap();
    let (files, bytes) = Store::convert(&path, temp.join("a.gstore")).unwrap();
    let report = Store::convert_with(&path, temp.join("b.gstore"), ConvertOptions::new()).unwrap();
    assert_eq!((report.files_copied, report.bytes_copied), (files, bytes));
    assert!(report.blobs_filled.is_empty());
    assert_eq!(report.fill_source, None);

    // A missing blob: the copy does not verify and is removed, with or without the
    // option-less convert_with.
    let (path, _, name, _) = store_without_its_blob(&temp);
    for (i, err) in [
        Store::convert(&path, temp.join("c"))
            .map(|_| ())
            .unwrap_err(),
        Store::convert_with(&path, temp.join("d"), ConvertOptions::new())
            .map(|_| ())
            .unwrap_err(),
    ]
    .into_iter()
    .enumerate()
    {
        let PersistError::Store { cause, reason, .. } = &err else {
            panic!("{err}");
        };
        assert_eq!(*cause, StoreFailure::CopyDoesNotVerify, "{i}: {reason}");
        assert!(
            reason.contains(name.rsplit('/').next().unwrap()),
            "{reason}"
        );
    }
    assert!(!temp.join("c").exists() || fs::read_dir(temp.join("c")).unwrap().next().is_none());
    assert!(!temp.join("d").exists() || fs::read_dir(temp.join("d")).unwrap().next().is_none());
}

// ---------------------------------------------------------------------------------------------
// `ConvertOptions::with_skip_missing_blobs`: the conversion completes without the blob.
// ---------------------------------------------------------------------------------------------

/// A closed directory store at `temp/<name>` holding the tiny model without its blob (removed
/// from `blobs/`); with `needed` its schema names the model as an `embedder`. Returns the
/// store path, the blob's relative name (`blobs/..`) and a directory laid out as a store's
/// `blobs/` holding the blob (a host's shared model directory, filled later).
fn store_lacking_its_blob(temp: &Temp, name: &str, needed: bool) -> (PathBuf, String, PathBuf) {
    let path = temp.join(name);
    let store = Store::create(&path, options()).unwrap();
    add_person(&store, "ann");
    let model = import(&store, 1, "acme/tiny").model;
    if needed {
        set_schema(&store, &model.identity);
    }
    let blob = blob_path(&path, &store, &model.identity);
    store.close().unwrap();
    let relative = blob.strip_prefix(&path).unwrap().to_path_buf();
    let blob_name = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let models = temp.join(&format!("{name}-models"));
    let mut shared = models.clone();
    for part in relative.components() {
        shared = shared.join(part.as_os_str());
    }
    fs::create_dir_all(shared.parent().unwrap()).unwrap();
    fs::rename(&blob, &shared).unwrap();
    (path, blob_name, models)
}

/// The creation parameters `Store::inspect` reports (rule 22), without the store id.
fn creation_parameters(path: &Path) -> (u32, u128, i64, u32, String) {
    let info = Store::inspect(path).unwrap();
    (
        info.format_version,
        info.graph_id,
        info.created_at,
        info.chunk_bytes,
        format!("{:?}", info.damage_policy),
    )
}

#[test]
fn the_default_still_refuses_a_blob_missing_from_both() {
    let temp = Temp::new("convert-skip-default");
    let (path, name, _) = store_lacking_its_blob(&temp, "s", true);
    let empty = temp.join("empty-models");
    fs::create_dir_all(&empty).unwrap();
    let err = Store::convert_with(
        &path,
        temp.join("copy"),
        ConvertOptions::new().with_fill_missing_blobs(BlobSource::store(&empty)),
    )
    .unwrap_err();
    let PersistError::Store { cause, reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*cause, StoreFailure::BlobUnavailable);
    assert!(reason.contains(&name), "{reason}");
    let err = Store::convert_with(&path, temp.join("copy2"), ConvertOptions::new()).unwrap_err();
    assert!(
        matches!(
            err,
            PersistError::Store {
                cause: StoreFailure::CopyDoesNotVerify,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn a_needed_model_blob_missing_from_both_is_skipped_listed_and_filled_later() {
    let temp = Temp::new("convert-skip-needed");
    let (path, name, models) = store_lacking_its_blob(&temp, "s", true);
    let empty = temp.join("empty-models");
    fs::create_dir_all(&empty).unwrap();
    let source_before = files_of(&path);

    let to = temp.join("copy.gstore");
    let report = Store::convert_with(
        &path,
        &to,
        ConvertOptions::new()
            .with_fill_missing_blobs(BlobSource::store(&empty))
            .with_skip_missing_blobs(),
    )
    .unwrap();
    assert_eq!(report.blobs_missing, vec![name.clone()]);
    assert!(report.blobs_filled.is_empty());
    assert!(report.files_copied > 0);
    assert_eq!(files_of(&path), source_before);

    // Rule 22: the copy carries the creation parameters, with a new store id.
    assert_eq!(creation_parameters(&to), creation_parameters(&path));
    assert_ne!(
        Store::inspect(&to).unwrap().store_id,
        Store::inspect(&path).unwrap().store_id
    );

    // The copy lists the model as missing; no placeholder blob was written.
    let listed = Store::models_dir(&to).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].repo, "acme/tiny");
    assert_eq!(listed[0].state, ModelState::Missing);
    let verified = Store::verify_dir(&to).unwrap();
    assert_eq!(verified.problems.len(), 1, "{:?}", verified.problems);
    assert_eq!(verified.problems[0].path, name);

    // The schema needs the model: the open is refused (not damage), naming it.
    let err = Store::open(&to, options()).map(|_| ()).unwrap_err();
    let PersistError::Store { cause, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*cause, StoreFailure::ModelUnavailable, "{err}");

    // A later fill from the models directory makes it whole.
    let filled = Store::restore_blobs_from(&to, BlobSource::store(&models)).unwrap();
    assert_eq!(filled.restored, vec![name.clone()]);
    assert!(filled.is_complete());
    let verified = Store::verify_dir(&to).unwrap();
    assert!(verified.is_ok(), "{:?}", verified.problems);
    let store = Store::open(&to, options()).unwrap();
    assert_ne!(store.models().unwrap()[0].state, ModelState::Missing);
    let people = store
        .graph()
        .read()
        .traversal()
        .v(None)
        .has_label("person")
        .count()
        .next()
        .unwrap();
    assert_eq!(people, Some(ElementProperty::from(1)));
    store.close().unwrap();
}

#[test]
fn a_store_with_a_model_nothing_needs_opens_and_queries_without_its_blob() {
    let temp = Temp::new("convert-skip-not-needed");
    let (path, name, models) = store_lacking_its_blob(&temp, "s", false);
    let to = temp.join("copy");
    // Without a fill source too.
    let report =
        Store::convert_with(&path, &to, ConvertOptions::new().with_skip_missing_blobs()).unwrap();
    assert_eq!(report.blobs_missing, vec![name.clone()]);
    assert_eq!(report.fill_source, None);

    let store = Store::open(&to, options()).unwrap();
    assert!(store.read_only_reason().unwrap().is_none());
    assert_eq!(store.models().unwrap()[0].state, ModelState::Missing);
    add_person(&store, "bob");
    let people = store
        .graph()
        .read()
        .traversal()
        .v(None)
        .has_label("person")
        .count()
        .next()
        .unwrap();
    assert_eq!(people, Some(ElementProperty::from(2)));
    // A call running the model fails with an error naming it.
    let err = store
        .graph()
        .read()
        .traversal()
        .inject(vec!["graph"])
        .embed_value(Some("acme/tiny"))
        .to_list()
        .unwrap_err();
    let text = graphersal::error::format_diagnostic(&*err);
    assert!(text.contains("acme/tiny"), "{text}");
    store.close().unwrap();

    let filled = Store::restore_blobs_from(&to, BlobSource::store(&models)).unwrap();
    assert_eq!(filled.restored, vec![name]);
    let store = Store::open(&to, options()).unwrap();
    assert_ne!(store.models().unwrap()[0].state, ModelState::Missing);
    #[cfg(feature = "model2vec")]
    {
        let vectors = store
            .graph()
            .read()
            .traversal()
            .inject(vec!["graph"])
            .embed_value(Some("acme/tiny"))
            .to_list()
            .unwrap();
        assert_eq!(vectors.len(), 1);
    }
    store.close().unwrap();
}

#[test]
fn skipping_tolerates_only_absent_blobs() {
    let temp = Temp::new("convert-skip-damaged");
    // A blob present in the source but damaged still fails the copy's verification.
    let path = temp.join("s");
    let store = Store::create(&path, options()).unwrap();
    let model = import(&store, 1, "acme/tiny").model;
    let blob = blob_path(&path, &store, &model.identity);
    store.close().unwrap();
    flip_byte(&blob);
    let to = temp.join("copy");
    let err = Store::convert_with(&path, &to, ConvertOptions::new().with_skip_missing_blobs())
        .unwrap_err();
    assert!(
        matches!(
            err,
            PersistError::Store {
                cause: StoreFailure::CopyDoesNotVerify,
                ..
            }
        ),
        "{err}"
    );
    assert!(!to.exists() || fs::read_dir(&to).unwrap().next().is_none());

    // A fill source holding the blob with other bytes is still refused.
    let (path, name, models) = store_lacking_its_blob(&temp, "t", true);
    let shared = name
        .split('/')
        .fold(models.clone(), |at, part| at.join(part));
    flip_byte(&shared);
    let to = temp.join("copy-t");
    let err = Store::convert_with(
        &path,
        &to,
        ConvertOptions::new()
            .with_fill_missing_blobs(BlobSource::store(&models))
            .with_skip_missing_blobs(),
    )
    .unwrap_err();
    let PersistError::Store { cause, reason, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(*cause, StoreFailure::BlobDamaged);
    assert!(reason.contains(&name), "{reason}");
    assert!(!to.exists() || fs::read_dir(&to).unwrap().next().is_none());
}
