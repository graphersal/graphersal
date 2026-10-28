//! Embedding models kept in a Store (feature `persist`; format spec 4.5, book page "Embedding
//! Models in a Store"): add, list, remove, the licence template, `verify` of every file, prune,
//! checkpoint and compact keep them, EVERY copy carries them (full and incremental backup, ZIP
//! backup and restore, fork, attic fork, repair, convert, upgrade, a packed snapshot) and the
//! shareable copies leave them out on request, hostile model files never panic. With the
//! feature `model2vec` (native only): the models the schema names are loaded at open, an
//! embedder the host registered wins, a missing model is a note. No network: the models are
//! tiny and synthetic.
#![cfg(all(feature = "persist", not(target_arch = "wasm32")))]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use graphersal::persist::{
    BaseModel, MemDir, ModelCopy, ModelRole, NewModel, PersistError, RecoveryTarget, RollbackMode,
    Store, StoreFailure, StoreOptions, StoredModel,
};
use graphersal::prelude::*;

/// A fresh directory under the system temp dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "graphersal-models-{}-{}-{name}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, part: &str) -> PathBuf {
        self.0.join(part)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options() -> StoreOptions {
    StoreOptions::new().with_checkpoint_after_bytes(0)
}

fn add_person(store: &Store, name: &str) {
    store
        .graph()
        .write()
        .traversal_mut()
        .add_v("person")
        .property("name", name)
        .to_list()
        .unwrap();
}

/// A model whose files are arbitrary bytes (nothing loads it in these tests): `seed` makes
/// different models.
fn fake_model(seed: u8) -> NewModel {
    NewModel::new()
        .with_file(
            "config.json",
            ModelRole::Config,
            Cursor::new(vec![b'{', seed, b'}']),
        )
        .with_file(
            "tokenizer.json",
            ModelRole::Tokenizer,
            Cursor::new(vec![seed; 100]),
        )
        .with_file(
            "model.safetensors",
            ModelRole::Weights,
            Cursor::new(vec![seed.wrapping_add(1); 5000]),
        )
        .with_file(
            "README.md",
            ModelRole::Card,
            Cursor::new(b"---\nlicense: mit\n---\n# A model\n".to_vec()),
        )
        .with_repo("acme/tiny")
        .with_license("mit")
}

/// The ids of the models a store keeps.
fn ids(models: &[StoredModel]) -> Vec<String> {
    models.iter().map(|m| m.id.clone()).collect()
}

/// The ids of the models in the store at `dir` (a path or memory).
fn ids_in(dir: impl graphersal::persist::IntoStoreDir) -> Vec<String> {
    ids(&Store::models_dir(dir).unwrap())
}

/// A store at `path` with one person and the model `fake_model(seed)`.
fn store_with_model(path: &Path, seed: u8) -> (Store, StoredModel) {
    let store = Store::create(path, options()).unwrap();
    add_person(&store, "ann");
    let added = store.add_model(fake_model(seed)).unwrap();
    assert!(!added.already_stored);
    (store, added.model)
}

/// Overwrites one byte of a file in the middle.
fn flip_byte(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x55;
    fs::write(path, bytes).unwrap();
}

#[test]
fn an_added_model_is_listed_with_its_identity_licence_and_files() {
    let temp = Temp::new("add");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    assert!(model.id.starts_with("model2vec-") && model.id.len() == 22);
    let hex = &model.id["model2vec-".len()..];
    assert_eq!(model.identity, format!("model2vec/acme/tiny@{hex}"));
    assert_eq!(model.family, "model2vec");
    assert_eq!(model.repo.as_deref(), Some("acme/tiny"));
    assert_eq!(model.license, "mit");
    // No licence file in the repository: the standard MIT text, marked as a template.
    assert_eq!(model.license_text, "template");
    let license = model
        .files
        .iter()
        .find(|f| f.name == "LICENSE")
        .expect("a template LICENSE");
    assert_eq!(license.role, "license");
    let text =
        String::from_utf8(store.read_model_file(&model.identity, "LICENSE").unwrap()).unwrap();
    assert!(text.contains("Permission is hereby granted") && text.contains("acme/tiny"));
    assert!(
        model
            .files
            .iter()
            .any(|f| f.name == "README.md" && f.role == "card")
    );
    assert_eq!(
        model.bytes,
        model.files.iter().map(|f| f.bytes).sum::<u64>()
    );
    assert!(model.problem.is_none());
    assert_eq!(store.models().unwrap(), vec![model.clone()]);
    assert_eq!(store.info().unwrap().models, vec![model.clone()]);
    assert!(
        temp.join("s/models")
            .join(&model.id)
            .join("graphersal-embed-model.json")
            .is_file()
    );

    // The same files again: nothing written, the stored model returned.
    let again = store.add_model(fake_model(1)).unwrap();
    assert!(again.already_stored);
    assert_eq!(again.model, model);
    // Another model is another directory; the identity and the id both find a model.
    let other = store.add_model(fake_model(2)).unwrap().model;
    assert_ne!(other.id, model.id);
    assert_eq!(store.models().unwrap().len(), 2);
    assert_eq!(
        store.read_model_file(&model.id, "config.json").unwrap(),
        vec![b'{', 1, b'}']
    );

    // The store reopens with its models; the closed store lists them too.
    store.close().unwrap();
    assert_eq!(ids_in(temp.join("s").as_path()).len(), 2);
    let store = Store::open(temp.join("s"), options()).unwrap();
    assert_eq!(store.models().unwrap().len(), 2);
}

#[test]
fn the_licence_text_comes_from_a_file_a_template_or_nowhere() {
    let store = Store::create(MemDir::new(), options()).unwrap();
    let with_file = fake_model(3).with_file(
        "LICENSE.txt",
        ModelRole::License,
        Cursor::new(b"Apache License\nVersion 2.0".to_vec()),
    );
    let model = store.add_model(with_file).unwrap().model;
    assert_eq!(model.license_text, "file");
    assert!(!model.files.iter().any(|f| f.name == "LICENSE"));

    let apache = fake_model(4).with_license("Apache-2.0");
    let model = store.add_model(apache).unwrap().model;
    assert_eq!(model.license_text, "template");
    let text = String::from_utf8(store.read_model_file(&model.id, "LICENSE").unwrap()).unwrap();
    assert!(text.contains("Apache License") && text.contains("Version 2.0"));

    let unknown = NewModel::new()
        .with_file(
            "config.json",
            ModelRole::Config,
            Cursor::new(b"{}".to_vec()),
        )
        .with_file(
            "tokenizer.json",
            ModelRole::Tokenizer,
            Cursor::new(b"t".to_vec()),
        )
        .with_file(
            "model.safetensors",
            ModelRole::Weights,
            Cursor::new(b"w".to_vec()),
        )
        .with_gated(true);
    let model = store.add_model(unknown).unwrap().model;
    assert_eq!(model.license, "unknown");
    assert_eq!(model.license_text, "none");
    assert!(model.gated);
    assert!(model.repo.is_none());
    assert!(model.identity.starts_with("model2vec@"));
}

/// A model made from base models under another licence carries that licence's standard text
/// too (one file per licence); a base model under the model's own licence adds none.
#[test]
fn the_licences_of_the_base_models_travel_with_the_model() {
    let store = Store::create(MemDir::new(), options()).unwrap();
    let model = fake_model(7)
        .with_base_model(BaseModel::new(
            "sentence-transformers/LaBSE",
            Some("apache-2.0".into()),
        ))
        .with_base_model(BaseModel::new("acme/other", Some("Apache-2.0".into())))
        .with_base_model(BaseModel::new("acme/same", Some("mit".into())))
        .with_base_model(BaseModel::new("acme/unknown", None));
    let model = store.add_model(model).unwrap().model;
    assert_eq!(model.license_text, "template", "the model's own MIT text");
    let licences: Vec<&str> = model
        .files
        .iter()
        .filter(|f| f.role == ModelRole::License.as_str())
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(licences, ["LICENSE", "LICENSE-BASE-apache-2.0"]);
    let text = String::from_utf8(
        store
            .read_model_file(&model.id, "LICENSE-BASE-apache-2.0")
            .unwrap(),
    )
    .unwrap();
    assert!(
        text.contains("acme/tiny was made from sentence-transformers/LaBSE, acme/other"),
        "{text}"
    );
    assert!(text.contains("Apache License") && text.contains("Version 2.0"));
    assert!(!text.contains("acme/same"), "{text}");
    // Listed, verified and carried like every other file of the model.
    assert_eq!(store.models().unwrap()[0].files, model.files);
    assert!(store.verify().unwrap().is_ok());
    let mut packed = Vec::new();
    store
        .export_packed_with(None, &mut packed, ModelCopy::Include)
        .unwrap();
    let copy = Store::create_from_packed(MemDir::new(), Cursor::new(packed), options()).unwrap();
    assert!(
        copy.read_model_file(&model.id, "LICENSE-BASE-apache-2.0")
            .is_ok()
    );
}

#[test]
fn an_invalid_model_is_refused_and_leaves_nothing() {
    let temp = Temp::new("invalid");
    let store = Store::create(temp.join("s"), options()).unwrap();
    let cases = [
        NewModel::new()
            .with_file(
                "config.json",
                ModelRole::Config,
                Cursor::new(b"{}".to_vec()),
            )
            .with_file(
                "tokenizer.json",
                ModelRole::Tokenizer,
                Cursor::new(b"t".to_vec()),
            ),
        fake_model(1).with_file("../x", ModelRole::Card, Cursor::new(b"x".to_vec())),
        fake_model(1).with_file("README.md", ModelRole::Card, Cursor::new(b"x".to_vec())),
        fake_model(1).with_file(
            "graphersal-embed-model.json",
            ModelRole::Card,
            Cursor::new(b"x".to_vec()),
        ),
        fake_model(1).with_repo("not a repo"),
        fake_model(1).with_license("MIT OR Apache-2.0"),
    ];
    for model in cases {
        let err = store.add_model(model).unwrap_err();
        assert!(
            matches!(
                err,
                PersistError::Store {
                    cause: StoreFailure::InvalidModel,
                    ..
                }
            ),
            "{err}"
        );
        let help = err.help().unwrap_or_default();
        assert!(help.contains("Model2Vec model"), "{help}");
    }
    let left: Vec<_> = fs::read_dir(temp.join("s/models"))
        .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    let err = store
        .remove_model("model2vec/acme/none@0123456789ab")
        .unwrap_err();
    assert!(
        matches!(
            err,
            PersistError::Store {
                cause: StoreFailure::UnknownModel,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn a_model_leaves_only_by_an_explicit_remove() {
    let temp = Temp::new("remove");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    // Checkpoints, prune and compact never touch the models.
    add_person(&store, "bob");
    store.checkpoint(None).unwrap();
    add_person(&store, "cid");
    store.checkpoint(None).unwrap();
    store.prune(store.graph().read().last_commit_seq()).unwrap();
    store.compact().unwrap();
    assert_eq!(ids(&store.models().unwrap()), vec![model.id.clone()]);
    assert!(store.verify().unwrap().is_ok());

    let removed = store.remove_model(&model.identity).unwrap();
    assert_eq!(removed.id, model.id);
    assert!(store.models().unwrap().is_empty());
    assert!(!temp.join("s/models").join(&model.id).exists());
    store.close().unwrap();

    // The same on the closed store, by the id.
    Store::add_model_dir(temp.join("s"), fake_model(5)).unwrap();
    let id = ids_in(temp.join("s").as_path()).remove(0);
    Store::remove_model_dir(temp.join("s"), &id).unwrap();
    assert!(ids_in(temp.join("s").as_path()).is_empty());
    // An open store holds the writer lock: a second writer is refused.
    let store = Store::open(temp.join("s"), options()).unwrap();
    assert!(matches!(
        Store::add_model_dir(temp.join("s"), fake_model(6)),
        Err(PersistError::Locked { .. })
    ));
    drop(store);
}

#[test]
fn verify_finds_a_damaged_model_and_the_store_stays_writable() {
    let temp = Temp::new("verify");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    let report = store.verify().unwrap();
    assert!(report.is_ok(), "{:?}", report.problems);
    assert_eq!(report.models_checked, 1);

    let dir = temp.join("s/models").join(&model.id);
    flip_byte(&dir.join("model.safetensors"));
    let report = store.verify().unwrap();
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    assert_eq!(
        report.problems[0].path,
        format!("models/{}/model.safetensors", model.id)
    );
    assert!(report.problems[0].reason.contains("SHA-256"));
    // A damaged model is no graph damage: the store keeps committing.
    assert!(!store.is_frozen_by_damage());
    add_person(&store, "bob");
    // Reading the file checks it too.
    assert!(matches!(
        store.read_model_file(&model.id, "model.safetensors"),
        Err(PersistError::Corrupt { .. })
    ));

    // A missing file, an unlisted file and a stray entry are problems as well.
    fs::remove_file(dir.join("README.md")).unwrap();
    fs::write(dir.join("extra.bin"), b"x").unwrap();
    fs::write(temp.join("s/models/stray"), b"x").unwrap();
    let report = Store::verify_dir(temp.join("s")).unwrap();
    let paths: Vec<&str> = report.problems.iter().map(|p| p.path.as_str()).collect();
    for expected in [
        format!("models/{}/README.md", model.id),
        format!("models/{}/extra.bin", model.id),
        "models/stray".to_string(),
    ] {
        assert!(paths.contains(&expected.as_str()), "{paths:?}");
    }
}

#[test]
fn every_copy_carries_the_models() {
    let temp = Temp::new("copies");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    let want = vec![model.id.clone()];

    // Full backup, then an increment that follows an added and a removed model.
    let report = store.backup(temp.join("backup")).unwrap();
    assert_eq!(ids(&report.models), want);
    assert_eq!(ids_in(temp.join("backup").as_path()), want);
    assert!(Store::verify_dir(temp.join("backup")).unwrap().is_ok());
    let second = store.add_model(fake_model(2)).unwrap().model;
    add_person(&store, "bob");
    let report = store.backup(temp.join("backup")).unwrap();
    assert!(report.incremental);
    let mut both = vec![model.id.clone(), second.id.clone()];
    both.sort();
    assert_eq!(ids_in(temp.join("backup").as_path()), both);
    store.remove_model(&second.id).unwrap();
    add_person(&store, "cid");
    store.backup(temp.join("backup")).unwrap();
    assert_eq!(ids_in(temp.join("backup").as_path()), want);

    // Restoring the backup in place keeps them; forking it carries them.
    let backup_fork = store_fork_of_backup(&temp);
    assert_eq!(ids_in(backup_fork.as_path()), want);

    // Fork and attic fork.
    let fork = store
        .fork(RecoveryTarget::Latest, temp.join("fork"))
        .unwrap();
    assert_eq!(ids(&fork.models), want);
    assert_eq!(ids_in(temp.join("fork").as_path()), want);
    assert!(
        fork.to_string()
            .contains("model carried: model2vec/acme/tiny@")
    );
    store.mark("before").unwrap();
    add_person(&store, "dan");
    store
        .rollback_to(RecoveryTarget::Mark("before".into()), RollbackMode::Move)
        .unwrap();
    let entry = store.attic_list().unwrap().remove(0);
    let attic = store
        .attic_fork(&entry.id, temp.join("attic-fork"))
        .unwrap();
    assert_eq!(ids(&attic.models), want);
    assert_eq!(ids_in(temp.join("attic-fork").as_path()), want);

    // ZIP backup and restore, with and without the models.
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = Cursor::new(Vec::new());
        let report = store.backup_zip(&mut zip).unwrap();
        assert_eq!(ids(&report.models), want);
        Store::restore_zip(Cursor::new(zip.into_inner()), temp.join("restored")).unwrap();
        assert_eq!(ids_in(temp.join("restored").as_path()), want);
        assert!(Store::verify_dir(temp.join("restored")).unwrap().is_ok());
        let mut zip = Cursor::new(Vec::new());
        let report = store.backup_zip_with(&mut zip, ModelCopy::Exclude).unwrap();
        assert!(report.models.is_empty());
        let bytes = zip.into_inner();
        assert!(
            !bytes.windows(7).any(|w| w == b"models/"),
            "an archive without models names none"
        );
        Store::restore_zip(Cursor::new(bytes), temp.join("restored-bare")).unwrap();
        assert!(ids_in(temp.join("restored-bare").as_path()).is_empty());
    }

    // A packed snapshot, with and without the models.
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    let imported =
        Store::create_from_packed(temp.join("from-packed"), packed.as_slice(), options()).unwrap();
    assert_eq!(ids(&imported.models().unwrap()), want);
    assert!(imported.verify().unwrap().is_ok());
    // A reader without models (the graph load) checks and ignores them.
    let graph = graphersal::persist::read_snapshot(
        packed.as_slice(),
        &graphersal::persist::ReadOptions::new(),
    )
    .unwrap();
    assert_eq!(graph.vertex_count(), store.graph().read().vertex_count());
    let mut bare = Vec::new();
    store
        .export_packed_with(None, &mut bare, ModelCopy::Exclude)
        .unwrap();
    assert!(bare.len() + 5000 < packed.len());
    let imported =
        Store::create_from_packed(temp.join("from-bare"), bare.as_slice(), options()).unwrap();
    assert!(imported.models().unwrap().is_empty());

    // Convert into a single file and back, repair.
    store.close().unwrap();
    Store::convert(temp.join("s"), temp.join("single.gstore")).unwrap();
    assert_eq!(ids_in(temp.join("single.gstore").as_path()), want);
    Store::convert(temp.join("single.gstore"), temp.join("back")).unwrap();
    assert_eq!(ids_in(temp.join("back").as_path()), want);
    let repair = Store::repair_dir(temp.join("s"), temp.join("repaired")).unwrap();
    assert_eq!(ids(&repair.models), want);
    assert!(repair.models_not_carried.is_empty());
    assert_eq!(ids_in(temp.join("repaired").as_path()), want);
}

/// Restores the backup in `temp/backup` into a fork (the backup itself stays a backup).
fn store_fork_of_backup(temp: &Temp) -> PathBuf {
    let target = temp.join("backup-fork");
    Store::fork_dir(temp.join("backup"), RecoveryTarget::Latest, &target).unwrap();
    target
}

#[test]
fn a_damaged_model_stops_a_copy_but_not_a_repair() {
    let temp = Temp::new("damaged");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    flip_byte(&temp.join("s/models").join(&model.id).join("tokenizer.json"));
    assert!(matches!(
        store.backup(temp.join("backup")),
        Err(PersistError::BackupDamaged(_))
    ));
    assert!(!temp.join("backup").join("GRAPH").exists());
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = Cursor::new(Vec::new());
        assert!(matches!(
            store.backup_zip(&mut zip),
            Err(PersistError::BackupDamaged(_))
        ));
        // Without the models, the ZIP backup works.
        let mut zip = Cursor::new(Vec::new());
        store.backup_zip_with(&mut zip, ModelCopy::Exclude).unwrap();
    }
    let mut packed = Vec::new();
    assert!(matches!(
        store.export_packed(None, &mut packed),
        Err(PersistError::Corrupt { .. })
    ));
    assert!(
        store
            .fork(RecoveryTarget::Latest, temp.join("fork"))
            .is_err()
    );
    assert!(!temp.join("fork").exists(), "a failed fork leaves nothing");
    store.close().unwrap();
    let repair = Store::repair_dir(temp.join("s"), temp.join("repaired")).unwrap();
    assert!(repair.models.is_empty());
    assert_eq!(repair.models_not_carried.len(), 1, "{repair}");
    assert!(repair.to_string().contains("model not carried"));
}

#[test]
fn an_upgraded_store_keeps_its_models() {
    let temp = Temp::new("upgrade");
    // The v0.1.0 fixture with a model directory copied in (a 0.1.x store never has one, but the
    // copying upgrade carries every file).
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/v0.1.0/store");
    let old = temp.join("old");
    copy_dir(&fixtures, &old);
    let (donor, model) = store_with_model(&temp.join("donor"), 7);
    donor.close().unwrap();
    copy_dir(
        &temp.join("donor/models").join(&model.id),
        &old.join("models").join(&model.id),
    );
    Store::upgrade(&old, temp.join("new")).unwrap();
    assert_eq!(ids_in(temp.join("new").as_path()), vec![model.id.clone()]);
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn a_single_file_and_a_memory_store_keep_models_the_same_way() {
    let temp = Temp::new("backends");
    let single = Store::create(temp.join("x.gstore"), options()).unwrap();
    add_person(&single, "ann");
    let model = single.add_model(fake_model(1)).unwrap().model;
    single.checkpoint(None).unwrap();
    single.compact().unwrap();
    assert!(single.verify().unwrap().is_ok());
    single.close().unwrap();
    assert_eq!(
        ids_in(temp.join("x.gstore").as_path()),
        vec![model.id.clone()]
    );
    let memory = MemDir::new();
    let mem_store = Store::create(&memory, options()).unwrap();
    assert_eq!(
        mem_store.add_model(fake_model(1)).unwrap().model.id,
        model.id
    );
    assert!(memory.file_names().iter().any(|n| n.starts_with("models/")));
}

#[test]
fn hostile_model_files_never_panic() {
    let temp = Temp::new("hostile");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    let manifest = temp
        .join("s/models")
        .join(&model.id)
        .join("graphersal-embed-model.json");
    let original = fs::read(&manifest).unwrap();
    let texts: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"null".to_vec(),
        b"{\"files\": 7}".to_vec(),
        b"{\"files\": [{\"name\": \"../../GRAPH\", \"size\": 1, \"sha256\": \"00\"}]}".to_vec(),
        vec![0xff; 64],
        "[".repeat(10_000).into_bytes(),
        String::from_utf8_lossy(&original)
            .replace("\"size\": ", "\"size\": 9")
            .into_bytes(),
        original[..original.len() / 2].to_vec(),
    ];
    for text in texts {
        fs::write(&manifest, &text).unwrap();
        let listed = store.models().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].problem.is_some() || !listed[0].files.is_empty());
        assert!(!store.verify().unwrap().is_ok());
        let _ = store.read_model_file(&model.id, "config.json");
        assert!(store.backup(temp.join("b")).is_err());
        let mut packed = Vec::new();
        let _ = store.export_packed(None, &mut packed);
    }
    fs::write(&manifest, &original).unwrap();
    assert!(store.verify().unwrap().is_ok());

    // A packed snapshot whose model file differs from its manifest is refused on import and
    // leaves nothing behind.
    store.checkpoint(None).unwrap();
    let mut packed = Vec::new();
    store.export_packed(None, &mut packed).unwrap();
    // A flipped byte in a model file's section: the packed CRC check refuses it.
    let damaged = temp.join("from-damaged");
    let mut cut = packed.clone();
    let at = cut.len() - 200;
    cut[at] ^= 1;
    assert!(Store::create_from_packed(&damaged, cut.as_slice(), options()).is_err());
    assert!(!damaged.exists() || fs::read_dir(&damaged).unwrap().next().is_none());
}

#[test]
fn the_store_info_names_the_models_of_the_schema() {
    let temp = Temp::new("info");
    let (store, model) = store_with_model(&temp.join("s"), 1);
    let schema = GraphSchema::from_json(&format!(
        r#"{{"mode": "open", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{"emb": {{"type": "array", "format": "vector", "dimension": 4, "embedder": "{}"}}}}}}}}}}}}"#,
        model.identity
    ))
    .unwrap();
    store
        .graph()
        .write()
        .traversal_mut()
        .set_schema(schema)
        .unwrap();
    assert_eq!(
        store.info().unwrap().schema_models,
        vec![model.identity.clone()]
    );
    store.close().unwrap();
    let info = Store::inspect(temp.join("s")).unwrap();
    assert_eq!(info.schema_models, vec![model.identity.clone()]);
    assert_eq!(ids(&info.models), vec![model.id.clone()]);
}

/// The models of a schema are loaded at open (feature `model2vec`).
#[cfg(feature = "model2vec")]
mod auto_load {
    use super::*;
    use graphersal::vector::{Embedders, Model2Vec};
    use std::sync::Arc;

    const VOCAB: [&str; 4] = ["[UNK]", "graph", "vector", "model"];

    fn tokenizer_json() -> String {
        let entries: Vec<String> = VOCAB
            .iter()
            .enumerate()
            .map(|(id, token)| format!("{token:?}: {id}"))
            .collect();
        format!(
            r#"{{"version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
                "normalizer": {{"type": "Lowercase"}}, "pre_tokenizer": {{"type": "Whitespace"}},
                "post_processor": null, "decoder": null,
                "model": {{"type": "WordLevel", "vocab": {{{}}}, "unk_token": "[UNK]"}}}}"#,
            entries.join(", ")
        )
    }

    fn safetensors() -> Vec<u8> {
        let data: Vec<u8> = (0..VOCAB.len() * 3)
            .flat_map(|i| (1.0f32 + i as f32).to_le_bytes())
            .collect();
        let mut header = format!(
            r#"{{"embeddings": {{"dtype": "F32", "shape": [{}, 3], "data_offsets": [0, {}]}}}}"#,
            VOCAB.len(),
            data.len()
        );
        while header.len() % 8 != 0 {
            header.push(' ');
        }
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&data);
        out
    }

    /// The tiny model as a directory (with the manifest naming `repo`).
    fn model_dir(temp: &Temp, repo: &str) -> PathBuf {
        let dir = temp.join("model");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), r#"{"normalize": true}"#).unwrap();
        fs::write(dir.join("tokenizer.json"), tokenizer_json()).unwrap();
        fs::write(dir.join("model.safetensors"), safetensors()).unwrap();
        fs::write(
            dir.join(Model2Vec::MANIFEST_FILE),
            format!(r#"{{"format": 1, "repo": "{repo}", "files": []}}"#),
        )
        .unwrap();
        dir
    }

    fn new_model(dir: &Path, repo: &str) -> NewModel {
        let [config, tokenizer, weights] = Model2Vec::model_files(dir).unwrap();
        NewModel::new()
            .with_file(
                "config.json",
                ModelRole::Config,
                fs::File::open(config).unwrap(),
            )
            .with_file(
                "tokenizer.json",
                ModelRole::Tokenizer,
                fs::File::open(tokenizer).unwrap(),
            )
            .with_file(
                "model.safetensors",
                ModelRole::Weights,
                fs::File::open(weights).unwrap(),
            )
            .with_repo(repo)
            .with_license("mit")
    }

    fn schema_naming(identity: &str) -> GraphSchema {
        GraphSchema::from_json(&format!(
            r#"{{"mode": "open", "vertices": {{"doc": {{"schema": {{"type": "object", "properties": {{"emb": {{"type": "array", "format": "vector", "dimension": 3, "embedder": "{identity}"}}}}}}}}}}}}"#
        ))
        .unwrap()
    }

    fn set_schema(store: &Store, identity: &str) {
        store
            .graph()
            .write()
            .traversal_mut()
            .set_schema(schema_naming(identity))
            .unwrap();
    }

    #[test]
    fn a_stored_model_the_schema_names_is_loaded_at_open() {
        for single_file in [false, true] {
            let temp = Temp::new("autoload");
            let dir = model_dir(&temp, "acme/tiny");
            let identity = Model2Vec::model_identity(&dir).unwrap();
            let path = if single_file {
                temp.join("s.gstore")
            } else {
                temp.join("s")
            };
            let store = Store::create(&path, options()).unwrap();
            let stored = store.add_model(new_model(&dir, "acme/tiny")).unwrap().model;
            // The store's identity of the files is the loader's.
            assert_eq!(stored.identity, identity);
            set_schema(&store, &identity);
            store.close().unwrap();

            let store = Store::open(&path, options()).unwrap();
            let found = store.stored_embedders(&Embedders::default()).unwrap();
            assert!(found.notes.is_empty(), "{:?}", found.notes);
            assert_eq!(
                found.loaded,
                vec![("acme/tiny".to_string(), identity.clone())]
            );
            let embedder = found.embedders.by_model(&identity).unwrap();
            assert_eq!(embedder.info().name, "acme/tiny");
            assert_eq!(embedder.info().dimension, 3);
            // The default stays the registry's.
            assert_eq!(
                found.embedders.default_embedder().info().name,
                "hash-ngram/1"
            );
            // A registered embedder of the same model wins: nothing is loaded.
            let own = Model2Vec::load("mine", &dir).unwrap();
            let registered = Embedders::builder().with(Arc::new(own)).unwrap().build();
            let found = store.stored_embedders(&registered).unwrap();
            assert!(found.loaded.is_empty());
            assert_eq!(
                found.embedders.by_model(&identity).unwrap().info().name,
                "mine"
            );
        }
    }

    /// A stored model the schema does not name yet is registered too: the first
    /// `embed_property` with it needs it.
    #[test]
    fn every_stored_model_is_registered_at_open() {
        let temp = Temp::new("all");
        let dir = model_dir(&temp, "acme/tiny");
        let identity = Model2Vec::model_identity(&dir).unwrap();
        let store = Store::create(temp.join("s"), options()).unwrap();
        store.add_model(new_model(&dir, "acme/tiny")).unwrap();
        let found = store.stored_embedders(&Embedders::default()).unwrap();
        assert!(found.notes.is_empty(), "{:?}", found.notes);
        assert_eq!(
            found.loaded,
            vec![("acme/tiny".to_string(), identity.clone())]
        );
        assert_eq!(
            found.embedders.get("acme/tiny").unwrap().info().model,
            identity
        );
    }

    /// A single-file store the process may not write (file 0444 in a 0555 directory, as a
    /// container's read-only file or mount): `Store::open_read_only` loads it and
    /// `Store::stored_embedders_dir` registers its models, and neither changes a byte nor
    /// creates a file (no lock, no temporary file beside it).
    #[cfg(unix)]
    #[test]
    fn a_write_protected_single_file_store_loads_read_only_with_its_models() {
        use std::os::unix::fs::PermissionsExt;
        let temp = Temp::new("readonly");
        let dir = model_dir(&temp, "acme/tiny");
        let identity = Model2Vec::model_identity(&dir).unwrap();
        let folder = temp.join("ro");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("db.gstore");
        let store = Store::create(&path, options()).unwrap();
        store.add_model(new_model(&dir, "acme/tiny")).unwrap();
        set_schema(&store, &identity);
        add_person(&store, "ann");
        store.close().unwrap();

        let bytes = fs::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let result = std::panic::catch_unwind(|| {
            let recovered = Store::open_read_only(&path, RecoveryTarget::Latest).unwrap();
            assert_eq!(recovered.graph.vertex_count(), 1);
            let schema = recovered.graph.get_schema().unwrap();
            let found =
                Store::stored_embedders_dir(&path, schema.as_ref(), &Embedders::default()).unwrap();
            assert!(found.notes.is_empty(), "{:?}", found.notes);
            assert_eq!(
                found.loaded,
                vec![("acme/tiny".to_string(), identity.clone())]
            );
            assert_eq!(
                found
                    .embedders
                    .by_model(&identity)
                    .unwrap()
                    .info()
                    .dimension,
                3
            );
        });
        let names: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        assert_eq!(names, vec![std::ffi::OsString::from("db.gstore")]);
        assert!(fs::read(&path).unwrap() == bytes, "the store file changed");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn a_model_neither_registered_nor_stored_is_a_note_with_the_command() {
        let temp = Temp::new("missing");
        let store = Store::create(temp.join("s"), options()).unwrap();
        set_schema(&store, "model2vec/acme/gone@0123456789ab");
        let found = store.stored_embedders(&Embedders::default()).unwrap();
        assert!(found.loaded.is_empty());
        assert_eq!(found.notes.len(), 1);
        assert!(
            found.notes[0].contains("graphersal embed-model download acme/gone --into"),
            "{}",
            found.notes[0]
        );
        // A damaged stored model is a note too, never a failure.
        let dir = model_dir(&temp, "acme/tiny");
        let identity = Model2Vec::model_identity(&dir).unwrap();
        let stored = store.add_model(new_model(&dir, "acme/tiny")).unwrap().model;
        set_schema(&store, &identity);
        flip_byte(
            &temp
                .join("s/models")
                .join(&stored.id)
                .join("model.safetensors"),
        );
        let found = store.stored_embedders(&Embedders::default()).unwrap();
        assert!(found.loaded.is_empty());
        assert!(
            found.notes[0].contains("does not load"),
            "{:?}",
            found.notes
        );
    }
}
