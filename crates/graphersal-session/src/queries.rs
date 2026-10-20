//! Saved queries in the session (`book/src/users_guide/saved_queries.md`): the operations behind
//! the playground's Saved queries menu, editor and run form, the dev server's
//! `/api/queries*` endpoints and its MCP tools `list_saved_queries` / `run_saved_query`.
//!
//! Every operation works on the catalog of the graph the session serves (a Store's graph too:
//! a change is a commit like any other, journaled and recorded in the dev server's change
//! feed). A call may write only the data its body's inferred effects name, and only a query
//! saved as one that writes (enforced by the engine: `graphersal::script::saved_query_effects`);
//! managing them (define, replace, drop, move) is the playground's business, under its
//! authorizer. Saving a query here sets its dialect from the inferred effects
//! (`graphersal::script::with_inferred_dialect`), like the DSL's `g.define_query`.
//!
//! JSON forms:
//!
//! - an **entry** (`list_queries`, `get_query`): `{name, folder, description, params: [{name,
//!   schema, default?, required, description?}], status: "ok"|"error", error?, writes,
//!   effects: [effect], denied_writes: [effect], language, dialect}` with an effect `{action,
//!   element, label, text}` (`element` / `label` `null` when open); `status: "error"` means the query cannot
//!   run (unavailable: its body does not compile, its dialect is unknown, ...); `writes` says a
//!   call may write, `denied_writes` lists the writes of a read-only query's body that a call
//!   denies; `language` is `"rhai"` or `"gql"` (derived from the `dialect`, for example
//!   `graphersal-rhai/1` or `graphersal-gql/2`; an unknown dialect gives its own name);
//!   `get_query` adds `body` (the stored text) and `meta`;
//! - a **spec** (`define_query`): `{name, folder?, description?, params?: [{name, schema,
//!   default?}], language?, body, meta?, replaces?}`: `language` is `"rhai"` (the default) or
//!   `"gql"`; a Rhai `body` is the text of ONE Rhai function named `name` whose parameters are
//!   the declared ones, a GQL `body` is one plain GQL query (no `g.gql(..)` around it) whose
//!   `$name`s are exactly the declared parameters, stored in the declared order; `replaces`
//!   names the query an edit started from (a rename stores the new name and drops the old one
//!   in one commit);
//! - **field errors** (a refused `define_query`): `[{field, message, help?}]` with `field` one
//!   of `name`, `folder`, `description`, `meta`, `language`, `body`, `params`,
//!   `params.<parameter>`, or `general`, so an editor shows each next to its input.

use graphersal::auth::{AccessRequestBuf, Resource};
use graphersal::catalog::{
    Definition, DefinitionKind, GQL_QUERY_DIALECT, QueryDefinition, QueryParam,
};
use graphersal::catalog::{catalog_from_json, catalog_to_json};
use graphersal::changes::Mutation;
use graphersal::prelude::*;
use graphersal::script::{saved_query_effects, with_inferred_dialect};
use serde_json::{Map, Value, json};

use crate::Session;

/// How deep the call text of a parameter set may nest (the value depth bound of a script).
const MAX_LITERAL_DEPTH: usize = 128;

/// A refused definition: what is wrong with which field of the editor.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldError {
    /// `name`, `folder`, `description`, `meta`, `language`, `body`, `params`,
    /// `params.<parameter>`, `general`; a compression rule's and a property index's form add their own fields
    /// (`label`, `path`, `key`, ...).
    pub field: String,
    /// One line: what is wrong.
    pub message: String,
    /// How to fix it, when the engine knows.
    pub help: Option<String>,
}

impl FieldError {
    pub(crate) fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            help: None,
        }
    }

    /// The field a catalog error belongs to, with the engine's message and help.
    pub(crate) fn of(err: &DefinitionError) -> Self {
        let field = match err {
            DefinitionError::InvalidName { .. } => "name".to_string(),
            DefinitionError::InvalidFolder { .. } => "folder".to_string(),
            DefinitionError::InvalidField { field, .. } => match field.as_str() {
                "source" | "dialect" => "body".to_string(),
                "description" | "meta" => field.clone(),
                _ => "general".to_string(),
            },
            DefinitionError::InvalidParameter { parameter, .. } => format!("params.{parameter}"),
            // What two definitions of the kind may not share: a compression rule's path, a
            // property index's key (the label and element kind come with it).
            DefinitionError::Conflict { kind, .. } if kind == "property index" => "key".to_string(),
            DefinitionError::Conflict { kind, .. } if kind == "compression rule" => {
                "path".to_string()
            }
            _ => "general".to_string(),
        };
        Self {
            field,
            message: err.to_string(),
            help: err.help(),
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        let mut out = json!({"field": self.field, "message": self.message});
        if let Some(help) = &self.help {
            out["help"] = json!(help);
        }
        out
    }
}

/// JSON of a value of the engine (plain JSON; a uuid as its canonical text).
/// One effect of a saved query: `{action, element, label, text}` (`element` / `label` `null`
/// when open: every kind, every label).
fn effect_json(effect: &AccessRequestBuf) -> Value {
    let request = effect.as_request();
    let (element, label) = match request.resource {
        Resource::Data { element, label } => (element.map(|kind| kind.to_string()), label),
        _ => (None, None),
    };
    json!({
        "action": request.action.kind().name(),
        "element": element,
        "label": label,
        "text": effect.to_string(),
    })
}

fn value_json(value: &ElementProperty) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Whether `folder` is `prefix` or a folder below it (`""` matches everything).
fn in_folder(folder: Option<&str>, prefix: &str) -> bool {
    prefix.is_empty()
        || folder.is_some_and(|folder| {
            folder == prefix
                || folder
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// A Rhai string literal that parses back to `text` (quote, backslash and control characters
/// escaped; nothing is cut).
fn rhai_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Rhai object-map key: bare when it is an identifier, a string literal otherwise.
fn rhai_key(key: &str) -> String {
    let mut chars = key.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if identifier {
        key.to_string()
    } else {
        rhai_string(key)
    }
}

/// The Rhai literal of a JSON value: `()`, `true`, `5`, `2.5`, `"x"`, `[..]`, `#{k: v}`.
fn rhai_literal(value: &Value, depth: usize, out: &mut String) -> Result<(), String> {
    if depth > MAX_LITERAL_DEPTH {
        return Err(format!(
            "the arguments nest deeper than {MAX_LITERAL_DEPTH} levels"
        ));
    }
    match value {
        Value::Null => out.push_str("()"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(int), _) => out.push_str(&int.to_string()),
            (None, Some(float)) if float.is_finite() => {
                let abs = float.abs();
                let text = if abs != 0.0 && !(1e-6..1e16).contains(&abs) {
                    format!("{float:e}")
                } else {
                    format!("{float}")
                };
                out.push_str(&text);
                if !text.contains(['.', 'e']) {
                    out.push_str(".0");
                }
            }
            _ => return Err(format!("the number {n} has no Rhai literal")),
        },
        Value::String(text) => out.push_str(&rhai_string(text)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                rhai_literal(item, depth + 1, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push_str("#{");
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&rhai_key(key));
                out.push_str(": ");
                rhai_literal(item, depth + 1, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// The call text of the saved query `name` with the named arguments `args` (a JSON object;
/// `null` or `{}`: no arguments): `g.query("top", #{country: "DE", limit: 50})`. Values are
/// written as Rhai literals (strings escaped, nothing cut), so the text runs exactly that call;
/// the engine checks them against the parameter schemas when it runs.
pub fn query_call_text(name: &str, args: &Value) -> Result<String, String> {
    call_text("g.query", name, args, "the arguments of a saved query")
}

/// The call text of the GQL query `query` with the parameters `params` (a JSON object, `$name`
/// in the query reads the entry `name`; `null` or `{}`: none): `g.gql("MATCH ..", #{min: 30})`.
/// The query is written as one Rhai string literal and the parameters as Rhai literals, so
/// parameter values are never spliced into the query text.
pub fn gql_call_text(query: &str, params: &Value) -> Result<String, String> {
    call_text("g.gql", query, params, "the parameters of a GQL query")
}

/// `<head>("<first>"[, #{args}])`; `what` names the arguments in the error of a non-object.
fn call_text(head: &str, first: &str, args: &Value, what: &str) -> Result<String, String> {
    let mut out = format!("{head}({}", rhai_string(first));
    match args {
        Value::Null => {}
        Value::Object(map) if map.is_empty() => {}
        Value::Object(_) => {
            out.push_str(", ");
            rhai_literal(args, 0, &mut out)?;
        }
        other => {
            return Err(format!(
                "{what} are a JSON object (parameter name -> value), not {}",
                match other {
                    Value::Array(_) => "an array",
                    Value::String(_) => "a string",
                    Value::Number(_) => "a number",
                    _ => "a boolean",
                }
            ));
        }
    }
    out.push(')');
    Ok(out)
}

/// A text field of a spec: `None` when absent, `null` or empty.
fn spec_text(spec: &Value, key: &str) -> Result<Option<String>, FieldError> {
    match spec.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(FieldError::new(key, format!("{key} must be a string"))),
    }
}

impl Session {
    /// The saved queries of the graph (`folder`: only those in it and below it, segment-wise
    /// like `g.queries(folder)`), by name: the entries described in the module docs, with
    /// `status` from compiling each body (a body that no longer compiles is listed with its
    /// error). Other kinds of definitions are not listed.
    pub fn list_queries(&self, folder: Option<&str>) -> Value {
        let queries = self.graph.queries(folder);
        Value::Array(
            queries
                .iter()
                .map(|(name, query)| self.query_entry(name, query, false))
                .collect(),
        )
    }

    /// The saved query `name` (its entry plus `body` and `meta`), `null` when there is none.
    pub fn get_query(&self, name: &str) -> Value {
        match self.graph.query(name) {
            Some(query) => self.query_entry(name, &query, true),
            None => Value::Null,
        }
    }

    /// How many saved queries the graph holds.
    pub fn query_count(&self) -> usize {
        self.graph.count_kind(DefinitionKind::QUERY)
    }

    fn query_entry(&self, name: &str, query: &QueryDefinition, full: bool) -> Value {
        let params: Vec<Value> = query
            .params()
            .iter()
            .map(|(parameter, param)| {
                let mut entry = json!({
                    "name": parameter,
                    "schema": value_json(&param.schema().to_value()),
                    "required": param.is_required(),
                });
                if let Some(default) = param.default_value() {
                    entry["default"] = value_json(default);
                }
                if let Some(description) = param
                    .schema()
                    .annotations()
                    .and_then(|a| a.description.as_deref())
                {
                    entry["description"] = json!(description);
                }
                entry
            })
            .collect();
        let mut entry = json!({
            "name": name,
            "folder": query.folder(),
            "description": query.description(),
            "params": params,
        });
        let catalog = |name: &str| self.graph.query(name);
        let effects = match saved_query_effects(&self.engine, &catalog, name, query) {
            Ok(effects) => {
                entry["status"] = json!("ok");
                effects
            }
            Err(reason) => {
                entry["status"] = json!("error");
                entry["error"] = json!(reason);
                Default::default()
            }
        };
        entry["writes"] = json!(effects.writes);
        entry["effects"] = effects.effects.iter().map(effect_json).collect();
        entry["denied_writes"] = effects.denied_writes.iter().map(effect_json).collect();
        entry["language"] = json!(query.language());
        entry["dialect"] = json!(query.dialect());
        if full {
            entry["body"] = json!(query.source());
            entry["meta"] = json!(query.meta());
        }
        entry
    }

    /// Defines (or replaces) a saved query from a spec (module docs): checked like the DSL's
    /// `g.define_query` (the catalog's rules, then the body compiled: exactly one function named
    /// like the query, its parameters the declared ones; a GQL body parsed, bound and lowered,
    /// its `$name`s the declared parameters), stored in ONE commit. Without
    /// `replaces` an existing query of that name is not overwritten (the editor's "New"); with
    /// `replaces` naming another query, the new name must be free and the old query is dropped
    /// in the same commit (a rename). Answers `{ok: true, query: <entry>}` or `{ok: false,
    /// errors: [{field, message, help?}]}`, never fails otherwise.
    pub fn define_query(&mut self, spec: &Value) -> Value {
        match self.define_query_checked(spec) {
            Ok(entry) => json!({"ok": true, "query": entry}),
            Err(errors) => json!({
                "ok": false,
                "errors": errors.iter().map(FieldError::to_value).collect::<Vec<_>>(),
            }),
        }
    }

    fn define_query_checked(&mut self, spec: &Value) -> Result<Value, Vec<FieldError>> {
        const KEYS: [&str; 8] = [
            "name",
            "folder",
            "description",
            "params",
            "language",
            "body",
            "meta",
            "replaces",
        ];
        let Some(object) = spec.as_object() else {
            return Err(vec![FieldError::new(
                "general",
                "the definition is a JSON object {name, folder?, description?, params?, \
                 language?, body, meta?, replaces?}",
            )]);
        };
        if let Some(unknown) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(vec![FieldError::new(
                "general",
                format!("unknown key '{unknown}' (the keys are {})", KEYS.join(", ")),
            )]);
        }
        let mut errors = Vec::new();
        let mut text = |key: &str| match spec_text(spec, key) {
            Ok(value) => value,
            Err(err) => {
                errors.push(err);
                None
            }
        };
        let name = text("name")
            .map(|n| n.trim().to_string())
            .unwrap_or_default();
        let folder = text("folder").map(|f| f.trim().to_string());
        let description = text("description");
        let meta = text("meta");
        let language = text("language");
        let body = text("body").unwrap_or_default();
        let replaces = text("replaces").filter(|old| *old != name);
        let is_edit = spec
            .get("replaces")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());

        if name.is_empty() {
            errors.push(FieldError::new(
                "name",
                "a saved query needs a name (a Rhai identifier such as top_customers)",
            ));
        }
        let gql = match language.as_deref().map(str::trim) {
            None | Some("rhai") => false,
            Some("gql") => true,
            Some(other) => {
                errors.push(FieldError::new(
                    "language",
                    format!("unknown language '{other}' (the languages are rhai and gql)"),
                ));
                false
            }
        };
        if body.trim().is_empty() {
            errors.push(FieldError::new(
                "body",
                if gql {
                    "the body is empty: write the GQL query"
                } else {
                    "the body is empty: write the function's body"
                },
            ));
        }

        // Each field on its own, so every wrong field is reported at once.
        // Fields other than the name are checked on a definition named like the query, or with
        // a valid stand-in name while the name itself is wrong.
        let mut probe_name = name.as_str();
        if name.is_empty() {
            probe_name = "q";
        } else if let Err(err) =
            Definition::query(name.as_str(), QueryDefinition::new("fn")).validate()
        {
            errors.push(FieldError::of(&err));
            probe_name = "q";
        }
        let mut query = QueryDefinition::new(body.clone());
        if gql {
            // The language travels as the dialect; `with_inferred_dialect` keeps it and sets the
            // version the plan's writes give.
            query = query.with_dialect(GQL_QUERY_DIALECT);
        }
        if let Some(folder) = &folder {
            query = query.with_folder(folder.clone());
            if let Err(err) = Definition::query(
                probe_name,
                QueryDefinition::new("fn").with_folder(folder.clone()),
            )
            .validate()
            {
                errors.push(FieldError::of(&err));
            }
        }
        if let Some(description) = &description {
            query = query.with_description(description.clone());
        }
        if let Some(meta) = &meta {
            query = query.with_meta(meta.clone());
        }

        let mut declared: Vec<(String, QueryParam)> = Vec::new();
        match spec.get("params") {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    let Some(parameter) = item.get("name").and_then(Value::as_str) else {
                        errors.push(FieldError::new(
                            "params",
                            format!("parameter {} has no name", index + 1),
                        ));
                        continue;
                    };
                    let parameter = parameter.trim().to_string();
                    let field = format!("params.{parameter}");
                    if declared.iter().any(|(known, _)| *known == parameter) {
                        errors.push(FieldError::new(
                            field,
                            format!("the parameter '{parameter}' is declared twice"),
                        ));
                        continue;
                    }
                    let mut declaration = Map::new();
                    for (key, value) in item.as_object().into_iter().flatten() {
                        if key != "name" {
                            declaration.insert(key.clone(), value.clone());
                        }
                    }
                    let value: ElementProperty =
                        match serde_json::from_value(Value::Object(declaration)) {
                            Ok(value) => value,
                            Err(err) => {
                                errors.push(FieldError::new(field, err.to_string()));
                                continue;
                            }
                        };
                    let checked = QueryParam::from_value(&parameter, &value).and_then(|param| {
                        Definition::query(
                            probe_name,
                            QueryDefinition::new("fn").with_param(parameter.clone(), param.clone()),
                        )
                        .validate()
                        .map(|()| param)
                    });
                    match checked {
                        Ok(param) => declared.push((parameter, param)),
                        Err(err) => {
                            let mut error = FieldError::of(&err);
                            error.field = field;
                            errors.push(error);
                        }
                    }
                }
            }
            Some(_) => errors.push(FieldError::new(
                "params",
                "params is a list [{name, schema, default?}]",
            )),
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        // The parameters in the order of the function's signature (as `g.define_query` stores
        // them); a body that does not compile keeps the declared order and fails below. A GQL
        // query keeps the order of the editor.
        if !gql
            && let Ok(ast) = self.engine.compile(&body)
            && let Some(function) = ast.iter_fn_def().next()
        {
            let signature: Vec<String> = function.params.iter().map(|p| p.to_string()).collect();
            declared.sort_by_key(|(parameter, _)| {
                signature
                    .iter()
                    .position(|p| p == parameter)
                    .unwrap_or(usize::MAX)
            });
        }
        for (parameter, param) in declared {
            query = query.with_param(parameter, param);
        }
        if let Err(err) = Definition::query(name.as_str(), query.clone()).validate() {
            return Err(vec![FieldError::of(&err)]);
        }
        // The body checked like `g.define_query` checks it, and the dialect its effects give.
        let catalog = |name: &str| self.graph.query(name);
        let query =
            with_inferred_dialect(&self.engine, &catalog, &name, query).map_err(|reason| {
                vec![FieldError::new(
                    "body",
                    format!("The body is not a valid saved query: {reason}"),
                )]
            })?;
        let definition = Definition::query(name.as_str(), query);

        // New, replace or rename: the name rules of the editor.
        self.graph
            .store_query(definition, is_edit, replaces.as_deref())?;
        Ok(self.get_query(&name))
    }

    /// The preview of a call of the saved query `name` with the named arguments `args` (a JSON
    /// object, as [`query_call_text`] takes them): the run form shows it before it runs a query
    /// that writes. The call runs as a dry run (`graphersal::script::eval_value_dry_run`, under
    /// the playground's authorizer) and everything is rolled back. Answers `{ok: true, changes:
    /// {vertices_added, vertices_dropped, edges_added, edges_dropped, properties_set,
    /// properties_removed, labels_changed, other}}` (counts of the net changes) or `{ok: false,
    /// error}` (the error JSON of a run).
    pub fn preview_query(&mut self, name: &str, args: &Value) -> Value {
        let script = match query_call_text(name, args) {
            Ok(script) => script,
            Err(message) => {
                return json!({"ok": false, "error": {"message": message, "diagnostic": message}});
            }
        };
        let any: std::sync::Arc<dyn graphersal::storage::AnyGraph> = self.graph.clone();
        let limits = &self.presets().limits.clone();
        match graphersal::script::eval_value_dry_run(
            any,
            &script,
            Default::default(),
            limits,
            crate::session::playground_authorizer(),
        ) {
            Ok((_, changes)) => {
                let mut counts = json!({
                    "vertices_added": 0, "vertices_dropped": 0, "edges_added": 0,
                    "edges_dropped": 0, "properties_set": 0, "properties_removed": 0,
                    "labels_changed": 0, "other": 0,
                });
                for mutation in changes.compact().mutations() {
                    let key = match mutation {
                        Mutation::AddVertex { .. } => "vertices_added",
                        Mutation::DropVertex { .. } => "vertices_dropped",
                        Mutation::AddEdge { .. } => "edges_added",
                        Mutation::DropEdge { .. } => "edges_dropped",
                        Mutation::SetProperty { .. } => "properties_set",
                        Mutation::RemoveProperty { .. } => "properties_removed",
                        Mutation::AddLabel { .. }
                        | Mutation::RemoveLabel { .. }
                        | Mutation::SetEdgeLabel { .. } => "labels_changed",
                        _ => "other",
                    };
                    counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
                }
                json!({"ok": true, "changes": counts})
            }
            Err(err) => json!({
                "ok": false,
                "error": crate::session::script_error(
                    &graphersal::script::map_eval_error(err, limits),
                    limits,
                ),
            }),
        }
    }

    /// Drops the saved query `name`: `true`, or `false` when there was none. One commit.
    pub fn drop_query(&mut self, name: &str) -> Result<bool, String> {
        self.graph.drop_definition(DefinitionKind::QUERY, name)
    }

    /// Moves the saved query `name` into `folder` (`None` or `""`: the root). One commit.
    pub fn move_query(&mut self, name: &str, folder: Option<&str>) -> Result<Value, String> {
        let folder = folder.map(str::trim).filter(|f| !f.is_empty());
        self.graph.move_query_into(name, folder)?;
        Ok(self.get_query(name))
    }

    /// The catalog as its JSON exchange form (`{"definitions": [..]}`, every kind: saved
    /// queries, compression rules and kinds this version does not know): the playground's
    /// catalog file (`graphersal-catalog-<stamp>.json`, Catalog ▸ Save catalog).
    pub fn save_catalog_text(&self) -> String {
        self.graph.catalog_text()
    }

    /// "Load catalog": stores every definition of a catalog file (`{"definitions": [..]}`,
    /// every kind) in ONE commit, replacing those of the same kind and name; with `replace` the
    /// definitions the file does not have are dropped first (the catalog becomes the file's). A
    /// compression rule in it runs its re-encode pass in that commit. Bodies of saved queries
    /// are not compiled here (a query that does not compile is listed with its error). Answers
    /// `{loaded, graph}` (`graph`: the graph info).
    pub fn load_catalog(&mut self, text: &str, replace: bool) -> Result<Value, String> {
        let definitions =
            catalog_from_json(text).map_err(|err| format_diagnostic(&GraphError::from(err)))?;
        let loaded = definitions.len();
        self.graph.load_catalog(definitions, replace)?;
        Ok(json!({"loaded": loaded, "graph": self.info()}))
    }
}

/// The saved queries of `g` in `folder` and below it (segment-wise like `g.queries(folder)`;
/// `None` or `""`: all), by name.
pub fn queries_in<S: GraphStorage>(g: &S, folder: Option<&str>) -> Vec<(String, QueryDefinition)> {
    let prefix = folder.unwrap_or("").trim().trim_end_matches('/');
    g.definitions()
        .filter(|definition| definition.kind() == DefinitionKind::QUERY)
        .filter_map(|definition| {
            let query = definition.as_query()?;
            in_folder(query.folder(), prefix)
                .then(|| (definition.name().to_string(), query.clone()))
        })
        .collect()
}

/// The saved query `name` of `g`, `None` when there is none.
pub fn query_in<S: GraphStorage>(g: &S, name: &str) -> Option<QueryDefinition> {
    g.definition(DefinitionKind::QUERY, name)
        .and_then(Definition::as_query)
        .cloned()
}

/// How many definitions of `kind` `g` holds.
pub fn count_kind<S: GraphStorage>(g: &S, kind: DefinitionKind) -> usize {
    g.definitions()
        .filter(|definition| definition.kind() == kind)
        .count()
}

/// Stores the checked saved query `definition` in `g` in ONE commit, with the editor's name
/// rules: a new query (not `is_edit`) or a rename (`replaces`) needs a free name; `replaces`
/// must still exist and is dropped in the same commit.
pub(crate) fn store_query<S: GraphStorage>(
    g: &mut S,
    definition: Definition,
    is_edit: bool,
    replaces: Option<&str>,
) -> Result<(), Vec<FieldError>> {
    let name = definition.name().to_string();
    let exists = |g: &S, name: &str| g.definition(DefinitionKind::QUERY, name).is_some();
    if (!is_edit || replaces.is_some()) && exists(g, &name) {
        return Err(vec![FieldError::new(
            "name",
            format!("a saved query named '{name}' exists already; choose another name"),
        )]);
    }
    if let Some(old) = replaces
        && !exists(g, old)
    {
        return Err(vec![FieldError::new(
            "general",
            format!("the saved query '{old}' no longer exists (dropped elsewhere?)"),
        )]);
    }
    g.transaction(|unit| {
        let mut source = GraphTraversalSource::new(&mut *unit);
        source.set_definition(definition)?;
        if let Some(old) = replaces {
            source.remove_definition(DefinitionKind::QUERY, old)?;
        }
        Ok::<(), BoxedTraverserError>(())
    })
    .map_err(|err| vec![FieldError::new("general", format_diagnostic(&*err))])
}

/// Removes the definition of `kind` named `name` from `g` (one commit): `true`, or `false`
/// when there was none; an error as its diagnostic text.
pub fn remove_definition_in<S: GraphStorage>(
    g: &mut S,
    kind: DefinitionKind,
    name: &str,
) -> Result<bool, String> {
    GraphTraversalSource::new(g)
        .remove_definition(kind, name)
        .map_err(|err| format_diagnostic(&*err))
}

/// Moves the saved query `name` of `g` into `folder` (`None`: the root), one commit; an error
/// as its diagnostic text.
pub fn move_query_in<S: GraphStorage>(
    g: &mut S,
    name: &str,
    folder: Option<&str>,
) -> Result<(), String> {
    GraphTraversalSource::new(g)
        .move_query(name, folder)
        .map_err(|err| format_diagnostic(&*err))
}

/// The catalog of `g` as its JSON exchange form (`{"definitions": [..]}`, every kind), with a
/// final line break.
pub fn catalog_text<S: GraphStorage>(g: &S) -> String {
    let mut text = catalog_to_json(g.definitions());
    text.push('\n');
    text
}

/// Stores every one of `definitions` in `g` in ONE commit, replacing those of the same kind
/// and name; with `replace` the definitions not among them are dropped first. An error as its
/// diagnostic text.
pub fn load_catalog_in<S: GraphStorage>(
    g: &mut S,
    definitions: Vec<Definition>,
    replace: bool,
) -> Result<(), String> {
    g.transaction(|unit| {
        let stale: Vec<(DefinitionKind, String)> = if replace {
            unit.definitions()
                .filter(|old| {
                    !definitions
                        .iter()
                        .any(|new| new.kind() == old.kind() && new.name() == old.name())
                })
                .map(|old| (old.kind(), old.name().to_string()))
                .collect()
        } else {
            Vec::new()
        };
        let mut source = GraphTraversalSource::new(&mut *unit);
        for (kind, name) in stale {
            source.remove_definition(kind, &name)?;
        }
        for definition in definitions {
            source.set_definition(definition)?;
        }
        Ok::<(), BoxedTraverserError>(())
    })
    .map_err(|err| format_diagnostic(&*err))
}

/// Copies `catalog` into `g` (a graph nobody sees yet, loaded from a file that carries no
/// catalog), in one commit.
pub(crate) fn carry_catalog<S: GraphStorage>(
    g: &mut S,
    catalog: Vec<Definition>,
) -> Result<(), String> {
    if catalog.is_empty() {
        return Ok(());
    }
    g.transaction(|unit| {
        for definition in catalog {
            unit.set_definition(definition)?;
        }
        Ok::<(), GraphError>(())
    })
    .map_err(|err| format_diagnostic(&err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_texts_are_rhai_literals() {
        assert_eq!(
            query_call_text("top", &Value::Null).unwrap(),
            "g.query(\"top\")"
        );
        assert_eq!(
            query_call_text("top", &json!({})).unwrap(),
            "g.query(\"top\")"
        );
        assert_eq!(
            query_call_text(
                "top",
                &json!({"country": "D\"E\\\n", "limit": 5, "ratio": 2.0, "tags": ["a", null], "on": true, "odd key": {"x": 1e300}})
            )
            .unwrap(),
            "g.query(\"top\", #{country: \"D\\\"E\\\\\\n\", limit: 5, \"odd key\": #{x: 1e300}, on: true, ratio: 2.0, tags: [\"a\", ()]})"
        );
        assert!(query_call_text("top", &json!([1])).is_err());
        let mut deep = json!(1);
        for _ in 0..200 {
            deep = json!([deep]);
        }
        assert!(query_call_text("top", &json!({"x": deep})).is_err());
    }

    #[test]
    fn gql_call_texts_keep_parameters_out_of_the_query() {
        assert_eq!(
            gql_call_text("MATCH (p) RETURN p", &Value::Null).unwrap(),
            "g.gql(\"MATCH (p) RETURN p\")"
        );
        assert_eq!(
            gql_call_text(
                "MATCH (p {name: \"x\"})\nWHERE p.age > $min RETURN p",
                &json!({"min": 30, "name": "a\"b"})
            )
            .unwrap(),
            "g.gql(\"MATCH (p {name: \\\"x\\\"})\\nWHERE p.age > $min RETURN p\", #{min: 30, name: \"a\\\"b\"})"
        );
        let err = gql_call_text("MATCH (p) RETURN p", &json!([1])).unwrap_err();
        assert!(err.starts_with("the parameters of a GQL query"), "{err}");
    }

    fn modern() -> Session {
        let mut session = Session::new();
        session.load_sample("modern").unwrap();
        session
    }

    fn run(session: &mut Session, script: &str) -> Value {
        serde_json::from_str(&session.execute(script, &crate::RunOptions::default())).unwrap()
    }

    fn older_than() -> Value {
        json!({
            "name": "older_than",
            "folder": "people/age",
            "description": "People older than a given age.",
            "params": [
                {"name": "age", "schema": {"type": "integer", "minimum": 0, "description": "years"}, "default": 30},
                {"name": "label", "schema": {"type": "string", "enum": ["person"]}},
            ],
            "body": "fn older_than(label, age) {\n  g.v().has_label(label).has(\"age\", P.gt(age)).values(\"name\")\n}",
        })
    }

    #[test]
    fn a_query_that_writes_is_listed_with_its_effects_and_previewed() {
        let mut s = modern();
        let answer = s.define_query(&json!({
            "name": "add_person",
            "params": [{"name": "name", "schema": {"type": "string"}}],
            "body": "fn add_person(name) { g.add_v(\"person\").property(\"name\", name).next() }",
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        let entry = s.get_query("add_person");
        assert_eq!(entry["dialect"], "graphersal-rhai/2");
        assert_eq!(entry["writes"], true);
        assert_eq!(
            entry["effects"][0],
            json!({"action": "Create", "element": "vertex", "label": "person",
                   "text": "Create Data(vertex \"person\")"})
        );
        assert_eq!(entry["denied_writes"], json!([]));
        assert_eq!(s.list_queries(None)[0]["writes"], true);
        // The preview runs the call as a dry run: nothing stays.
        let preview = s.preview_query("add_person", &json!({"name": "ann"}));
        assert_eq!(preview["ok"], true, "{preview}");
        assert_eq!(preview["changes"]["vertices_added"], 1);
        assert_eq!(preview["changes"]["properties_set"], 0);
        assert_eq!(
            run(&mut s, "g.v().count().next()")["execution"]["results"][0],
            6
        );
        let failed = s.preview_query("add_person", &json!({"name": 5}));
        assert_eq!(failed["ok"], false);
        assert!(
            failed["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Invalid argument 'name'"),
            "{failed}"
        );
        // Saved again without writes: the read-only dialect.
        let answer = s.define_query(&json!({
            "name": "add_person", "replaces": "add_person",
            "params": [{"name": "name", "schema": {"type": "string"}}],
            "body": "fn add_person(name) { g.v().has(\"name\", name).count() }",
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(s.get_query("add_person")["dialect"], "graphersal-rhai/1");
        assert_eq!(s.get_query("add_person")["writes"], false);
        // A body that writes the schema is refused at its field.
        let refused = s.define_query(&json!({"name": "s", "body": "fn s() { g.set_schema(#{}) }"}));
        assert_eq!(refused["errors"][0]["field"], "body");
        assert!(
            refused["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("it changes the schema"),
            "{refused}"
        );
    }

    #[test]
    fn a_query_written_in_gql_is_defined_listed_and_run() {
        let mut s = modern();
        let answer = s.define_query(&json!({
            "name": "older",
            "language": "gql",
            "params": [{"name": "min", "schema": {"type": "integer"}, "default": 30}],
            "body": "MATCH (p:person) WHERE p.age > $min RETURN p.name AS name ORDER BY name",
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        let entry = &answer["query"];
        assert_eq!(entry["language"], "gql");
        assert_eq!(entry["dialect"], "graphersal-gql/1");
        assert_eq!(entry["writes"], false);
        assert!(
            entry["body"]
                .as_str()
                .unwrap()
                .starts_with("MATCH (p:person)")
        );
        let listed = &s.list_queries(None)[0];
        assert_eq!(listed["language"], "gql");
        assert_eq!(listed["dialect"], "graphersal-gql/1");
        assert!(listed.get("body").is_none());

        let text = query_call_text("older", &json!({"min": 29})).unwrap();
        let r = run(&mut s, &format!("{text}.to_list()"));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(
            r["execution"]["results"],
            json!([{"name": "josh"}, {"name": "peter"}])
        );

        // A GQL query that writes: the writing dialect and its effects, previewed as a dry run.
        let answer = s.define_query(&json!({
            "name": "add_person",
            "language": "gql",
            "params": [{"name": "name", "schema": {"type": "string"}}],
            "body": "INSERT (:person {name: $name})",
        }));
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(answer["query"]["dialect"], "graphersal-gql/2");
        assert_eq!(answer["query"]["writes"], true);
        assert_eq!(
            answer["query"]["effects"][0]["text"],
            "Create Data(vertex \"person\")"
        );
        let preview = s.preview_query("add_person", &json!({"name": "ann"}));
        assert_eq!(preview["changes"]["vertices_added"], 1, "{preview}");
        // A Rhai query keeps its language.
        assert_eq!(s.define_query(&older_than())["query"]["language"], "rhai");

        // Refused: a parameter the query does not declare, one it never reads, a GQL error,
        // an unknown language.
        let refused = s.define_query(&json!({
            "name": "q", "language": "gql", "body": "MATCH (p) WHERE p.age > $min RETURN p",
        }));
        assert_eq!(refused["errors"][0]["field"], "body", "{refused}");
        assert!(
            refused["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("$min"),
            "{refused}"
        );
        let refused = s.define_query(&json!({
            "name": "q", "language": "gql", "body": "MATCH (p) RETURN p",
            "params": [{"name": "min", "schema": {"type": "integer"}}],
        }));
        assert!(
            refused["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("'min'"),
            "{refused}"
        );
        let refused = s.define_query(&json!({
            "name": "q", "language": "gql", "body": "MATCH (p RETURN p",
        }));
        assert_eq!(refused["errors"][0]["field"], "body", "{refused}");
        let refused = s.define_query(&json!({
            "name": "q", "language": "cypher", "body": "MATCH (p) RETURN p",
        }));
        assert_eq!(refused["errors"][0]["field"], "language", "{refused}");
        assert_eq!(s.query_count(), 3);
    }

    #[test]
    fn define_list_get_run_rename_and_drop() {
        let mut s = modern();
        let answer = s.define_query(&older_than());
        assert_eq!(answer["ok"], true, "{answer}");
        // Stored in the signature's order, not the declared one.
        let entry = &answer["query"];
        assert_eq!(entry["params"][0]["name"], "label");
        assert_eq!(entry["params"][0]["required"], true);
        assert_eq!(entry["params"][1]["default"], 30);
        assert_eq!(entry["params"][1]["description"], "years");
        assert_eq!(entry["status"], "ok");
        assert!(entry["body"].as_str().unwrap().starts_with("fn older_than"));
        assert_eq!(s.info()["savedQueries"], 1);

        let list = s.list_queries(None);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert!(list[0].get("body").is_none());
        assert_eq!(s.list_queries(Some("people")).as_array().unwrap().len(), 1);
        assert_eq!(s.list_queries(Some("peop")).as_array().unwrap().len(), 0);

        // The call text runs the query.
        let text = query_call_text("older_than", &json!({"label": "person"})).unwrap();
        let r = run(&mut s, &format!("{text}.to_list()"));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["execution"]["results"], json!(["josh", "peter"]));
        // A violated constraint is the engine's error.
        let text = query_call_text("older_than", &json!({"label": "person", "age": -1})).unwrap();
        let r = run(&mut s, &text);
        assert_eq!(r["ok"], false);

        // New with a taken name: refused at the name.
        let again = s.define_query(&older_than());
        assert_eq!(again["ok"], false);
        assert_eq!(again["errors"][0]["field"], "name");
        // An edit in place.
        let mut edit = older_than();
        edit["replaces"] = json!("older_than");
        edit["description"] = json!("changed");
        assert_eq!(s.define_query(&edit)["query"]["description"], "changed");
        // A rename: new name stored, the old one dropped, one commit.
        let before = s.graph().last_commit_seq();
        let mut renamed = older_than();
        renamed["name"] = json!("elders");
        renamed["body"] =
            json!("fn elders(label, age) { g.v().has_label(label).has(\"age\", P.gt(age)) }");
        renamed["replaces"] = json!("older_than");
        assert_eq!(s.define_query(&renamed)["ok"], true);
        assert_eq!(s.graph().last_commit_seq(), before + 1);
        assert!(s.get_query("older_than").is_null());
        assert_eq!(s.get_query("elders")["name"], "elders");

        assert_eq!(
            s.move_query("elders", Some("archive")).unwrap()["folder"],
            "archive"
        );
        assert_eq!(
            s.move_query("elders", Some("")).unwrap()["folder"],
            Value::Null
        );
        assert!(s.drop_query("elders").unwrap());
        assert!(!s.drop_query("elders").unwrap());
        assert_eq!(s.query_count(), 0);
    }

    #[test]
    fn a_refused_definition_names_its_fields() {
        let mut s = modern();
        let bad = json!({
            "name": "fn",
            "folder": "/bad/",
            "params": [
                {"name": "g", "schema": {"type": "integer"}},
                {"name": "limit", "schema": {"type": "integer", "minimum": 1}, "default": 0},
                {"name": "x", "schema": {"type": "integr"}},
            ],
            "body": "fn x() {}",
        });
        let answer = s.define_query(&bad);
        assert_eq!(answer["ok"], false);
        let fields: Vec<&str> = answer["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["field"].as_str().unwrap())
            .collect();
        assert!(fields.contains(&"name"), "{fields:?}");
        assert!(fields.contains(&"folder"), "{fields:?}");
        assert!(fields.contains(&"params.limit"), "{fields:?}");
        assert!(fields.contains(&"params.x"), "{fields:?}");
        assert!(answer["errors"][0]["help"].is_string());

        // The body is checked once the fields are valid.
        let answer = s.define_query(&json!({
            "name": "q", "params": [{"name": "a", "schema": {"type": "integer"}}],
            "body": "fn q(b) { b }",
        }));
        assert_eq!(answer["errors"][0]["field"], "body", "{answer}");
        let answer = s.define_query(&json!({"name": "q", "body": "fn q( {"}));
        assert_eq!(answer["errors"][0]["field"], "body");
        let answer = s.define_query(&json!({"name": "q", "body": "", "oops": 1}));
        assert_eq!(answer["errors"][0]["field"], "general");
        assert_eq!(s.query_count(), 0);
    }

    #[test]
    fn a_catalog_file_round_trips_and_survives_a_graph_file_load() {
        let mut s = modern();
        assert_eq!(s.define_query(&older_than())["ok"], true);
        let file = s.save_catalog_text();
        assert!(file.contains("\"older_than\""));

        // A run that defines a query reports the catalog change.
        let r = run(
            &mut s,
            "g.define_query(#{name: \"n\", body: \"fn n() { g.v().count() }\"})",
        );
        assert_eq!(r["changes"]["definitions"], true, "{r}");
        assert_eq!(r["changes"]["data"], false);

        // Load into a fresh graph: merge, then replace.
        let mut other = modern();
        assert_eq!(
            other.define_query(&json!({"name": "mine", "body": "fn mine() { 1 }"}))["ok"],
            true
        );
        let loaded = other.load_catalog(&file, false).unwrap();
        assert_eq!(loaded["loaded"], 1);
        assert_eq!(other.query_count(), 2);
        other.load_catalog(&file, true).unwrap();
        assert_eq!(other.query_count(), 1);
        assert!(other.load_catalog("{\"definitions\": 3}", false).is_err());

        // A graph file carries no catalog: the session keeps it across the load.
        let saved = other.save_graphson().unwrap();
        other
            .load_graphson(saved["text"].as_str().unwrap().as_bytes(), "g.json", None)
            .unwrap();
        assert_eq!(other.query_count(), 1);
        // A sample starts without one.
        other.load_sample("modern").unwrap();
        assert_eq!(other.query_count(), 0);
    }

    #[test]
    fn a_catalog_file_round_trips_property_indexes() {
        let mut s = modern();
        let r = run(&mut s, "g.create_index(#{label: \"person\", key: \"age\"})");
        assert_eq!(r["changes"]["definitions"], true, "{r}");
        let file = s.save_catalog_text();
        assert!(file.contains("\"kind\":\"property_index\""), "{file}");

        let mut other = modern();
        let loaded = other.load_catalog(&file, true).unwrap();
        assert_eq!(loaded["loaded"], 1);
        assert_eq!(other.save_catalog_text(), file);
        // The loaded definition is served: the profile shows the index.
        let r = run(
            &mut other,
            "g.v().has_label(\"person\").has(\"age\", P.gt(30)).profile().to_string()",
        );
        assert!(r.to_string().contains("[index: vertex person.age]"), "{r}");
    }

    #[test]
    fn folders_match_by_segment() {
        assert!(in_folder(Some("reports/sales"), "reports"));
        assert!(in_folder(Some("reports"), "reports"));
        assert!(!in_folder(Some("reports2"), "reports"));
        assert!(!in_folder(None, "reports"));
        assert!(in_folder(None, ""));
    }

    /// The modern graph twice: as a `TraversalGraph` and wrapped in the `Forwarding` storage
    /// (not `TraversalGraph`, own handle types), for the generic-helper comparisons.
    fn modern_pair() -> (
        TraversalGraph,
        graphersal_storage_tests::forwarding::Forwarding,
    ) {
        (
            TraversalGraph::tinkerpop_modern(),
            graphersal_storage_tests::forwarding::Forwarding::new(
                TraversalGraph::tinkerpop_modern(),
            ),
        )
    }

    #[test]
    fn the_catalog_on_a_foreign_storage_equals_that_of_a_traversal_graph() {
        let (mut tg, mut fw) = modern_pair();
        let query = |source: &str| {
            Definition::query("older", QueryDefinition::new(source).with_folder("reports"))
        };
        for g in [&mut tg as &mut dyn FnMutStore, &mut fw] {
            g.store(query("fn older() { g.v().count() }"), false, None)
                .unwrap();
            // A new query of an existing name is refused, an edit replaces it.
            assert_eq!(
                g.store(query("fn older() { 1 }"), false, None).unwrap_err()[0].field,
                "name"
            );
            g.store(query("fn older() { 2 }"), true, None).unwrap();
        }
        assert_eq!(
            queries_in(&tg, Some("reports")),
            queries_in(&fw, Some("reports"))
        );
        assert_eq!(queries_in(&fw, Some("other")).len(), 0);
        assert_eq!(query_in(&tg, "older"), query_in(&fw, "older"));
        assert_eq!(count_kind(&fw, DefinitionKind::QUERY), 1);
        move_query_in(&mut tg, "older", None).unwrap();
        move_query_in(&mut fw, "older", None).unwrap();
        let text = catalog_text(&tg);
        assert_eq!(text, catalog_text(&fw));
        assert_eq!(
            remove_definition_in(&mut fw, DefinitionKind::QUERY, "older"),
            Ok(true)
        );
        assert_eq!(count_kind(&fw, DefinitionKind::QUERY), 0);
        let definitions = catalog_from_json(&text).unwrap();
        load_catalog_in(&mut fw, definitions.clone(), true).unwrap();
        assert_eq!(catalog_text(&fw), text);
        let mut empty = graphersal_storage_tests::forwarding::Forwarding::default();
        carry_catalog(&mut empty, definitions).unwrap();
        assert_eq!(catalog_text(&empty), text);
    }

    /// `store_query` on either storage, behind one object-safe call for the loop above.
    trait FnMutStore {
        fn store(
            &mut self,
            definition: Definition,
            is_edit: bool,
            replaces: Option<&str>,
        ) -> Result<(), Vec<FieldError>>;
    }

    impl<S: GraphStorage> FnMutStore for S {
        fn store(
            &mut self,
            definition: Definition,
            is_edit: bool,
            replaces: Option<&str>,
        ) -> Result<(), Vec<FieldError>> {
            store_query(self, definition, is_edit, replaces)
        }
    }
}
