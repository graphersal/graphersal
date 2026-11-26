//! The Hugging Face download of Model2Vec models (feature `model-download`,
//! `persist::ModelCache::download`): against a fake endpoint on 127.0.0.1 serving the TINY
//! synthetic model of `model_fixture.rs`, never the network. Success, the events, the token
//! sent to the endpoint's host only, the failures (each leaves no model), cancellation, and a
//! staged download imported into a Store without touching the cache.

#![cfg(all(feature = "model-download", not(target_arch = "wasm32")))]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use graphersal::exec::CancelToken;
use graphersal::persist::{
    DownloadEvent, ImportModelOptions, ModelCache, ModelDownload, ModelDownloadError, ModelRole,
    ModelSource, Store,
};

use crate::model_fixture::{CONFIG, Temp, options, safetensors, tokenizer_json};

// ---------------------------------------------------------------------------------------------
// A fake Hugging Face endpoint
// ---------------------------------------------------------------------------------------------

/// One answer: status, headers, body, and how many body bytes are really sent (fewer than
/// `Content-Length`: a broken-off download).
#[derive(Clone)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    sent: Option<usize>,
}

impl Answer {
    fn ok(body: Vec<u8>) -> Self {
        Answer {
            status: 200,
            headers: Vec::new(),
            body,
            sent: None,
        }
    }

    fn status(status: u16) -> Self {
        Answer {
            status,
            headers: Vec::new(),
            body: b"no".to_vec(),
            sent: None,
        }
    }

    fn redirect(status: u16, location: &str) -> Self {
        Answer::status(status).header("Location", location)
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

type Routes = HashMap<String, Answer>;

/// The path and the Authorization header of every request the endpoint saw.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A minimal HTTP/1.1 server (one request per connection) answering by path.
struct Endpoint {
    port: u16,
    seen: Seen,
}

impl Endpoint {
    fn start(make: impl FnOnce(u16) -> Routes) -> Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = make(port);
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Ok(clone) = stream.try_clone() else {
                    continue;
                };
                let mut reader = BufReader::new(clone);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("");
                let path = path.split('?').next().unwrap_or("").to_string();
                let mut auth = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("authorization")
                    {
                        auth = Some(value.trim().to_string());
                    }
                }
                log.lock().unwrap().push((path.clone(), auth));
                let answer = routes.get(&path).cloned().unwrap_or(Answer {
                    status: 404,
                    headers: Vec::new(),
                    body: b"not found".to_vec(),
                    sent: None,
                });
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    answer.status,
                    answer.body.len()
                );
                for (name, value) in &answer.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let sent = answer.sent.unwrap_or(answer.body.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&answer.body[..sent]);
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        Endpoint { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    fn paths(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn git_blob_id(bytes: &[u8]) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    context.update(format!("blob {}\0", bytes.len()).as_bytes());
    context.update(bytes);
    hex(context.finish().as_ref())
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The repository `acme/tiny` at `main`, the Hugging Face way: config.json directly (its git
/// blob id as the ETag), tokenizer.json through a relative 307, the weights through a 302 to
/// another host (`localhost`) with the stated SHA-256; a card, a LICENSE and the metadata
/// (licence apache-2.0, a base model, not gated).
fn routes(port: u16, weights: Vec<u8>, stated_sha: &str) -> Routes {
    let config = CONFIG.as_bytes().to_vec();
    let tokenizer = tokenizer_json().into_bytes();
    let base = "/acme/tiny/resolve/main";
    let mut routes = HashMap::new();
    routes.insert(
        format!("{base}/config.json"),
        Answer::ok(config.clone())
            .header("ETag", &format!("\"{}\"", git_blob_id(&config)))
            .header("X-Repo-Commit", COMMIT),
    );
    routes.insert(
        format!("{base}/tokenizer.json"),
        Answer::redirect(
            307,
            "/api/resolve-cache/acme/tiny/c0ffee/tokenizer.json?x=1",
        )
        .header("X-Linked-Etag", &format!("\"{}\"", git_blob_id(&tokenizer))),
    );
    routes.insert(
        "/api/resolve-cache/acme/tiny/c0ffee/tokenizer.json".to_string(),
        Answer::ok(tokenizer),
    );
    routes.insert(
        format!("{base}/model.safetensors"),
        Answer::redirect(
            302,
            &format!("http://localhost:{port}/cdn/model.safetensors?sig=abc"),
        )
        .header("X-Linked-Etag", &format!("\"{stated_sha}\""))
        .header("X-Linked-Size", &weights.len().to_string()),
    );
    routes.insert(
        "/cdn/model.safetensors".to_string(),
        Answer::ok(weights).header("ETag", "\"cdn-etag\""),
    );
    routes.insert(
        format!("{base}/README.md"),
        Answer::ok(b"---\nlicense: apache-2.0\n---\n# tiny\n".to_vec()),
    );
    routes.insert(
        format!("{base}/LICENSE"),
        Answer::ok(b"Apache License 2.0 (test text)\n".to_vec()),
    );
    routes.insert(
        "/api/models/acme/tiny/revision/main".to_string(),
        Answer::ok(
            br#"{"cardData": {"license": "apache-2.0", "base_model": "acme/base"}, "gated": false,
                "siblings": [{"rfilename": "config.json"}, {"rfilename": "LICENSE"}]}"#
                .to_vec(),
        ),
    );
    routes.insert(
        "/api/models/acme/base".to_string(),
        Answer::ok(br#"{"cardData": {"license": "mit"}}"#.to_vec()),
    );
    routes
}

fn good(port: u16) -> Routes {
    let weights = safetensors(1);
    let sha = sha256(&weights);
    routes(port, weights, &sha)
}

/// Hidden entries (staging) and `.part` files anywhere under `dir`.
fn leftovers(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.ends_with(".part") {
                found.push(entry.path().display().to_string());
            }
            if entry.path().is_dir() {
                stack.push(entry.path());
            }
        }
    }
    found
}

/// The events of a download as text lines (`started`, `done <file>`, `note`).
fn record(events: &mut Vec<String>) -> impl FnMut(&DownloadEvent<'_>) + '_ {
    move |event| match *event {
        DownloadEvent::Started {
            repo,
            revision,
            endpoint,
            ..
        } => events.push(format!("started {repo} {revision} {endpoint}")),
        DownloadEvent::FileDone { name, bytes, .. } => events.push(format!("done {name} {bytes}")),
        DownloadEvent::Note { text, .. } => events.push(format!("note {text}")),
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
fn a_download_installs_the_checked_model_with_its_manifest() {
    let endpoint = Endpoint::start(good);
    let temp = Temp::new("download");
    let cache = ModelCache::at(temp.join("models"));
    let request = ModelDownload::new("acme/tiny")
        .with_endpoint(endpoint.url())
        .with_token("secret-token");
    let mut events = Vec::new();
    let mut progress = 0usize;
    let report = cache
        .download(&request, |event| {
            if matches!(event, DownloadEvent::Progress { .. }) {
                progress += 1;
            }
            record(&mut events)(event);
        })
        .unwrap();

    let dir = cache.path_of("acme/tiny");
    assert_eq!(report.dir, dir);
    assert!(
        report.identity.starts_with("model2vec/acme/tiny@"),
        "{}",
        report.identity
    );
    assert_eq!(report.dimension, Some(3));
    assert_eq!(report.commit.as_deref(), Some(COMMIT));
    assert_eq!(report.license, "apache-2.0");
    assert!(report.license_file);
    assert!(!report.gated);
    assert_eq!(report.base_models.len(), 1);
    assert_eq!(report.base_models[0].repo, "acme/base");
    assert_eq!(report.base_models[0].license.as_deref(), Some("mit"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    let names: Vec<(&str, ModelRole)> = report
        .files
        .iter()
        .map(|f| (f.name.as_str(), f.role))
        .collect();
    assert_eq!(
        names,
        vec![
            ("config.json", ModelRole::Config),
            ("tokenizer.json", ModelRole::Tokenizer),
            ("model.safetensors", ModelRole::Weights),
            ("README.md", ModelRole::Card),
            ("LICENSE", ModelRole::License),
        ]
    );
    let weights = safetensors(1);
    assert_eq!(report.files[2].sha256, sha256(&weights));
    assert_eq!(report.files[2].size, weights.len() as u64);
    assert_eq!(
        std::fs::read(dir.join("model.safetensors")).unwrap(),
        weights
    );

    // The manifest (format 2) and the identity a list computes from the installed files.
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(ModelCache::MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(manifest["format"], 2);
    assert_eq!(manifest["repo"], "acme/tiny");
    assert_eq!(manifest["commit"], COMMIT);
    assert_eq!(manifest["license"], "apache-2.0");
    assert_eq!(manifest["license_text"], "file");
    assert_eq!(manifest["base_models"][0]["license"], "mit");
    assert_eq!(manifest["files"][4]["role"], "license");
    assert!(manifest["downloaded_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(ModelCache::identity(&dir).unwrap(), report.identity);
    assert!(leftovers(temp.join("models").as_path()).is_empty());

    // The events: started first (the endpoint without its trailing slash), one done per file.
    let trimmed = endpoint.url().trim_end_matches('/').to_string();
    assert_eq!(events[0], format!("started acme/tiny main {trimmed}"));
    assert_eq!(
        events[1..]
            .iter()
            .map(|e| e.split(' ').nth(1).unwrap())
            .collect::<Vec<_>>(),
        vec![
            "config.json",
            "tokenizer.json",
            "model.safetensors",
            "README.md",
            "LICENSE"
        ]
    );
    assert!(progress >= 5);

    // The token went to the endpoint's host only, never to the redirect's other host.
    let seen = endpoint.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|(path, _)| path.starts_with("/cdn/")));
    for (path, auth) in &seen {
        if path.starts_with("/cdn/") {
            assert_eq!(auth, &None, "{path}");
        } else {
            assert_eq!(auth.as_deref(), Some("Bearer secret-token"), "{path}");
        }
    }

    // A second download replaces the first one.
    let again = cache.download(&request, |_| {}).unwrap();
    assert_eq!(again.identity, report.identity);
    assert!(leftovers(temp.join("models").as_path()).is_empty());
}

#[test]
fn a_failed_download_leaves_no_model() {
    type Make = Box<dyn FnOnce(u16) -> Routes>;
    type Check = fn(&ModelDownloadError) -> bool;
    let weights = safetensors(1);
    let sha = sha256(&weights);
    let cases: Vec<(&str, Make, Check, &str)> = vec![
        (
            "sha256 mismatch",
            Box::new({
                let weights = weights.clone();
                move |port| routes(port, weights, &"0".repeat(64))
            }),
            |e| matches!(e, ModelDownloadError::Integrity { .. }),
            "nothing was kept",
        ),
        (
            "git blob id mismatch",
            Box::new(|port| {
                let mut routes = good(port);
                routes.insert(
                    "/acme/tiny/resolve/main/config.json".to_string(),
                    Answer::ok(CONFIG.as_bytes().to_vec())
                        .header("ETag", &format!("\"{}\"", "1".repeat(40))),
                );
                routes
            }),
            |e| matches!(e, ModelDownloadError::Integrity { .. }),
            "git blob id",
        ),
        (
            "broken off",
            Box::new({
                let (weights, sha) = (weights.clone(), sha.clone());
                move |port| {
                    let mut routes = routes(port, weights.clone(), &sha);
                    let mut cut = Answer::ok(weights.clone());
                    cut.sent = Some(weights.len() / 2);
                    routes.insert("/cdn/model.safetensors".to_string(), cut);
                    routes
                }
            }),
            |e| matches!(e, ModelDownloadError::Transfer { .. }),
            "broke off",
        ),
        (
            "missing file",
            Box::new(|port| {
                let mut routes = good(port);
                routes.remove("/acme/tiny/resolve/main/model.safetensors");
                routes
            }),
            |e| matches!(e, ModelDownloadError::NotFound { .. }),
            "has no model.safetensors",
        ),
        (
            "licence file listed but not served",
            Box::new(|port| {
                let mut routes = good(port);
                routes.remove("/acme/tiny/resolve/main/LICENSE");
                routes
            }),
            |e| matches!(e, ModelDownloadError::NotFound { .. }),
            "has no LICENSE (HTTP 404)",
        ),
        (
            "not a model",
            Box::new(|port| {
                let garbage = b"not a safetensors file".to_vec();
                let sha = sha256(&garbage);
                routes(port, garbage, &sha)
            }),
            |e| matches!(e, ModelDownloadError::NotAModel { .. }),
            "does not load as a Model2Vec model",
        ),
        (
            "refused",
            Box::new(|_| {
                HashMap::from([(
                    "/acme/tiny/resolve/main/config.json".to_string(),
                    Answer::status(401),
                )])
            }),
            |e| matches!(e, ModelDownloadError::Refused { status: 401, .. }),
            "HF_TOKEN",
        ),
        (
            "server error",
            Box::new(|_| {
                HashMap::from([(
                    "/acme/tiny/resolve/main/config.json".to_string(),
                    Answer::status(500),
                )])
            }),
            |e| {
                matches!(
                    e,
                    ModelDownloadError::Transfer {
                        status: Some(500),
                        ..
                    }
                )
            },
            "HTTP 500",
        ),
    ];
    for (what, make, check, needle) in cases {
        let endpoint = Endpoint::start(make);
        let temp = Temp::new("download-failures");
        let cache = ModelCache::at(temp.join("models"));
        let request = ModelDownload::new("acme/tiny").with_endpoint(endpoint.url());
        let err = cache.download(&request, |_| {}).unwrap_err();
        assert!(check(&err), "{what}: {err:?}");
        assert!(err.to_string().contains(needle), "{what}: {err}");
        assert!(
            !cache.path_of("acme/tiny").exists(),
            "{what}: no model left"
        );
        assert!(
            leftovers(temp.join("models").as_path()).is_empty(),
            "{what}: {:?}",
            leftovers(temp.join("models").as_path())
        );
    }
}

#[test]
fn an_invalid_request_fetches_nothing() {
    let endpoint = Endpoint::start(good);
    let temp = Temp::new("download-invalid");
    let cache = ModelCache::at(temp.join("models"));
    for request in [
        ModelDownload::new("../../etc"),
        ModelDownload::new("a/b/c"),
        ModelDownload::new("acme/tiny").with_revision("../x"),
        ModelDownload::new("acme/tiny").with_revision(""),
    ] {
        let request = request.with_endpoint(endpoint.url());
        let err = cache.download(&request, |_| {}).unwrap_err();
        assert!(
            matches!(err, ModelDownloadError::InvalidRequest { .. }),
            "{err:?}"
        );
    }
    assert!(endpoint.paths().is_empty(), "{:?}", endpoint.paths());
    assert!(!temp.join("models").exists());
}

#[test]
fn a_cancelled_download_stops_and_leaves_nothing() {
    // Cancelled before it starts: nothing fetched.
    let endpoint = Endpoint::start(good);
    let temp = Temp::new("download-cancel");
    let cache = ModelCache::at(temp.join("models"));
    let cancel = CancelToken::new();
    cancel.cancel();
    let request = ModelDownload::new("acme/tiny")
        .with_endpoint(endpoint.url())
        .with_cancel(cancel.clone());
    let err = cache.download(&request, |_| {}).unwrap_err();
    assert!(
        matches!(err, ModelDownloadError::Cancelled { .. }),
        "{err:?}"
    );
    assert!(endpoint.paths().is_empty());

    // Cancelled by the progress callback in the middle of the first file.
    cancel.reset();
    let err = cache
        .download(&request, |event| {
            if matches!(event, DownloadEvent::Progress { .. }) {
                cancel.cancel();
            }
        })
        .unwrap_err();
    assert!(
        matches!(err, ModelDownloadError::Cancelled { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("acme/tiny"), "{err}");
    assert_eq!(
        endpoint.paths(),
        vec!["/acme/tiny/resolve/main/config.json"]
    );
    assert!(!cache.path_of("acme/tiny").exists());
    assert!(leftovers(temp.join("models").as_path()).is_empty());
}

#[test]
fn unreadable_metadata_is_a_note_and_an_unknown_licence_a_warning() {
    let endpoint = Endpoint::start(|port| {
        let mut routes = good(port);
        routes.remove("/api/models/acme/tiny/revision/main");
        routes
    });
    let temp = Temp::new("download-metadata");
    let cache = ModelCache::at(temp.join("models"));
    let request = ModelDownload::new("acme/tiny").with_endpoint(endpoint.url());
    let mut events = Vec::new();
    let report = cache.download(&request, record(&mut events)).unwrap();
    assert_eq!(report.license, "unknown");
    assert!(!report.license_file, "no file list, no licence file");
    assert_eq!(report.notes.len(), 1);
    assert!(report.notes[0].contains("HTTP 404"), "{:?}", report.notes);
    // The note comes after the three model files and before the card.
    let note_at = events.iter().position(|e| e.starts_with("note ")).unwrap();
    assert_eq!(note_at, 4, "{events:?}");
    assert_eq!(report.warnings.len(), 1);
    assert!(
        report.warnings[0].contains("unknown"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn a_staged_download_goes_into_a_store_and_never_into_the_cache() {
    let endpoint = Endpoint::start(good);
    let temp = Temp::new("download-staged");
    let models = temp.join("models");
    let cache = ModelCache::at(&models);
    let request = ModelDownload::new("acme/tiny").with_endpoint(endpoint.url());
    let staged = cache.download_staged(&request, |_| {}).unwrap();
    assert!(staged.dir().join("model.safetensors").is_file());
    assert_eq!(staged.report().dir, staged.dir());
    assert!(!cache.path_of("acme/tiny").exists());

    let store_dir = temp.join("store");
    Store::create(&store_dir, options())
        .unwrap()
        .close()
        .unwrap();
    let report = Store::import_model_dir(
        &store_dir,
        ModelSource::Files(staged.new_model().unwrap()),
        ImportModelOptions::new(),
    )
    .unwrap();
    assert_eq!(report.model.identity, staged.report().identity);
    assert_eq!(report.model.license, "apache-2.0");
    drop(staged);
    assert!(!cache.path_of("acme/tiny").exists());
    assert!(leftovers(&models).is_empty(), "{:?}", leftovers(&models));
}

/// The blob of a model is its content address: two downloads of the same repository at the
/// same commit, at different times, give the SAME blob, whether the download goes into a store
/// straight away (`ModelSource::Files`) or through the models directory (`ModelSource::Dir` of
/// the installed download, as `embed-model import` does). The download time stays in the models
/// directory's manifest and never reaches the blob, so a store waiting for that blob (a missing
/// one, a `.gsnap` exported without it) is served by a fresh download.
#[test]
fn a_download_of_the_same_commit_gives_the_same_blob() {
    let endpoint = Endpoint::start(good);
    let temp = Temp::new("download-same-blob");
    let request = ModelDownload::new("acme/tiny").with_endpoint(endpoint.url());
    let new_store = |name: &str| {
        let dir = temp.join(name);
        Store::create(&dir, options()).unwrap().close().unwrap();
        dir
    };
    let import = |store: &Path, source: ModelSource| {
        Store::import_model_dir(store, source, ImportModelOptions::new())
            .unwrap()
            .model
    };

    // First download, straight into a store.
    let first = ModelCache::at(temp.join("models-1"))
        .download_staged(&request, |_| {})
        .unwrap();
    let one = new_store("store-1");
    let from_files = import(&one, ModelSource::Files(first.new_model().unwrap()));
    let first_manifest =
        std::fs::read_to_string(first.dir().join(ModelCache::MANIFEST_FILE)).unwrap();
    drop(first);

    // The second one a second later: into a store, then into the models directory and from
    // there into a third store.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = ModelCache::at(temp.join("models-2"))
        .download_staged(&request, |_| {})
        .unwrap();
    let two = new_store("store-2");
    let again = import(&two, ModelSource::Files(second.new_model().unwrap()));
    let installed = second.install().unwrap();
    let second_manifest =
        std::fs::read_to_string(installed.dir.join(ModelCache::MANIFEST_FILE)).unwrap();
    let three = new_store("store-3");
    let from_dir = import(&three, ModelSource::Dir(installed.dir.clone()));

    // The download times differ (the models directory's manifests) ...
    let downloaded_at = |text: &str| {
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        value["downloaded_at"].as_str().unwrap().to_string()
    };
    assert_ne!(
        downloaded_at(&first_manifest),
        downloaded_at(&second_manifest)
    );
    // ... the blobs do not.
    assert_eq!(from_files.commit.as_deref(), Some(COMMIT));
    assert_eq!(again.blob_hash, from_files.blob_hash);
    assert_eq!(from_dir.blob_hash, from_files.blob_hash);
    assert_eq!(from_dir.identity, from_files.identity);
    assert_eq!(from_dir.bytes, from_files.bytes);

    // The blob itself carries no time.
    let hash = &from_files.blob_hash;
    let blob = std::fs::read(one.join("blobs").join(&hash[..2]).join(hash)).unwrap();
    let text = String::from_utf8_lossy(&blob);
    assert!(
        !text.contains("downloaded_at"),
        "the blob names a download time"
    );
    assert!(!text.contains("added_at"), "the blob names an import time");
}

/// A host checks a request before it starts a download (`ModelDownload::validate`): the same
/// check, and the same error, as the download's own first step.
#[test]
fn a_host_validates_a_request_before_the_download() {
    let endpoint = Endpoint::start(good);
    let temp = Temp::new("download-validate");
    let cache = ModelCache::at(temp.join("models"));
    for ok in [
        ModelDownload::new("acme/tiny"),
        ModelDownload::new("bert-base-uncased"),
        ModelDownload::new("a.b/c_d-e").with_revision("refs/pr/1"),
        ModelDownload::new("acme/tiny").with_revision(COMMIT),
    ] {
        assert_eq!(ok.validate(), Ok(()), "{ok:?}");
        assert!(ModelDownload::is_repo_name(ok.repo()), "{ok:?}");
    }
    for bad in [
        ModelDownload::new(""),
        ModelDownload::new("../../etc"),
        ModelDownload::new("a/b/c"),
        ModelDownload::new("a/.hidden"),
        ModelDownload::new("a b/c"),
        ModelDownload::new("acme/tiny").with_revision("../x"),
        ModelDownload::new("acme/tiny").with_revision("/main"),
        ModelDownload::new("acme/tiny").with_revision("main branch"),
        ModelDownload::new("acme/tiny").with_revision(""),
    ] {
        let bad = bad.with_endpoint(endpoint.url());
        let refused = bad.validate().unwrap_err();
        assert!(
            matches!(refused, ModelDownloadError::InvalidRequest { .. }),
            "{refused:?}"
        );
        assert_eq!(
            ModelDownload::is_repo_name(bad.repo()),
            refused.to_string().contains("is not a revision"),
            "{refused}"
        );
        assert_eq!(cache.download(&bad, |_| {}).unwrap_err(), refused);
    }
    assert!(endpoint.paths().is_empty(), "{:?}", endpoint.paths());
    assert!(!temp.join("models").exists());
}
