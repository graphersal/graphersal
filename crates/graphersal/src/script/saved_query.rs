//! Saved queries in the DSL (`book/src/users_guide/saved_queries.md`): defining them
//! (`g.define_query(#{..})`, `g.define_queries(folder, source)`), calling them
//! (`g.query(name, #{..})`) and managing them (`g.queries()`, `g.get_query(name)`,
//! `g.drop_query(name)`, `g.move_query(name, folder)`, `g.describe_query(name, text)`).
//!
//! A saved query is stored in the graph's catalog of definitions
//! ([`crate::catalog`]) as the text of ONE Rhai function, or (feature `gql`, `language: "gql"`,
//! `saved_query_gql.rs`) of one GQL query, which a call runs as `g.gql(body, args)` without
//! evaluating Rhai, under the same rules. A call:
//!
//! 1. asks the caller's authorizer for `Execute` on the query (before anything runs);
//! 2. binds the named arguments against the parameter schemas (unknown, missing, wrong type or
//!    constraint: an error naming the parameter; defaults substituted; the only conversion is
//!    the schema's write coercion, `int64` -> `float64` and a canonical uuid string);
//! 3. compiles the body, through a per-thread cache keyed by the query's name, dialect,
//!    parameter names and source text, so a changed definition (also one restored by a
//!    rollback) never meets a stale compiled form;
//! 4. runs the function with the caller's engine in a scope whose `g` is the caller's graph
//!    under a restriction:
//!    - a query of the dialect [`QUERY_DIALECT`] (or one called from a read-only body) runs
//!      READ-ONLY ([`ReadOnly`]): nothing the body runs, and nothing built from what it
//!      returns, can write data, the schema, the catalog or files (a graph the body builds
//!      itself stays writable);
//!    - a query of the dialect [`WRITING_QUERY_DIALECT`] may write the data its body's
//!      inferred effects name ([`effects`], with the effects of the queries it calls), and
//!      nothing else ([`Effects`]): the whole effect set is asked of the caller's authorizer
//!      first, then the body runs as ONE unit (a savepoint inside an open one) whose commit
//!      carries the query's name and arguments ([`CommitMetadata`]); a failure rolls back
//!      everything it wrote;
//!
//!    Nested calls inherit the restriction; the call depth is bounded by [`MAX_QUERY_DEPTH`];
//! 5. returns the function's result (a traversal can be continued by the caller, read-only, a
//!    value is a value); a returned traversal with a writing step is refused.
//!
//! The dialect is not chosen by the user: every save ([`ScriptGraphTraversalSource::define_query`],
//! [`ScriptGraphTraversalSource::define_queries`], [`with_inferred_dialect`]) sets
//! [`WRITING_QUERY_DIALECT`] when the inferred effects are not empty and [`QUERY_DIALECT`]
//! otherwise. Opening a graph never changes a dialect.
//!
//! Errors from the body keep their diagnostic and gain a line `in saved query: "name"` (one per
//! nested call), so the origin is named.

use super::effects::{self, BodyEffects};
#[cfg(feature = "gql")]
use super::saved_query_gql;
use super::*;
use crate::error::{
    SAVED_QUERY_DATA_ONLY, SAVED_QUERY_OUTSIDE_EFFECTS, SAVED_QUERY_READ_ONLY, ScriptError,
};
use crate::graph::definition::{
    Definition, DefinitionKind, GQL_QUERY_DIALECT, GQL_WRITING_QUERY_DIALECT, QUERY_DIALECT,
    QueryDefinition, QueryParam, WRITING_QUERY_DIALECT,
};
use crate::graph::{
    AccessRequest, AccessRequestBuf, Action, Authorizer, CommitMetadata, Denied, Resource,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The deepest nesting of saved-query calls (a query calling a query ...); a deeper call fails
/// with [`ScriptError::QueryDepthExceeded`].
pub(crate) const MAX_QUERY_DEPTH: usize = 16;

/// How many compiled bodies the per-thread cache keeps before it starts over.
const CACHE_CAPACITY: usize = 256;

/// The authorizer a saved query runs under: the caller's, minus every write. `Create`,
/// `Update` and `Delete` on data, the schema, the catalog and files are denied with a reason
/// starting with [`SAVED_QUERY_READ_ONLY`]; everything else is the caller's authorizer's
/// decision.
pub(crate) struct ReadOnly {
    inner: Arc<dyn Authorizer>,
}

impl Authorizer for ReadOnly {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        let writes = matches!(
            request.action,
            Action::Create | Action::Update | Action::Delete
        ) && matches!(
            request.resource,
            Resource::Data { .. }
                | Resource::Schema
                | Resource::Definition { .. }
                | Resource::File { .. }
        );
        if writes {
            return Err(Denied::new(format!(
                "{SAVED_QUERY_READ_ONLY}: it cannot create, change or delete data, the schema, \
                 saved queries or files"
            )));
        }
        self.inner.authorize(request)
    }

    fn resolve_file(&self, path: &std::path::Path) -> Result<std::path::PathBuf, Denied> {
        self.inner.resolve_file(path)
    }
}

/// The authorizer a saved query that writes runs under: the caller's, restricted to the
/// inferred `effects`. A write of data that no effect covers ([`effects::covers`]) is denied
/// (it means the inference missed it, a bug, and the message says so); writes of the schema,
/// the catalog and files are denied as under [`ReadOnly`]; everything else is the caller's
/// authorizer's decision.
pub(crate) struct Effects {
    inner: Arc<dyn Authorizer>,
    query: String,
    effects: Vec<AccessRequestBuf>,
}

impl Authorizer for Effects {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Denied> {
        let writes = matches!(
            request.action,
            Action::Create | Action::Update | Action::Delete
        );
        match request.resource {
            Resource::Data { .. } if writes => {
                if !self
                    .effects
                    .iter()
                    .any(|effect| effects::covers(effect, request))
                {
                    return Err(Denied::new(format!(
                        "{SAVED_QUERY_OUTSIDE_EFFECTS} ({}), and this write of \"{}\" is not \
                         among them; the inference missed it, a bug of Graphersal",
                        effects_text(&self.effects),
                        self.query,
                    )));
                }
            }
            Resource::Schema | Resource::Definition { .. } | Resource::File { .. } if writes => {
                return Err(Denied::new(format!(
                    "{SAVED_QUERY_DATA_ONLY}: \"{}\" cannot create, change or delete the \
                     schema, saved queries or files",
                    self.query
                )));
            }
            _ => {}
        }
        self.inner.authorize(request)
    }

    fn resolve_file(&self, path: &std::path::Path) -> Result<std::path::PathBuf, Denied> {
        self.inner.resolve_file(path)
    }
}

/// The effects as one line of text (`Create Data(vertex "person"), Delete Data`).
fn effects_text(effects: &[AccessRequestBuf]) -> String {
    let texts: Vec<String> = effects.iter().map(ToString::to_string).collect();
    texts.join(", ")
}

/// A compiled body: the body to run, its signature (the parameters in stored order) and its own
/// effects (`Err`: why the body cannot be a query that writes, [`effects::analyze`]).
struct Compiled {
    body: Body,
    signature: Vec<String>,
    effects: Result<BodyEffects, String>,
}

/// What a call runs.
enum Body {
    /// The AST holding the one Rhai function.
    Rhai(rhai::AST),
    /// The query text of the definition, checked (`saved_query_gql.rs`); a call runs it as
    /// `g.gql(text, params)`.
    #[cfg(feature = "gql")]
    Gql,
}

/// Whether a query of `dialect` may write (its effects are asked and allowed at a call).
fn is_writing(dialect: &str) -> bool {
    dialect == WRITING_QUERY_DIALECT || dialect == GQL_WRITING_QUERY_DIALECT
}

/// Whether `dialect` names the GQL language (any version, also one this version does not run):
/// [`with_inferred_dialect`] keeps the language of the definition it is given.
fn is_gql_language(dialect: &str) -> bool {
    dialect.starts_with("graphersal-gql/")
}

/// The dialects this version runs, as the text of an unknown dialect's reason.
fn served_dialects() -> String {
    let rhai = format!("'{QUERY_DIALECT}' and '{WRITING_QUERY_DIALECT}'");
    if cfg!(feature = "gql") {
        format!("{rhai}, and for GQL '{GQL_QUERY_DIALECT}' and '{GQL_WRITING_QUERY_DIALECT}'")
    } else {
        rhai
    }
}

/// What [`compile`] yields: the compiled body or why it is not a valid body.
type CompileResult = Rc<Result<Compiled, String>>;

/// The cache key: everything the compile check depends on.
#[derive(PartialEq, Eq, Hash)]
struct CacheKey {
    name: String,
    dialect: String,
    params: Vec<String>,
    source: String,
}

thread_local! {
    /// Compiled bodies of this thread (Rhai's AST is not `Send`). Keyed by the definition's
    /// text, so every change of a definition (also a rollback) picks its own entry.
    static COMPILED: RefCell<HashMap<CacheKey, CompileResult>> = RefCell::new(HashMap::new());
}

/// The compiled body of the saved query `name`, from the cache or compiled with `engine`.
fn compile(engine: &Engine, name: &str, query: &QueryDefinition) -> CompileResult {
    let key = CacheKey {
        name: name.to_string(),
        dialect: query.dialect().to_string(),
        params: query.params().iter().map(|(p, _)| p.clone()).collect(),
        source: query.source().to_string(),
    };
    if let Some(hit) = COMPILED.with(|cache| cache.borrow().get(&key).cloned()) {
        return hit;
    }
    let compiled = Rc::new(compile_uncached(engine, name, query));
    COMPILED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(key, compiled.clone());
    });
    compiled
}

/// Checks the body of the saved query `name` the way a call does, without running it: the
/// dialect is one this version runs, the source compiles with `engine` and is exactly one
/// function named `name` whose parameters are the declared ones, and a query of the dialect
/// [`WRITING_QUERY_DIALECT`] has a body whose effects can be inferred. `Err` is the reason (the
/// `status: "error"` text of `g.queries()`: the query is unavailable). Front ends use it before
/// storing a definition they built themselves (the playground's editor) and to list a catalog
/// with its status; the compiled form is cached per thread like a call's.
/// [`saved_query_effects`] also gives what a call may write.
pub fn check_saved_query(
    engine: &Engine,
    name: &str,
    query: &QueryDefinition,
) -> Result<(), String> {
    match compile(engine, name, query).as_ref() {
        Ok(_) => Ok(()),
        Err(reason) => Err(reason.clone()),
    }
}

/// What a call of a saved query may write, inferred from its body now
/// ([`saved_query_effects`], the `writes` / `effects` / `denied_writes` of `g.queries()`).
/// Never stored: a newer version may infer more or less for the same text.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedQueryEffects {
    /// Whether a call may write: the dialect is [`WRITING_QUERY_DIALECT`] and `effects` is not
    /// empty.
    pub writes: bool,
    /// The data writes a call may make (requests as the authorizer sees them; an open element
    /// kind or label covers every value), with those of the queries it calls. Empty unless
    /// `writes`.
    pub effects: Vec<AccessRequestBuf>,
    /// The writes the body of a read-only query (dialect [`QUERY_DIALECT`]) has, which a call
    /// denies; saving the query again makes it a query that writes. Empty otherwise.
    pub denied_writes: Vec<AccessRequestBuf>,
}

/// The effects of the saved query `name` (definition `query`), resolving the queries it calls
/// through `catalog` (a query by name, `None` when there is none): what a call may write, or
/// `Err` with the reason the query cannot run ([`check_saved_query`]). Front ends list a
/// catalog with it (`R/O` / `R/W` badges, the MCP `writes` flag).
pub fn saved_query_effects(
    engine: &Engine,
    catalog: &dyn Fn(&str) -> Option<QueryDefinition>,
    name: &str,
    query: &QueryDefinition,
) -> Result<SavedQueryEffects, String> {
    if let Some(reason) = query.unsupported_reason() {
        return Err(reason);
    }
    let compiled = compile(engine, name, query);
    let compiled = compiled.as_ref().as_ref().map_err(Clone::clone)?;
    let Ok(own) = &compiled.effects else {
        return Ok(SavedQueryEffects::default());
    };
    let (effects, _) = resolve_effects(engine, catalog, name, own);
    Ok(if is_writing(query.dialect()) {
        SavedQueryEffects {
            writes: !effects.is_empty(),
            effects,
            denied_writes: Vec::new(),
        }
    } else {
        SavedQueryEffects {
            writes: false,
            effects: Vec::new(),
            denied_writes: effects,
        }
    })
}

/// `query` (the definition of the saved query `name`) checked like
/// [`ScriptGraphTraversalSource::define_query`] checks a body, with the dialect its inferred
/// effects give: [`WRITING_QUERY_DIALECT`] when they are not empty, [`QUERY_DIALECT`]
/// otherwise (also lowering a query that no longer writes). A definition whose dialect names GQL
/// (`graphersal-gql/..`, for example [`GQL_QUERY_DIALECT`]) is a GQL query and gets
/// [`GQL_WRITING_QUERY_DIALECT`] or [`GQL_QUERY_DIALECT`] the same way (its effects are the
/// writes of its plan). The queries it calls are resolved through `catalog`. `Err` is why it
/// cannot be stored: the body does not compile, is not one function named `name` with the
/// declared parameters, writes the schema, the catalog or files, or writes data while hiding
/// what it calls (`Fn`, `call`, `eval`, `import`, a computed query name); a GQL body does not
/// parse, bind or lower, or reads a parameter it does not declare (or declares one it does not
/// read). Front ends that build definitions themselves (the playground's editor) store what it
/// returns.
pub fn with_inferred_dialect(
    engine: &Engine,
    catalog: &dyn Fn(&str) -> Option<QueryDefinition>,
    name: &str,
    query: QueryDefinition,
) -> Result<QueryDefinition, String> {
    let gql = is_gql_language(query.dialect());
    let query = query.with_dialect(if gql {
        GQL_QUERY_DIALECT
    } else {
        QUERY_DIALECT
    });
    let compiled = compile_uncached(engine, name, &query)?;
    let own = compiled.effects?;
    let (effects, dynamic) = resolve_effects(engine, catalog, name, &own);
    dialect_for(gql, &effects, dynamic.as_deref()).map(|dialect| query.with_dialect(dialect))
}

/// The dialect of a query (written in GQL when `gql`) with the inferred `effects` whose body
/// (or a called query) uses the `dynamic` construct; `Err` for a dynamic body that writes.
fn dialect_for(
    gql: bool,
    effects: &[AccessRequestBuf],
    dynamic: Option<&str>,
) -> Result<&'static str, String> {
    match (effects.is_empty(), dynamic) {
        (true, _) if gql => Ok(GQL_QUERY_DIALECT),
        (false, None) if gql => Ok(GQL_WRITING_QUERY_DIALECT),
        (true, _) => Ok(QUERY_DIALECT),
        (false, None) => Ok(WRITING_QUERY_DIALECT),
        (false, Some(construct)) => Err(format!(
            "it writes data ({}) and uses {construct}, which hides what it calls, so its \
             effects cannot be inferred; call steps and saved queries by their literal names",
            effects_text(effects)
        )),
    }
}

/// The effects of the query `name` whose own effects are `own` together with those of every
/// query it calls that writes (dialect [`WRITING_QUERY_DIALECT`]), looked up in `catalog`; and
/// the first construct that hides calls, if any.
fn resolve_effects(
    engine: &Engine,
    catalog: &dyn Fn(&str) -> Option<QueryDefinition>,
    name: &str,
    own: &BodyEffects,
) -> (Vec<AccessRequestBuf>, Option<String>) {
    effects::transitive(name, &mut |next| {
        if next == name {
            return Some(effects::Node {
                effects: own.clone(),
                counts: true,
            });
        }
        node_of(engine, next, &catalog(next)?)
    })
}

/// The node of the stored query `name` for [`effects::transitive`]: its own effects, counted
/// when it writes; `None` when it cannot run.
fn node_of(engine: &Engine, name: &str, query: &QueryDefinition) -> Option<effects::Node> {
    let compiled = compile(engine, name, query);
    let compiled = compiled.as_ref().as_ref().ok()?;
    Some(effects::Node {
        effects: compiled.effects.as_ref().ok()?.clone(),
        counts: is_writing(query.dialect()),
    })
}

/// Compiles a body and checks it: the dialect is one this version runs, the text is exactly one
/// function named `name` (no statements beside it) whose parameters are the declared ones, and
/// a query that writes has a body whose effects can be inferred. A GQL body is checked by
/// [`saved_query_gql::check`].
fn compile_uncached(
    engine: &Engine,
    name: &str,
    query: &QueryDefinition,
) -> Result<Compiled, String> {
    #[cfg(feature = "gql")]
    if query.dialect() == GQL_QUERY_DIALECT || query.dialect() == GQL_WRITING_QUERY_DIALECT {
        let declared: Vec<&str> = query.params().iter().map(|(p, _)| p.as_str()).collect();
        let checked = saved_query_gql::check(name, query.source(), &declared, None)
            .map_err(|error| error.reason())?;
        return Ok(Compiled {
            body: Body::Gql,
            signature: declared.iter().map(ToString::to_string).collect(),
            effects: Ok(BodyEffects {
                writes: checked.writes,
                calls: checked.calls,
                dynamic: None,
            }),
        });
    }
    let writing = query.dialect() == WRITING_QUERY_DIALECT;
    if query.dialect() != QUERY_DIALECT && !writing {
        return Err(format!(
            "its dialect is '{}', this version runs {}",
            query.dialect(),
            served_dialects()
        ));
    }
    let ast = engine
        .compile(query.source())
        .map_err(|err| format!("it does not compile: {err}"))?;
    let signature = single_function(&ast, name)?;
    let declared: Vec<&str> = query.params().iter().map(|(p, _)| p.as_str()).collect();
    check_signature(&signature, &declared)?;
    let effects = effects::analyze(&ast);
    if writing {
        match &effects {
            Err(reason) => {
                return Err(format!(
                    "it is saved as a query that writes, but its effects cannot be inferred: \
                     {reason}"
                ));
            }
            Ok(BodyEffects {
                dynamic: Some(construct),
                ..
            }) => {
                return Err(format!(
                    "it is saved as a query that writes, but it uses {construct}, which hides \
                     what it calls, so its effects cannot be inferred"
                ));
            }
            Ok(_) => {}
        }
    }
    Ok(Compiled {
        body: Body::Rhai(ast),
        signature,
        effects,
    })
}

/// The signature of the one function `ast` defines, which must be named `name`.
fn single_function(ast: &rhai::AST, name: &str) -> Result<Vec<String>, String> {
    if !ast.statements().is_empty() {
        return Err(
            "it has statements outside the function; the body is one function and nothing else"
                .to_string(),
        );
    }
    let functions: Vec<_> = ast.iter_fn_def().collect();
    let function = match functions.as_slice() {
        [] => {
            return Err(format!(
                "it defines no function (expected fn {name}(..) {{ .. }})"
            ));
        }
        [one] => one,
        many => {
            let names: Vec<&str> = many.iter().map(|f| f.name.as_str()).collect();
            return Err(format!(
                "it defines {} functions ({}), a saved query is exactly one",
                many.len(),
                names.join(", ")
            ));
        }
    };
    if function.name.as_str() != name {
        return Err(format!(
            "it defines the function '{}', but the query is named '{name}'",
            function.name
        ));
    }
    if function.this_type.is_some() {
        return Err("a method function (fn Type.name()) cannot be a saved query".to_string());
    }
    Ok(function.params.iter().map(|p| p.to_string()).collect())
}

/// Whether the function's `signature` and the `declared` parameters name the same set.
fn check_signature(signature: &[String], declared: &[&str]) -> Result<(), String> {
    if let Some(missing) = signature.iter().find(|p| !declared.contains(&p.as_str())) {
        return Err(format!(
            "the function's parameter '{missing}' is not declared in params (declare it with a \
             type name such as \"integer\" or with #{{schema: .., default: ..}})"
        ));
    }
    if let Some(extra) = declared.iter().find(|p| !signature.iter().any(|s| s == *p)) {
        return Err(format!(
            "params declares '{extra}', which the function does not take"
        ));
    }
    Ok(())
}

/// The description a function's doc comments give (`///` lines or a `/** .. */` block), with
/// the comment leaders removed; `None` without doc comments.
fn doc_text<S: AsRef<str>>(comments: &[S]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for comment in comments {
        let comment = comment.as_ref();
        if let Some(block) = comment.strip_prefix("/**") {
            let block = block.strip_suffix("*/").unwrap_or(block);
            for line in block.lines() {
                let line = line.trim();
                let line = line.strip_prefix('*').map(str::trim_start).unwrap_or(line);
                lines.push(line.to_string());
            }
        } else {
            for line in comment.lines() {
                let line = line.trim_start();
                let line = line.strip_prefix("///").unwrap_or(line);
                lines.push(
                    line.strip_prefix(' ')
                        .unwrap_or(line)
                        .trim_end()
                        .to_string(),
                );
            }
        }
    }
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The byte offset of a Rhai position (1-based line and character column) in `source`.
fn offset_of(source: &str, position: rhai::Position) -> Option<usize> {
    let line = position.line()?;
    let column = position.position()?;
    let mut start = 0;
    for _ in 1..line {
        start += source[start..].find('\n')? + 1;
    }
    source[start..]
        .char_indices()
        .nth(column.checked_sub(1)?)
        .map(|(index, _)| start + index)
}

/// The text of one function of `source` as a body of its own: `fn name(params) { .. }` with
/// the function's block (from its `{` at `open` to its `}` at `close`) copied verbatim,
/// comments inside it included.
fn function_text(
    source: &str,
    name: &str,
    params: &[String],
    (open, close): (rhai::Position, rhai::Position),
) -> Result<String, String> {
    let located = offset_of(source, open)
        .zip(offset_of(source, close))
        .filter(|(start, end)| {
            start <= end && source[*start..].starts_with('{') && source[*end..].starts_with('}')
        });
    let Some((start, end)) = located else {
        return Err(format!(
            "the body of the function '{name}' could not be located"
        ));
    };
    Ok(format!(
        "fn {name}({}) {}",
        params.join(", "),
        &source[start..=end]
    ))
}

/// A script error with the diagnostic of `err`.
pub(super) fn fail(err: ScriptError) -> Box<EvalAltResult> {
    script_error_from_script(err)
}

/// A script error with the diagnostic of a graph error (a catalog change that failed).
pub(super) fn graph_fail(err: crate::error::GraphError) -> Box<EvalAltResult> {
    script_error(Box::new(crate::TraverserError::from(err)))
}

/// Adds the line `in saved query: "name"` to an error coming out of the query's body (before
/// its `Help:`), so the origin is named; nested calls add one line each, innermost first.
/// Resource-limit and termination errors pass through unchanged, so a host still recognizes
/// them (`resource_limit_from_rhai`, a cancelled run).
fn in_saved_query(err: EvalAltResult, name: &str) -> Box<EvalAltResult> {
    let err = match err {
        EvalAltResult::ErrorInFunctionCall(function, _, inner, _) if function == name => inner,
        other => Box::new(other),
    };
    let (mut text, position) = match *err {
        EvalAltResult::ErrorRuntime(diagnostic, position) if diagnostic.is_string() => {
            (diagnostic.to_string(), position)
        }
        limit @ (EvalAltResult::ErrorTooManyOperations(..)
        | EvalAltResult::ErrorStackOverflow(..)
        | EvalAltResult::ErrorDataTooLarge(..)
        | EvalAltResult::ErrorTooManyVariables(..)
        | EvalAltResult::ErrorTooManyModules(..)
        | EvalAltResult::ErrorTerminated(..)) => return Box::new(limit),
        other => {
            let position = other.position();
            let mut other = map_eval_error(Box::new(other), &ScriptLimits::default());
            other.clear_position();
            (format!("Error: {other}"), position)
        }
    };
    let mut line = format!("  in saved query: \"{name}\"");
    if let (Some(row), Some(column)) = (position.line(), position.position()) {
        line.push_str(&format!(" (line {row}, position {column} of its body)"));
    }
    let at = text.find("\nHelp: ").unwrap_or(text.len());
    add_stack_line(&mut text, at, &line);
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(text),
        rhai::Position::NONE,
    ))
}

/// Inserts the stack line `line` at `at` (the end of the stack, before `Help:`). A line equal
/// to the one before it is counted instead of repeated (`in saved query: "r" (×16)` for a
/// runaway recursion).
fn add_stack_line(text: &mut String, at: usize, line: &str) {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let previous = &text[start..at];
    let count = if previous == line {
        Some(2)
    } else {
        previous
            .strip_prefix(line)
            .and_then(|rest| rest.strip_prefix(" (×"))
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|n| n.parse::<u32>().ok())
            .map(|n| n.saturating_add(1))
    };
    match count {
        Some(n) => text.replace_range(start..at, &format!("{line} (×{n})")),
        None => text.insert_str(at, &format!("\n{line}")),
    }
}

/// The first non-data value in a call argument (a traversal, a closure, a token object, ...):
/// its type name, or `None` when the value is data. Bounded by `levels` (a deeper value fails
/// the depth check of the conversion afterwards).
fn non_data(value: &Dynamic, levels: usize) -> Option<String> {
    if value.is_string()
        || value.is_int()
        || value.is_float()
        || value.is_bool()
        || value.is_unit()
        || value.is::<ScriptUuid>()
        || value.is::<ScriptVector>()
        || value.is::<ScriptMap>()
    {
        return None;
    }
    let inner = levels.checked_sub(1)?;
    if let Some(items) = value.read_lock::<rhai::Array>() {
        return items.iter().find_map(|item| non_data(item, inner));
    }
    if let Some(map) = value.read_lock::<rhai::Map>() {
        return map.values().find_map(|item| non_data(item, inner));
    }
    Some(type_name_of(value))
}

/// Where `value` holds a traversal whose plan writes (also inside arrays and maps, bounded by
/// `levels`): `location` extended by the index or key path, or `None`.
fn returned_write(value: &Dynamic, location: &str, levels: usize) -> Option<String> {
    if let Some(source) = value.read_lock::<ScriptGraphTraversalSource>() {
        return source.plan_writes().then(|| location.to_string());
    }
    let inner = levels.checked_sub(1)?;
    let base = if location == "the result" {
        "result"
    } else {
        location
    };
    if let Some(items) = value.read_lock::<rhai::Array>() {
        return items
            .iter()
            .enumerate()
            .find_map(|(index, item)| returned_write(item, &format!("{base}[{index}]"), inner));
    }
    if let Some(map) = value.read_lock::<rhai::Map>() {
        return map
            .iter()
            .find_map(|(key, item)| returned_write(item, &format!("{base}.{key}"), inner));
    }
    None
}

/// What the body of a saved query that writes returned, with every traversal in it that writes
/// (also inside arrays and maps, bounded by `levels`) run now, inside the call's unit, and
/// replaced by a reading traversal over its results (`inject(results..)` on the same graph; an
/// anonymous one is left, and refused):
/// `fn add(n) { g.add_v("person").property("name", n) }` writes when it is called, and
/// `g.query("add", #{n: "x"}).count()` continues what it wrote like the result of a read-only
/// query.
fn run_returned_writes(
    ctx: &NativeCallContext,
    mut value: Dynamic,
    levels: usize,
) -> Result<Dynamic, Box<EvalAltResult>> {
    if value.is::<ScriptGraphTraversalSource>() {
        let mut source = value.cast::<ScriptGraphTraversalSource>();
        // An anonymous traversal (`__.add_v(..)`) is a child for the caller's steps, not a
        // query of the graph: it stays as it is, and the call refuses it below.
        if source.anonymous || !source.plan_writes() {
            return Ok(Dynamic::from(source));
        }
        let results = value_args(ctx, source.to_list()?)?;
        let reading = ScriptGraphTraversalSource {
            g: source.g.clone(),
            steps: Vec::new(),
            options: source.options.clone(),
            access: source.access.clone(),
            private: source.private,
            anonymous: false,
            now: None,
        }
        .add_step(Step::Inject(results));
        return Ok(Dynamic::from(reading));
    }
    let Some(inner) = levels.checked_sub(1) else {
        return Ok(value);
    };
    if let Ok(mut items) = value.as_array_mut() {
        for item in items.iter_mut() {
            *item = run_returned_writes(ctx, std::mem::take(item), inner)?;
        }
    } else if let Ok(mut map) = value.as_map_mut() {
        for item in map.values_mut() {
            *item = run_returned_writes(ctx, std::mem::take(item), inner)?;
        }
    }
    Ok(value)
}

/// What a saved query that writes returns, made safe to use after its unit: every traversal in
/// `value` (also inside arrays and maps, bounded by `levels`) that targets the unit's `private`
/// graph is retargeted to the caller's `shared` one, and every traversal gets the read-only
/// `access` (it runs outside the unit and outside the query's effects).
fn seal_returned(
    mut value: Dynamic,
    private: &Arc<dyn AnyGraph>,
    shared: &Arc<dyn AnyGraph>,
    access: &Arc<access::ScriptAccess>,
    levels: usize,
) -> Dynamic {
    if let Some(mut source) = value.write_lock::<ScriptGraphTraversalSource>() {
        if source
            .g
            .as_ref()
            .is_some_and(|g| plan::same_graph(g, private))
        {
            source.g = Some(shared.clone());
        }
        source.access = access.clone();
    } else if let Some(inner) = levels.checked_sub(1)
        && let Ok(mut items) = value.as_array_mut()
    {
        for item in items.iter_mut() {
            *item = seal_returned(std::mem::take(item), private, shared, access, inner);
        }
    } else if let Some(inner) = levels.checked_sub(1)
        && let Ok(mut map) = value.as_map_mut()
    {
        for item in map.values_mut() {
            *item = seal_returned(std::mem::take(item), private, shared, access, inner);
        }
    }
    value
}

/// A string argument that may also be `()` (absent): `None` for `()` and for `""`.
pub(super) fn optional_text(
    operation: &str,
    what: &str,
    value: Dynamic,
) -> Result<Option<String>, Box<EvalAltResult>> {
    if value.is_unit() {
        return Ok(None);
    }
    if value.is_string() {
        let text = value.to_string();
        return Ok((!text.is_empty()).then_some(text));
    }
    Err(fail(ScriptError::ArgumentMismatch {
        step_name: operation.to_string(),
        expected: format!("{what} as a string (or () for none)"),
        actual: type_name_of(&value),
    }))
}

/// The parameter list of a definition from a function `signature` and the declarations in
/// `declared` (parameter name -> a type name or `#{schema, default}`), in signature order.
fn params_of(
    ctx: &NativeCallContext,
    query: &str,
    signature: &[String],
    declared: Option<rhai::Map>,
) -> Result<Vec<(String, QueryParam)>, Box<EvalAltResult>> {
    let declared = declared.unwrap_or_default();
    let names: Vec<&str> = declared.keys().map(|key| key.as_str()).collect();
    check_signature(signature, &names).map_err(|reason| {
        fail(ScriptError::InvalidQueryBody {
            query: query.to_string(),
            reason,
        })
    })?;
    let mut params = Vec::with_capacity(signature.len());
    for parameter in signature {
        let Some(declaration) = declared.get(parameter.as_str()) else {
            continue;
        };
        let value = value_arg(ctx, declaration.clone())?;
        let param = QueryParam::from_value(parameter, &value)
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        params.push((parameter.clone(), param));
    }
    Ok(params)
}

/// The definition of the saved query `name` written in GQL (`language: "gql"`): `body` checked
/// by [`saved_query_gql::check`] against the parameters declared in `declared` (as in
/// [`params_of`]), stored in the order the query first reads them, in the dialect
/// [`GQL_QUERY_DIALECT`] ([`with_inferred_dialect`] sets the one its plan gives). A GQL error
/// keeps its diagnostic (GQLSTATUS, caret, help) and gains the line `in saved query: "name"`.
#[cfg(feature = "gql")]
fn gql_definition(
    ctx: &NativeCallContext,
    name: &str,
    body: String,
    declared: Option<rhai::Map>,
    graph: Option<&dyn AnyGraph>,
) -> Result<QueryDefinition, Box<EvalAltResult>> {
    let names: Vec<String> = declared
        .as_ref()
        .map(|map| map.keys().map(ToString::to_string).collect())
        .unwrap_or_default();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    // Its calls of saved queries are checked against the graph's catalog (stage 3h).
    let catalog = graph.map(saved_query_gql::GraphCatalog);
    let catalog = catalog
        .as_ref()
        .map(|c| c as &dyn crate::gql::procedure::Procedures);
    let checked =
        saved_query_gql::check(name, &body, &names, catalog).map_err(|error| match error {
            saved_query_gql::GqlBodyError::Gql(error) => in_saved_query(*script_error(error), name),
            saved_query_gql::GqlBodyError::Parameters(error) => fail(error),
        })?;
    let params = params_of(ctx, name, &checked.parameters, declared)?;
    let mut query = QueryDefinition::new(body).with_dialect(GQL_QUERY_DIALECT);
    for (parameter, param) in params {
        query = query.with_param(parameter, param);
    }
    Ok(query)
}

/// A map argument (or `()` for none), for `operation`.
pub(super) fn optional_map(
    operation: &str,
    what: &str,
    value: Dynamic,
) -> Result<Option<rhai::Map>, Box<EvalAltResult>> {
    if value.is_unit() {
        return Ok(None);
    }
    if value.is_map() {
        return Ok(Some(value.cast::<rhai::Map>()));
    }
    Err(fail(ScriptError::ArgumentMismatch {
        step_name: operation.to_string(),
        expected: what.to_string(),
        actual: type_name_of(&value),
    }))
}

/// One effect as a script map: `#{action, element, label, text}` (`element` / `label` `()`
/// when open: every kind, every label).
fn effect_entry(effect: &AccessRequestBuf) -> Dynamic {
    let request = effect.as_request();
    let mut map = rhai::Map::new();
    map.insert(
        "action".into(),
        Dynamic::from(request.action.kind().name().to_string()),
    );
    let (element, label) = match request.resource {
        Resource::Data { element, label } => (element, label),
        _ => (None, None),
    };
    map.insert(
        "element".into(),
        element.map_or(Dynamic::UNIT, |kind| Dynamic::from(kind.to_string())),
    );
    map.insert(
        "label".into(),
        label.map_or(Dynamic::UNIT, |label| Dynamic::from(label.to_string())),
    );
    map.insert("text".into(), Dynamic::from(effect.to_string()));
    Dynamic::from_map(map)
}

/// The entry of `queries()` / `get_query()` for a saved query: name, folder, description,
/// language and dialect ([`QueryDefinition::language`]: `"rhai"` or `"gql"`), params (in
/// signature order: `#{name, schema, default?, required}`), status (`"ok"` or `"error"` with
/// `error`, the reason it cannot run: it is unavailable), `writes`, `effects` and
/// `denied_writes` ([`SavedQueryEffects`], each effect `#{action, element, label, text}`); with
/// `full` also body and meta. `catalog` resolves the queries it calls.
fn query_entry(
    engine: &Engine,
    catalog: &dyn Fn(&str) -> Option<QueryDefinition>,
    name: &str,
    query: &QueryDefinition,
    full: bool,
) -> rhai::Map {
    let text = |value: Option<&str>| value.map_or(Dynamic::UNIT, |v| Dynamic::from(v.to_string()));
    let mut map = rhai::Map::new();
    map.insert("name".into(), Dynamic::from(name.to_string()));
    map.insert("folder".into(), text(query.folder()));
    map.insert("description".into(), text(query.description()));
    map.insert(
        "language".into(),
        Dynamic::from(query.language().to_string()),
    );
    map.insert("dialect".into(), Dynamic::from(query.dialect().to_string()));
    let params: rhai::Array = query
        .params()
        .iter()
        .map(|(parameter, param)| {
            let mut entry = rhai::Map::new();
            entry.insert("name".into(), Dynamic::from(parameter.clone()));
            entry.insert(
                "schema".into(),
                element_property_to_dynamic(param.schema().to_value()),
            );
            if let Some(default) = param.default_value() {
                entry.insert(
                    "default".into(),
                    element_property_to_dynamic(default.clone()),
                );
            }
            entry.insert("required".into(), Dynamic::from_bool(param.is_required()));
            Dynamic::from_map(entry)
        })
        .collect();
    map.insert("params".into(), Dynamic::from_array(params));
    let effects = match saved_query_effects(engine, catalog, name, query) {
        Ok(effects) => {
            map.insert("status".into(), Dynamic::from("ok".to_string()));
            effects
        }
        Err(reason) => {
            map.insert("status".into(), Dynamic::from("error".to_string()));
            map.insert("error".into(), Dynamic::from(reason));
            SavedQueryEffects::default()
        }
    };
    let list = |effects: &[AccessRequestBuf]| {
        Dynamic::from_array(effects.iter().map(effect_entry).collect())
    };
    map.insert("writes".into(), Dynamic::from_bool(effects.writes));
    map.insert("effects".into(), list(&effects.effects));
    map.insert("denied_writes".into(), list(&effects.denied_writes));
    if full {
        map.insert("body".into(), Dynamic::from(query.source().to_string()));
        map.insert("meta".into(), text(query.meta()));
    }
    map
}

/// Whether `folder` is `prefix` or a folder below it (`prefix` without a trailing `/`; the
/// empty prefix matches everything).
fn in_folder(folder: Option<&str>, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    folder.is_some_and(|folder| {
        folder == prefix
            || folder
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The catalog of `graph` as the lookup [`saved_query_effects`] and [`with_inferred_dialect`]
/// take: the saved query of a name, `None` when there is none (or it is kept opaque).
fn stored_queries(graph: &Arc<dyn AnyGraph>) -> impl Fn(&str) -> Option<QueryDefinition> + '_ {
    move |name| {
        graph
            .definition(DefinitionKind::QUERY, name)
            .and_then(|definition| definition.as_query().cloned())
    }
}

impl ScriptGraphTraversalSource {
    /// The graph of this source, or a script error naming `operation`.
    fn catalog_graph(&self, operation: &str) -> Result<&Arc<dyn AnyGraph>, Box<EvalAltResult>> {
        self.g.as_ref().ok_or_else(|| {
            Box::new(EvalAltResult::ErrorRuntime(
                Dynamic::from(format!("{operation}: no graph associated with this source")),
                rhai::Position::NONE,
            ))
        })
    }

    /// Asks the session's authorizer for `action` on the saved query `name` (`None`: every
    /// saved query).
    fn require_query(
        &self,
        operation: &str,
        action: Action<'_>,
        name: Option<&str>,
    ) -> Result<(), Box<EvalAltResult>> {
        self.require(
            operation,
            AccessRequest::new(
                action,
                Resource::definition(Some(DefinitionKind::QUERY), name),
            ),
        )
    }

    /// Stores `queries` (name, definition) in ONE unit (all or none), asking for `Create` on
    /// each new one and `Update` on each replaced one first.
    fn store_queries(
        &self,
        operation: &str,
        queries: Vec<(String, QueryDefinition)>,
    ) -> Result<(), Box<EvalAltResult>> {
        let graph = self.catalog_graph(operation)?;
        for (name, _) in &queries {
            let exists = graph.definition(DefinitionKind::QUERY, name).is_some();
            let action = if exists {
                Action::Update
            } else {
                Action::Create
            };
            self.require_query(operation, action, Some(name))?;
        }
        graph
            .set_definitions(
                queries
                    .into_iter()
                    .map(|(name, query)| Definition::query(name, query))
                    .collect(),
            )
            .map_err(script_error)
    }

    /// `g.define_query(#{name, body, language?, folder?, description?, params?, meta?})` /
    /// `g.defineQuery(..)`: stores (or replaces) a saved query. With `language` `"rhai"` (the
    /// default) `body` is the text of one Rhai function named `name` whose parameters are
    /// declared in `params` (parameter name -> a type name such as `"integer"`, or
    /// `#{schema: <schema node>, default: <value>}`); `description` defaults to the function's
    /// doc comment. With `language: "gql"` (feature `gql`) `body` is one GQL query whose
    /// `$name` references are exactly the declared parameters (stored in the order of their
    /// first use). Checked before anything is stored: the body compiles and is exactly that
    /// function (a GQL body parses, binds and lowers), the parameters match its signature (the
    /// parameters the GQL query reads), and everything the catalog validates (name, folder,
    /// sizes, schemas, defaults). A unit of its own (a savepoint inside a whole-script unit).
    /// Asks for `Create` on the query (`Update` when it replaces one).
    pub fn define_query(
        ctx: NativeCallContext,
        this: &mut Self,
        spec: Dynamic,
    ) -> Result<(), Box<EvalAltResult>> {
        const KEYS: [&str; 7] = [
            "name",
            "body",
            "language",
            "folder",
            "description",
            "params",
            "meta",
        ];
        let operation = "define_query";
        let actual = type_name_of(&spec);
        let Some(mut spec) = spec.try_cast::<rhai::Map>() else {
            return Err(fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected: "a map #{name, body, language?, folder?, description?, params?, meta?}"
                    .to_string(),
                actual,
            }));
        };
        if let Some(unknown) = spec.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected: format!("only the keys {}", KEYS.join(", ")),
                actual: format!("the key '{unknown}'"),
            }));
        }
        let mut take = |key: &str| spec.remove(key).unwrap_or(Dynamic::UNIT);
        let name = optional_text(operation, "name", take("name"))?.ok_or_else(|| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected: "the key name (the query's name)".to_string(),
                actual: "no name".to_string(),
            })
        })?;
        let gql = match optional_text(operation, "language", take("language"))?.as_deref() {
            None | Some("rhai") => false,
            Some("gql") if cfg!(feature = "gql") => true,
            Some(other) => {
                return Err(fail(ScriptError::ArgumentMismatch {
                    step_name: operation.to_string(),
                    expected: if cfg!(feature = "gql") {
                        "language \"rhai\" (the default) or \"gql\"".to_string()
                    } else {
                        "language \"rhai\" (this build has no GQL: feature gql)".to_string()
                    },
                    actual: format!("the language '{other}'"),
                }));
            }
        };
        let body = optional_text(operation, "body", take("body"))?.ok_or_else(|| {
            fail(ScriptError::ArgumentMismatch {
                step_name: operation.to_string(),
                expected: if gql {
                    "the key body (the text of one GQL query)".to_string()
                } else {
                    "the key body (the text of one Rhai function)".to_string()
                },
                actual: "no body".to_string(),
            })
        })?;
        let folder = optional_text(operation, "folder", take("folder"))?;
        let description = optional_text(operation, "description", take("description"))?;
        let meta = optional_text(operation, "meta", take("meta"))?;
        let declared = optional_map(
            operation,
            "params as a map (parameter name -> type name or #{schema, default})",
            take("params"),
        )?;

        // The name first (a keyword or a too long name would only confuse the compile check).
        Definition::query(name.as_str(), QueryDefinition::new("fn"))
            .validate()
            .map_err(|err| graph_fail(crate::error::GraphError::from(err)))?;
        #[cfg(feature = "gql")]
        if gql {
            let query = gql_definition(&ctx, &name, body, declared, this.g.as_deref())?;
            let query = match folder {
                Some(folder) => query.with_folder(folder),
                None => query,
            };
            let query = match description {
                Some(description) => query.with_description(description),
                None => query,
            };
            let query = match meta {
                Some(meta) => query.with_meta(meta),
                None => query,
            };
            let graph = this.catalog_graph(operation)?.clone();
            let query = with_inferred_dialect(ctx.engine(), &stored_queries(&graph), &name, query)
                .map_err(|reason| {
                    fail(ScriptError::InvalidQueryBody {
                        query: name.clone(),
                        reason,
                    })
                })?;
            return this.store_queries(operation, vec![(name, query)]);
        }
        let ast = ctx.engine().compile(&body).map_err(|err| {
            fail(ScriptError::InvalidQueryBody {
                query: name.clone(),
                reason: format!("it does not compile: {err}"),
            })
        })?;
        let invalid = |reason: String| {
            fail(ScriptError::InvalidQueryBody {
                query: name.clone(),
                reason,
            })
        };
        let signature = single_function(&ast, &name).map_err(invalid)?;
        let doc = ast
            .iter_fn_def()
            .next()
            .and_then(|function| doc_text(&function.comments[..]));
        let params = params_of(&ctx, &name, &signature, declared)?;

        let mut query = QueryDefinition::new(body);
        if let Some(folder) = folder {
            query = query.with_folder(folder);
        }
        if let Some(description) = description.or(doc) {
            query = query.with_description(description);
        }
        if let Some(meta) = meta {
            query = query.with_meta(meta);
        }
        for (parameter, param) in params {
            query = query.with_param(parameter, param);
        }
        let graph = this.catalog_graph(operation)?.clone();
        let query = with_inferred_dialect(ctx.engine(), &stored_queries(&graph), &name, query)
            .map_err(invalid)?;
        this.store_queries(operation, vec![(name, query)])
    }

    /// `g.define_queries(folder, source)` / `g.defineQueries(..)`: [`Self::define_queries_with`]
    /// for functions without parameters.
    pub fn define_queries(
        ctx: NativeCallContext,
        this: &mut Self,
        folder: Dynamic,
        source: String,
    ) -> Result<rhai::Array, Box<EvalAltResult>> {
        Self::define_queries_with(ctx, this, folder, source, Dynamic::UNIT)
    }

    /// `g.define_queries(folder, source, params)` / `g.defineQueries(..)`: stores every function
    /// of `source` as a saved query of its own, named like the function, in `folder` (`""` or
    /// `()`: the root), described by its doc comment (`///` lines or a `/** */` block), with
    /// the parameters declared in `params` (function name -> its parameter map, as in
    /// [`Self::define_query`]). All queries are stored in ONE unit (all or none); returns their
    /// names. Asks for `Create` (`Update`) on each.
    pub fn define_queries_with(
        ctx: NativeCallContext,
        this: &mut Self,
        folder: Dynamic,
        source: String,
        params: Dynamic,
    ) -> Result<rhai::Array, Box<EvalAltResult>> {
        let operation = "define_queries";
        let folder = optional_text(operation, "the folder", folder)?;
        let mut declared = optional_map(
            operation,
            "params as a map (function name -> its parameter map)",
            params,
        )?
        .unwrap_or_default();
        let invalid = |query: &str, reason: String| {
            fail(ScriptError::InvalidQueryBody {
                query: query.to_string(),
                reason,
            })
        };
        let ast = ctx
            .engine()
            .compile(&source)
            .map_err(|err| invalid("*", format!("the source does not compile: {err}")))?;
        if !ast.statements().is_empty() {
            return Err(invalid(
                "*",
                "the source has statements outside its functions; it holds functions only"
                    .to_string(),
            ));
        }
        let mut functions: Vec<_> = ast.iter_fn_def().collect();
        functions.sort_by(|a, b| a.name.cmp(&b.name));
        if functions.is_empty() {
            return Err(invalid("*", "the source defines no function".to_string()));
        }
        let mut queries = Vec::with_capacity(functions.len());
        for (index, function) in functions.iter().enumerate() {
            let name = function.name.to_string();
            if index > 0 && functions[index - 1].name == function.name {
                return Err(invalid(
                    &name,
                    "the source defines two functions of this name; a saved query's name is \
                     unique"
                        .to_string(),
                ));
            }
            if function.this_type.is_some() {
                return Err(invalid(
                    &name,
                    "a method function (fn Type.name()) cannot be a saved query".to_string(),
                ));
            }
            let signature: Vec<String> = function.params.iter().map(|p| p.to_string()).collect();
            let text = function_text(
                &source,
                &name,
                &signature,
                (function.body.start_position(), function.body.end_position()),
            )
            .map_err(|reason| invalid(&name, reason))?;
            let own = match declared.remove(name.as_str()) {
                None => None,
                Some(value) => optional_map(operation, "the parameter map of a function", value)?,
            };
            let params = params_of(&ctx, &name, &signature, own)?;
            let mut query = QueryDefinition::new(text);
            if let Some(folder) = &folder {
                query = query.with_folder(folder.clone());
            }
            if let Some(description) = doc_text(&function.comments[..]) {
                query = query.with_description(description);
            }
            for (parameter, param) in params {
                query = query.with_param(parameter, param);
            }
            // The extracted text must stand on its own as the body of the query.
            let own = compile_uncached(ctx.engine(), &name, &query)
                .and_then(|compiled| compiled.effects)
                .map_err(|reason| invalid(&name, reason))?;
            queries.push((name, query, own));
        }
        if let Some(unknown) = declared.keys().next() {
            return Err(invalid(
                unknown,
                "params declares parameters for this name, but the source defines no such \
                 function"
                    .to_string(),
            ));
        }
        // The dialects: a query of the source calling another one of it sees that one's effects
        // (it is saved in the same unit), any other through the catalog.
        let stored = stored_queries(this.catalog_graph(operation)?);
        let mut dialects = Vec::with_capacity(queries.len());
        for (name, _, _) in &queries {
            let (effects, dynamic) = effects::transitive(name, &mut |next| match queries
                .iter()
                .find(|(other, _, _)| other == next)
            {
                Some((_, _, own)) => Some(effects::Node {
                    effects: own.clone(),
                    counts: true,
                }),
                None => node_of(ctx.engine(), next, &stored(next)?),
            });
            dialects.push(
                dialect_for(false, &effects, dynamic.as_deref()).map_err(|r| invalid(name, r))?,
            );
        }
        let names: rhai::Array = queries
            .iter()
            .map(|(name, _, _)| Dynamic::from(name.clone()))
            .collect();
        let queries = queries
            .into_iter()
            .zip(dialects)
            .map(|((name, query, _), dialect)| (name, query.with_dialect(dialect)))
            .collect();
        this.store_queries(operation, queries)?;
        Ok(names)
    }

    /// `g.query(name)`: [`Self::query_with`] without arguments (every parameter takes its
    /// default).
    pub fn query(
        ctx: NativeCallContext,
        this: &mut Self,
        name: &str,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        this.call_query(&ctx, name, rhai::Map::new())
    }

    /// `g.query(name, #{param: value, ..})`: calls the saved query `name` with named
    /// arguments and returns its result as is: a traversal (continue it like any other, it
    /// stays read-only) or a value. Only at the start of a traversal, on `g` (not after steps,
    /// not on `__`). See the module docs for the call rules.
    pub fn query_with(
        ctx: NativeCallContext,
        this: &mut Self,
        name: &str,
        args: Dynamic,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let args = optional_map(
            "query",
            "the named arguments as a map, for example #{limit: 5}",
            args,
        )?
        .unwrap_or_default();
        this.call_query(&ctx, name, args)
    }

    /// [`Self::call_query_inner`], counted in the query statistics as `saved:<name>` (the call
    /// itself: a returned traversal runs later and counts under its plan's fingerprint).
    fn call_query(
        &self,
        ctx: &NativeCallContext,
        name: &str,
        args: rhai::Map,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let started = web_time::Instant::now();
        let result = self.call_query_inner(ctx, name, args);
        if let Some(stats) = self.g.as_ref().and_then(|g| g.workload_stats()) {
            use crate::graph::workload::{Sample, SeriesKey, SeriesKind, WorkloadLevel};
            let origin = crate::graph::workload::scope::current();
            if !origin.technical && stats.level() != WorkloadLevel::Off {
                let mut sample = Sample::new(0, started.elapsed().as_micros() as u64);
                if result.is_err() {
                    sample = sample.failed();
                }
                let now = web_time::SystemTime::now()
                    .duration_since(web_time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_micros() as i64);
                let key = SeriesKey::new(
                    SeriesKind::Query(format!("saved:{name}").into()),
                    origin.channel,
                );
                stats.record(key, now, sample);
            }
        }
        result
    }

    fn call_query_inner(
        &self,
        ctx: &NativeCallContext,
        name: &str,
        mut args: rhai::Map,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let not_at_start = |position: &str| {
            fail(ScriptError::QueryNotAtStart {
                name: name.to_string(),
                position: position.to_string(),
            })
        };
        if self.anonymous {
            return Err(not_at_start("__ (an anonymous traversal)"));
        }
        if !self.steps.is_empty() {
            return Err(not_at_start("other steps"));
        }
        if self.access.query_depth >= MAX_QUERY_DEPTH {
            return Err(fail(ScriptError::QueryDepthExceeded {
                name: name.to_string(),
                limit: MAX_QUERY_DEPTH,
            }));
        }
        self.require_query("query", Action::Execute, Some(name))?;
        let graph = self.catalog_graph("query")?;
        let definition = graph
            .definition(DefinitionKind::QUERY, name)
            .ok_or_else(|| {
                graph_fail(crate::error::GraphError::from(
                    crate::error::DefinitionError::NotFound {
                        kind: DefinitionKind::QUERY.to_string(),
                        name: name.to_string(),
                    },
                ))
            })?;
        let invalid_body = |reason: String| {
            fail(ScriptError::InvalidQueryBody {
                query: name.to_string(),
                reason,
            })
        };
        let Some(query) = definition.as_query() else {
            return Err(invalid_body(
                "it is kept opaque (this version could not decode it)".to_string(),
            ));
        };
        if let Some(reason) = query.unsupported_reason() {
            return Err(invalid_body(reason));
        }
        let compiled = compile(ctx.engine(), name, query);
        let compiled = match compiled.as_ref() {
            Ok(compiled) => compiled,
            Err(reason) => return Err(invalid_body(reason.clone())),
        };

        // The arguments, in signature order.
        if let Some(unknown) = args
            .keys()
            .find(|key| !compiled.signature.iter().any(|p| p == key.as_str()))
        {
            return Err(fail(ScriptError::UnknownQueryArgument {
                query: name.to_string(),
                argument: unknown.to_string(),
                parameters: compiled.signature.clone(),
            }));
        }
        let depth = access::value_depth_of(ctx);
        let mut values = Vec::with_capacity(compiled.signature.len());
        let mut bound = Vec::with_capacity(compiled.signature.len());
        for parameter in &compiled.signature {
            let Some(param) = query.param(parameter) else {
                return Err(invalid_body(format!(
                    "the parameter '{parameter}' is not declared"
                )));
            };
            let invalid = |reason: String| {
                fail(ScriptError::InvalidQueryArgument {
                    query: name.to_string(),
                    parameter: parameter.clone(),
                    reason,
                })
            };
            let value = match args.remove(parameter.as_str()) {
                Some(value) => {
                    if let Some(type_name) = non_data(&value, depth) {
                        return Err(invalid(format!(
                            "a {type_name} is not a data value; arguments are values only"
                        )));
                    }
                    let value = value_arg(ctx, value)?;
                    param
                        .check_value(name, parameter, value)
                        .map_err(|err| invalid(err.to_string()))?
                }
                None => match param.default_value() {
                    Some(default) => default.clone(),
                    None => {
                        return Err(fail(ScriptError::MissingQueryArgument {
                            query: name.to_string(),
                            parameter: parameter.clone(),
                            expected: param.schema().non_null_type_name(),
                        }));
                    }
                },
            };
            bound.push((parameter.clone(), value.clone()));
            values.push(element_property_to_dynamic(value));
        }

        // What the call may write: the inferred effects of a query that writes (with those of
        // the queries it calls), none when it is read-only or called from a read-only body.
        let effects = match &compiled.effects {
            Ok(own) if is_writing(query.dialect()) && !self.access.read_only => {
                resolve_effects(ctx.engine(), &stored_queries(graph), name, own).0
            }
            _ => Vec::new(),
        };
        // A read-only body: the caller's access, read-only, one level deeper.
        let read_only = Arc::new(access::ScriptAccess {
            authorizer: if self.access.read_only {
                self.access.authorizer.clone()
            } else {
                Arc::new(ReadOnly {
                    inner: self.access.authorizer.clone(),
                })
            },
            presets: self.access.presets.clone(),
            max_value_depth: self.access.max_value_depth,
            read_only: true,
            query_depth: self.access.query_depth + 1,
            embedders: self.access.embedders.clone(),
        });
        if effects.is_empty() {
            let result =
                self.run_body(ctx, compiled, query, name, graph, read_only, values, &bound)?;
            // A GQL query whose plan writes runs now, so its writes meet the read-only
            // authorizer like those of a Rhai body (denied before anything is written).
            #[cfg(feature = "gql")]
            let result = match compiled.body {
                Body::Gql => run_returned_writes(ctx, result, depth)
                    .map_err(|err| in_saved_query(*err, name))?,
                Body::Rhai(_) => result,
            };
            if let Some(location) = returned_write(&result, "the result", depth) {
                return Err(fail(ScriptError::QueryReturnedWrite {
                    query: name.to_string(),
                    location,
                }));
            }
            return Ok(result);
        }

        // A body that writes. The whole effect set is asked of the caller's authorizer before
        // anything runs, so a denial comes before the first write.
        for effect in &effects {
            let request = effect.as_request();
            self.access
                .authorizer
                .authorize(&request)
                .map_err(|denied| {
                    script_error(crate::traversal::permission_check::permission_denied(
                        &format!("query(\"{name}\")"),
                        &request,
                        denied.reason(),
                    ))
                })?;
        }
        let writing = Arc::new(access::ScriptAccess {
            authorizer: Arc::new(Effects {
                inner: self.access.authorizer.clone(),
                query: name.to_string(),
                effects,
            }),
            presets: self.access.presets.clone(),
            max_value_depth: self.access.max_value_depth,
            read_only: false,
            query_depth: self.access.query_depth + 1,
            embedders: self.access.embedders.clone(),
        });
        let mut arguments = crate::value::ValueMap::new();
        for (parameter, value) in &bound {
            arguments.insert(ElementProperty::from(parameter.as_str()), value.clone());
        }
        let metadata = CommitMetadata::new()
            .with_attribute("saved_query", name)
            .with_attribute("saved_query_args", arguments.into_property());
        // ONE unit over the whole body (a savepoint inside an open one): the storage moves into
        // a private graph the body writes. A returned traversal that writes runs in the unit
        // too and is replaced by a reading one over its results; what is returned is then
        // retargeted to the caller's graph and keeps only the read-only access (a returned
        // traversal runs after the unit).
        let mut values = Some(values);
        let mut body = |private: &Arc<dyn AnyGraph>| {
            let Some(values) = values.take() else {
                return plan::UnitBody(Err(fail(ScriptError::InvalidQueryBody {
                    query: name.to_string(),
                    reason: "its unit ran twice".to_string(),
                })));
            };
            plan::UnitBody(
                self.run_body(
                    ctx,
                    compiled,
                    query,
                    name,
                    private,
                    writing.clone(),
                    values,
                    &bound,
                )
                .and_then(|value| {
                    run_returned_writes(ctx, value, depth).map_err(|err| in_saved_query(*err, name))
                })
                .and_then(|value| match returned_write(&value, "the result", depth) {
                    Some(location) => Err(fail(ScriptError::QueryReturnedWrite {
                        query: name.to_string(),
                        location,
                    })),
                    None => Ok(value),
                })
                .map(|value| seal_returned(value, private, graph, &read_only, depth)),
            )
        };
        graph
            .run_unit(plan::UnitKind::Call(metadata), &mut body)
            .0
            .map(|(value, _)| value)
    }

    /// Runs the compiled body of the saved query `name` (definition `query`) with the argument
    /// `values` (in signature order; `bound` the same as data values) on `graph` under
    /// `access`; its result as it is. A GQL body
    /// runs no Rhai: its result is the traversal `g.gql(body, params)` of `graph` under
    /// `access`, not run yet.
    #[allow(clippy::too_many_arguments)]
    fn run_body(
        &self,
        ctx: &NativeCallContext,
        compiled: &Compiled,
        query: &QueryDefinition,
        name: &str,
        graph: &Arc<dyn AnyGraph>,
        access: Arc<access::ScriptAccess>,
        values: Vec<Dynamic>,
        bound: &[(String, ElementProperty)],
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let g = ScriptGraphTraversalSource {
            g: Some(graph.clone()),
            steps: Vec::new(),
            options: self.options.clone(),
            access: access.clone(),
            private: false,
            anonymous: false,
            now: None,
        };
        let ast = match &compiled.body {
            Body::Rhai(ast) => ast,
            #[cfg(feature = "gql")]
            Body::Gql => {
                let params = bound
                    .iter()
                    .map(|(parameter, value)| (CompactString::new(parameter), value.clone()))
                    .collect();
                let mut g = g;
                return g
                    .gql_step(query.source().to_string(), params)
                    .map(|source| Dynamic::from(source.stamped(ctx)))
                    .map_err(|err| in_saved_query(*err, name));
            }
        };
        #[cfg(not(feature = "gql"))]
        let _ = (query, bound);
        let mut scope = eval::scope_with_access(g, access);
        // The body sees the caller's `now()` (one value per evaluation, `access.rs`).
        let tag = access::nested_tag(ctx, access::ScriptAccess::of_context(ctx));
        ctx.engine()
            .call_fn_with_options::<Dynamic>(
                rhai::CallFnOptions::new()
                    .eval_ast(false)
                    .rewind_scope(true)
                    .with_tag(tag),
                &mut scope,
                ast,
                name,
                values,
            )
            .map_err(|err| in_saved_query(*err, name))
    }

    /// `g.queries()`: every saved query as a list of maps, by name (name,
    /// folder, description, params, status and error). Asks for `Read` on the saved queries.
    pub fn queries(
        ctx: NativeCallContext,
        this: &mut Self,
    ) -> Result<rhai::Array, Box<EvalAltResult>> {
        Self::queries_in(ctx, this, String::new())
    }

    /// `g.queries(folder)`: the saved queries in `folder` and the folders below it
    /// (`"reports"` lists `reports` and `reports/sales`; a trailing `/` is ignored).
    pub fn queries_in(
        ctx: NativeCallContext,
        this: &mut Self,
        folder: String,
    ) -> Result<rhai::Array, Box<EvalAltResult>> {
        this.require_query("queries", Action::Read, None)?;
        let prefix = folder.trim().trim_end_matches('/');
        let graph = this.catalog_graph("queries")?;
        let catalog = stored_queries(graph);
        let definitions = graph.definitions(Some(DefinitionKind::QUERY));
        Ok(definitions
            .iter()
            .filter_map(|definition| {
                let query = definition.as_query()?;
                in_folder(query.folder(), prefix).then(|| {
                    Dynamic::from_map(query_entry(
                        ctx.engine(),
                        &catalog,
                        definition.name(),
                        query,
                        false,
                    ))
                })
            })
            .collect())
    }

    /// `g.get_query(name)` / `g.getQuery(name)`: the saved query as a map (the entry of
    /// [`Self::queries`] plus `body`, `dialect` and `meta`), `()` when there is none. Asks for
    /// `Read` on it.
    pub fn get_query(
        ctx: NativeCallContext,
        this: &mut Self,
        name: &str,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        this.require_query("get_query", Action::Read, Some(name))?;
        let graph = this.catalog_graph("get_query")?;
        let definition = graph.definition(DefinitionKind::QUERY, name);
        Ok(definition
            .as_ref()
            .and_then(Definition::as_query)
            .map_or(Dynamic::UNIT, |query| {
                Dynamic::from_map(query_entry(
                    ctx.engine(),
                    &stored_queries(graph),
                    name,
                    query,
                    true,
                ))
            }))
    }

    /// `g.drop_query(name)` / `g.dropQuery(name)`: removes the saved query; `false` when there
    /// is none. A unit of its own; asks for `Delete` on it. Callers of a dropped query fail
    /// when they call it (dependencies are not tracked).
    pub fn drop_query(&mut self, name: &str) -> Result<bool, Box<EvalAltResult>> {
        self.require_query("drop_query", Action::Delete, Some(name))?;
        self.catalog_graph("drop_query")?
            .remove_definition(DefinitionKind::QUERY, name)
            .map_err(script_error)
    }

    /// `g.move_query(name, folder)` / `g.moveQuery(..)`: moves the saved query into `folder`
    /// (`""` or `()`: the root). Callers name a query without its folder, so moving breaks
    /// none. Asks for `Update` on it.
    pub fn move_query(&mut self, name: &str, folder: Dynamic) -> Result<(), Box<EvalAltResult>> {
        let folder = optional_text("move_query", "the folder", folder)?;
        self.require_query("move_query", Action::Update, Some(name))?;
        self.catalog_graph("move_query")?
            .move_query(name, folder.as_deref())
            .map_err(script_error)
    }

    /// `g.describe_query(name, text)` / `g.describeQuery(..)`: sets the description of the
    /// saved query (`""` or `()` removes it). Asks for `Update` on it.
    pub fn describe_query(
        &mut self,
        name: &str,
        description: Dynamic,
    ) -> Result<(), Box<EvalAltResult>> {
        let description = optional_text("describe_query", "the description", description)?;
        self.require_query("describe_query", Action::Update, Some(name))?;
        self.catalog_graph("describe_query")?
            .describe_query(name, description.as_deref())
            .map_err(script_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_comments_become_descriptions() {
        assert_eq!(
            doc_text(&["/// First line.\n/// Second.".to_string()]).as_deref(),
            Some("First line.\nSecond.")
        );
        assert_eq!(
            doc_text(&["/** Block\n * doc */".to_string()]).as_deref(),
            Some("Block\ndoc")
        );
        assert_eq!(doc_text::<String>(&[]), None);
    }

    #[test]
    fn folders_match_by_segment() {
        assert!(in_folder(Some("reports"), "reports"));
        assert!(in_folder(Some("reports/sales"), "reports"));
        assert!(!in_folder(Some("reportsx"), "reports"));
        assert!(!in_folder(None, "reports"));
        assert!(in_folder(None, ""));
    }

    #[test]
    fn function_text_is_cut_from_the_source() {
        let engine = engine();
        let source = "\n/// d\nfn a(x, y) {\n  x + y // }\n}\nfn b() { `}` }\n";
        let ast = engine.compile(source).unwrap();
        let mut texts: Vec<String> = ast
            .iter_fn_def()
            .map(|f| {
                let params: Vec<String> = f.params.iter().map(|p| p.to_string()).collect();
                function_text(
                    source,
                    &f.name,
                    &params,
                    (f.body.start_position(), f.body.end_position()),
                )
                .unwrap()
            })
            .collect();
        texts.sort();
        assert_eq!(texts, ["fn a(x, y) {\n  x + y // }\n}", "fn b() { `}` }"]);
    }
}

#[cfg(test)]
mod stack_line_tests {
    use super::add_stack_line;

    fn add(text: &str, line: &str) -> String {
        let mut text = text.to_string();
        let at = text.find("\nHelp: ").unwrap_or(text.len());
        add_stack_line(&mut text, at, line);
        text
    }

    #[test]
    fn equal_lines_are_counted_different_ones_kept() {
        let a = "  in saved query: \"r\"";
        let b = "  in saved query: \"outer\"";
        let once = add("Error: x\nHelp: fix", a);
        assert_eq!(once, "Error: x\n  in saved query: \"r\"\nHelp: fix");
        let twice = add(&once, a);
        assert_eq!(twice, "Error: x\n  in saved query: \"r\" (×2)\nHelp: fix");
        let thrice = add(&twice, a);
        assert_eq!(thrice, "Error: x\n  in saved query: \"r\" (×3)\nHelp: fix");
        let outer = add(&thrice, b);
        assert_eq!(
            outer,
            "Error: x\n  in saved query: \"r\" (×3)\n  in saved query: \"outer\"\nHelp: fix"
        );
        assert_eq!(add("Error: x", a), "Error: x\n  in saved query: \"r\"");
    }
}
