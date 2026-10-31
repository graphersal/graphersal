//! The saved queries that write a vector property directly (`.property("emb", ..)` with a
//! literal key) on a label where that property is, or after the suggested schema patch will
//! be, the target of a vector rule (`from`): once the rule exists such a write is refused
//! (`GraphError::DerivedProperty`). Front ends list them in the upgrade report, in the notes of
//! a `.gsnap` loaded without a store and in the fill preview of a schema change that adds a
//! rule. The scan only reports: a saved query is never rewritten.

use rhai::{AST, Expr, Stmt};

use crate::catalog::{Definition, QueryDefinition, VectorPropertyWrite};
use crate::graph::ElementKind;
use crate::graph::schema::GraphSchema;

/// A vector property a direct write must not touch: the element kind, the label, the
/// property, and whether the rule exists already (`false`: the property has a model but no
/// `from` yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorTarget {
    pub(crate) kind: ElementKind,
    pub(crate) label: String,
    pub(crate) property: String,
    pub(crate) rule_exists: bool,
}

/// The vector properties of `schema` that are rule targets (`from`) or have a model without a
/// rule (`embedder` only: the upgrade suggests adding `from` to them).
pub(crate) fn vector_targets(schema: Option<&GraphSchema>) -> Vec<VectorTarget> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let roots = schema
        .vertices
        .iter()
        .map(|(label, v)| (ElementKind::Vertex, label, &v.schema))
        .chain(
            schema
                .edges
                .iter()
                .map(|(label, e)| (ElementKind::Edge, label, &e.schema)),
        );
    let mut targets = Vec::new();
    for (kind, label, root) in roots {
        for (key, node) in &root.properties {
            let Some(vector) = node.vector.as_ref() else {
                continue;
            };
            if vector.embedder.is_none() && vector.from.is_none() {
                continue;
            }
            targets.push(VectorTarget {
                kind,
                label: label.to_string(),
                property: key.to_string(),
                rule_exists: vector.from.is_some(),
            });
        }
    }
    targets
}

/// The saved queries among `definitions` (Rhai bodies that compile and may write; GQL bodies
/// and other kinds are skipped) whose body writes one of the vector properties of `schema`
/// (rule targets, and properties with a model but no rule yet) by its literal name, on a label
/// their effects may write: one entry per query, element kind and property, in the order of
/// `definitions`. Nothing is changed: a saved query is never rewritten.
pub fn saved_queries_writing_vectors<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
    schema: Option<&GraphSchema>,
) -> Vec<VectorPropertyWrite> {
    let targets = vector_targets(schema);
    writing(definitions, &targets)
}

/// [`saved_queries_writing_vectors`] for the given targets.
pub(crate) fn writing<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
    targets: &[VectorTarget],
) -> Vec<VectorPropertyWrite> {
    if targets.is_empty() {
        return Vec::new();
    }
    let definitions: Vec<&Definition> = definitions.into_iter().collect();
    let queries: Vec<(&str, &QueryDefinition)> = definitions
        .iter()
        .filter_map(|d| d.as_query().map(|q| (d.name(), q)))
        .collect();
    let catalog = |name: &str| {
        queries
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, q)| (*q).clone())
    };
    let engine = super::engine();
    let mut found = Vec::new();
    for (name, query) in &queries {
        if !is_rhai(query) {
            continue;
        }
        let Ok(ast) = engine.compile(query.source()) else {
            continue;
        };
        let keys = written_keys(&ast);
        if keys.is_empty() {
            continue;
        }
        // The labels its writes may touch (an open label or kind covers every one).
        let Ok(effects) = super::saved_query_effects(&engine, &catalog, name, query) else {
            continue;
        };
        let scopes: Vec<(Option<ElementKind>, Option<String>)> = effects
            .effects
            .iter()
            .filter_map(|request| match request.as_request().resource {
                crate::graph::Resource::Data { element, label } => {
                    Some((element, label.map(str::to_string)))
                }
                _ => None,
            })
            .collect();
        if scopes.is_empty() {
            continue;
        }
        for key in &keys {
            for kind in [ElementKind::Vertex, ElementKind::Edge] {
                let mut labels = Vec::new();
                let mut rule_exists = false;
                for target in targets
                    .iter()
                    .filter(|t| t.kind == kind && &t.property == key)
                {
                    let touched = scopes.iter().any(|(element, label)| {
                        element.is_none_or(|e| e == kind)
                            && label.as_deref().is_none_or(|l| l == target.label)
                    });
                    if touched && !labels.contains(&target.label) {
                        labels.push(target.label.clone());
                        rule_exists |= target.rule_exists;
                    }
                }
                if !labels.is_empty() {
                    found.push(VectorPropertyWrite {
                        query: name.to_string(),
                        element: kind,
                        property: key.clone(),
                        labels,
                        rule_exists,
                    });
                }
            }
        }
    }
    found
}

fn is_rhai(query: &QueryDefinition) -> bool {
    query.dialect() == crate::catalog::QUERY_DIALECT
        || query.dialect() == crate::catalog::WRITING_QUERY_DIALECT
}

/// The property keys `ast` writes by a literal name: the key argument of every `property`
/// call (the first string argument, after an optional cardinality).
fn written_keys(ast: &AST) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    ast.walk(&mut |path| {
        let call = match path.last() {
            Some(rhai::ASTNode::Expr(Expr::MethodCall(call, _) | Expr::FnCall(call, _)))
            | Some(rhai::ASTNode::Stmt(Stmt::FnCall(call, _))) => call,
            _ => return true,
        };
        if call.name.as_str() != "property" {
            return true;
        }
        let key = call.args.iter().take(2).find_map(|arg| match arg {
            Expr::StringConstant(text, _) => Some(text.to_string()),
            Expr::DynamicConstant(value, _) => value
                .read_lock::<rhai::ImmutableString>()
                .map(|text| text.to_string()),
            _ => None,
        });
        if let Some(key) = key
            && !keys.contains(&key)
        {
            keys.push(key);
        }
        true
    });
    keys
}

/// Fills [`PlannedFill::direct_writes`](crate::schema::PlannedFill::direct_writes): the saved
/// queries of `storage`'s catalog that write a fill's property directly on its label (the
/// preview of a schema change that adds or changes vector rules).
pub(crate) fn attach_direct_writes<S: crate::graph::GraphStorage + ?Sized>(
    storage: &S,
    fills: &mut [crate::graph::vector_rules::PlannedFill],
) -> Result<(), crate::error::GraphError> {
    if fills.is_empty() {
        return Ok(());
    }
    let targets: Vec<VectorTarget> = fills
        .iter()
        .map(|fill| VectorTarget {
            kind: fill.element(),
            label: fill.label().to_string(),
            property: fill.to().to_string(),
            rule_exists: true,
        })
        .collect();
    let definitions: Vec<Definition> = storage
        .definitions()?
        .filter(|d| d.kind() == crate::graph::DefinitionKind::QUERY)
        .cloned()
        .collect();
    let writes = writing(&definitions, &targets);
    for fill in fills.iter_mut() {
        let queries: Vec<String> = writes
            .iter()
            .filter(|w| {
                w.element == fill.element()
                    && w.property == fill.to()
                    && w.labels.iter().any(|l| l == fill.label())
            })
            .map(|w| w.query.clone())
            .collect();
        fill.set_direct_writes(queries);
    }
    Ok(())
}
