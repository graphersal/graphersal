//! Vector provenance: the schema keyword `embedder` of a vector property records the model
//! identity ([`EmbedderInfo::model`](super::EmbedderInfo::model)) of the embedder that
//! produced its vectors, per (label, property). A vector rule (keyword `from`,
//! `graph/vector_rules.rs`) fills such a property; a text query of `nearest`/`similarity`
//! embeds its text with the registered embedder of the declared model
//! ([`text_query_candidates`]). Book page "Vectors", section "Which model produced a
//! property".

use std::sync::Arc;

use compact_str::CompactString;

use super::{EmbedError, Embedder, Embedders, quoted};
use crate::graph::schema::{GraphSchema, SchemaMode};

/// The schema of `schema` when it is attached (mode `open` or `closed`), else
/// [`EmbedError::SchemaNotAttached`] naming `operation`.
pub(crate) fn attached<'s>(
    schema: Option<&'s GraphSchema>,
    operation: &str,
) -> Result<&'s GraphSchema, EmbedError> {
    match schema {
        None => Err(EmbedError::SchemaNotAttached {
            operation: operation.to_string(),
            found: "the graph has no schema".to_string(),
        }),
        Some(schema) if schema.mode == SchemaMode::None => Err(EmbedError::SchemaNotAttached {
            operation: operation.to_string(),
            found: "its schema has mode none".to_string(),
        }),
        Some(schema) => Ok(schema),
    }
}

/// The embedders a text query on the property `key` may need: one per distinct model the
/// attached `schema` declares for `key` on any vertex or edge label, with the first label
/// declaring it and the registered embedder of that model (`None`: none registered). Refused
/// without an attached schema, without a top-level name and when no label declares a model.
pub(crate) fn text_query_candidates(
    schema: Option<&GraphSchema>,
    operation: &str,
    key: &str,
    registry: &Embedders,
) -> Result<Vec<TextCandidate>, EmbedError> {
    let schema = attached(schema, operation)?;
    let mut candidates: Vec<TextCandidate> = Vec::new();
    let roots = schema
        .vertices
        .iter()
        .map(|(label, v)| (label, &v.schema))
        .chain(schema.edges.iter().map(|(label, e)| (label, &e.schema)));
    for (_, root) in roots {
        let Some(model) = root
            .properties
            .get(key)
            .and_then(|node| node.vector.as_ref())
            .and_then(|vector| vector.embedder.as_deref())
        else {
            continue;
        };
        if candidates.iter().any(|c| c.model == model) {
            continue;
        }
        candidates.push(TextCandidate {
            model: CompactString::new(model),
            embedder: registry.by_model(model).cloned(),
        });
    }
    if candidates.is_empty() {
        return Err(EmbedError::NoDeclaredEmbedder {
            key: quoted(key),
            on: String::new(),
        });
    }
    if candidates.iter().all(|c| c.embedder.is_none()) {
        return Err(EmbedError::ModelNotRegistered {
            model: quoted(&candidates[0].model),
            key: quoted(key),
        });
    }
    Ok(candidates)
}

/// One model a text query may embed with ([`text_query_candidates`]).
#[derive(Clone)]
pub(crate) struct TextCandidate {
    /// The declared model identity.
    pub(crate) model: CompactString,
    /// The registered embedder of that model.
    pub(crate) embedder: Option<Arc<dyn Embedder>>,
}

/// Every model identity the schema's `embedder` keywords name, with one place each names it
/// (`label.key`), in schema order (vertex labels, then edge labels), each model once: what a
/// Store looks up among its models at open (format spec 4.5).
#[cfg_attr(
    not(all(
        feature = "persist",
        feature = "model2vec",
        not(target_arch = "wasm32")
    )),
    allow(dead_code)
)]
pub(crate) fn schema_models(schema: &GraphSchema) -> Vec<(String, String)> {
    let mut models: Vec<(String, String)> = Vec::new();
    let roots = schema
        .vertices
        .iter()
        .map(|(label, v)| (label, &v.schema))
        .chain(schema.edges.iter().map(|(label, e)| (label, &e.schema)));
    for (label, root) in roots {
        for (key, node) in &root.properties {
            let Some(model) = node.vector.as_ref().and_then(|v| v.embedder.as_deref()) else {
                continue;
            };
            if !models.iter().any(|(m, _)| m == model) {
                models.push((model.to_string(), format!("{label}.{key}")));
            }
        }
    }
    models
}
