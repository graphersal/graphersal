//! DSL functions a release removed: the help a call of one gets ([`removed_function_help`],
//! used by [`map_eval_error`](super::map_eval_error)) and the scan that lists the saved queries
//! of a catalog calling one ([`saved_queries_using_removed`]), each with its replacement. A
//! saved query is never rewritten: the scan only reports.

use rhai::{AST, Dynamic, Expr, Stmt};

use crate::catalog::{Definition, QUERY_DIALECT, QueryDefinition, WRITING_QUERY_DIALECT};

/// The replacement of a DSL function a release removed, for a script written before it:
/// `Some(help)` when `signature` (a Rhai "function not found" signature or a plain name) names
/// one.
pub(crate) fn removed_function_help(signature: &str) -> Option<&'static str> {
    let name = signature.split([' ', '(']).next()?.trim();
    removed(name).map(|function| function.help)
}

/// A function some release removed.
struct Removed {
    /// The canonical (snake_case) name.
    name: &'static str,
    /// The release that removed it.
    removed_in: &'static str,
    /// What a call gets after "Help:" (the replacement, with a runnable example).
    help: &'static str,
}

const EMBED_PROPERTY: Removed = Removed {
    name: "embed_property",
    removed_in: "0.5.0",
    help: "embed_property was removed in 0.5.0. Declare a vector rule instead: the keyword \
           \"from\" on the vector property names its text property, and the engine fills and \
           refreshes the vectors at every write: g.patch_schema(#{vertices: #{doc: #{schema: \
           #{type: \"object\", properties: #{emb: #{type: \"array\", format: \"vector\", \
           embedder: \"hash-ngram/1\", from: \"text\"}}}}}}) (book page \"Vector Rules\"). With an \
           embedder of the host, write the vectors in your own query: \
           g.v().has_label(\"doc\").property(\"emb\", __.embed(\"text\")).iterate().",
};

/// The removed function a call named `name` (either spelling) refers to.
fn removed(name: &str) -> Option<&'static Removed> {
    match name {
        "embed_property" | "embedProperty" => Some(&EMBED_PROPERTY),
        _ => None,
    }
}

/// One call of a removed DSL function in the body of a saved query
/// ([`saved_queries_using_removed`]).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedFunctionUse {
    /// The saved query's name in the catalog.
    pub query: String,
    /// The removed function the body calls (its snake_case name, `embed_property`).
    pub function: String,
    /// The release that removed it (`0.5.0`).
    pub removed_in: String,
    /// What replaces the call, in words with an example (the help a call gets).
    pub replacement: String,
    /// For `embed_property`: the schema patch (JSON, for `set_schema`/`patch_schema`) that
    /// declares the vector rule replacing the call, when the call names its label, `from` and
    /// `to` as literal strings (its `embedder`, when literal, becomes the rule's model, else
    /// `hash-ngram/1`; a rule accepts only `hash-ngram/1` and the store's models). `None` when
    /// the call computes them.
    pub schema_patch: Option<String>,
}

impl RemovedFunctionUse {
    /// One line for a report: `saved query "q" calls embed_property (removed in 0.5.0); ...`.
    pub fn note(&self) -> String {
        let fix = match &self.schema_patch {
            Some(patch) => format!("declare the vector rule instead (schema patch): {patch}"),
            None => self.replacement.clone(),
        };
        format!(
            "saved query \"{}\" calls {} (removed in {}); {fix}",
            self.query, self.function, self.removed_in
        )
    }
}

/// The calls of removed DSL functions in the Rhai saved queries among `definitions` (other
/// kinds and GQL bodies are skipped, a body that does not compile is skipped), one entry per
/// call, in the order of `definitions` and of the body. Nothing is changed: a saved query is
/// never rewritten.
pub fn saved_queries_using_removed<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
) -> Vec<RemovedFunctionUse> {
    let engine = super::engine();
    let mut uses = Vec::new();
    for definition in definitions {
        let Some(query) = definition.as_query() else {
            continue;
        };
        if !is_rhai(query) {
            continue;
        }
        let Ok(ast) = engine.compile(query.source()) else {
            continue;
        };
        uses.extend(uses_in(definition.name(), &ast));
    }
    uses
}

fn is_rhai(query: &QueryDefinition) -> bool {
    query.dialect() == QUERY_DIALECT || query.dialect() == WRITING_QUERY_DIALECT
}

/// The calls of removed functions in `ast`.
fn uses_in(query: &str, ast: &AST) -> Vec<RemovedFunctionUse> {
    let mut uses = Vec::new();
    ast.walk(&mut |path| {
        let call = match path.last() {
            Some(rhai::ASTNode::Expr(Expr::MethodCall(call, _) | Expr::FnCall(call, _)))
            | Some(rhai::ASTNode::Stmt(Stmt::FnCall(call, _))) => call,
            _ => return true,
        };
        if let Some(function) = removed(call.name.as_str()) {
            let schema_patch = if function.name == EMBED_PROPERTY.name {
                call.args.first().and_then(embed_property_patch)
            } else {
                None
            };
            uses.push(RemovedFunctionUse {
                query: query.to_string(),
                function: function.name.to_string(),
                removed_in: function.removed_in.to_string(),
                replacement: function.help.to_string(),
                schema_patch,
            });
        }
        true
    });
    uses
}

/// The vector-rule schema patch replacing `g.embed_property(options)`, when the options are a
/// map literal naming `label`, `from` and `to` as strings.
fn embed_property_patch(options: &Expr) -> Option<String> {
    let entries: Vec<(String, Option<String>)> = match options {
        Expr::Map(map, _) => map
            .0
            .iter()
            .map(|(key, value)| {
                let text = match value {
                    Expr::StringConstant(text, _) => Some(text.to_string()),
                    Expr::DynamicConstant(value, _) => text_of(value),
                    _ => None,
                };
                (key.name.to_string(), text)
            })
            .collect(),
        Expr::DynamicConstant(value, _) => value
            .read_lock::<rhai::Map>()?
            .iter()
            .map(|(key, value)| (key.to_string(), text_of(value)))
            .collect(),
        _ => return None,
    };
    let get = |key: &str| {
        entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let label = get("label")??;
    let from = get("from")??;
    let to = get("to")??;
    let embedder = match get("embedder") {
        None => "hash-ngram/1".to_string(),
        Some(model) => model?,
    };
    let kind = match get("element") {
        None => "vertices",
        Some(Some(element)) if element == "vertex" => "vertices",
        Some(Some(element)) if element == "edge" => "edges",
        Some(_) => return None,
    };
    let json =
        |text: &str| crate::value::physical::to_json_text(&crate::ElementProperty::from(text)).ok();
    let (label, to, embedder, from) = (json(&label)?, json(&to)?, json(&embedder)?, json(&from)?);
    Some(format!(
        "{{\"{kind}\": {{{label}: {{\"schema\": {{\"type\": \"object\", \"properties\": {{{to}: {{\"type\": \"array\", \
         \"format\": \"vector\", \"embedder\": {embedder}, \"from\": {from}}}}}}}}}}}}}",
    ))
}

fn text_of(value: &Dynamic) -> Option<String> {
    value
        .read_lock::<rhai::ImmutableString>()
        .map(|text| text.to_string())
}
