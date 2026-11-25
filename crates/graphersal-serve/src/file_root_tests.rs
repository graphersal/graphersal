//! Tests of the file root ([`ContextBuilder::file_root`], plan
//! `ai/graphersal-server-access/PLAN.md` A6): with a root, every file path a client names
//! (`jobs/define`, `catalog/load`, the `dir` of `store/fork`, `store/attic/fork`,
//! `store/backup`) must resolve inside it; an absolute path elsewhere, a `..` escape and a
//! symbolic link pointing out are refused. Without a root nothing changes. Real session
//! threads, Stores in the temp directory, no HTTP library.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use graphersal::auth::{AccessRequest, Action, Resource};
use graphersal::catalog::{Definition, JobDefinition, JobTask, catalog_to_json};
use graphersal::persist::{Store, StoreOptions};
use graphersal::prelude::TraversalGraph;
use graphersal_session::{Session, SessionPresets};
use serde_json::{Value, json};

use crate::{
    Context, ContextBuilder, Exposure, HostCheck, Method, OpenStore, Request, Response,
    ServedSource, handle, launch, open_store,
};

const HOST: &str = "127.0.0.1:8080";

fn check() -> HostCheck {
    HostCheck::new(Exposure::Loopback, 8080, &format!("http://{HOST}/"))
}

fn get<'a>(url: &str) -> Request<'a> {
    Request::new(Method::Get, url, Cursor::new(&b""[..]))
        .with_header("Host", HOST)
        .with_header("X-Graphersal-Client", "tab-1")
}

fn post<'a>(url: &str, body: &'a [u8]) -> Request<'a> {
    Request::new(Method::Post, url, Cursor::new(body))
        .with_header("Host", HOST)
        .with_header("X-Graphersal-Client", "tab-1")
        .with_header("Content-Type", "application/json")
        .with_body_length(Some(body.len()))
}

fn text(response: &Response) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn body_json(response: &Response) -> Value {
    serde_json::from_slice(response.body()).expect("a JSON answer")
}

fn call(context: &Arc<Context>, url: &str, body: &Value) -> Response {
    let bytes = body.to_string().into_bytes();
    handle(context, post(url, &bytes))
}

fn assert_ok(answer: &Response, what: &str) -> Value {
    assert_eq!(answer.status(), 200, "{what}: {}", text(answer));
    body_json(answer)
}

fn assert_outside(answer: &Response, what: &str) {
    assert_eq!(answer.status(), 403, "{what}: {}", text(answer));
    let error = body_json(answer)["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        error.contains("paths must lie inside the server's file root"),
        "{what}: {error}"
    );
}

/// The layout of one test: `base/data` (the store), `base/root` (the file root, with
/// `root/out` a symbolic link to `base/outside`), `base/outside`.
struct Layout {
    base: PathBuf,
    store: PathBuf,
    root: PathBuf,
    outside: PathBuf,
}

impl Layout {
    fn new(name: &str) -> Layout {
        let base = std::env::temp_dir().join(format!(
            "graphersal_serve_file_root_{}_{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).expect("the root");
        std::fs::create_dir_all(&outside).expect("the outside directory");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("out")).expect("the symbolic link");
        // Canonical paths, so comparisons hold where the temp directory is a link (macOS).
        let base = std::fs::canonicalize(&base).expect("canonical base");
        Layout {
            store: base.join("data"),
            root: base.join("root"),
            outside: base.join("outside"),
            base,
        }
    }

    /// The escaping `dir`s: absolute elsewhere, `..`, and (on Unix) the symbolic link.
    fn escapes(&self, leaf: &str) -> Vec<String> {
        let mut escapes = vec![
            self.outside.join(leaf).display().to_string(),
            Path::new("..")
                .join("outside")
                .join(leaf)
                .display()
                .to_string(),
            self.root
                .join("..")
                .join("outside")
                .join(leaf)
                .display()
                .to_string(),
        ];
        if cfg!(unix) {
            escapes.push(Path::new("out").join(leaf).display().to_string());
        }
        escapes
    }

    /// Whether nothing was written outside the root.
    fn outside_is_empty(&self) -> bool {
        std::fs::read_dir(&self.outside)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A context over the modern sample in a Store at `dir`, with a mark `start`.
fn context_at(dir: &Path, root: Option<PathBuf>) -> Arc<Context> {
    Store::create_with(dir, TraversalGraph::tinkerpop_modern(), StoreOptions::new())
        .expect("the store is created")
        .close()
        .expect("the store closes");
    let store = open_store(dir, OpenStore::new())
        .expect("the store opens")
        .store;
    let (launched, ()) = launch(
        ServedSource::Store(store),
        SessionPresets::new(),
        |_: &mut Session| Ok::<_, String>(()),
        None,
    )
    .expect("the launch");
    let builder: ContextBuilder = launched.context_builder(check()).file_root(root);
    let context = builder.build();
    assert_ok(
        &call(&context, "/api/store/mark", &json!({"name": "start"})),
        "mark",
    );
    context
}

fn backup_job(path: &str) -> Value {
    json!({"name": "b", "schedule": "0 3 1 1 *", "action": "backup", "path": path})
}

/// A catalog text with one backup job `b` whose stored path (and file effect) is `path`.
fn catalog_with_backup(path: &Path) -> String {
    let job = JobDefinition::new(
        "0 3 1 1 *",
        JobTask::Backup {
            path: path.display().to_string(),
        },
    )
    .with_effects(vec![
        AccessRequest::new(Action::Create, Resource::File { path }).to_owned_request(),
    ]);
    catalog_to_json([&Definition::job("b", job)])
}

fn load_catalog(context: &Arc<Context>, catalog: &str) -> Response {
    handle(
        context,
        post("/api/catalog/load", catalog.as_bytes()).with_body_length(Some(catalog.len())),
    )
}

fn has_job(context: &Arc<Context>) -> bool {
    let jobs = body_json(&handle(context, get("/api/jobs")));
    jobs.to_string().contains("\"b\"")
}

#[test]
fn jobs_define_resolves_backup_paths_under_the_root() {
    let layout = Layout::new("jobs");
    let context = context_at(&layout.store, Some(layout.root.clone()));
    for escape in layout.escapes("b") {
        let answer = assert_ok(
            &call(&context, "/api/jobs/define", &backup_job(&escape)),
            &escape,
        );
        assert_eq!(answer["ok"], false, "{escape}: {answer}");
        assert!(
            answer.to_string().contains("outside the file root"),
            "{answer}"
        );
        assert!(!has_job(&context), "{escape}");
    }
    let inside = layout.root.join("abs").display().to_string();
    for path in ["rel", inside.as_str()] {
        let answer = assert_ok(&call(&context, "/api/jobs/define", &backup_job(path)), path);
        assert_eq!(answer["ok"], true, "{path}: {answer}");
    }
    // The stored file effect is the resolved path inside the root.
    let catalog = text(&handle(&context, get("/api/catalog/export")));
    assert!(
        catalog.contains(
            &layout
                .root
                .join("abs")
                .display()
                .to_string()
                .replace('\\', "\\\\")
        ),
        "{catalog}"
    );
    drop(context);
}

#[test]
fn catalog_load_refuses_jobs_writing_outside_the_root() {
    let layout = Layout::new("catalog");
    let context = context_at(&layout.store, Some(layout.root.clone()));
    let mut escapes = vec![
        layout.outside.join("b"),
        layout.root.join("..").join("outside").join("b"),
        PathBuf::from("rel"),
    ];
    if cfg!(unix) {
        escapes.push(layout.root.join("out").join("b"));
    }
    for escape in escapes {
        let answer = load_catalog(&context, &catalog_with_backup(&escape));
        assert_outside(&answer, &escape.display().to_string());
        assert!(
            text(&answer).contains("nothing was loaded"),
            "{}",
            text(&answer)
        );
        assert!(!has_job(&context), "{}", escape.display());
    }
    let answer = load_catalog(&context, &catalog_with_backup(&layout.root.join("b")));
    assert_ok(&answer, "inside");
    assert!(has_job(&context));
    // A catalog that does not parse is still the load's own 4xx, not a file-root refusal.
    let broken = load_catalog(&context, "{");
    assert_ne!(broken.status(), 200);
    assert_ne!(broken.status(), 403, "{}", text(&broken));
    drop(context);
}

#[test]
fn fork_and_backup_dirs_must_lie_inside_the_root() {
    let layout = Layout::new("store");
    let context = context_at(&layout.store, Some(layout.root.clone()));
    // An attic entry to fork: a commit, then a rollback to the mark.
    let added = call(
        &context,
        "/api/execute",
        &json!({"script": "g.add_v(\"thing\").iterate()"}),
    );
    assert_ok(&added, "add");
    let rolled = assert_ok(
        &call(
            &context,
            "/api/store/rollback",
            &json!({"target": {"mark": "start"}}),
        ),
        "rollback",
    );
    let attic = rolled["rollback"]["attic"]
        .as_str()
        .unwrap_or_else(|| panic!("an attic entry: {rolled}"))
        .to_string();
    let routes = [
        ("/api/store/fork", json!({"target": {"mark": "start"}})),
        ("/api/store/attic/fork", json!({"id": attic})),
        ("/api/store/backup", json!({})),
    ];
    for (n, (url, body)) in routes.iter().enumerate() {
        for escape in layout.escapes(&format!("t{n}")) {
            let mut body = body.clone();
            body["dir"] = json!(escape);
            assert_outside(&call(&context, url, &body), &format!("{url} {escape}"));
        }
        // A relative dir is resolved under the root, an absolute one inside it is accepted.
        let relative = format!("rel{n}");
        let absolute = layout.root.join(format!("abs{n}"));
        for dir in [relative.clone(), absolute.display().to_string()] {
            let mut body = body.clone();
            body["dir"] = json!(dir);
            assert_ok(&call(&context, url, &body), &format!("{url} {dir}"));
        }
        assert!(layout.root.join(&relative).exists(), "{url}");
        assert!(absolute.exists(), "{url}");
    }
    assert!(layout.outside_is_empty());
    // The default directory next to the store is unchanged, outside the root as it is.
    assert_ok(
        &call(&context, "/api/store/backup", &json!({})),
        "default backup",
    );
    assert!(layout.base.join("data-backup").is_dir());
    assert_ok(
        &call(
            &context,
            "/api/store/fork",
            &json!({"target": {"mark": "start"}}),
        ),
        "default fork",
    );
    assert!(layout.base.join("data-fork-mark-start").is_dir());
    drop(context);
}

#[test]
fn a_missing_root_refuses_a_requested_dir() {
    let layout = Layout::new("missing");
    let context = context_at(&layout.store, Some(layout.base.join("nowhere")));
    let answer = call(
        &context,
        "/api/store/backup",
        &json!({"dir": layout.base.join("nowhere").join("b").display().to_string()}),
    );
    assert_eq!(answer.status(), 500, "{}", text(&answer));
    assert!(text(&answer).contains("file root"), "{}", text(&answer));
    drop(context);
}

#[test]
fn without_a_root_every_path_is_accepted_as_before() {
    let layout = Layout::new("none");
    let context = context_at(&layout.store, None);
    assert_eq!(context.file_root(), None);
    let outside = layout.outside.join("job").display().to_string();
    let answer = assert_ok(
        &call(&context, "/api/jobs/define", &backup_job(&outside)),
        "define",
    );
    assert_eq!(answer["ok"], true, "{answer}");
    let answer = load_catalog(
        &context,
        &catalog_with_backup(&layout.outside.join("loaded")),
    );
    assert_ok(&answer, "catalog");
    let dir = layout.outside.join("fork").display().to_string();
    assert_ok(
        &call(
            &context,
            "/api/store/fork",
            &json!({"target": {"mark": "start"}, "dir": dir}),
        ),
        "fork",
    );
    assert!(layout.outside.join("fork").is_dir());
    // A relative dir is next to the store, not under any root.
    assert_ok(
        &call(&context, "/api/store/backup", &json!({"dir": "bk"})),
        "backup",
    );
    assert!(layout.base.join("bk").is_dir());
    drop(context);
}
