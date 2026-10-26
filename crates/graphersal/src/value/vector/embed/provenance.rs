//! Vector provenance: the schema keyword `embedder` of a vector property records the model
//! identity ([`EmbedderInfo::model`]) of the embedder that produced its vectors, per (label,
//! property). `embed_property` declares and checks it ([`plan`]); a text query of
//! `nearest`/`similarity` embeds its text with the registered embedder of the declared model
//! ([`text_query_candidates`]). Book page "Vectors", section "Which model produced a property".

use std::sync::Arc;

use compact_str::CompactString;

use super::{EmbedError, Embedder, EmbedderInfo, Embedders, quoted};
use crate::error::{SchemaError, TraverserError};
use crate::graph::GraphNodeType;
use crate::graph::schema::{GraphSchema, PropertySchema, SchemaMode, VectorSchema};

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

/// What `embed_property` has to do to the schema before it writes.
#[derive(Debug)]
pub(crate) enum Provenance {
    /// The property is declared for the embedder's model already.
    Declared,
    /// Store this schema (in the unit of the first page): the property declared as vectors of
    /// the embedder, or its `embedder` added.
    Change(GraphSchema),
}

fn element_name(kind: GraphNodeType) -> &'static str {
    match kind {
        GraphNodeType::Vertex => "vertex",
        GraphNodeType::Edge => "edge",
    }
}

/// The vector declaration `embed_property` writes for `info`'s vectors.
fn declaration(info: &EmbedderInfo) -> PropertySchema {
    let mut node = PropertySchema::vector(info.element, u32::try_from(info.dimension).ok());
    if let Some(vector) = node.vector.as_mut() {
        vector.metric = Some(info.metric);
        vector.embedder = Some(info.model.to_string());
    }
    node
}

/// The type the embedder writes, for a type mismatch: `vector<f32, 256> from "hash-ngram/1"`.
fn written_type(info: &EmbedderInfo) -> String {
    format!(
        "{} from the embedder {}",
        VectorSchema::new(info.element)
            .with_dimension(u32::try_from(info.dimension).unwrap_or(u32::MAX))
            .type_name(),
        quoted(&info.name)
    )
}

/// What `embed_property` writing `info`'s vectors into the top-level property `key` of the
/// elements of (`kind`, `label`) needs from the stored `schema`: refused without an attached
/// schema ([`EmbedError::SchemaNotAttached`]), for a declaration that is not a vector of the
/// embedder's dimension (another element kind is fine: the write quantizes to it) (the schema's [`SchemaError::TypeMismatch`]) and for a
/// declaration of another model ([`EmbedError::EmbedderMismatch`]); otherwise nothing, or the
/// schema with the declaration added (the whole vector node when the property is undeclared,
/// the label too when it is; only `embedder` on a matching vector declaration without one).
pub(crate) fn plan(
    schema: Option<&GraphSchema>,
    kind: GraphNodeType,
    label: &str,
    key: &str,
    info: &EmbedderInfo,
) -> Result<Provenance, Box<TraverserError>> {
    let schema =
        attached(schema, "embed_property").map_err(|err| Box::new(TraverserError::Embed(err)))?;
    let Some(node) = schema
        .label_root(kind, label)
        .and_then(|root| root.properties.get(key))
    else {
        let mut changed = schema.clone();
        let root = match kind {
            GraphNodeType::Vertex => {
                &mut changed
                    .vertices
                    .entry(label.to_string())
                    .or_default()
                    .schema
            }
            GraphNodeType::Edge => &mut changed.edges.entry(label.to_string()).or_default().schema,
        };
        root.properties.insert(key.to_string(), declaration(info));
        return Ok(Provenance::Change(changed));
    };
    let mismatch = || {
        Box::new(TraverserError::Schema(SchemaError::TypeMismatch {
            element_type: element_name(kind),
            label: label.to_string(),
            property_name: key.to_string(),
            expected_type: node.type_name(),
            actual_type: written_type(info),
        }))
    };
    let Some(vector) = node.vector.as_ref() else {
        return Err(mismatch());
    };
    // Another element kind is fine: the write quantizes to the declared one (the schema's own
    // coercion); another dimension is not.
    if vector
        .dimension
        .is_some_and(|dimension| dimension as usize != info.dimension)
    {
        return Err(mismatch());
    }
    match &vector.embedder {
        Some(declared) if declared.as_str() == info.model.as_str() => Ok(Provenance::Declared),
        Some(declared) => Err(Box::new(TraverserError::Embed(
            EmbedError::EmbedderMismatch {
                property: format!("{}.{}", quoted(label), quoted(key)),
                declared: quoted(declared),
                embedder: format!(
                    "the embedder {} has the model {}",
                    quoted(&info.name),
                    quoted(&info.model)
                ),
            },
        ))),
        None => {
            let mut changed = schema.clone();
            let root = match kind {
                GraphNodeType::Vertex => changed.vertices.get_mut(label).map(|v| &mut v.schema),
                GraphNodeType::Edge => changed.edges.get_mut(label).map(|e| &mut e.schema),
            };
            if let Some(vector) = root
                .and_then(|root| root.properties.get_mut(key))
                .and_then(|node| node.vector.as_mut())
            {
                vector.embedder = Some(info.model.to_string());
            }
            Ok(Provenance::Change(changed))
        }
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
