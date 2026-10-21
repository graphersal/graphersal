//! Authorization (book page "Permissions"): everything is allowed by
//! default, in the Rust API and the script entry; a host plugs in an `Authorizer` (its own impl
//! or the ready-made `AccessPolicy`); one check per execution, before anything runs, on the plan
//! as written (child traversals included); file access limited to a host-configured root; a
//! graph the script built itself is private and writable; `Resource::Custom` serves an
//! embedding server.
//!
//! This module uses `graphersal::script` directly, NOT the trusted `script_host` helper.
#![cfg(feature = "script")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use graphersal::auth::AllowAll;
use graphersal::auth::{
    AccessPolicy, AccessRequest, AccessRequestBuf, Action, ActionKind, Authorizer, Denied,
    Resource, ResourceKind,
};
use graphersal::exec::RenderOptions;
use graphersal::prelude::*;
use graphersal::script::rhai::Dynamic;
use graphersal::script::{self, ScriptLimits};
use graphersal::storage::ElementKind;

fn modern() -> Arc<Graph> {
    Arc::new(GraphSource::tinkerpop_modern())
}

/// Runs `script` checked by `authorizer`.
fn run(
    graph: &Arc<Graph>,
    script: &str,
    authorizer: impl Authorizer + 'static,
) -> Result<Dynamic, String> {
    script::eval_value_with_limits(
        graph.clone(),
        script,
        Default::default(),
        &ScriptLimits::default(),
        Arc::new(authorizer),
    )
    .map_err(|err| err.to_string())
}

/// Asserts a `PermissionDenied` for `step` whose request renders starting with `request`
/// (`"Update Data"`, `"Create File("`).
fn assert_denied(result: Result<Dynamic, String>, step: &str, request: &str) {
    let err = result.expect_err("the script must be denied");
    let expected = format!("Permission denied for '{step}' ({request}");
    assert!(err.contains(&expected), "expected `{expected}` in:\n{err}");
}

fn count(graph: &Arc<Graph>) -> usize {
    graph.read().vertex_count()
}

/// A fresh, empty directory under the system temp dir (unique per test and process).
fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "graphersal-permissions-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    dir
}

fn forward(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// An authorizer that allows everything and records every request it was asked.
#[derive(Default)]
struct Recorder(Mutex<Vec<AccessRequestBuf>>);

impl Authorizer for Recorder {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        if let Ok(mut seen) = self.0.lock() {
            seen.push(request.to_owned_request());
        }
        Ok(())
    }
}

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|seen| seen.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }
}

mod default_allow_all {
    use super::*;

    #[test]
    fn the_script_entry_allows_everything_by_default() {
        let graph = modern();
        let n = script::eval_value(graph.clone(), r#"g.addV("x").next(); g.V().count().next()"#)
            .unwrap();
        assert_eq!(n.as_int().unwrap(), 7);
        let mut scope = script::graph_scope(graph.clone());
        let n = script::engine()
            .eval_with_scope::<Dynamic>(
                &mut scope,
                r#"g.with("evaluationTimeout", 0).V().drop().toList(); g.V().count().next()"#,
            )
            .unwrap();
        assert_eq!(n.as_int().unwrap(), 0);
        assert!(
            script::eval_value(
                graph,
                "g.statistics(); g.memory_usage(); g.get_schema(); g.infer_schema(); \
                 g.set_schema(#{mode: \"open\"}); g.patch_schema(#{mode: \"none\"})"
            )
            .is_ok()
        );
    }

    #[test]
    fn the_rust_api_allows_everything_unless_the_host_attaches_an_authorizer() {
        let graph = GraphSource::tinkerpop_modern();
        let mut g = graph.write();
        assert!(g.traversal_mut().add_v("trusted").to_list().is_ok());
        let err = g
            .traversal_mut()
            .with_authorizer(Arc::new(AccessPolicy::read_only()))
            .v(None::<()>)
            .union(vec![__::out(None::<()>), __::add_v("hidden")])
            .to_list()
            .unwrap_err();
        let TraverserError::PermissionDenied { step, request, .. } = err.root_cause() else {
            panic!("{err:?}");
        };
        assert_eq!(step, "add_v");
        assert_eq!(request.as_request().action, Action::Create);
        assert_eq!(
            request.as_request().resource,
            Resource::data(Some(ElementKind::Vertex), Some("hidden"))
        );
        // Located in the plan as written: step 1 (`union`), child 1, step 0.
        assert_eq!(err.step_location().expect("located").step_id, "1.1.0");
        assert_eq!(g.vertex_count(), 7);
        // Options set before `with_authorizer` are the host's presets.
        let preset = g
            .traversal_mut()
            .with("evaluationTimeout", 5000)
            .with_authorizer(Arc::new(AccessPolicy::read_only()))
            .v(None::<()>)
            .count()
            .to_list();
        assert!(preset.is_ok(), "{preset:?}");
        let after = g
            .traversal_mut()
            .with_authorizer(Arc::new(AccessPolicy::read_only()))
            .with("evaluationTimeout", 5000)
            .v(None::<()>)
            .count()
            .to_list()
            .unwrap_err();
        let TraverserError::PermissionDenied { request, .. } = &*after else {
            panic!("{after:?}");
        };
        assert_eq!(
            request.as_request(),
            AccessRequest::new(
                Action::Update,
                Resource::Option {
                    key: "evaluationTimeout"
                }
            )
        );
    }

    #[test]
    fn execute_reports_the_denial_in_its_error_without_throwing() {
        let graph = modern();
        let value = run(
            &graph,
            r#"let r = g.addV("x").execute(); r.error.message"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        let message = value.into_string().unwrap();
        assert!(
            message.contains(r#"Permission denied for 'add_v' (Create Data(vertex "x"))"#),
            "{message}"
        );
        assert_eq!(count(&graph), 6);
    }

    #[test]
    fn a_traversal_returned_without_a_terminal_is_checked_when_rendered() {
        let policy: Arc<dyn Authorizer> = Arc::new(AccessPolicy::deny_all());
        let mut scope = script::graph_scope_with_options(modern(), &[], policy.clone());
        let value = script::engine_with_limits(&ScriptLimits::default(), policy)
            .eval_with_scope::<Dynamic>(&mut scope, "g.V()")
            .unwrap();
        let rendered =
            script::try_render_value(value, VisualizeFormat::Auto, &RenderOptions::unlimited())
                .unwrap_err();
        assert!(rendered.contains("Permission denied"), "{rendered}");
    }
}

mod request_mapping {
    use super::*;

    /// Denies every write of `created` edges and of elements whose label the request leaves
    /// open (they could be `created` edges).
    struct ProtectCreated;

    impl Authorizer for ProtectCreated {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            let writes = matches!(
                request.action,
                Action::Create | Action::Update | Action::Delete
            );
            match request.resource {
                Resource::Data { element, label }
                    if writes
                        && element != Some(ElementKind::Vertex)
                        && label.is_none_or(|label| label == "created") =>
                {
                    Err(Denied::new("created edges are protected"))
                }
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn a_write_names_the_kind_and_edge_labels_the_plan_states_before_it() {
        let graph = modern();
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.e().has_label("knows").limit(1).property("w", 1).iterate();
               g.v().has_label("person").property("x", 1).iterate();
               g.v().values("name").drop().iterate();"#,
            recorder.clone(),
        )
        .unwrap();
        let seen = recorder.seen();
        assert!(
            seen.contains(&r#"Update Data(edge "knows")"#.to_string()),
            "{seen:?}"
        );
        // A vertex has a label set: has_label states the kind only.
        assert!(
            seen.contains(&"Update Data(vertex)".to_string()),
            "{seen:?}"
        );
        assert!(seen.contains(&"Delete Data".to_string()), "{seen:?}");
        // A policy protecting one edge label lets writes of other labelled edges through.
        let _ = run(
            &graph,
            r#"g.e().has_label("knows").drop().iterate()"#,
            ProtectCreated,
        )
        .unwrap();
        let err = run(&graph, r#"g.e().drop().iterate()"#, ProtectCreated).unwrap_err();
        assert!(err.contains("created edges are protected"), "{err}");
        // The denied step is located even though its request depends on the steps before it.
        assert!(err.contains("at #1: e().drop()"), "{err}");
        assert_eq!(
            run(&graph, "g.E().count().next()", AllowAll)
                .unwrap()
                .as_int()
                .unwrap(),
            4
        );
    }

    #[test]
    fn each_distinct_request_is_asked_once_in_plan_order() {
        let graph = modern();
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.V().outE("knows").inV().out().addV("person").property("x", 1)
                .addE("rel").to(__.V("1")).drop().toList()"#,
            recorder.clone(),
        )
        .unwrap();
        assert_eq!(
            recorder.seen(),
            [
                // `V()`, then `outE()`; `inV()`, `out()` and `__.V("1")` repeat these two
                "Read Data(vertex)",
                "Read Data(edge)",
                "Create Data(vertex \"person\")",
                // a write of the elements the steps before it state
                "Update Data(vertex \"person\")",
                "Create Data(edge \"rel\")",
                // the `to(..)` modulator asks nothing
                "Delete Data(edge \"rel\")",
            ]
        );
    }

    #[test]
    fn only_steps_that_reach_graph_data_ask_for_a_read() {
        let graph = modern();
        let recorder = Arc::new(Recorder::default());
        let names = run(
            &graph,
            r#"g.V().has("age", P.gt(30)).values("name").limit(2).toList()"#,
            recorder.clone(),
        )
        .unwrap();
        assert_eq!(names.into_array().unwrap().len(), 2);
        assert_eq!(recorder.seen(), ["Read Data(vertex)"]);
    }

    #[test]
    fn value_only_steps_need_no_permission() {
        let graph = modern();
        let sum = run(
            &graph,
            "g.inject(1, 2).sum().next()",
            AccessPolicy::deny_all(),
        )
        .unwrap();
        assert_eq!(sum.as_int().unwrap(), 3);
        let n = run(
            &graph,
            r#"g.inject(1).math("_ * 2").is(P.gt(1)).count().next()"#,
            AccessPolicy::deny_all(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 1);
        // A step that reaches the graph is still denied, also in the middle of a traversal.
        assert_denied(
            run(&graph, "g.V().toList()", AccessPolicy::deny_all()),
            "v",
            "Read Data(vertex)",
        );
        assert_denied(
            run(&graph, "g.inject(1).V().toList()", AccessPolicy::deny_all()),
            "v",
            "Read Data(vertex)",
        );
    }

    /// No live element handle survives a run: `next()` returns a detached copy of the vertex (a
    /// `Vertex` value). Reading the copy reads no graph data and asks nothing, also under
    /// `deny_all()`; passing it back (`g.inject(v)`) is covered by
    /// `injecting_an_element_value_asks_read_data`.
    #[test]
    fn a_held_element_is_a_materialized_copy_and_reading_it_asks_nothing() {
        let graph = modern();
        let mut scope = script::graph_scope(graph.clone());
        let v = script::engine()
            .eval_with_scope::<Dynamic>(&mut scope, r#"g.V("1").next()"#)
            .unwrap();
        assert!(v.is::<script::ScriptVertex>(), "{v:?}");
        let recorder = Arc::new(Recorder::default());
        let authorizer: Arc<dyn Authorizer> = recorder.clone();
        let mut scope = script::graph_scope_with_options(graph.clone(), &[], authorizer.clone());
        scope.push("v", v.clone());
        let n = script::engine_with_limits(&ScriptLimits::default(), authorizer)
            .eval_with_scope::<Dynamic>(&mut scope, "v.properties.len()")
            .unwrap();
        assert_eq!(n.as_int().unwrap(), 2);
        assert!(recorder.seen().is_empty(), "{:?}", recorder.seen());

        let denied: Arc<dyn Authorizer> = Arc::new(AccessPolicy::deny_all());
        let mut scope = script::graph_scope_with_options(graph, &[], denied.clone());
        scope.push("v", v);
        let name = script::engine_with_limits(&ScriptLimits::default(), denied)
            .eval_with_scope::<Dynamic>(&mut scope, "v.name")
            .unwrap();
        assert_eq!(name.into_string().unwrap(), "marko");
    }

    #[test]
    fn merges_schema_options_and_source_methods_have_their_requests() {
        let graph = modern();
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.with("repeat.max_loops", 5).mergeV(#{name: "zed"}).toList();
               g.V().inferSchema().next(); g.infer_schema(); g.statistics(); g.V().removeProperty("age").toList();"#,
            recorder.clone(),
        )
        .unwrap();
        assert_eq!(
            recorder.seen(),
            [
                "Update Option(\"repeat.max_loops\")",
                "Create Data(vertex)",
                "Update Data(vertex)",
                "Read Data(vertex)",
                "Read Schema",
                "Read Schema",
                "Read Data",
                "Read Data(vertex)",
                "Update Data(vertex)",
            ]
        );
    }
}

/// A role-based authorizer as a server would build it per connection, e.g. from the roles of a
/// JWT: the subject lives in the instance, the library only asks.
mod custom_authorizer {
    use super::*;

    #[derive(Clone, Copy)]
    enum Role {
        Viewer,
        Editor,
        Admin,
    }

    struct Session {
        role: Role,
    }

    impl Authorizer for Session {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            let allowed = match (self.role, request.action, request.resource) {
                (Role::Admin, _, _) => true,
                // Server-owned resources: everybody may open the shared database.
                (
                    _,
                    Action::Custom("open"),
                    Resource::Custom {
                        kind: "database",
                        name,
                    },
                ) => name == Some("shared"),
                (_, Action::Read, Resource::Data { .. } | Resource::Schema) => true,
                // Editors change data, except the `secret` label.
                (
                    Role::Editor,
                    Action::Create | Action::Update | Action::Delete,
                    Resource::Data { label, .. },
                ) => label != Some("secret"),
                _ => false,
            };
            if allowed {
                Ok(())
            } else {
                Err(Denied::new(format!("not allowed for this role: {request}")))
            }
        }
    }

    #[test]
    fn denies_by_action() {
        let graph = modern();
        let viewer = || Session { role: Role::Viewer };
        assert!(run(&graph, "g.V().count().next()", viewer()).is_ok());
        assert_denied(
            run(&graph, r#"g.V().property("x", 1).toList()"#, viewer()),
            "property",
            "Update Data",
        );
        assert_denied(
            run(&graph, "g.E().drop().toList()", viewer()),
            "drop",
            "Delete Data",
        );
        assert_eq!(graph.read().edge_count(), 6);
        let editor = Session { role: Role::Editor };
        assert!(run(&graph, r#"g.V().property("x", 1).toList()"#, editor).is_ok());
    }

    #[test]
    fn denies_by_resource_kind() {
        let graph = modern();
        let editor = || Session { role: Role::Editor };
        assert_denied(
            run(
                &graph,
                r#"g.with("evaluationTimeout", 0).V().toList()"#,
                editor(),
            ),
            "with(\"evaluationTimeout\")",
            "Update Option(\"evaluationTimeout\")",
        );
        #[cfg(feature = "io")]
        assert_denied(
            run(&graph, r#"g.export_graphml("x.graphml")"#, editor()),
            "export_graphml",
            "Create File(\"x.graphml\")",
        );
        let admin = Session { role: Role::Admin };
        assert!(
            run(
                &graph,
                r#"g.with("evaluationTimeout", 0).V().toList()"#,
                admin
            )
            .is_ok()
        );
    }

    #[test]
    fn denies_by_label() {
        let graph = modern();
        let editor = || Session { role: Role::Editor };
        assert!(run(&graph, r#"g.addV("public").next()"#, editor()).is_ok());
        let err = run(&graph, r#"g.addV("secret").next()"#, editor()).unwrap_err();
        assert!(
            err.contains(r#"Permission denied for 'add_v' (Create Data(vertex "secret")): not allowed for this role"#),
            "{err}"
        );
        assert_denied(
            run(
                &graph,
                r#"g.V("1").addE("secret").to(__.V("2")).next()"#,
                editor(),
            ),
            "add_e",
            "Create Data(edge \"secret\")",
        );
        // A multi-label vertex asks once per label.
        assert_denied(
            run(&graph, r#"g.addV(["public", "secret"]).next()"#, editor()),
            "add_v",
            "Create Data(vertex \"secret\")",
        );
        assert_eq!(count(&graph), 7);
    }

    #[test]
    fn a_server_checks_its_own_resources_with_the_same_authorizer() {
        let viewer = Session { role: Role::Viewer };
        let open = |name| {
            AccessRequest::new(
                Action::Custom("open"),
                Resource::Custom {
                    kind: "database",
                    name: Some(name),
                },
            )
        };
        assert!(viewer.authorize(&open("shared")).is_ok());
        let denied = viewer.authorize(&open("payroll")).unwrap_err();
        assert_eq!(
            denied.reason(),
            r#"not allowed for this role: open database("payroll")"#
        );
        assert!(
            Session { role: Role::Admin }
                .authorize(&open("payroll"))
                .is_ok()
        );
        // The ready-made policy decides custom requests by kind.
        assert!(AccessPolicy::allow_all().authorize(&open("x")).is_ok());
        assert!(
            AccessPolicy::allow_all()
                .deny_resource(ResourceKind::Custom)
                .authorize(&open("x"))
                .is_err()
        );
        assert!(
            AccessPolicy::allow_all()
                .deny(ActionKind::Custom, ResourceKind::Custom)
                .allows(ActionKind::Read, ResourceKind::Custom)
        );
    }
}

mod access_policy {
    use super::*;

    #[test]
    fn read_only_reads_and_cannot_write() {
        let graph = modern();
        let read = AccessPolicy::read_only;
        assert_eq!(
            run(&graph, "g.V().count().next()", read())
                .unwrap()
                .as_int()
                .unwrap(),
            6
        );
        assert_denied(
            run(&graph, r#"g.V().property("x", 1).toList()"#, read()),
            "property",
            "Update Data",
        );
        assert_denied(
            run(&graph, "g.V().drop().toList()", read()),
            "drop",
            "Delete Data",
        );
        assert_denied(
            run(&graph, r#"g.mergeV(#{name: "zed"}).toList()"#, read()),
            "merge_v",
            "Create Data(vertex)",
        );
        assert_denied(
            run(&graph, r#"g.V().removeProperty("age").toList()"#, read()),
            "remove_property",
            "Update Data",
        );
        assert_denied(
            run(&graph, r#"g.V().addLabel("x").toList()"#, read()),
            "add_label",
            "Update Data(vertex)",
        );
        assert_denied(
            run(&graph, r#"g.V().drop_label("person").toList()"#, read()),
            "drop_label",
            "Update Data(vertex)",
        );
        assert_denied(
            run(&graph, r#"g.E().set_label("x").toList()"#, read()),
            "set_label",
            "Update Data(edge)",
        );
        assert_eq!(count(&graph), 6);
        assert!(
            run(
                &graph,
                "g.statistics(); g.memory_usage(); g.get_schema(); g.infer_schema(); \
                 g.diff_schema(`{}`); g.validate_schema(`{}`); g.validate_schema_patch(`{}`)",
                read()
            )
            .is_ok()
        );
    }

    #[test]
    fn deny_all_denies_the_graph_but_not_plain_computation() {
        let graph = modern();
        let none = AccessPolicy::deny_all;
        assert_denied(
            run(&graph, "g.V().count().next()", none()),
            "v",
            "Read Data(vertex)",
        );
        assert_denied(
            run(&graph, "g.statistics()", none()),
            "statistics",
            "Read Data",
        );
        for (script, operation) in [
            ("g.get_schema()", "get_schema"),
            ("g.inferSchema()", "infer_schema"),
            ("g.diff_schema(`{}`)", "diff_schema"),
            ("g.validate_schema(`{}`)", "validate_schema"),
            (
                "g.validate_schema(`{}`, SchemaMode.closed)",
                "validate_schema",
            ),
            ("g.validateSchemaPatch(`{}`)", "validate_schema_patch"),
        ] {
            assert_denied(run(&graph, script, none()), operation, "Read Schema");
        }
        assert_eq!(run(&graph, "1 + 2", none()).unwrap().as_int().unwrap(), 3);
    }

    #[test]
    fn write_without_read_can_add_but_not_traverse() {
        let graph = modern();
        let write = || {
            AccessPolicy::deny_all()
                .allow(ActionKind::Create, ResourceKind::Data)
                .allow(ActionKind::Update, ResourceKind::Data)
                .allow(ActionKind::Delete, ResourceKind::Data)
        };
        assert!(run(&graph, r#"g.addV("x").property("k", 1).next()"#, write()).is_ok());
        assert_eq!(count(&graph), 7);
        assert_denied(
            run(&graph, "g.V().drop().toList()", write()),
            "v",
            "Read Data(vertex)",
        );
        assert_eq!(count(&graph), 7);
    }

    #[test]
    fn administrative_options_need_update_on_option_and_harmless_ones_do_not() {
        let graph = modern();
        for key in [
            r#""evaluationTimeout", 1000"#,
            r#""repeat.max_loops", 5"#,
            r#""bulk.max_expansion", 10"#,
            r#""traversal.max_traversers", 10"#,
            r#""traversal.max_value_depth", 10"#,
            r#""traversal.max_materialized_bytes", 10"#,
            r#""traversal.max_string_bytes", 10"#,
            r#""memory.limit", 10"#,
            r#""memory.headroom", 10"#,
            r#""optimizer.disabled", ["all"]"#,
            r#""path.analysis", false"#,
            r#""bulk.merge", false"#,
        ] {
            let name = key.split('"').nth(1).unwrap();
            assert_denied(
                run(
                    &graph,
                    &format!("g.with({key}).V().count().next()"),
                    AccessPolicy::read_only(),
                ),
                &format!("with(\"{name}\")"),
                &format!("Update Option(\"{name}\")"),
            );
        }
        for harmless in [
            r#""repeat.order", "dfs""#,
            r#""bulk.one", true"#,
            r#""random.seed", 42"#,
            r#""render.max_rows", 5"#,
            r#""render.spelling", "camel""#,
            r#""no.such.option", 1"#,
        ] {
            assert!(
                run(
                    &graph,
                    &format!("g.with({harmless}).V().count().next()"),
                    AccessPolicy::read_only()
                )
                .is_ok(),
                "{harmless}"
            );
        }
        let options = AccessPolicy::read_only().allow(ActionKind::Update, ResourceKind::Option);
        assert!(
            run(
                &graph,
                r#"g.with("evaluationTimeout", 1000).V().count().next()"#,
                options
            )
            .is_ok()
        );
    }

    #[test]
    fn a_host_preset_option_is_not_the_query_setting() {
        let graph = modern();
        let read: Arc<dyn Authorizer> = Arc::new(AccessPolicy::read_only());
        let timeout = ElementProperty::from(30_000);
        let engine = script::engine_with_limits(&ScriptLimits::default(), read.clone());
        let mut scope = script::graph_scope_with_options(
            graph.clone(),
            &[("evaluationTimeout", timeout)],
            read,
        );
        // The preset itself runs; setting it to the same value is no change either.
        let n = engine
            .eval_with_scope::<Dynamic>(&mut scope, "g.V().count().next()")
            .unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
        assert!(
            engine
                .eval_with_scope::<Dynamic>(
                    &mut scope,
                    r#"g.with("evaluationTimeout", 30000).V().count().next()"#
                )
                .is_ok()
        );
        // Raising (or removing) it is the query's own administrative setting.
        let err = engine
            .eval_with_scope::<Dynamic>(
                &mut scope,
                r#"g.with("evaluationTimeout", 0).V().count().next()"#,
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                r#"Permission denied for 'with("evaluationTimeout")' (Update Option("evaluationTimeout"))"#
            ),
            "{err}"
        );
    }
}

mod plan_as_written {
    use super::*;

    #[test]
    fn a_mutation_inside_a_child_traversal_is_denied_before_anything_runs() {
        let graph = modern();
        for (query, step, request) in [
            (
                r#"g.V().union(__.out(), __.addV("x")).toList()"#,
                "add_v",
                "Create Data(vertex \"x\")",
            ),
            (
                r#"g.V().sideEffect(__.property("seen", true)).toList()"#,
                "property",
                "Update Data",
            ),
            (
                r#"g.V().local(__.coalesce(__.out(), __.drop())).toList()"#,
                "drop",
                "Delete Data",
            ),
            (
                r#"g.V().group().by(__.addV("y")).toList()"#,
                "add_v",
                "Create Data(vertex \"y\")",
            ),
            (
                r#"g.V().repeat(__.out().property("hop", 1)).times(2).toList()"#,
                "property",
                "Update Data",
            ),
            (
                r#"g.V().where(__.out().addE("x").to(__.V("1"))).toList()"#,
                "add_e",
                "Create Data(edge \"x\")",
            ),
        ] {
            assert_denied(run(&graph, query, AccessPolicy::read_only()), step, request);
        }
        // Nothing ran: no property, no element was added.
        assert_eq!(count(&graph), 6);
        assert_eq!(graph.read().edge_count(), 6);
        assert_eq!(
            run(
                &graph,
                r#"g.V().has("seen").count().next()"#,
                AccessPolicy::read_only()
            )
            .unwrap()
            .as_int()
            .unwrap(),
            0
        );
    }

    #[test]
    fn optimizer_fusion_cannot_hide_a_step() {
        let graph = modern();
        // `add_property_fold` folds the `property()` steps into `add_v`, and `count_pushdown`
        // folds `count()` into the source step; the check sees the plan as written.
        let write = || {
            AccessPolicy::deny_all()
                .allow(ActionKind::Create, ResourceKind::Data)
                .allow(ActionKind::Update, ResourceKind::Data)
        };
        assert_denied(
            run(
                &graph,
                r#"g.addV("a").property("k", 1).id().toList()"#,
                AccessPolicy::deny_all().allow(ActionKind::Create, ResourceKind::Data),
            ),
            "property",
            "Update Data",
        );
        assert_denied(
            run(
                &graph,
                r#"g.V().hasLabel("person").count().next()"#,
                write(),
            ),
            "v",
            "Read Data(vertex)",
        );
        assert_eq!(count(&graph), 6);
        // The denied step is named as written, also when it only exists before optimization.
        assert_denied(
            run(
                &graph,
                r#"g.inject(1).addV("a").property("k", 1).toList()"#,
                AccessPolicy::read_only(),
            ),
            "add_v",
            "Create Data(vertex \"a\")",
        );
    }
}

mod private_graphs {
    use super::*;

    #[test]
    fn a_graph_the_script_builds_itself_is_writable_without_asking() {
        let graph = modern();
        let n = run(
            &graph,
            r#"let mine = GraphSource::empty();
               mine.addV("x").property("k", 1).next();
               __.addV("scratch").next();
               _g.addV("scratch").next();
               mine.V().count().next()"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 1);
        assert_eq!(count(&graph), 6);
    }

    #[test]
    fn mutating_a_subgraph_result_is_not_a_write_to_the_host_graph() {
        let graph = modern();
        let n = run(
            &graph,
            r#"let sg = g.E().hasLabel("knows").subgraph("sg").cap("sg").next();
               sg.addV("extra").next();
               sg.V().drop().toList();
               let tg = g.E().hasLabel("created").toGraph();
               tg.addV("more").next();
               tg.V().count().next()"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
        assert_eq!(count(&graph), 6);
        assert_eq!(graph.read().edge_count(), 6);
    }

    #[test]
    fn a_private_graph_is_not_readable_without_read() {
        let graph = modern();
        assert_denied(
            run(
                &graph,
                "GraphSource::tinkerpop_modern().V().count().next()",
                AccessPolicy::deny_all(),
            ),
            "v",
            "Read Data(vertex)",
        );
    }

    #[test]
    fn a_subgraph_into_the_host_graph_needs_create() {
        let host = Arc::new(GraphSource::empty());
        let query = r#"GraphSource::tinkerpop_modern().withSideEffect("sg", g).E().hasLabel("knows")
                       .subgraph("sg").cap("sg").next(); 1"#;
        assert_denied(
            run(&host, query, AccessPolicy::read_only()),
            "with_side_effect",
            "Create Data",
        );
        assert_eq!(host.read().edge_count(), 0);
        let write = AccessPolicy::read_only().allow(ActionKind::Create, ResourceKind::Data);
        let _ = run(&host, query, write).unwrap();
        assert_eq!(host.read().edge_count(), 2);
        // A private target needs nothing beyond the private graph's own relaxation.
        let private_target = r#"let t = GraphSource::empty();
            g.withSideEffect("sg", t).E().subgraph("sg").cap("sg").next(); t.E().count().next()"#;
        assert_eq!(
            run(&host, private_target, AccessPolicy::read_only())
                .unwrap()
                .as_int()
                .unwrap(),
            2
        );
    }
}

#[cfg(feature = "io")]
mod file_io {
    use super::*;

    fn files() -> AccessPolicy {
        AccessPolicy::read_only().allow_resource(ResourceKind::File)
    }

    #[test]
    fn export_and_import_ask_for_the_file() {
        let graph = modern();
        let dir = fresh_dir("needs-file-io");
        let path = forward(&dir.join("out.graphml"));
        assert_denied(
            run(
                &graph,
                &format!(r#"g.export_graphml("{path}")"#),
                AccessPolicy::read_only(),
            ),
            "export_graphml",
            "Create File(",
        );
        assert!(!dir.join("out.graphml").exists());
        let _ = run(&graph, &format!(r#"g.export_graphml("{path}")"#), files()).unwrap();
        assert!(dir.join("out.graphml").exists());
        // Importing writes into the graph: file access alone is not enough.
        let target = Arc::new(GraphSource::empty());
        assert_denied(
            run(&target, &format!(r#"g.import_graphml("{path}")"#), files()),
            "import_graphml",
            "Create Data",
        );
        let all = files().allow(ActionKind::Create, ResourceKind::Data);
        let _ = run(&target, &format!(r#"g.import_graphml("{path}")"#), all).unwrap();
        assert_eq!(count(&target), 6);
        // GraphSource::file reads a file into a private graph.
        assert_denied(
            run(
                &graph,
                &format!(r#"GraphSource::file("{path}").V().count().next()"#),
                AccessPolicy::read_only(),
            ),
            "GraphSource::file",
            "Read File(",
        );
        let n = run(
            &graph,
            &format!(r#"GraphSource::file("{path}").V().count().next()"#),
            files(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "persist")]
    #[test]
    fn snapshot_export_asks_for_the_file_and_mark_for_update() {
        let graph = modern();
        let dir = fresh_dir("snapshot-needs-file-io");
        let path = forward(&dir.join("g.gsnap"));
        for spelling in ["export_snapshot", "exportSnapshot"] {
            assert_denied(
                run(
                    &graph,
                    &format!(r#"g.{spelling}("{path}")"#),
                    AccessPolicy::read_only(),
                ),
                "export_snapshot",
                "Create File(",
            );
        }
        assert!(!dir.join("g.gsnap").exists());
        let _ = run(&graph, &format!(r#"g.exportSnapshot("{path}")"#), files()).unwrap();
        assert!(dir.join("g.gsnap").exists());
        assert_denied(
            run(
                &graph,
                &format!(r#"GraphSource::file("{path}")"#),
                AccessPolicy::deny_all(),
            ),
            "GraphSource::file",
            "Read File(",
        );
        let n = run(
            &graph,
            &format!(r#"GraphSource::file("{path}").V().count().next()"#),
            files(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
        assert_denied(
            run(&graph, r#"g.mark("m")"#, AccessPolicy::read_only()),
            "mark",
            "Update Data",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn graphson_export_and_import_ask_for_the_file() {
        let graph = modern();
        let dir = fresh_dir("graphson-needs-file-io");
        let path = forward(&dir.join("out.json"));
        for spelling in ["export_graphson", "exportGraphson"] {
            assert_denied(
                run(
                    &graph,
                    &format!(r#"g.{spelling}("{path}")"#),
                    AccessPolicy::read_only(),
                ),
                "export_graphson",
                "Create File(",
            );
        }
        assert!(!dir.join("out.json").exists());
        let _ = run(&graph, &format!(r#"g.exportGraphson("{path}")"#), files()).unwrap();
        assert!(dir.join("out.json").exists());
        let target = Arc::new(GraphSource::empty());
        for spelling in ["import_graphson", "importGraphson"] {
            assert_denied(
                run(&target, &format!(r#"g.{spelling}("{path}")"#), files()),
                "import_graphson",
                "Create Data",
            );
            assert_denied(
                run(
                    &target,
                    &format!(r#"g.{spelling}("{path}")"#),
                    AccessPolicy::read_only(),
                ),
                "import_graphson",
                "",
            );
        }
        let all = files().allow(ActionKind::Create, ResourceKind::Data);
        let _ = run(&target, &format!(r#"g.importGraphson("{path}")"#), all).unwrap();
        assert_eq!(count(&target), 6);
        // GraphSource::file reads a `.json` file as GraphSON.
        let n = run(
            &graph,
            &format!(r#"GraphSource::file("{path}").V().count().next()"#),
            files(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_root_confines_every_path() {
        let graph = modern();
        let root = fresh_dir("root");
        let outside = fresh_dir("outside");
        let rooted = || files().with_file_root(&root);
        // A relative path resolves under the root.
        let _ = run(&graph, r#"g.export_graphml("inside.graphml")"#, rooted()).unwrap();
        assert!(root.join("inside.graphml").exists());
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let _ = run(
            &graph,
            r#"g.export_graphml("sub/../sub/nested.graphml")"#,
            rooted(),
        )
        .unwrap();
        assert!(root.join("sub/nested.graphml").exists());
        // Leaving it is denied: `..`, an absolute path elsewhere.
        for path in [
            "../escape.graphml".to_string(),
            "sub/../../escape.graphml".to_string(),
            forward(&outside.join("abs.graphml")),
        ] {
            let err = run(&graph, &format!(r#"g.export_graphml("{path}")"#), rooted()).unwrap_err();
            assert!(
                err.contains("Permission denied for 'export_graphml' (Create File("),
                "{err}"
            );
            assert!(err.contains("outside the file root"), "{err}");
        }
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        // Reading is confined the same way.
        std::fs::copy(root.join("inside.graphml"), outside.join("secret.graphml")).unwrap();
        let secret = forward(&outside.join("secret.graphml"));
        let err = run(
            &graph,
            &format!(r#"GraphSource::file("{secret}")"#),
            rooted(),
        )
        .unwrap_err();
        assert!(err.contains("outside the file root"), "{err}");
        let n = run(
            &graph,
            r#"GraphSource::file("inside.graphml").V().count().next()"#,
            rooted(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_root_is_denied() {
        let graph = modern();
        let root = fresh_dir("link-root");
        let outside = fresh_dir("link-outside");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let rooted = files().with_file_root(&root);
        let err = run(&graph, r#"g.export_graphml("link/out.graphml")"#, rooted).unwrap_err();
        assert!(err.contains("outside the file root"), "{err}");
        assert!(!outside.join("out.graphml").exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_missing_file_root_denies_instead_of_panicking() {
        let graph = modern();
        let rooted = files()
            .with_file_root(std::env::temp_dir().join("graphersal-permissions-no-such-root"));
        let err = run(&graph, r#"g.export_graphml("x.graphml")"#, rooted).unwrap_err();
        assert!(err.contains("cannot be resolved"), "{err}");
    }

    #[test]
    fn a_host_resolves_files_with_the_same_policy() {
        let root = fresh_dir("host-root");
        let policy = AccessPolicy::allow_all().with_file_root(&root);
        let resolved = policy
            .resolve_file(std::path::Path::new("data.graphml"))
            .unwrap();
        assert!(resolved.starts_with(std::fs::canonicalize(&root).unwrap()));
        assert!(
            policy
                .resolve_file(std::path::Path::new("../data.graphml"))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The schema methods take a map or JSON text, never a path: no file access at all.
    #[test]
    fn schema_changes_ask_for_update_on_the_schema_and_no_file() {
        let graph = modern();
        for (script, operation) in [
            (r#"g.set_schema(`{"mode": "open"}`)"#, "set_schema"),
            (r#"g.setSchema(`{}`, SchemaMode.open)"#, "set_schema"),
            (r#"g.patchSchema(#{mode: "open"})"#, "patch_schema"),
        ] {
            assert_denied(run(&graph, script, files()), operation, "Update Schema");
            let _ = run(
                &graph,
                script,
                AccessPolicy::read_only().allow(ActionKind::Update, ResourceKind::Schema),
            )
            .unwrap();
        }
    }
}

mod rhai_import {
    use super::*;

    fn import(script: &str, policy: AccessPolicy) -> Result<Dynamic, String> {
        let policy: Arc<dyn Authorizer> = Arc::new(policy);
        let engine = script::engine_with_limits(&ScriptLimits::default(), policy.clone());
        let mut scope = script::graph_scope_with_options(modern(), &[], policy);
        engine
            .eval_with_scope::<Dynamic>(&mut scope, script)
            .map_err(|err| err.to_string())
    }

    #[test]
    fn import_asks_to_execute_the_module_file() {
        let err = import(r#"import "anything" as m; 1"#, AccessPolicy::read_only()).unwrap_err();
        assert!(
            err.contains(r#"Permission denied for 'import' (Execute File("anything.rhai"))"#),
            "{err}"
        );
        // Allowed by default: a missing module is just not found.
        let err = script::engine()
            .eval::<i64>(r#"import "graphersal-no-such-module" as m; 1"#)
            .unwrap_err()
            .to_string();
        assert!(!err.contains("Permission denied"), "{err}");
        let dir = fresh_dir("import-default");
        std::fs::write(dir.join("helpers.rhai"), "fn answer() { 42 }").unwrap();
        let module = forward(&dir.join("helpers"));
        let value = script::engine()
            .eval::<i64>(&format!(r#"import "{module}" as h; h::answer()"#))
            .unwrap();
        assert_eq!(value, 42);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_resolves_under_the_file_root_and_not_outside_it() {
        let root = fresh_dir("import-root");
        let outside = fresh_dir("import-outside");
        std::fs::write(root.join("helpers.rhai"), "fn answer() { 42 }").unwrap();
        std::fs::write(outside.join("evil.rhai"), "fn answer() { 666 }").unwrap();
        let rooted = || {
            AccessPolicy::read_only()
                .allow(ActionKind::Execute, ResourceKind::File)
                .with_file_root(&root)
        };
        let value = import(r#"import "helpers" as h; h::answer()"#, rooted()).unwrap();
        assert_eq!(value.as_int().unwrap(), 42);
        let evil = forward(&outside.join("evil"));
        for path in [
            format!(
                "../graphersal-permissions-{}-import-outside/evil",
                std::process::id()
            ),
            evil,
        ] {
            let err =
                import(&format!(r#"import "{path}" as h; h::answer()"#), rooted()).unwrap_err();
            assert!(err.contains("outside the file root"), "{path}: {err}");
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}

mod atomic_unit {
    use super::*;

    fn atomic(graph: &Arc<Graph>, script: &str) -> Result<Dynamic, String> {
        script::eval_value_atomic(
            graph.clone(),
            script,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(AllowAll),
        )
        .map_err(|err| err.to_string())
    }

    #[test]
    fn a_returned_closure_is_rejected_and_rolls_back() {
        let graph = modern();
        for (script, location) in [
            (r#"g.addV("a").next(); |x| x + 1"#, "the result"),
            (
                r#"g.addV("a").next(); [1, 2, || g.V().count().next()]"#,
                "result[2]",
            ),
            (
                r#"g.addV("a").next(); fn helper(x) { x } #{inner: #{f: Fn("helper")}}"#,
                "result.inner.f",
            ),
        ] {
            let err = atomic(&graph, script).unwrap_err();
            assert!(
                err.contains("cannot return a closure or function pointer"),
                "{err}"
            );
            assert!(err.contains(location), "{location}: {err}");
            assert_eq!(count(&graph), 6, "{script}");
        }
        // Calling the closure inside the script and returning data is fine.
        let n = atomic(&graph, r#"let f = || g.V().count().next(); f.call()"#).unwrap();
        assert_eq!(n.as_int().unwrap(), 6);
    }

    #[test]
    fn the_atomic_entry_asks_the_authorizer_too() {
        let graph = modern();
        let err = script::eval_value_atomic(
            graph.clone(),
            r#"g.addV("a").next()"#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(AccessPolicy::read_only()),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains(r#"Permission denied for 'add_v' (Create Data(vertex "a"))"#),
            "{err}"
        );
        assert_eq!(count(&graph), 6);
    }
}

/// Saved queries (`book/src/users_guide/saved_queries.md`): `Execute` on the query to call
/// it (asked before the body runs), `Read` to list, `Create`/`Update`/`Delete` to manage; a call
/// runs under the read-only restriction whatever the host allows.
mod saved_queries {
    use super::*;

    /// The modern graph with the saved query `people` (defined with everything allowed).
    fn with_query() -> Arc<Graph> {
        let graph = modern();
        let _ = run(
            &graph,
            r#"g.define_query(#{name: "people", body: "fn people() { g.v().has_label(\"person\") }"})"#,
            AllowAll,
        )
        .unwrap();
        graph
    }

    #[test]
    fn calling_asks_execute_on_the_query_before_the_body_runs() {
        let graph = with_query();
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.query("people").count().next()"#,
            recorder.clone(),
        )
        .unwrap();
        let seen: Vec<String> = recorder
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.to_string())
            .collect();
        assert_eq!(seen[0], r#"Execute Definition(query "people")"#, "{seen:?}");
        assert!(
            seen[1..]
                .iter()
                .any(|request| request.starts_with("Read Data")),
            "{seen:?}"
        );

        let denied = run(
            &graph,
            r#"g.query("people").count().next()"#,
            AccessPolicy::allow_all().deny(ActionKind::Execute, ResourceKind::Definition),
        );
        assert_denied(denied, "query", r#"Execute Definition(query "people")"#);
    }

    #[test]
    fn a_read_only_host_calls_and_lists_but_does_not_define() {
        let graph = with_query();
        let policy = AccessPolicy::read_only;
        assert_eq!(
            run(&graph, r#"g.query("people").count().next()"#, policy())
                .unwrap()
                .as_int()
                .unwrap(),
            4
        );
        assert!(run(&graph, r#"g.queries()"#, policy()).is_ok());
        assert!(run(&graph, r#"g.get_query("people")"#, policy()).is_ok());
        assert_denied(
            run(
                &graph,
                r#"g.define_query(#{name: "x", body: "fn x() { 1 }"})"#,
                policy(),
            ),
            "define_query",
            r#"Create Definition(query "x")"#,
        );
        assert_denied(
            run(
                &graph,
                r#"g.define_query(#{name: "people", body: "fn people() { 1 }"})"#,
                policy(),
            ),
            "define_query",
            r#"Update Definition(query "people")"#,
        );
        assert_denied(
            run(&graph, r#"g.drop_query("people")"#, policy()),
            "drop_query",
            r#"Delete Definition(query "people")"#,
        );
        assert_denied(
            run(&graph, r#"g.move_query("people", "x")"#, policy()),
            "move_query",
            "Update Definition",
        );
        assert_denied(
            run(&graph, r#"g.describe_query("people", "x")"#, policy()),
            "describe_query",
            "Update Definition",
        );
        assert_denied(
            run(&graph, r#"g.define_queries("", "fn y() { 1 }")"#, policy()),
            "define_queries",
            r#"Create Definition(query "y")"#,
        );
    }

    #[test]
    fn listing_asks_read_on_the_saved_queries() {
        let graph = with_query();
        let no_read = || AccessPolicy::allow_all().deny(ActionKind::Read, ResourceKind::Definition);
        assert_denied(
            run(&graph, r#"g.queries()"#, no_read()),
            "queries",
            "Read Definition(query)",
        );
        assert_denied(
            run(&graph, r#"g.get_query("people")"#, no_read()),
            "get_query",
            r#"Read Definition(query "people")"#,
        );
        // Calling needs Execute only.
        assert!(run(&graph, r#"g.query("people").to_list()"#, no_read()).is_ok());
    }

    #[test]
    fn the_body_runs_with_the_hosts_reads_and_never_its_writes() {
        let graph = with_query();
        // The host denies reading data: the body cannot read either.
        assert_denied(
            run(
                &graph,
                r#"g.query("people").to_list()"#,
                AccessPolicy::allow_all().deny(ActionKind::Read, ResourceKind::Data),
            ),
            "v",
            "Read Data",
        );
        // The host allows everything: a write through a query result is still denied.
        let err = run(&graph, r#"g.query("people").drop().to_list()"#, AllowAll).unwrap_err();
        assert!(err.contains("Permission denied for 'drop'"), "{err}");
        assert!(err.contains("a saved query is read-only"), "{err}");
        assert_eq!(count(&graph), 6);
    }

    /// The modern graph with the saved query `renew` that writes: it creates a `person` and
    /// deletes every `knows` edge.
    fn with_writing_query() -> Arc<Graph> {
        let graph = modern();
        let _ = run(
            &graph,
            r#"g.define_query(#{name: "renew", body: `fn renew() {
                g.add_v("person").iterate();
                g.e().has_label("knows").drop().iterate()
            }`})"#,
            AllowAll,
        )
        .unwrap();
        graph
    }

    #[test]
    fn a_query_that_writes_asks_its_whole_effect_set_before_the_body_runs() {
        let graph = with_writing_query();
        let recorder = Arc::new(Recorder::default());
        let _ = run(&graph, r#"g.query("renew")"#, recorder.clone()).unwrap();
        let seen = recorder.seen();
        assert_eq!(
            seen[..3],
            [
                r#"Execute Definition(query "renew")"#.to_string(),
                r#"Create Data(vertex "person")"#.to_string(),
                r#"Delete Data(edge "knows")"#.to_string(),
            ],
            "{seen:?}"
        );
    }

    #[test]
    fn a_query_that_writes_is_bounded_by_the_callers_rights() {
        let graph = with_writing_query();
        // The host may create but not delete: the call is denied before its first write.
        let err = run(
            &graph,
            r#"g.query("renew")"#,
            AccessPolicy::allow_all().deny(ActionKind::Delete, ResourceKind::Data),
        )
        .unwrap_err();
        assert!(
            err.contains(r#"Permission denied for 'query("renew")' (Delete Data(edge "knows"))"#),
            "{err}"
        );
        assert_eq!(count(&graph), 6);
        // A read-only host: denied too, nothing written.
        let err = run(&graph, r#"g.query("renew")"#, AccessPolicy::read_only()).unwrap_err();
        assert!(
            err.contains("Permission denied for 'query(\"renew\")'"),
            "{err}"
        );
        assert_eq!(count(&graph), 6);
        // The host allows it: the call writes.
        let _ = run(&graph, r#"g.query("renew")"#, AllowAll).unwrap();
        assert_eq!(count(&graph), 7);
    }

    /// Denies creating edges, allows everything else.
    #[cfg(feature = "gql")]
    struct NoEdgeCreation;

    #[cfg(feature = "gql")]
    impl Authorizer for NoEdgeCreation {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            match (request.action, request.resource) {
                (
                    Action::Create,
                    Resource::Data {
                        element: Some(ElementKind::Edge),
                        ..
                    },
                ) => Err(Denied::new("no new edges")),
                _ => Ok(()),
            }
        }
    }

    /// A saved query written in GQL is authorized like a Rhai one of the same dialect class:
    /// `Execute` first, a read-only one runs with the host's reads and never writes, one that
    /// writes asks its whole effect set (the plan's writes, per label) before anything runs and
    /// is bounded by the caller's rights.
    #[cfg(feature = "gql")]
    #[test]
    fn a_gql_query_is_authorized_like_a_rhai_query_of_its_dialect() {
        let graph = modern();
        let _ = run(
            &graph,
            r#"g.define_query(#{name: "people", language: "gql",
                   body: "MATCH (p:person) RETURN p.name AS name"});
               g.define_query(#{name: "adopt", language: "gql", params: #{owner: "string"},
                   body: "MATCH (p:person {name: $owner}) INSERT (p)-[:OWNS]->(:Pet {owner: $owner})"})"#,
            AllowAll,
        )
        .unwrap();
        // Execute on the query first.
        assert_denied(
            run(
                &graph,
                r#"g.query("people").to_list()"#,
                AccessPolicy::allow_all().deny(ActionKind::Execute, ResourceKind::Definition),
            ),
            "query",
            r#"Execute Definition(query "people")"#,
        );
        // A read-only query reads with the host's rights and never writes.
        assert_denied(
            run(
                &graph,
                r#"g.query("people").to_list()"#,
                AccessPolicy::allow_all().deny(ActionKind::Read, ResourceKind::Data),
            ),
            "v",
            "Read Data",
        );
        let err = run(&graph, r#"g.query("people").drop().iterate()"#, AllowAll).unwrap_err();
        assert!(err.contains("a saved query is read-only"), "{err}");
        assert_eq!(count(&graph), 6);
        // A query that writes: the effect set is asked before the body runs.
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.query("adopt", #{owner: "marko"})"#,
            recorder.clone(),
        )
        .unwrap();
        let seen = recorder.seen();
        assert_eq!(
            seen[..3],
            [
                r#"Execute Definition(query "adopt")"#.to_string(),
                r#"Create Data(vertex "Pet")"#.to_string(),
                r#"Create Data(edge "OWNS")"#.to_string(),
            ],
            "{seen:?}"
        );
        assert_eq!(count(&graph), 7);
        // The caller may not create edges: denied before the first write.
        let err = run(
            &graph,
            r#"g.query("adopt", #{owner: "josh"})"#,
            NoEdgeCreation,
        )
        .unwrap_err();
        assert!(
            err.contains(r#"Permission denied for 'query("adopt")' (Create Data(edge "OWNS"))"#),
            "{err}"
        );
        assert_eq!(count(&graph), 7);
        // A read-only host: denied, nothing written.
        let err = run(
            &graph,
            r#"g.query("adopt", #{owner: "josh"})"#,
            AccessPolicy::read_only(),
        )
        .unwrap_err();
        assert!(
            err.contains(r#"Permission denied for 'query("adopt")'"#),
            "{err}"
        );
        assert_eq!(count(&graph), 7);
        // Defining asks Create on the query.
        assert_denied(
            run(
                &graph,
                r#"g.define_query(#{name: "x", language: "gql", body: "MATCH (n) RETURN n"})"#,
                AccessPolicy::read_only(),
            ),
            "define_query",
            r#"Create Definition(query "x")"#,
        );
    }
}

/// Denies reading vertices, allows everything else.
struct NoVertexReads;

impl Authorizer for NoVertexReads {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        if request
            .to_owned_request()
            .to_string()
            .starts_with("Read Data(vertex")
        {
            return Err(Denied::new("vertices are hidden"));
        }
        Ok(())
    }
}

/// `inject(v)` of a vertex or an edge value emits the stored element of its id, so it reads
/// graph data and asks Read Data like `V()`/`E()` (issue #2): an edge's endpoint cannot be used
/// to read a vertex the policy hides. Plain values ask nothing, as before.
#[test]
fn injecting_an_element_value_asks_read_data() {
    let graph = modern();
    let mut scope = script::graph_scope(graph.clone());
    let v = script::engine()
        .eval_with_scope::<Dynamic>(&mut scope, r#"g.V("1").next()"#)
        .unwrap();
    let recorder = Arc::new(Recorder::default());
    let authorizer: Arc<dyn Authorizer> = recorder.clone();
    let mut scope = script::graph_scope_with_options(graph.clone(), &[], authorizer.clone());
    scope.push("v", v);
    let names = script::engine_with_limits(&ScriptLimits::default(), authorizer)
        .eval_with_scope::<Dynamic>(&mut scope, r#"g.inject(v).values("name").toList()"#)
        .unwrap();
    assert_eq!(names.into_array().unwrap().len(), 1);
    assert!(
        recorder
            .seen()
            .iter()
            .any(|seen| seen.starts_with("Read Data(vertex")),
        "{:?}",
        recorder.seen()
    );

    // The edge may be read, its endpoint vertex may not.
    assert_denied(
        run(
            &graph,
            r#"let e = g.E("0").next(); g.inject(e.out_v).values("name").toList()"#,
            NoVertexReads,
        ),
        "inject",
        "Read Data(vertex",
    );
    assert_denied(
        run(&graph, r#"let v = g.V("1").next(); v.id"#, NoVertexReads),
        "v",
        "Read Data(vertex",
    );
    // A plain value asks nothing.
    let recorder = Arc::new(Recorder::default());
    let _ = run(&graph, "g.inject(1, \"a\").toList()", recorder.clone()).unwrap();
    assert!(recorder.seen().is_empty(), "{:?}", recorder.seen());
}

#[test]
fn has_with_a_traversal_asks_for_what_its_child_does() {
    let graph = modern();
    // A reading child is allowed read-only.
    let found = run(
        &graph,
        r#"g.V().has("name", __.to_lower().is("marko")).count().next()"#,
        AccessPolicy::read_only(),
    )
    .unwrap();
    assert_eq!(found.as_int().unwrap(), 1);
    // A writing child asks Create Data and is denied before anything runs.
    let before = count(&graph);
    assert_denied(
        run(
            &graph,
            r#"g.V().has("name", __.addV("copy")).iterate()"#,
            AccessPolicy::read_only(),
        ),
        "add_v",
        "Create Data",
    );
    assert_eq!(count(&graph), before);
}

/// Embedders (book page "Vectors"): the `embed` step and the function `embed(text)` ask
/// `Execute` on the embedder, the built-in one included; `read_only()` allows it; a saved query
/// may embed.
mod embedders {
    use super::*;

    /// Denies `Execute` on every embedder, allows everything else.
    struct NoEmbedders;

    impl Authorizer for NoEmbedders {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            match request.resource {
                Resource::Embedder { name } => Err(Denied::new(format!("no embedder {name}"))),
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn the_step_and_the_function_ask_execute_on_the_embedder() {
        let graph = modern();
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"let q = embed("marko"); g.v().embed("name").embed("name").count().next()"#,
            recorder.clone(),
        )
        .unwrap();
        let asked: Vec<String> = recorder
            .seen()
            .into_iter()
            .filter(|seen| seen.contains("Embedder"))
            .collect();
        // Once for the function, once for the traversal (two steps, one distinct request).
        assert_eq!(
            asked,
            vec![
                r#"Execute Embedder("hash-ngram/1")"#.to_string(),
                r#"Execute Embedder("hash-ngram/1")"#.to_string()
            ]
        );
    }

    #[test]
    fn a_denied_embedder_stops_the_step_and_the_function() {
        let graph = modern();
        assert_denied(
            run(&graph, r#"g.v().embed("name").to_list()"#, NoEmbedders),
            "embed",
            r#"Execute Embedder("hash-ngram/1")"#,
        );
        assert_denied(
            run(&graph, r#"embed("marko")"#, NoEmbedders),
            "embed",
            r#"Execute Embedder("hash-ngram/1")"#,
        );
        // Inside a child traversal, before anything is written.
        assert_denied(
            run(
                &graph,
                r#"g.v().property("emb", __.embed("name")).iterate()"#,
                NoEmbedders,
            ),
            "embed",
            r#"Execute Embedder("hash-ngram/1")"#,
        );
        assert!(
            run(&graph, r#"g.v().has("emb").count().next()"#, AllowAll)
                .unwrap()
                .as_int()
                .unwrap()
                == 0
        );
        // A policy denies the resource kind.
        let policy = AccessPolicy::allow_all().deny(ActionKind::Execute, ResourceKind::Embedder);
        assert_denied(
            run(&graph, r#"embed("marko")"#, policy),
            "embed",
            "Execute Embedder",
        );
    }

    #[test]
    fn read_only_allows_embedding_and_a_saved_query_may_embed() {
        let graph = modern();
        assert!(AccessPolicy::read_only().allows(ActionKind::Execute, ResourceKind::Embedder));
        assert!(AccessPolicy::allow_all().allows(ActionKind::Execute, ResourceKind::Embedder));
        let n = run(
            &graph,
            r#"let q = embed("marko"); g.v().has_label("person").embed("name").count().next()"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 4);
        let _ = run(
            &graph,
            r#"g.define_query(#{name: "similar", params: #{text: "string"},
                 body: "fn similar(text) { g.v().has_label(\"person\").embed(\"name\").similarity(embed(text)) }"})"#,
            AllowAll,
        )
        .unwrap();
        let scores = run(
            &graph,
            r#"g.query("similar", #{text: "marko"}).to_list().len()"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        assert_eq!(scores.as_int().unwrap(), 4);
    }

    #[test]
    fn an_owned_embedder_request_round_trips() {
        let request = AccessRequest::new(
            Action::Execute,
            Resource::Embedder {
                name: "acme/minilm",
            },
        );
        let owned = request.to_owned_request();
        assert_eq!(owned.as_request(), request);
        assert_eq!(owned.to_string(), r#"Execute Embedder("acme/minilm")"#);
        assert_eq!(request.resource.kind(), ResourceKind::Embedder);
        assert_eq!(ResourceKind::Embedder.name(), "Embedder");
    }
}

/// The job `embed_property` and the text queries of `nearest`/`similarity` (book page
/// "Vectors"): the job asks `Read` and `Update` on the label's data, `Execute` on the embedder
/// and `Update` on the schema when it declares the property; it is denied in a saved query;
/// a text query asks `Execute` on the embedder of the declared model.
mod embed_jobs {
    use super::*;

    const ATTACH: &str = r#"g.set_schema(g.infer_schema(), SchemaMode.open)"#;
    const JOB: &str = r#"g.embed_property(#{label: "person", from: "name", to: "emb"})"#;

    #[test]
    fn the_job_asks_for_the_data_the_embedder_and_the_schema() {
        let graph = modern();
        let _ = run(&graph, ATTACH, AllowAll).unwrap();
        let recorder = Arc::new(Recorder::default());
        let _ = run(&graph, JOB, recorder.clone()).unwrap();
        assert_eq!(
            recorder.seen(),
            [
                r#"Read Data(vertex "person")"#,
                r#"Update Data(vertex "person")"#,
                r#"Execute Embedder("hash-ngram/1")"#,
                "Update Schema",
            ]
        );
        // Declared already: the schema is not asked for again.
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.embed_property(#{label: "person", from: "name", to: "emb", only_missing: false})"#,
            recorder.clone(),
        )
        .unwrap();
        assert!(!recorder.seen().iter().any(|seen| seen == "Update Schema"));
    }

    #[test]
    fn a_denied_request_stops_the_job_before_anything_is_written() {
        let graph = modern();
        let _ = run(&graph, ATTACH, AllowAll).unwrap();
        assert_denied(
            run(&graph, JOB, AccessPolicy::read_only()),
            "embed_property",
            r#"Update Data(vertex "person")"#,
        );
        let no_schema = AccessPolicy::allow_all().deny(ActionKind::Update, ResourceKind::Schema);
        assert_denied(
            run(&graph, JOB, no_schema),
            "embed_property",
            "Update Schema",
        );
        let no_embedder =
            AccessPolicy::allow_all().deny(ActionKind::Execute, ResourceKind::Embedder);
        assert_denied(
            run(&graph, JOB, no_embedder),
            "embed_property",
            r#"Execute Embedder("hash-ngram/1")"#,
        );
        assert_eq!(
            run(&graph, r#"g.v().has("emb").count().next()"#, AllowAll)
                .unwrap()
                .as_int()
                .unwrap(),
            0
        );
    }

    #[test]
    fn a_saved_query_cannot_run_the_job() {
        let graph = modern();
        let _ = run(&graph, ATTACH, AllowAll).unwrap();
        // Stored through the Rust API in the read-only dialect (as by 0.2.0 or a catalog file):
        // `g.define_query` refuses such a body (it changes the schema).
        graph
            .write()
            .traversal_mut()
            .define_query(
                "fill",
                graphersal::catalog::QueryDefinition::new(
                    r#"fn fill() { g.embed_property(#{label: "person", from: "name", to: "emb"}) }"#,
                ),
            )
            .unwrap();
        let err = run(&graph, r#"g.query("fill")"#, AllowAll).unwrap_err();
        assert!(
            err.contains("Permission denied for 'embed_property'"),
            "{err}"
        );
        assert!(err.contains("a saved query is read-only"), "{err}");
    }

    #[test]
    fn a_text_query_asks_execute_on_the_embedder_of_its_model() {
        let graph = modern();
        let _ = run(&graph, &format!("{ATTACH}; {JOB}"), AllowAll).unwrap();
        let recorder = Arc::new(Recorder::default());
        let _ = run(
            &graph,
            r#"g.v().nearest("emb", "marco", 1).similarity("emb", "marco").to_list()"#,
            recorder.clone(),
        )
        .unwrap();
        assert!(
            recorder
                .seen()
                .iter()
                .any(|seen| seen == r#"Execute Embedder("hash-ngram/1")"#),
            "{:?}",
            recorder.seen()
        );
        let no_embedder =
            AccessPolicy::allow_all().deny(ActionKind::Execute, ResourceKind::Embedder);
        assert_denied(
            run(
                &graph,
                r#"g.v().nearest("emb", "marco", 1).to_list()"#,
                no_embedder,
            ),
            "nearest",
            r#"Execute Embedder("hash-ngram/1")"#,
        );
        // read_only() allows a text query.
        let n = run(
            &graph,
            r#"g.v().nearest("emb", "marco", 2).count().next()"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        assert_eq!(n.as_int().unwrap(), 2);
    }
}

/// `g.estimate_compression(..)` reads the data of its label (issue #12): a `Read` on `Data`,
/// never a catalog write, so a read-only policy allows it.
mod compression_estimate {
    use super::*;

    #[test]
    fn an_estimate_is_a_read_of_the_label_data() {
        let graph = modern();
        let script = r#"g.estimate_compression(#{label: "person", path: "name"}).values_found"#;
        assert_eq!(
            run(&graph, script, AccessPolicy::read_only())
                .unwrap()
                .as_int()
                .unwrap(),
            4
        );
        assert_denied(
            run(
                &graph,
                script,
                AccessPolicy::allow_all().deny(ActionKind::Read, ResourceKind::Data),
            ),
            "estimate_compression",
            "Read Data(vertex \"person\")",
        );
        let edges = r#"g.estimate_compression(#{element: "edge", label: "knows", path: "weight"})"#;
        assert_denied(
            run(&graph, edges, AccessPolicy::deny_all()),
            "estimate_compression",
            "Read Data(edge \"knows\")",
        );
        // The Rust API asks the same request.
        let lock = graph.read();
        let g = GraphTraversalSource::new(&*lock).with_authorizer(Arc::new(
            AccessPolicy::allow_all().deny(ActionKind::Read, ResourceKind::Data),
        ));
        let request = graphersal::catalog::CompressionEstimateRequest::new(
            ElementKind::Vertex,
            "person",
            "name",
        )
        .unwrap();
        let err = g.estimate_compression(&request).unwrap_err().to_string();
        assert!(err.contains("Permission denied"), "{err}");
    }
}

/// `g.gql()` (feature `gql`) asks exactly what its Gremlin twin asks: the lowered plan is an
/// ordinary plan, checked once before it runs. A read names the element kind (never a label,
/// like every Gremlin read), so denying the reads of edges refuses an edge pattern and lets a
/// node pattern through; `gql()` never asks for a write.
#[cfg(feature = "gql")]
mod gql {
    use super::*;

    /// Denies every read of edges (and of data of an unknown kind).
    struct NoEdges;

    impl Authorizer for NoEdges {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            match (request.action, request.resource) {
                (Action::Read, Resource::Data { element, .. })
                    if element != Some(ElementKind::Vertex) =>
                {
                    Err(Denied::new("edges are private"))
                }
                _ => Ok(()),
            }
        }
    }

    /// The requests `script` asks, allowed.
    fn requests(graph: &Arc<Graph>, script: &str) -> Vec<String> {
        let recorder = Arc::new(Recorder::default());
        let _ =
            run(graph, script, recorder.clone()).unwrap_or_else(|err| panic!("{script}: {err}"));
        recorder.seen()
    }

    #[test]
    fn a_denied_read_refuses_gql_like_its_gremlin_twin() {
        let graph = modern();
        // The Gremlin twin and the GQL query are refused with the same step and request.
        for script in [
            r#"g.V().hasLabel("person").out("knows").values("name").toList()"#,
            r#"g.gql("MATCH (p:person)-[:knows]->(f) RETURN f.name").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        for script in [
            r#"g.V().hasLabel("person").outE("knows").inV().values("name").toList()"#,
            r#"g.gql("MATCH (p:person)-[e:knows]->(f) RETURN e.weight").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out_e", "Read Data(edge)");
        }
        // Refused before it runs, also when no row would reach the edge step.
        let err = run(
            &graph,
            r#"g.gql("MATCH (p:nobody)-[:knows]->(f) RETURN f").toList()"#,
            NoEdges,
        )
        .unwrap_err();
        assert!(err.contains("edges are private"), "{err}");
        // A node pattern reads vertices only: allowed, like its twin.
        let names = run(
            &graph,
            r#"g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS n ORDER BY n").select("n").toList()"#,
            NoEdges,
        )
        .unwrap();
        assert_eq!(names.into_array().unwrap().len(), 2);
        // The Rust API asks the same.
        let lock = graph.read();
        let g = GraphTraversalSource::new(&*lock).with_authorizer(Arc::new(NoEdges));
        let err = format_diagnostic(
            &g.gql("MATCH (a)-[:created]->(s) RETURN s.name")
                .to_list()
                .unwrap_err(),
        );
        assert!(
            err.contains("Permission denied for 'out' (Read Data(edge)): edges are private"),
            "{err}"
        );
        let g = GraphTraversalSource::new(&*lock).with_authorizer(Arc::new(NoEdges));
        assert_eq!(
            g.gql("MATCH (s:software) RETURN s")
                .to_list()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn gql_asks_the_same_requests_as_its_gremlin_twin() {
        let graph = modern();
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person) RETURN p.name").toList()"#,
                r#"g.V().hasLabel("person").values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person)-[e:knows]->(f) RETURN f.name").toList()"#,
                r#"g.V().hasLabel("person").outE("knows").inV().values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (a)-[:created]-(b) RETURN b").toList()"#,
                r#"g.V().both("created").toList()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
    }

    #[test]
    fn gql_never_asks_for_a_write() {
        let graph = modern();
        for query in [
            "MATCH (p:person) WHERE p.age > 30 RETURN p.name ORDER BY p.name LIMIT 1",
            "MATCH (a)-[e:knows]->(b), (b)-[:created]->(c) RETURN a, e, b, c",
            "MATCH (a)-[:created]->(s)<-[:created]-(b) WHERE a <> b RETURN DISTINCT s.name",
            "MATCH (a)-[e]-(b) RETURN element_id(e), labels(a), type(e)",
            // Write clauses not yet supported fail before anything is asked of the data.
            "MATCH (p) FOREACH (x IN [1] | SET p.age = x) RETURN p",
            "MATCH (p) FOREACH (x IN [1] | DELETE p)",
        ] {
            let recorder = Arc::new(Recorder::default());
            let script = format!("g.gql({query:?}).toList()");
            let _ = run(&graph, &script, recorder.clone());
            let seen = recorder.seen();
            assert!(
                seen.iter().all(|request| request.starts_with("Read ")),
                "{query}: {seen:?}"
            );
        }
        // So a read-only host runs every GQL query, and the graph stays unchanged.
        let rows = run(
            &graph,
            r#"g.gql("MATCH (a)-[:knows]->(b) RETURN a.name, b.name").toList()"#,
            AccessPolicy::read_only(),
        )
        .unwrap();
        assert_eq!(rows.into_array().unwrap().len(), 2);
        assert_eq!(count(&graph), 6);
        assert_eq!(graph.read().edge_count(), 6);
    }

    /// Stage 1b: an edge read only inside a quantified edge (a `repeat()` body), an optional
    /// part (an `optional()` child) or a pattern after a `WITH` is refused before the run, by
    /// the same step and request as the Gremlin twin. Reads never name a label, so the denial
    /// is by element kind, as for every Gremlin read.
    #[test]
    fn a_denied_read_in_a_1b_construct_refuses_gql_like_its_gremlin_twin() {
        let graph = modern();
        // A quantified edge: the edge step of the repeat() body.
        for script in [
            r#"g.V().hasLabel("person").repeat(__.outE("knows").inV()).emit().times(2).values("name").toList()"#,
            r#"g.gql("MATCH (p:person)-[:knows]->{1,2}(f) RETURN f.name").toList()"#,
            r#"g.gql("MATCH (p:person)-[:knows*]->(f) RETURN count(*) AS c").toList()"#,
            r#"g.gql("MATCH x = (p:person)-[:knows]->{0,1}(f) RETURN x").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out_e", "Read Data(edge)");
        }
        // An optional part: the edge step inside optional().
        for script in [
            r#"g.V().hasLabel("person").optional(__.out("created")).values("name").toList()"#,
            r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN p.name, s.name").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // After a WITH, projecting or materializing (an aggregate, ORDER BY with LIMIT).
        for script in [
            r#"g.V().hasLabel("person").limit(2).out("created").values("name").toList()"#,
            r#"g.gql("MATCH (p:person) WITH p MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
            r#"g.gql("MATCH (p:person) WITH p, count(*) AS c MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
            r#"g.gql("MATCH (p:person) WITH p ORDER BY p.age LIMIT 2 MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // Refused before it runs, also when no row would reach the optional part or the
        // quantified edge.
        for script in [
            r#"g.gql("MATCH (p:nobody) OPTIONAL MATCH (p)-[:created]->(s) RETURN s").toList()"#,
            r#"g.gql("MATCH (p:nobody)-[:knows]->{0,3}(f) RETURN f").toList()"#,
            r#"g.gql("MATCH (p:nobody) WITH p LIMIT 0 MATCH (p)-[:created]->(s) RETURN s").toList()"#,
        ] {
            let err = run(&graph, script, NoEdges).unwrap_err();
            assert!(err.contains("edges are private"), "{script}: {err}");
        }
        // Node-only 1b queries read vertices only: allowed.
        let rows = run(
            &graph,
            r#"g.gql("MATCH (p:person) WITH p.age AS a LET b = a + 1 FILTER b > 30 RETURN count(*) AS c, collect_list(b) AS bs").toList()"#,
            NoEdges,
        )
        .unwrap();
        assert_eq!(rows.into_array().unwrap().len(), 1);
    }

    #[test]
    fn gql_1b_asks_the_same_requests_as_its_gremlin_twin() {
        let graph = modern();
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person)-[:knows]->{1,2}(f) RETURN f.name").toList()"#,
                r#"g.V().hasLabel("person").repeat(__.outE("knows").inV()).emit().times(2).values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
                r#"g.V().hasLabel("person").optional(__.out("created")).values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) WITH p, count(*) AS c MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
                r#"g.V().hasLabel("person").out("created").values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN count(*) AS c").toList()"#,
                r#"g.V().hasLabel("person").count().toList()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
        // And a 1b query never asks for a write.
        for query in [
            "MATCH (p:person) OPTIONAL MATCH (p)-[e:knows]->{1,2}(f) RETURN p.name, count(f) AS c",
            "MATCH x = (a)-[:created]-{1,2}(b) WITH x, length(x) AS l RETURN l, count(*) AS c",
        ] {
            let script = format!("g.gql({query:?}).toList()");
            let seen = requests(&graph, &script);
            assert!(
                seen.iter().all(|request| request.starts_with("Read ")),
                "{query}: {seen:?}"
            );
        }
    }

    /// Denies every read of `person` vertices: a read whose label is not stated (every read)
    /// is possibly one, so all vertex reads are denied, edge reads are not.
    struct NoPersons;

    impl Authorizer for NoPersons {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            match (request.action, request.resource) {
                (Action::Read, Resource::Data { element, label })
                    if element != Some(ElementKind::Edge)
                        && label.is_none_or(|label| label == "person") =>
                {
                    Err(Denied::new("persons are private"))
                }
                _ => Ok(()),
            }
        }
    }

    /// Design F8 (stage 1c): `startNode(e)` / `endNode(e)`, `nodes(p)`, `keys` and
    /// `properties` ask no permission of their own. They read vertices the pattern already
    /// read: every edge pattern stands between two node patterns, whose lowering reads the
    /// endpoint vertices, so a denied vertex read refuses the query before it runs, like the
    /// Gremlin twin `e().out_v()`. (The GQL plan starts at `v()`, the twin at `e()`, so the
    /// denied step differs; the request is the same.)
    #[test]
    fn a_denied_vertex_read_refuses_start_node_like_out_v() {
        let graph = modern();
        // The edges alone are readable in Gremlin.
        let weights = run(&graph, r#"g.E().values("weight").toList()"#, NoPersons).unwrap();
        assert_eq!(weights.into_array().unwrap().len(), 6);
        assert_denied(
            run(&graph, r#"g.E().outV().values("name").toList()"#, NoPersons),
            "out_v",
            "Read Data(vertex)",
        );
        assert_denied(
            run(&graph, r#"g.E().inV().values("name").toList()"#, NoPersons),
            "in_v",
            "Read Data(vertex)",
        );
        for query in [
            "MATCH ()-[e]->() RETURN startNode(e).name AS s",
            "MATCH ()-[e:knows]->() RETURN element_id(endNode(e)) AS t",
            "MATCH (a)-[e]-(b) RETURN a IS SOURCE OF e AS s",
            "MATCH p = ()-[:knows]->() RETURN nodes(p) AS n",
            "MATCH ()-[e]->() RETURN keys(e) AS k, properties(e) AS p",
            "MATCH (x:nobody)-[e]->() RETURN startNode(e) AS s",
        ] {
            let script = format!("g.gql({query:?}).toList()");
            assert_denied(run(&graph, &script, NoPersons), "v", "Read Data(vertex)");
        }
    }

    /// The functions of stage 1c add no request: a query with them asks exactly what the same
    /// pattern without them asks, and the same set as the Gremlin twin of the endpoints.
    #[test]
    fn gql_1c_functions_add_no_request() {
        let graph = modern();
        let set = |script: &str| {
            let mut seen = requests(&graph, script);
            seen.sort();
            seen.dedup();
            seen
        };
        for (with_functions, without) in [
            (
                r#"g.gql("MATCH ()-[e]->() RETURN startNode(e), endNode(e), keys(e), properties(e)").toList()"#,
                r#"g.gql("MATCH ()-[e]->() RETURN e").toList()"#,
            ),
            (
                r#"g.gql("MATCH p = (a)-[:knows]->{1,2}(b) RETURN nodes(p), relationships(p), a IS SOURCE OF head(relationships(p))").toList()"#,
                r#"g.gql("MATCH p = (a)-[:knows]->{1,2}(b) RETURN p").toList()"#,
            ),
            (
                r#"g.gql("MATCH (n) RETURN toString(n.age), upper(left(n.name, 2)), round(sqrt(n.age), 1), range(1, 3), CAST(n.age AS STRING)").toList()"#,
                r#"g.gql("MATCH (n) RETURN n").toList()"#,
            ),
        ] {
            assert_eq!(requests(&graph, with_functions), requests(&graph, without));
        }
        assert_eq!(
            set(r#"g.gql("MATCH ()-[e]->() RETURN startNode(e), endNode(e)").toList()"#),
            set(r#"g.E().project("s", "t").by(__.outV()).by(__.inV()).toList()"#)
        );
    }

    /// Stage 2a: `gql:lookup` (a component found by id or index, design M3) reads vertices
    /// like the `v()` it replaces: a denied vertex read refuses it before it runs, at that step,
    /// with the request `g.V().hasLabel(..)` is refused with, also when it is the only vertex
    /// read of the query and no row would reach it.
    #[test]
    fn a_denied_read_refuses_gql_lookup_like_v_has_label() {
        let graph = modern();
        assert_denied(
            run(
                &graph,
                r#"g.V().hasLabel("person").values("name").toList()"#,
                NoPersons,
            ),
            "v",
            "Read Data(vertex)",
        );
        let by_id = r#"g.gql("FOR id IN ['1', '4'] MATCH (n:person) WHERE element_id(n) = id RETURN n.name").toList()"#;
        let by_index =
            r#"g.gql("FOR x IN ['marko'] MATCH (n:person {name: x}) RETURN n.age").toList()"#;
        let no_row =
            r#"g.gql("FOR id IN [] MATCH (n:person) WHERE element_id(n) = id RETURN n").toList()"#;
        assert!(requests(&graph, by_index).contains(&"Read Data(vertex)".to_string()));
        // The index: created allowed, before the denied runs.
        let _ = requests(&graph, r#"g.create_index(#{label: "person", key: "name"})"#);
        for script in [by_id, by_index, no_row] {
            let profile = run(
                &graph,
                &script.replace(".toList()", ".profile().to_string()"),
                Recorder::default(),
            )
            .unwrap_or_else(|err| panic!("{script}: {err}"))
            .to_string();
            assert!(profile.contains("gql:lookup("), "{script}: {profile}");
            assert!(!profile.contains("\nv()"), "{script}: {profile}");
            assert_denied(
                run(&graph, script, NoPersons),
                "gql:lookup",
                "Read Data(vertex)",
            );
        }
        // It asks exactly what the Gremlin twin asks.
        assert_eq!(
            requests(&graph, by_id),
            requests(
                &graph,
                r#"g.inject(0).V().hasLabel("person").values("name").toList()"#
            )
        );
    }

    /// Stage 2a: a query without `MATCH` (`FOR`, `RETURN`, a leading `FILTER`) starts from the
    /// unit row `inject(0)` and reads no vertex, so it runs where vertex reads are denied, asking
    /// what `g.inject(0)` asks; a `MATCH` after a `FOR` (a per-row scan or a lookup of another
    /// label) asks what its Gremlin twin asks and is refused or allowed like it.
    #[test]
    fn for_and_unit_row_queries_ask_what_their_gremlin_twins_ask() {
        let graph = modern();
        for script in [
            r#"g.gql("FOR x IN [1, 2] WITH ORDINALITY i RETURN x, i").toList()"#,
            r#"g.gql("RETURN 1 AS x").toList()"#,
            r#"g.gql("FILTER true UNWIND [1] AS x RETURN x").toList()"#,
        ] {
            assert!(run(&graph, script, NoPersons).is_ok(), "{script}");
            assert_eq!(
                requests(&graph, script),
                requests(&graph, r#"g.inject(0).toList()"#),
                "{script}"
            );
        }
        // A per-row scan of persons after FOR: refused at v(), as g.inject(0).V().hasLabel(..).
        let scan = r#"g.gql("FOR n IN ['marko'] MATCH (p:person) WHERE p.name = n RETURN p.age").toList()"#;
        assert_denied(run(&graph, scan, NoPersons), "v", "Read Data(vertex");
        assert_eq!(
            requests(&graph, scan),
            requests(
                &graph,
                r#"g.inject(0).V().hasLabel("person").values("age").toList()"#
            )
        );
        // A lookup of software by id after FOR: allowed like g.inject(0).V().hasLabel("software").
        let software = r#"g.gql("FOR id IN ['3'] MATCH (s:software) WHERE element_id(s) = id RETURN s.lang").toList()"#;
        assert_eq!(
            run(&graph, software, NoPersons).is_ok(),
            run(
                &graph,
                r#"g.inject(0).V().hasLabel("software").values("lang").toList()"#,
                NoPersons
            )
            .is_ok(),
            "{software}"
        );
        assert_eq!(
            requests(&graph, software),
            requests(
                &graph,
                r#"g.inject(0).V().hasLabel("software").values("lang").toList()"#
            )
        );
    }

    /// Stage 2b: a top-level `[NOT] EXISTS` (or a pattern predicate) is a `filter(__..)` /
    /// `not(__..)` child, and the authorization walk visits it: an edge read reached only inside
    /// the subquery (the query's own pattern reads vertices only) is refused before the run, at
    /// the child's step, like the Gremlin twin `where(__.out(..))` / `not(__.out(..))`; allowed,
    /// it asks what the twin asks.
    #[test]
    fn a_denied_read_inside_exists_refuses_gql_like_where() {
        let graph = modern();
        for script in [
            r#"g.V().hasLabel("person").where(__.out("created")).values("name").toList()"#,
            r#"g.gql("MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name").toList()"#,
            r#"g.gql("MATCH (p:person) WHERE (p)-[:created]->() RETURN p.name").toList()"#,
            r#"g.V().hasLabel("person").not(__.out("created")).values("name").toList()"#,
            r#"g.gql("MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name").toList()"#,
            r#"g.gql("MATCH (p:person) FILTER NOT EXISTS { MATCH (p)-[:created]->(s) WHERE s.lang = 'java' } RETURN p.name").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // Nested: the edge read sits in the inner child only.
        for script in [
            r#"g.V().hasLabel("person").as("p").where(__.V().hasLabel("software").where(__.in("created").where(P.eq("p")))).values("name").toList()"#,
            r#"g.gql("MATCH (p:person) WHERE EXISTS { MATCH (q:software) WHERE EXISTS { (q)<-[:created]-(p) } } RETURN p.name").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "in", "Read Data(edge)");
        }
        // The node pattern alone is allowed.
        assert!(
            run(
                &graph,
                r#"g.gql("MATCH (p:person) RETURN p.name").toList()"#,
                NoEdges
            )
            .is_ok()
        );
        // Refused before it runs, also when no row would reach the child.
        let err = run(
            &graph,
            r#"g.gql("MATCH (p:nobody) WHERE NOT (p)-->() RETURN p").toList()"#,
            NoEdges,
        )
        .unwrap_err();
        assert!(err.contains("edges are private"), "{err}");
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person) WHERE EXISTS { (p)-[:created]->() } RETURN p.name").toList()"#,
                r#"g.V().hasLabel("person").where(__.out("created")).values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) WHERE NOT (p)-[:created]->() RETURN p.name").toList()"#,
                r#"g.V().hasLabel("person").not(__.out("created")).values("name").toList()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
    }

    /// Stage 2b.T5: a subquery anywhere in an expression runs as a child of the `gql:*` step
    /// evaluating it (`gql:filter`, `gql:project`, `gql:order`, `gql:aggregate`, `gql:for`); the
    /// authorization walk visits those children, so an edge read reached only there is refused
    /// before the run, also when no row would reach it, like the Gremlin `where(__.out(..))`.
    #[test]
    fn a_denied_read_inside_a_hosted_exists_refuses_gql() {
        let graph = modern();
        for script in [
            // gql:filter: under OR
            r#"g.gql("MATCH (p:person) WHERE p.age > 30 OR EXISTS { (p)-[:created]->() } RETURN p.name").toList()"#,
            // gql:filter: under CASE
            r#"g.gql("MATCH (p:person) WHERE CASE WHEN (p)-[:created]->() THEN true ELSE false END RETURN p.name").toList()"#,
            // gql:project: a RETURN column
            r#"g.gql("MATCH (p:person) RETURN p.name AS n, EXISTS { (p)-[:created]->() } AS c").toList()"#,
            // gql:order: an ORDER BY key
            r#"g.gql("MATCH (p:person) RETURN p.name AS n ORDER BY EXISTS { (p)-[:created]->() }").toList()"#,
            // gql:aggregate: an aggregate's argument and a grouping key
            r#"g.gql("MATCH (p:person) RETURN count(CASE WHEN (p)-[:created]->() THEN 1 END) AS c").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN EXISTS { (p)-[:created]->() } AS e, count(*) AS c").toList()"#,
            // gql:for: the list
            r#"g.gql("MATCH (p:person) FOR x IN [EXISTS { (p)-[:created]->() }] RETURN x").toList()"#,
            // no row reaches the host
            r#"g.gql("MATCH (p:nobody) RETURN EXISTS { (p)-[:created]->() } AS c").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // Without the subquery every query above is allowed.
        assert!(
            run(
                &graph,
                r#"g.gql("MATCH (p:person) WHERE p.age > 30 RETURN p.name AS n ORDER BY n").toList()"#,
                NoEdges
            )
            .is_ok()
        );
        // Allowed, a hosted child asks what the Gremlin twin's child asks.
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person) RETURN EXISTS { (p)-[:created]->() } AS c").toList()"#,
                r#"g.V().hasLabel("person").where(__.out("created")).values("name").toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) WHERE p.age > 30 OR (p)-[:created]->() RETURN p.name").toList()"#,
                r#"g.V().hasLabel("person").where(__.out("created")).values("name").toList()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
    }

    /// Stage 2c.T3: a pattern comprehension runs as a child of the `gql:*` step evaluating it
    /// (ending in `gql:value`); the authorization walk visits it in every host and nested in
    /// another comprehension's `|` part, so an edge read reached only there is refused before
    /// the run, also when no row reaches it, like the Gremlin `project().by(__.out(..).fold())`.
    #[test]
    fn a_denied_read_inside_a_pattern_comprehension_refuses_gql() {
        let graph = modern();
        for script in [
            // gql:project: a RETURN column, inside a map projection
            r#"g.gql("MATCH (p:person) RETURN [(p)-[:created]->(s) | s.name] AS l").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN p {.name, made: [(p)-[:created]->(s) | s.name]} AS m").toList()"#,
            // gql:filter: WHERE
            r#"g.gql("MATCH (p:person) WHERE size([(p)-[:created]->(s) | s]) > 1 RETURN p.name").toList()"#,
            // gql:order: an ORDER BY key
            r#"g.gql("MATCH (p:person) RETURN p.name AS n ORDER BY size([(p)-[:created]->(s) | s])").toList()"#,
            // gql:aggregate: an aggregate's argument
            r#"g.gql("MATCH (p:person) RETURN collect_list([(p)-[:created]->(s) | s.name]) AS c").toList()"#,
            // gql:for: the list
            r#"g.gql("MATCH (p:person) FOR x IN [(p)-[:created]->(s) | s.name] RETURN x").toList()"#,
            // nested in another comprehension's `|` part (gql:value hosts it)
            r#"g.gql("MATCH (p:person) RETURN [(p)-[:knows]->(f) | [(f)-[:created]->(s) | s.name]] AS l").toList()"#,
            // no row reaches the host
            r#"g.gql("MATCH (p:nobody) RETURN [(p)-[:created]->(s) | s.name] AS l").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // Allowed, the hosted children (the nested one under `gql:value` included) ask what
        // the Gremlin twin's children ask, request for request.
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person) RETURN [(p)-[:created]->(s) | s.name] AS l").toList()"#,
                r#"g.V().hasLabel("person").project("l").by(__.out("created").values("name").fold()).toList()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN [(p)-[:knows]->(f) | [(f)-[:created]->(s) | s.name]] AS l").toList()"#,
                r#"g.V().hasLabel("person").project("l").by(__.out("knows").map(__.out("created").values("name").fold()).fold()).toList()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
    }

    /// Stage 2b.T6: a vertex read reached only inside a subquery (the unit row and `FOR`
    /// read none outside it; a read request states no label, so a denied label is a denied
    /// vertex read) is refused before the run like the Gremlin twin, on the fast path and
    /// through every hook, also when no row reaches the subquery; allowed, the query asks what
    /// the twin asks.
    #[test]
    fn a_denied_read_reached_only_inside_exists_refuses_gql_like_its_twin() {
        let graph = modern();
        // Outside the subqueries these queries read no vertex and are allowed.
        assert!(
            run(
                &graph,
                r#"g.gql("FOR x IN [1, 2] FILTER x > 1 RETURN x ORDER BY x").toList()"#,
                NoPersons
            )
            .is_ok()
        );
        for script in [
            r#"g.inject(0).where(__.V().hasLabel("person").has("age", P.gt(30))).toList()"#,
            // fast path: filter() / not() on the unit row and after FOR
            r#"g.gql("FILTER EXISTS { MATCH (p:person) WHERE p.age > 30 } RETURN 1 AS x").toList()"#,
            r#"g.gql("FOR n IN ['marko'] FILTER NOT EXISTS { MATCH (p:person {name: n}) } RETURN n").toList()"#,
            // hooks: gql:filter, gql:project, gql:order, gql:aggregate, gql:for
            r#"g.gql("FOR x IN [1] FILTER x = 2 OR EXISTS { MATCH (p:person) } RETURN x").toList()"#,
            r#"g.gql("RETURN EXISTS { MATCH (p:person) } AS e").toList()"#,
            r#"g.gql("FOR x IN [1, 2] RETURN x ORDER BY EXISTS { MATCH (p:person) WHERE p.age = x }").toList()"#,
            r#"g.gql("FOR x IN [1, 2] RETURN count(CASE WHEN EXISTS { MATCH (p:person) } THEN x END) AS c").toList()"#,
            r#"g.gql("FOR x IN [EXISTS { MATCH (p:person) }] RETURN x").toList()"#,
            // no row reaches the subquery
            r#"g.gql("FOR x IN [] FILTER EXISTS { MATCH (p:person) } RETURN x").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoPersons), "v", "Read Data(vertex)");
        }
        // Allowed, the fast path asks what the Gremlin twin asks.
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("FILTER EXISTS { MATCH (p:person) WHERE p.age > 30 } RETURN 1 AS x").toList()"#
            ),
            requests(
                &graph,
                r#"g.inject(0).where(__.V().hasLabel("person").has("age", P.gt(30))).toList()"#
            )
        );
    }
    /// Stage 2d.T5: a `CALL { }` body is the child of `flat_map(__..)` (`OPTIONAL CALL` inside
    /// `optional(__..)`), and the authorization walk visits it: an edge read reached only in the
    /// body (the query's own pattern reads vertices only) is refused before the run, at the
    /// body's step, also when no row reaches the `CALL`, nested, in a `UNION` operand of the
    /// body, like the Gremlin `flatMap(__.out(..))`; a vertex read only in an uncorrelated body
    /// likewise; allowed, it asks what the twin asks.
    #[test]
    fn a_denied_read_inside_a_call_body_refuses_gql_like_flat_map() {
        let graph = modern();
        for script in [
            r#"g.V().hasLabel("person").flatMap(__.out("created")).toList()"#,
            r#"g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s } RETURN s.name").toList()"#,
            r#"g.gql("MATCH (p:person) OPTIONAL CALL (p) { MATCH (p)-[:created]->(s) RETURN s } RETURN s.name").toList()"#,
            r#"g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN count(s) AS c } RETURN c").toList()"#,
            r#"g.gql("MATCH (p:person) CALL (p) { RETURN 1 AS x UNION ALL MATCH (p)-[:created]->(s) RETURN 2 AS x } RETURN x").toList()"#,
            r#"g.gql("MATCH (p:person) CALL (p) { CALL (p) { MATCH (p)-[:created]->(s) RETURN s } RETURN s } RETURN s.name").toList()"#,
            r#"g.gql("MATCH (p:nobody) CALL (p) { MATCH (p)-[:created]->(s) RETURN s } RETURN s").toList()"#,
            r#"g.gql("CALL { MATCH (p:person)-[:created]->(s) RETURN s } RETURN s").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        for script in [
            r#"g.gql("FOR x IN [1] CALL { MATCH (p:person) RETURN p.name AS n } RETURN n").toList()"#,
            r#"g.gql("FOR x IN [1] OPTIONAL CALL (x) { MATCH (p:person) WHERE p.age = x RETURN p } RETURN p").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoPersons), "v", "Read Data(vertex)");
        }
        // Without the body the query is allowed.
        assert!(
            run(
                &graph,
                r#"g.gql("MATCH (p:person) CALL (p) { RETURN p.name AS n } RETURN n").toList()"#,
                NoEdges
            )
            .is_ok()
        );
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("MATCH (p:person) CALL (p) { MATCH (p)-[:created]->(s) RETURN s } RETURN s.name").toList()"#
            ),
            requests(
                &graph,
                r#"g.V().hasLabel("person").flatMap(__.out("created")).values("name").toList()"#
            )
        );
    }

    /// Stage 2d.T6: a read only a later `UNION` operand or a block after `NEXT` makes is asked
    /// like the Gremlin `union()` twin: the authorization walk visits every operand child of the
    /// start `union()` (and of a mid-traversal one after `NEXT`), also when the first operand
    /// reads nothing denied.
    #[test]
    fn a_denied_read_reached_only_in_a_union_operand_refuses_gql_like_union() {
        let graph = modern();
        for script in [
            r#"g.union(__.inject(1), __.V().hasLabel("person").out("created")).toList()"#,
            r#"g.gql("RETURN 1 AS x UNION ALL MATCH (p:person)-[:created]->(s) RETURN 2 AS x").toList()"#,
            r#"g.gql("RETURN 1 AS x UNION MATCH (p:person)-[:created]->(s) RETURN 2 AS x").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN p.name AS n UNION MATCH (p:person)-[:created]->(s) RETURN s.name AS n").toList()"#,
            r#"g.gql("RETURN 1 AS x NEXT RETURN x UNION MATCH (p:person)-[:created]->(s) RETURN 2 AS x").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
            r#"g.gql("MATCH (p:nobody) RETURN p NEXT MATCH (p)-[:created]->(s) RETURN s.name").toList()"#,
            r#"g.gql("RETURN 1 AS x UNION ALL MATCH (p:nobody)-[:created]->(s) RETURN 2 AS x").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // A vertex read only in a later operand.
        for script in [
            r#"g.gql("RETURN 1 AS x UNION ALL MATCH (p:person) RETURN 2 AS x").toList()"#,
            r#"g.gql("RETURN 1 AS x UNION RETURN 3 AS x NEXT MATCH (p:person) RETURN x").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoPersons), "v", "Read Data(vertex)");
        }
        // Without the denied operand the query is allowed.
        assert!(
            run(
                &graph,
                r#"g.gql("RETURN 1 AS x UNION ALL MATCH (p:person) RETURN 2 AS x").toList()"#,
                NoEdges
            )
            .is_ok()
        );
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("RETURN 1 AS x UNION ALL MATCH (p:person)-[:created]->(s) RETURN 2 AS x").toList()"#
            ),
            requests(
                &graph,
                r#"g.union(__.inject(1), __.V().hasLabel("person").out("created")).toList()"#
            )
        );
    }

    /// Stage 2d.T5c: a read only a `COUNT { }` or `COLLECT { }` body (or a whole-query
    /// `EXISTS` body) makes is asked like the Gremlin `local(__..count())` / `fold()` twin:
    /// the authorization walk visits the children of the hosting `gql:*` step (MATCH-only and
    /// whole-query bodies, under every host, nested, uncorrelated).
    #[test]
    fn a_denied_read_inside_count_or_collect_refuses_gql_like_local() {
        let graph = modern();
        for script in [
            r#"g.V().hasLabel("person").local(__.out("created").count()).toList()"#,
            r#"g.V().hasLabel("person").local(__.out("created").values("name").fold()).toList()"#,
            r#"g.gql("MATCH (p:person) RETURN COUNT { (p)-[:created]->() } AS c").toList()"#,
            r#"g.gql("MATCH (p:person) WHERE COUNT { (p)-[:created]->() } > 0 RETURN p.name").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN p.name AS n ORDER BY COUNT { (p)-[:created]->() }").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN sum(COUNT { (p)-[:created]->() }) AS c").toList()"#,
            r#"g.gql("MATCH (p:person) FOR x IN [COUNT { (p)-[:created]->() }] RETURN x").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN COUNT { MATCH (p)-[:created]->(s) RETURN s.name AS x UNION RETURN 1 AS x } AS c").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN COLLECT { MATCH (p)-[:created]->(s) RETURN s.name } AS l").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN COLLECT { MATCH (p)-[:created]->(s) WITH s ORDER BY s.name RETURN s.name } AS l").toList()"#,
            r#"g.gql("MATCH (p:person) RETURN COLLECT { RETURN COUNT { (p)-[:created]->() } AS c } AS l").toList()"#,
            r#"g.gql("MATCH (p:person) WHERE EXISTS { MATCH (p)-[:created]->(s) RETURN count(s) AS c } RETURN p.name").toList()"#,
            r#"g.gql("RETURN COUNT { MATCH (p:person)-[:created]->(s) } AS c").toList()"#,
            r#"g.gql("RETURN COLLECT { MATCH (p:person)-[:created]->(s) RETURN s } AS l").toList()"#,
            r#"g.gql("MATCH (p:nobody) RETURN COLLECT { MATCH (p)-[:created]->(s) RETURN s } AS l").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "out", "Read Data(edge)");
        }
        // A vertex read only a body's child-leading scan makes (uncorrelated, or correlated by
        // a condition no index serves).
        for script in [
            r#"g.gql("FOR x IN [1] RETURN COUNT { MATCH (p:person) } AS c").toList()"#,
            r#"g.gql("FOR x IN [1] RETURN COLLECT { MATCH (p:person) RETURN p.name } AS l").toList()"#,
            r#"g.gql("FOR x IN [29] RETURN COUNT { MATCH (p:person) WHERE p.age = x } AS c").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoPersons), "v", "Read Data(vertex)");
        }
        // Without the body the query is allowed.
        assert!(
            run(
                &graph,
                r#"g.gql("MATCH (p:person) RETURN COLLECT { RETURN p.name AS n } AS l").toList()"#,
                NoEdges
            )
            .is_ok()
        );
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("MATCH (p:person) RETURN COUNT { (p)-[:created]->() } AS c").toList()"#
            ),
            requests(
                &graph,
                r#"g.V().hasLabel("person").local(__.out("created").count()).toList()"#
            )
        );
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("MATCH (p:person) RETURN COLLECT { MATCH (p)-[:created]->(s) RETURN s.name } AS l").toList()"#
            ),
            requests(
                &graph,
                r#"g.V().hasLabel("person").local(__.out("created").values("name").fold()).toList()"#
            )
        );
    }

    /// Stage 2e.T5: a parenthesized path pattern is `repeat(__.<body>)` (or its body inline),
    /// and the authorization walk visits the body: an edge read only a body hop makes (the
    /// outer pattern reads vertices only) is refused before the run at the body's edge step,
    /// like the Gremlin `repeat(__.outE(..).inV())` twin, for every quantifier, walked from
    /// either end, in a two-hop body, inside `OPTIONAL MATCH`, `EXISTS`, a pattern predicate, a
    /// pattern comprehension, `COUNT { }` and `CALL`, also when no row reaches it. A vertex read
    /// only a body's node label asks (a read states no label) is refused likewise when it is
    /// the only vertex read, in an uncorrelated subquery body. Allowed, it asks what the twin
    /// asks.
    #[test]
    fn a_denied_read_inside_a_parenthesized_path_pattern_refuses_gql_like_repeat() {
        let graph = modern();
        // Without a quantifier (and with `?`, one iteration at most) an unread edge is the 1a
        // `out()` hop; a repeating body labels its edges, so its hop is `out_e()`.
        for (script, step) in [
            (
                r#"g.V().hasLabel("person").repeat(__.outE("knows").inV()).emit().times(2).values("name").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) ((x)-[:knows]->(y)){1,2} (f) RETURN f.name").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) ((x)-[e:knows]->(y)) (f) RETURN f.name").toList()"#,
                "out",
            ),
            (
                r#"g.gql("MATCH (p:person) ((x)-[:knows]->(y))? (f) RETURN f.name").toList()"#,
                "out",
            ),
            (
                r#"g.gql("MATCH (p:person) ((x)-[:knows]->(y))* (f) RETURN count(*) AS c").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) ((x)-[:knows]->(m)-[:created]->(y)){1,2} (f) RETURN f.name").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:nobody) ((x)-[:knows]->(y)){1,2} (f) RETURN f").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) OPTIONAL MATCH (p) ((x)-[:knows]->(y)){1,2} (f) RETURN f.name").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) WHERE EXISTS { (p) ((x)-[:knows]->(y)){1,2} } RETURN p.name").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) WHERE (p) (()-[:knows]->()){1,2} RETURN p.name").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN [(p) ((x)-[:knows]->(y)){1,2} (f) | f.name] AS l").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) RETURN COUNT { (p) ((x)-[:knows]->(y))+ } AS c").toList()"#,
                "out_e",
            ),
            (
                r#"g.gql("MATCH (p:person) CALL (p) { MATCH (p) ((x)-[:knows]->(y)){1,2} (f) RETURN f } RETURN f.name").toList()"#,
                "out_e",
            ),
        ] {
            assert_denied(run(&graph, script, NoEdges), step, "Read Data(edge)");
        }
        // Walked from the far end the hop is reversed: the edge step is in_e.
        for script in [
            r#"g.V().has("name", "lop").repeat(__.inE("created").outV()).times(1).values("name").toList()"#,
            r#"g.gql("MATCH (a) ((x)-[:created]->(y)){1} (s:software {name: 'lop'}) RETURN a.name").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoEdges), "in_e", "Read Data(edge)");
        }
        // Either direction: the first hop of the `union()` of both.
        assert_denied(
            run(
                &graph,
                r#"g.gql("MATCH (p:person) ((x)-[:knows]-(y)){1,2} (f) RETURN f.name").toList()"#,
                NoEdges,
            ),
            "out_e",
            "Read Data(edge)",
        );
        // A vertex read that only a body's node pattern (with its label) makes.
        for script in [
            r#"g.gql("FOR i IN [1] RETURN COUNT { MATCH ((x:person)-[:knows]->(y)){1,2} } AS c").toList()"#,
            r#"g.gql("FOR i IN [] RETURN COLLECT { MATCH ((x:person)-[:knows]->(y:person))+ RETURN y } AS l").toList()"#,
        ] {
            assert_denied(run(&graph, script, NoPersons), "v", "Read Data(vertex)");
        }
        // Allowed, a parenthesized path pattern asks what its Gremlin twin asks.
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("MATCH (p:person) ((x)-[:knows]->(y)){1,2} (f) RETURN f.name").toList()"#
            ),
            requests(
                &graph,
                r#"g.V().hasLabel("person").repeat(__.outE("knows").inV()).emit().times(2).values("name").toList()"#
            )
        );
        for query in [
            "MATCH (p:person) ((x)-[e:knows]->(m)<-[f]-(y) WHERE m.age > 20)? (b) RETURN p, b",
            "MATCH p = (a) ((x)-->(y)){1,2} (b) RETURN p",
            "MATCH (a) WHERE (a) (()-->()){2} RETURN [(a) ((x)-->(y))+ | y] AS l",
        ] {
            let script = format!("g.gql({query:?}).toList()");
            let seen = requests(&graph, &script);
            assert!(
                seen.iter().all(|request| request.starts_with("Read ")),
                "{query}: {seen:?}"
            );
        }
    }

    /// Denies every write of data labelled `secret` and of data whose label the request leaves
    /// open (it could be `secret`); reads and other writes are allowed.
    struct NoSecret;

    impl Authorizer for NoSecret {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
            let writes = matches!(
                request.action,
                Action::Create | Action::Update | Action::Delete
            );
            match request.resource {
                Resource::Data { label, .. }
                    if writes && label.is_none_or(|label| label == "secret") =>
                {
                    Err(Denied::new("secret data is protected"))
                }
                _ => Ok(()),
            }
        }
    }

    /// Stage 3a: `INSERT` asks `Create` per label of each new node and per new edge, exactly
    /// like `add_v(labels)` / `add_e(label)`, and nothing for its properties.
    #[test]
    fn gql_insert_asks_create_like_add_v_and_add_e() {
        let graph = modern();
        for (query, twin) in [
            (
                r#"g.gql("INSERT (:x)").iterate()"#,
                r#"g.addV("x").iterate()"#,
            ),
            (
                r#"g.gql("INSERT (:a&b)").iterate()"#,
                r#"g.addV(["a", "b"]).iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) INSERT (p)-[:r]->(:x)").iterate()"#,
                r#"g.V().hasLabel("person").as("p").addV("x").addE("r").from("p").iterate()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
        assert_eq!(
            requests(&graph, r#"g.gql("INSERT (:x)-[:r]->(:y)").iterate()"#),
            [
                "Create Data(vertex \"x\")",
                "Create Data(vertex \"y\")",
                "Create Data(edge \"r\")",
            ]
        );
    }

    /// The one deliberate difference from the Gremlin twin (3a.T3): `INSERT` sets a new
    /// element's properties as part of creating it and asks only `Create`; `add_v(..)
    /// .property(..)` asks `Create` and `Update` (the authorization check runs on the plan as
    /// written, before the optimizer folds the property into the creation). A policy denying
    /// only `Update` therefore allows the GQL insert and refuses its Gremlin twin.
    #[test]
    fn gql_insert_with_properties_asks_only_create_unlike_add_v_property() {
        let graph = modern();
        assert_eq!(
            requests(&graph, r#"g.gql("INSERT (:x {k: 1})").iterate()"#),
            ["Create Data(vertex \"x\")"]
        );
        assert_eq!(
            requests(&graph, r#"g.addV("x").property("k", 1).iterate()"#),
            ["Create Data(vertex \"x\")", "Update Data(vertex \"x\")"]
        );
        let no_update = || AccessPolicy::allow_all().deny(ActionKind::Update, ResourceKind::Data);
        let before = count(&graph);
        assert!(
            run(
                &graph,
                r#"g.gql("INSERT (:x {k: 1})").iterate()"#,
                no_update()
            )
            .is_ok()
        );
        assert_eq!(count(&graph), before + 1);
        assert_denied(
            run(
                &graph,
                r#"g.addV("x").property("k", 1).iterate()"#,
                no_update(),
            ),
            "property",
            "Update Data(vertex \"x\")",
        );
        assert_eq!(count(&graph), before + 1);
    }

    /// Label-level denials of `INSERT` mirror `add_v` / `add_e`: the same request, refused before
    /// anything runs.
    #[test]
    fn a_denied_label_refuses_gql_insert_like_add_v_and_add_e() {
        let graph = modern();
        assert!(run(&graph, r#"g.gql("INSERT (:public)").iterate()"#, NoSecret).is_ok());
        assert_eq!(count(&graph), 7);
        for (script, step) in [
            (r#"g.addV("secret").iterate()"#, "add_v"),
            (r#"g.gql("INSERT (:secret)").iterate()"#, "gql:insert_node"),
            (
                r#"g.gql("CREATE (:secret {k: 1})").iterate()"#,
                "gql:insert_node",
            ),
            // A multi-label vertex asks once per label.
            (r#"g.addV(["public", "secret"]).iterate()"#, "add_v"),
            (
                r#"g.gql("INSERT (:public&secret)").iterate()"#,
                "gql:insert_node",
            ),
            // The MATCH before it would run first: refused before it does.
            (
                r#"g.gql("MATCH (p:person) INSERT (:public), (:secret)").iterate()"#,
                "gql:insert_node",
            ),
        ] {
            assert_denied(
                run(&graph, script, NoSecret),
                step,
                "Create Data(vertex \"secret\")",
            );
        }
        for (script, step) in [
            (
                r#"g.V("1").addE("secret").to(__.V("2")).iterate()"#,
                "add_e",
            ),
            (
                r#"g.gql("MATCH (a {name: 'marko'}), (b {name: 'josh'}) INSERT (a)-[:secret]->(b)").iterate()"#,
                "gql:insert_edge",
            ),
            (
                r#"g.gql("INSERT (:public)-[:secret]->(:public)").iterate()"#,
                "gql:insert_edge",
            ),
        ] {
            assert_denied(
                run(&graph, script, NoSecret),
                step,
                "Create Data(edge \"secret\")",
            );
        }
        assert_eq!(count(&graph), 7);
        assert_eq!(graph.read().edge_count(), 6);
    }

    /// A read-only host (`AccessPolicy::read_only()`, and the policy of the playground's and the
    /// MCP server's read-only mode: every Create / Update / Delete on data, schema and
    /// definitions denied) refuses a GQL write before anything runs, and still runs GQL reads.
    #[test]
    fn a_read_only_host_refuses_a_gql_write_before_it_runs() {
        let graph = modern();
        let mut mcp_read_only = AccessPolicy::allow_all().deny_resource(ResourceKind::File);
        for resource in [
            ResourceKind::Data,
            ResourceKind::Schema,
            ResourceKind::Definition,
        ] {
            for action in [ActionKind::Create, ActionKind::Update, ActionKind::Delete] {
                mcp_read_only = mcp_read_only.deny(action, resource);
            }
        }
        for policy in [AccessPolicy::read_only(), mcp_read_only] {
            for (script, step, request) in [
                (
                    r#"g.gql("INSERT (:x)").iterate()"#,
                    "gql:insert_node",
                    "Create Data(vertex \"x\")",
                ),
                (
                    r#"g.gql("MATCH (a {name: 'marko'}), (b {name: 'josh'}) INSERT (a)-[:r]->(b)").iterate()"#,
                    "gql:insert_edge",
                    "Create Data(edge \"r\")",
                ),
                (
                    r#"g.gql("MATCH (p:person) CALL (p) { INSERT (p)-[:r]->(:x) } RETURN p.name").toList()"#,
                    "gql:insert_node",
                    "Create Data(vertex \"x\")",
                ),
            ] {
                assert_denied(run(&graph, script, policy.clone()), step, request);
            }
            let rows = run(
                &graph,
                r#"g.gql("MATCH (p:person) RETURN p.name").toList()"#,
                policy,
            )
            .unwrap();
            assert_eq!(rows.into_array().unwrap().len(), 4);
        }
        assert_eq!(count(&graph), 6);
        assert_eq!(graph.read().edge_count(), 6);
    }

    /// A saved query whose body calls `g.gql("INSERT ...")` writes (dialect `/2`): its call asks
    /// the effects (the labels the insert creates) of the caller first, so a host without the
    /// right is refused before the body runs, like a Gremlin writing query.
    #[test]
    fn a_saved_query_running_a_gql_insert_is_bounded_by_the_callers_rights() {
        let graph = modern();
        let _ = run(
            &graph,
            r#"g.define_query(#{name: "adopt", body: `fn adopt() {
                g.gql("MATCH (p:person {name: 'marko'}) INSERT (p)-[:owns]->(:secret)").iterate()
            }`})"#,
            AllowAll,
        )
        .unwrap();
        let recorder = Arc::new(Recorder::default());
        let _ = run(&graph, r#"g.query("adopt")"#, recorder.clone()).unwrap();
        let seen = recorder.seen();
        assert_eq!(
            seen[..3],
            [
                r#"Execute Definition(query "adopt")"#.to_string(),
                r#"Create Data(vertex "secret")"#.to_string(),
                r#"Create Data(edge "owns")"#.to_string(),
            ],
            "{seen:?}"
        );
        assert_eq!(count(&graph), 7);
        let err = run(&graph, r#"g.query("adopt")"#, NoSecret).unwrap_err();
        assert!(
            err.contains(
                r#"Permission denied for 'query("adopt")' (Create Data(vertex "secret"))"#
            ),
            "{err}"
        );
        assert_denied(
            run(&graph, r#"g.query("adopt")"#, AccessPolicy::read_only()),
            "query(\"adopt\")",
            "Create Data(vertex \"secret\")",
        );
        assert_eq!(count(&graph), 7);
    }

    /// Stage 3c: `SET` / `REMOVE` ask `Update` on the data of each item's target exactly like
    /// their Gremlin twins `property()`, `remove_property()`, `add_label()`, `drop_label()`
    /// (design C6, D3): a node `Data(vertex)`, an edge of a stated label `Data(edge "label")`,
    /// a target whose kind only the run knows `Data` (each request once).
    #[test]
    fn gql_set_and_remove_ask_update_like_their_gremlin_twins() {
        let graph = modern();
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person) SET p.k = 1").iterate()"#,
                r#"g.v().has_label("person").property("k", 1).iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) REMOVE p.age").iterate()"#,
                r#"g.v().has_label("person").remove_property("age").iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) SET p:A:B").iterate()"#,
                r#"g.v().has_label("person").add_label("A").add_label("B").iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p:person) REMOVE p:A").iterate()"#,
                r#"g.v().has_label("person").drop_label("A").iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p)-[e:knows]->() SET e.weight = 1").iterate()"#,
                r#"g.v().out_e("knows").property("weight", 1).iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p)-[e]->() REMOVE e.weight").iterate()"#,
                r#"g.v().out_e().remove_property("weight").iterate()"#,
            ),
        ] {
            assert_eq!(requests(&graph, query), requests(&graph, twin), "{query}");
        }
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("MATCH (p:person)-[e:knows|created]->(x) SET p.k = 1, e.k = 2, p:L").iterate()"#
            ),
            [
                "Read Data(vertex)",
                "Read Data(edge)",
                "Update Data(vertex)",
                "Update Data(edge \"knows\")",
                "Update Data(edge \"created\")",
            ]
        );
        // A FOR / UNWIND item: the kind is known at run time only.
        assert_eq!(
            requests(
                &graph,
                r#"g.gql("MATCH (p:person) WITH collect(p) AS ps FOR x IN ps SET x.k = 1").iterate()"#
            ),
            ["Read Data(vertex)", "Update Data"]
        );
    }

    /// A denied `Update` refuses a GQL `SET` / `REMOVE` before it runs, with the same request
    /// as the Gremlin twin; an edge label the policy allows lets the edge write through.
    #[test]
    fn a_denied_update_refuses_gql_set_and_remove_like_their_gremlin_twins() {
        let graph = modern();
        let no_update = || AccessPolicy::allow_all().deny(ActionKind::Update, ResourceKind::Data);
        for (script, step) in [
            (
                r#"g.v().has_label("person").property("k", 1).iterate()"#,
                "property",
            ),
            (
                r#"g.gql("MATCH (p:person) SET p.k = 1").iterate()"#,
                "gql:set",
            ),
            (
                r#"g.gql("MATCH (p:person) REMOVE p.age").iterate()"#,
                "gql:remove",
            ),
            (r#"g.gql("MATCH (p) SET p:Seen").iterate()"#, "gql:set"),
            (
                r#"g.gql("MATCH (p) CALL (p) { REMOVE p:person } RETURN p").toList()"#,
                "gql:remove",
            ),
        ] {
            assert_denied(
                run(&graph, script, no_update()),
                step,
                "Update Data(vertex)",
            );
            assert_denied(run(&graph, script, NoSecret), step, "Update Data(vertex)");
            assert_denied(
                run(&graph, script, AccessPolicy::read_only()),
                step,
                "Update Data(vertex)",
            );
        }
        // Label-level: an edge whose pattern states its label asks for that label only.
        assert!(
            run(
                &graph,
                r#"g.gql("MATCH ()-[e:knows]->() SET e.weight = 0.5").iterate()"#,
                NoSecret,
            )
            .is_ok()
        );
        for (script, step) in [
            (
                r#"g.e().has_label("secret").property("weight", 1).iterate()"#,
                "property",
            ),
            (
                r#"g.gql("MATCH ()-[e:secret]->() SET e.weight = 1").iterate()"#,
                "gql:set",
            ),
        ] {
            assert_denied(
                run(&graph, script, NoSecret),
                step,
                "Update Data(edge \"secret\")",
            );
        }
        assert_denied(
            run(
                &graph,
                r#"g.gql("MATCH ()-[e]->() REMOVE e.weight").iterate()"#,
                NoSecret,
            ),
            "gql:remove",
            "Update Data(edge)",
        );
        // Only the allowed edge write happened.
        let rows = run(
            &graph,
            r#"g.gql("MATCH (p) WHERE p.k IS NOT NULL OR NOT p:person RETURN p").toList()"#,
            AllowAll,
        )
        .unwrap();
        assert_eq!(rows.into_array().unwrap().len(), 2);
        let weights = run(
            &graph,
            r#"g.e().has_label("knows").values("weight").toList()"#,
            AllowAll,
        )
        .unwrap();
        assert_eq!(weights.to_string(), "[0.5, 0.5]");
        assert_eq!(
            run(
                &graph,
                r#"g.v().has_label("person").count().next()"#,
                AllowAll
            )
            .unwrap()
            .as_int()
            .ok(),
            Some(4)
        );
    }

    /// Stage 3d: `DELETE` asks `Delete` on the data of each target exactly like its `drop()`
    /// twin (design C3): a node `Data(vertex)`, an edge of a stated label `Data(edge "label")`,
    /// a path, a list or a value whose kind only the run knows `Data`; `DETACH` asks nothing
    /// more (as `drop()` of a vertex).
    #[test]
    fn gql_delete_asks_delete_like_its_drop_twin() {
        for (query, twin) in [
            (
                r#"g.gql("MATCH (p:person) DETACH DELETE p").iterate()"#,
                r#"g.v().has_label("person").drop().iterate()"#,
            ),
            (
                r#"g.gql("MATCH (p:person)-[e]-() DELETE p, e").iterate()"#,
                r#"g.v().has_label("person").side_effect(__.both_e().drop()).drop().iterate()"#,
            ),
            (
                r#"g.gql("MATCH ()-[e:knows]->() DELETE e").iterate()"#,
                r#"g.v().out_e("knows").drop().iterate()"#,
            ),
            (
                r#"g.gql("MATCH ()-[e]->() DELETE e").iterate()"#,
                r#"g.v().out_e().drop().iterate()"#,
            ),
            (
                r#"g.gql("MATCH x = (p {name: 'vadas'})<--() DETACH DELETE x").iterate()"#,
                r#"g.v().has("name", "vadas").in().path().unfold().drop().iterate()"#,
            ),
            (
                r#"g.gql("MATCH (s:software) WITH collect(s) AS l DETACH DELETE l").iterate()"#,
                r#"g.v().has_label("software").fold().unfold().drop().iterate()"#,
            ),
        ] {
            let mut gql = requests(&modern(), query);
            let mut gremlin = requests(&modern(), twin);
            gql.sort();
            gql.dedup();
            gremlin.sort();
            gremlin.dedup();
            assert_eq!(gql, gremlin, "{query}");
        }
        assert_eq!(
            requests(
                &modern(),
                r#"g.gql("MATCH (p:person)-[e:knows|created]->(x) DELETE e DETACH DELETE p, [x]").iterate()"#
            ),
            [
                "Read Data(vertex)",
                "Read Data(edge)",
                "Delete Data(edge \"knows\")",
                "Delete Data(edge \"created\")",
                "Delete Data(vertex)",
                "Delete Data",
            ]
        );
    }

    /// A denied `Delete` refuses a GQL `DELETE` before it runs, with the same request as the
    /// `drop()` twin; an edge label the policy allows lets the edge delete through.
    #[test]
    fn a_denied_delete_refuses_gql_delete_like_its_drop_twin() {
        let graph = modern();
        let no_delete = || AccessPolicy::allow_all().deny(ActionKind::Delete, ResourceKind::Data);
        for (script, step) in [
            (r#"g.v().has_label("person").drop().iterate()"#, "drop"),
            (
                r#"g.gql("MATCH (p:person) DETACH DELETE p").iterate()"#,
                "gql:delete",
            ),
            (
                r#"g.gql("MATCH (p) CALL (p) { DETACH DELETE p } RETURN 1").toList()"#,
                "gql:delete",
            ),
        ] {
            assert_denied(
                run(&graph, script, no_delete()),
                step,
                "Delete Data(vertex)",
            );
            assert_denied(run(&graph, script, NoSecret), step, "Delete Data(vertex)");
            assert_denied(
                run(&graph, script, AccessPolicy::read_only()),
                step,
                "Delete Data(vertex)",
            );
        }
        for (script, step) in [
            (r#"g.e().has_label("secret").drop().iterate()"#, "drop"),
            (
                r#"g.gql("MATCH ()-[e:secret]->() DELETE e").iterate()"#,
                "gql:delete",
            ),
        ] {
            assert_denied(
                run(&graph, script, NoSecret),
                step,
                "Delete Data(edge \"secret\")",
            );
        }
        assert_denied(
            run(
                &graph,
                r#"g.gql("MATCH x = ()-[:knows]->() DETACH DELETE x").iterate()"#,
                NoSecret,
            ),
            "gql:delete",
            "Delete Data",
        );
        assert_eq!(count(&graph), 6);
        assert_eq!(graph.read().edge_count(), 6);
        // Label-level: an edge whose pattern states its label asks for that label only.
        assert!(
            run(
                &graph,
                r#"g.gql("MATCH ()-[e:knows]->() DELETE e").iterate()"#,
                NoSecret,
            )
            .is_ok()
        );
        assert_eq!(graph.read().edge_count(), 4);
    }

    /// Stage 3e (design C7): MERGE asks `Read` for its match child, `Create` per label of each
    /// element its pattern can create (as `INSERT`) and `Update` for its ON items (as `SET`):
    /// the requests of its pure-GQL twin `CALL { FILTER NOT EXISTS { MATCH .. } INSERT .. }`
    /// plus the ON items. Unlike `merge_v()` (always `Create` + `Update` on `Data(vertex)`) it
    /// asks no `Update` without ON items. The step's own requests come before the ones of its
    /// children (the match child's reads).
    #[test]
    fn gql_merge_asks_read_create_and_the_updates_of_its_on_items() {
        for (query, twin) in [
            (
                r#"g.gql("FOR x IN [1, 2] MERGE (:N {k: x})").iterate()"#,
                r#"g.gql("FOR x IN [1, 2] CALL (x) { FILTER NOT EXISTS { MATCH (n:N {k: x}) } INSERT (:N {k: x}) }").iterate()"#,
            ),
            (
                r#"g.gql("MERGE (:A:B)").iterate()"#,
                r#"g.gql("CALL { FILTER NOT EXISTS { MATCH (n:A&B) } INSERT (:A&B) }").iterate()"#,
            ),
            (
                r#"g.gql("MATCH (a:person), (b:software) MERGE (a)-[:uses]->(b)").iterate()"#,
                r#"g.gql("MATCH (a:person), (b:software) CALL (a, b) { FILTER NOT EXISTS { MATCH (a)-[:uses]->(b) } INSERT (a)-[:uses]->(b) }").iterate()"#,
            ),
            (
                r#"g.gql("MATCH (a:person) MERGE (a)-[:likes]->(:fruit {by: a.name})").iterate()"#,
                r#"g.gql("MATCH (a:person) CALL (a) { FILTER NOT EXISTS { MATCH (a)-[:likes]->(:fruit {by: a.name}) } INSERT (a)-[:likes]->(:fruit {by: a.name}) }").iterate()"#,
            ),
        ] {
            let mut gql = requests(&modern(), query);
            let mut pure = requests(&modern(), twin);
            gql.sort();
            gql.dedup();
            pure.sort();
            pure.dedup();
            assert_eq!(gql, pure, "{query}");
        }
        assert_eq!(
            requests(&modern(), r#"g.gql("MERGE (n:N {k: 1})").iterate()"#),
            ["Create Data(vertex \"N\")", "Read Data(vertex)"]
        );
        assert_eq!(
            requests(
                &modern(),
                r#"g.gql("MERGE (n:N {k: 1}) ON CREATE SET n.a = 1 ON MATCH SET n:Seen").iterate()"#
            ),
            [
                "Create Data(vertex \"N\")",
                "Update Data(vertex)",
                "Read Data(vertex)"
            ]
        );
        assert_eq!(
            requests(
                &modern(),
                r#"g.gql("MATCH (a {name: 'marko'}) MERGE (a)-[e:likes]->(f) ON MATCH SET e.w = 1, f.seen = true").iterate()"#
            ),
            [
                "Read Data(vertex)",
                "Create Data(vertex)",
                "Create Data(edge \"likes\")",
                "Update Data(edge \"likes\")",
                "Update Data(vertex)",
                "Read Data(edge)"
            ]
        );
        // The Gremlin twin asks Update without onMatch / onCreate options too.
        let twin = requests(&modern(), r#"g.merge_v(#{"T.label": "N", k: 1}).iterate()"#);
        assert!(twin.iter().any(|r| r == "Update Data(vertex)"), "{twin:?}");
    }

    /// A denial refuses a MERGE before it runs: a read inside the match child (`Read`), the
    /// create part also when every row would only match (`Create`, a label the policy protects
    /// included), the ON items (`Update`); a CALL body with a MERGE the same.
    #[test]
    fn a_denied_read_create_or_update_refuses_a_gql_merge() {
        let graph = modern();
        // The match child reads edges.
        for (script, step) in [
            // The match child starts at the current element `b`.
            (
                r#"g.gql("MATCH (a {name: 'marko'}), (b {name: 'josh'}) MERGE (a)-[:knows]->(b)").iterate()"#,
                "in_e",
            ),
            (
                r#"g.gql("MATCH (a:person) CALL (a) { MERGE (a)-[:knows]->(:person) } RETURN a").toList()"#,
                "out_e",
            ),
        ] {
            assert_denied(run(&graph, script, NoEdges), step, "Read Data(edge)");
        }
        // A node MERGE reads vertices only: allowed under NoEdges.
        assert!(
            run(
                &graph,
                r#"g.gql("MERGE (p:person {name: 'marko'})").iterate()"#,
                NoEdges
            )
            .is_ok()
        );
        let no_create = || AccessPolicy::allow_all().deny(ActionKind::Create, ResourceKind::Data);
        for (script, request) in [
            (
                r#"g.gql("MERGE (p:person {name: 'marko'})").iterate()"#,
                "Create Data(vertex \"person\")",
            ),
            (
                r#"g.gql("MATCH (a {name: 'marko'}), (b {name: 'josh'}) MERGE (a)-[:knows]->(b)").iterate()"#,
                "Create Data(edge \"knows\")",
            ),
            (
                r#"g.gql("MATCH (a:person) CALL (a) { MERGE (a)-[:x]->(:y) } RETURN a").toList()"#,
                "Create Data(vertex \"y\")",
            ),
        ] {
            assert_denied(run(&graph, script, no_create()), "gql:merge", request);
            assert_denied(
                run(&graph, script, AccessPolicy::read_only()),
                "gql:merge",
                request,
            );
        }
        // A protected label: the MERGE of a secret node is refused, others pass.
        for script in [
            r#"g.gql("MERGE (:secret)").iterate()"#,
            r#"g.gql("MERGE (:public&secret {k: 1})").iterate()"#,
        ] {
            assert_denied(
                run(&graph, script, NoSecret),
                "gql:merge",
                "Create Data(vertex \"secret\")",
            );
        }
        assert_denied(
            run(
                &graph,
                r#"g.gql("MATCH (a {name: 'marko'}) MERGE (a)-[:secret]->(:public)").iterate()"#,
                NoSecret,
            ),
            "gql:merge",
            "Create Data(edge \"secret\")",
        );
        assert!(run(&graph, r#"g.gql("MERGE (:public)").iterate()"#, NoSecret).is_ok());
        // ON items: Update, even when the row only matches.
        let no_update = || AccessPolicy::allow_all().deny(ActionKind::Update, ResourceKind::Data);
        for script in [
            r#"g.gql("MERGE (p:person {name: 'marko'}) ON MATCH SET p.age = 1").iterate()"#,
            r#"g.gql("MERGE (p:person {name: 'zed'}) ON CREATE SET p:New").iterate()"#,
        ] {
            assert_denied(
                run(&graph, script, no_update()),
                "gql:merge",
                "Update Data(vertex)",
            );
        }
        assert!(
            run(
                &graph,
                r#"g.gql("MERGE (p:person {name: 'marko'})").iterate()"#,
                no_update()
            )
            .is_ok()
        );
        // Only the allowed MERGEs ran: one `public` node, nothing else.
        assert_eq!(count(&graph), 7);
        assert_eq!(graph.read().edge_count(), 6);
        let ages = run(
            &graph,
            r#"g.v().has("name", "marko").values("age").toList()"#,
            AllowAll,
        )
        .unwrap();
        assert_eq!(ages.to_string(), "[29]");
    }
}
