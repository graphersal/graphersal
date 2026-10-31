//! Maps engine failures (`Box<rhai::EvalAltResult>`, GraphSON/GraphML `IOError`) onto the
//! `graphersal.GraphersalError` Python exception, or onto one of its subclasses for the failures
//! a caller handles on purpose (`PermissionDeniedError`, `CommitRejectedError`,
//! `ReturnedClosureError`).

use graphersal::script::rhai::EvalAltResult;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

pyo3::create_exception!(
    graphersal,
    GraphersalError,
    PyException,
    "Raised for any script/engine error: parse errors, TraverserError, GraphSON/GraphML import errors.\n\n\
     Carries a `help` attribute with the engine's `help()` guidance text when the underlying\n\
     error provides one (`None` otherwise), and a `cause` attribute: for a refused Store\n\
     operation the name of its cause (`\"name_taken\"`, `\"backup_running\"`, ...), `None`\n\
     otherwise. Subclasses: `PermissionDeniedError`,\n\
     `CommitRejectedError`, `ReturnedClosureError`."
);

pyo3::create_exception!(
    graphersal,
    PermissionDeniedError,
    GraphersalError,
    "Raised when a script makes a request (an action on a resource, such as create on data)\n\
     that its `policy` does not allow, or uses a file path outside the policy's file root. The\n\
     denied traversal did not run."
);

pyo3::create_exception!(
    graphersal,
    CommitRejectedError,
    GraphersalError,
    "Raised when a commit hook of the graph vetoed the commit: everything the unit changed was\n\
     rolled back. Python cannot register commit hooks, so this is raised only for a\n\
     graph whose hooks were registered on the Rust side."
);

pyo3::create_exception!(
    graphersal,
    ReturnedClosureError,
    GraphersalError,
    "Raised when a script run as one unit (`atomic=True`, `Graph.dry_run`) returns a closure or\n\
     function pointer (also inside a list or dict). Everything the script changed was rolled\n\
     back. Return data instead: run the closure inside the script and return its result."
);

pyo3::create_exception!(
    graphersal,
    VectorFillWarning,
    pyo3::exceptions::PyUserWarning,
    "Issued by `Graph.set_schema` / `Graph.patch_schema` BEFORE a schema change that fills\n\
     vectors (a vector rule added or changed, book page \"Vector Rules\"): the text names the\n\
     elements, the model and the time, and that the whole database is locked while it runs.\n\
     Turned into an error (`warnings.simplefilter(\"error\", VectorFillWarning)`) it stops the\n\
     change before anything is applied."
);

/// The failures a Python caller can tell apart by exception class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    PermissionDenied,
    CommitRejected,
    ReturnedClosure,
    Other,
}

/// The rendered `Display` prefixes of the engine errors that get their own exception class:
/// `TraverserError::PermissionDenied`, `GraphError::CommitRejected` and
/// `ScriptError::ReturnedClosure` (`graphersal/src/error.rs`).
///
/// The structured error does not survive the Rhai boundary (every engine error crosses it as its
/// rendered diagnostic, see `split_message_and_help`), so the kind is read back from that text: a
/// diagnostic names its error on the first line (`Error: ..`) or, when it is wrapped (a step
/// failure, a script error), on a `Caused by: ..` line. The unit tests below produce each of
/// these errors through the engine, so a change of the engine's wording fails them instead of
/// silently degrading the exception class.
const KIND_PREFIXES: [(&str, ErrorKind); 3] = [
    ("Permission denied for '", ErrorKind::PermissionDenied),
    (
        "The commit was rejected by the commit hook '",
        ErrorKind::CommitRejected,
    ),
    (
        "A script run as one unit cannot return a closure",
        ErrorKind::ReturnedClosure,
    ),
];

/// The kind of the error a rendered diagnostic's message part (help text already split off)
/// describes.
fn classify(message: &str) -> ErrorKind {
    for line in message.lines() {
        let line = line.trim_start();
        let Some(text) = line
            .strip_prefix("Error: ")
            .or_else(|| line.strip_prefix("Caused by: "))
        else {
            continue;
        };
        for (prefix, kind) in KIND_PREFIXES {
            if text.starts_with(prefix) {
                return kind;
            }
        }
    }
    ErrorKind::Other
}

/// Splits a rendered `TraverserError` diagnostic (built by `graphersal::error::format_diagnostic`)
/// back into its message and help text.
///
/// `eval_value`/`eval_value_with_params` already flatten the structured error into one string
/// before it crosses into Rhai (`script/mod.rs` wraps every `TraverserError` as
/// `EvalAltResult::ErrorRuntime(Dynamic::from(format_diagnostic(&err)), ..)`), so there is no
/// structured `TraverserError`/`help()` left to read on this side of that boundary. Help text is
/// always appended last, as `"\nHelp: {first line}"` followed by `"\n      {continuation}"` lines
/// (six spaces, matching `"Help: "`'s width) — see `push_details`/`format_diagnostic` in
/// `graphersal::error`. Splitting on that exact, stable marker recovers the two parts without
/// depending on `Debug` formatting (CLAUDE.md rule 14: the marker is part of the diagnostic's
/// public rendered text, not a `Debug` impl).
fn split_message_and_help(diagnostic: &str) -> (String, Option<String>) {
    match diagnostic.find("\nHelp: ") {
        Some(idx) => {
            let message = diagnostic[..idx].to_string();
            let help_block = &diagnostic[idx + 1..]; // starts at "Help: ..."
            let help = help_block
                .lines()
                .enumerate()
                .map(|(i, line)| {
                    if i == 0 {
                        line.trim_start_matches("Help: ")
                    } else {
                        line.trim_start_matches("      ")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            (message, Some(help))
        }
        None => (diagnostic.to_string(), None),
    }
}

/// The message and help text of a script evaluation failure.
fn eval_error_parts(err: &EvalAltResult) -> (String, Option<String>) {
    match err {
        EvalAltResult::ErrorRuntime(diagnostic, ..) if diagnostic.is_string() => {
            split_message_and_help(&diagnostic.to_string())
        }
        EvalAltResult::ErrorTerminated(diagnostic, ..) if diagnostic.is_string() => {
            split_message_and_help(&diagnostic.to_string())
        }
        other => (other.to_string(), None),
    }
}

/// Converts a script evaluation failure into a `GraphersalError` (or the subclass of its kind),
/// attaching `help` text when the underlying diagnostic carried one.
pub(crate) fn map_eval_error(py: Python<'_>, err: &EvalAltResult) -> PyErr {
    let (message, help) = eval_error_parts(err);
    new_graphersal_error(py, message, help)
}

/// Converts a GraphSON/GraphML import/export failure into a `GraphersalError`. `IOError` is a
/// `DiagnosticError`, so its `help()` text (when any) is included the same way.
pub(crate) fn map_io_error(py: Python<'_>, err: graphersal::error::IOError) -> PyErr {
    let message = graphersal::error::format_diagnostic(&err);
    let (message, help) = split_message_and_help(&message);
    new_graphersal_error(py, message, help)
}

/// Converts the rendered diagnostic of an engine error (`format_diagnostic`: message, then
/// `Help: ..`) into a `GraphersalError` (or the subclass of its kind) with its `help` text. The
/// schema methods use it: their errors are rendered where they happen, off the GIL.
pub(crate) fn map_diagnostic(py: Python<'_>, diagnostic: &str) -> PyErr {
    let (message, help) = split_message_and_help(diagnostic);
    new_graphersal_error(py, message, help)
}

fn new_graphersal_error(py: Python<'_>, message: String, help: Option<String>) -> PyErr {
    let err = match classify(&message) {
        ErrorKind::PermissionDenied => PyErr::new::<PermissionDeniedError, _>(message),
        ErrorKind::CommitRejected => PyErr::new::<CommitRejectedError, _>(message),
        ErrorKind::ReturnedClosure => PyErr::new::<ReturnedClosureError, _>(message),
        ErrorKind::Other => PyErr::new::<GraphersalError, _>(message),
    };
    let help_value: Py<PyAny> = match help {
        Some(text) => text
            .into_pyobject(py)
            .map(|s| s.into_any().unbind())
            .unwrap_or_else(|_| py.None()),
        None => py.None(),
    };
    let _ = err.value(py).setattr("help", help_value);
    // A Store failure sets its cause afterwards (`store.rs`); every other error has none.
    let _ = err.value(py).setattr("cause", py.None());
    err
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use graphersal::GraphSource;
    use graphersal::script::{ScriptLimits, eval_value_atomic, eval_value_with_limits};
    use graphersal::{
        auth::{AccessPolicy, AllowAll, Authorizer},
        changes::{ChangeSet, CommitHook, HookError},
        storage::GraphStorage,
    };

    #[test]
    fn splits_message_and_single_line_help() {
        let diagnostic = "Error: something broke\nHelp: try this instead";
        let (message, help) = split_message_and_help(diagnostic);
        assert_eq!(message, "Error: something broke");
        assert_eq!(help.as_deref(), Some("try this instead"));
    }

    #[test]
    fn splits_message_and_multi_line_help() {
        let diagnostic =
            "Error: something broke\n  detail: value\nHelp: first line\n      second line";
        let (message, help) = split_message_and_help(diagnostic);
        assert_eq!(message, "Error: something broke\n  detail: value");
        assert_eq!(help.as_deref(), Some("first line\nsecond line"));
    }

    #[test]
    fn no_help_marker_returns_none() {
        let diagnostic = "Error: parse failure at line 1";
        let (message, help) = split_message_and_help(diagnostic);
        assert_eq!(message, diagnostic);
        assert_eq!(help, None);
    }

    fn kind_of(result: Result<graphersal::script::rhai::Dynamic, Box<EvalAltResult>>) -> ErrorKind {
        let err = result.expect_err("the script must fail");
        let (message, help) = eval_error_parts(&err);
        assert!(help.is_some(), "the help text is kept: {message}");
        classify(&message)
    }

    fn run(
        graph: Arc<graphersal::prelude::Graph>,
        script: &str,
        authorizer: impl Authorizer + 'static,
    ) -> Result<graphersal::script::rhai::Dynamic, Box<EvalAltResult>> {
        eval_value_with_limits(
            graph,
            script,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(authorizer),
        )
    }

    #[test]
    fn classifies_permission_denied_of_a_step_and_of_a_file_operation() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        assert_eq!(
            kind_of(run(
                graph.clone(),
                r#"g.addV("x").next()"#,
                AccessPolicy::read_only()
            )),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            kind_of(run(
                graph,
                r#"g.export_graphml("x.graphml")"#,
                AccessPolicy::read_only()
            )),
            ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn classifies_a_returned_closure() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let result = eval_value_atomic(
            graph,
            "|x| x + 1",
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(AllowAll),
        );
        assert_eq!(kind_of(result), ErrorKind::ReturnedClosure);
    }

    struct Veto;

    impl CommitHook for Veto {
        fn name(&self) -> &str {
            "veto"
        }

        fn before_commit(&mut self, _changes: &ChangeSet) -> Result<(), HookError> {
            Err("no writes today".into())
        }
    }

    #[test]
    fn classifies_a_commit_rejected_by_a_hook() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        graph
            .write()
            .register_commit_hook(Box::new(Veto))
            .expect("TraversalGraph supports commit hooks");
        assert_eq!(
            kind_of(run(graph.clone(), r#"g.addV("x").next()"#, AllowAll)),
            ErrorKind::CommitRejected
        );
        let atomic = eval_value_atomic(
            graph,
            r#"g.addV("x").next()"#,
            Default::default(),
            &ScriptLimits::default(),
            Arc::new(AllowAll),
        );
        assert_eq!(kind_of(atomic), ErrorKind::CommitRejected);
    }

    #[test]
    fn other_errors_keep_the_base_class() {
        let graph = Arc::new(GraphSource::tinkerpop_modern());
        let result = run(graph, r#"g.V().values("name").sum().next()"#, AllowAll);
        assert_eq!(kind_of(result), ErrorKind::Other);
        assert_eq!(classify("Error: parse failure"), ErrorKind::Other);
    }
}

/// Emits `text` as one ``UserWarning`` (a note of a load or an open).
pub(crate) fn warn(py: Python<'_>, text: &str) -> PyResult<()> {
    let text = std::ffi::CString::new(text.replace('\0', " ")).unwrap_or_default();
    PyErr::warn(
        py,
        &py.get_type::<pyo3::exceptions::PyUserWarning>(),
        &text,
        1,
    )
}
