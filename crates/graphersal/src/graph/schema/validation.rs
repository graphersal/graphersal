use crate::error::SchemaError;
use crate::graph::schema::coerce::coerce_to_declared;
use crate::graph::schema::nested::{self, NestedCtx};
use crate::graph::schema::{GraphSchema, PropertySchema, SchemaMode};
use crate::value::json_path::JsonPathSegment;
use crate::value::{ElementProperty, PropertyMap};
use compact_str::CompactString;
use std::ops::ControlFlow;

/// The labels of an endpoint for diagnostics: `A:B`, or empty when unlabeled.
pub(super) fn join_labels(labels: &[CompactString]) -> String {
    labels
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join(":")
}

/// The schema root one label contributes: the label name and its `object` node.
pub(crate) type LabelRoot<'a> = (&'a str, &'a PropertySchema);

/// Schema enforcement on write: coercion to the declared type, then validation.
///
/// # The declaring-label rule
///
/// The same rule holds for vertices and edges, on add and on set:
///
/// - **Coercion** uses the declaration of the *first* label, in the element's label order, whose
///   schema declares the key (see `schema/coerce.rs`: a canonical string becomes a `uuid`, an
///   `int64` becomes a `float64`; nothing else converts).
/// - **Validation** then checks the coerced value against *every* label that declares the key,
///   so conflicting declarations across labels surface as errors.
/// - **Undeclared keys**: a top-level property no label declares is allowed when ANY of the
///   element's labels admits additional properties (its root's `additionalProperties`, or the
///   mode's default: `Open` admits, `Closed` does not). The same "ANY label" rule as topology.
/// - **Required keys**: every label's `required` list must be satisfied (absence is the
///   violation; whether `null` is allowed is the property's own `type`).
///
/// # Nested content
///
/// The content of an `object`/`array`/`anyOf` value is validated at any depth, with errors
/// naming the full canonical path (`$.a.b[1].c`); see `schema/nested.rs` for the rules. A
/// nested write (`property(jpath(..), v)`) coerces the leaf at the node declared at that
/// location.
///
/// A schema's `default`/`defaultFunction` is filled in before this check, by the create paths
/// only (`schema/defaults.rs`); the validator itself never fills an absent property.
///
/// In [`SchemaMode::None`] nothing is coerced or validated.
pub(crate) struct SchemaValidator;

impl SchemaValidator {
    /// Coerces and validates the properties of a vertex being inserted, in place.
    ///
    /// `labels` is the vertex's full label set: each label is checked against its own
    /// `VertexSchema` entry, if one exists.
    ///
    /// - `SchemaMode::Open`: a label with no schema entry is permitted unchecked, and an empty
    ///   `labels` has nothing to check.
    /// - `SchemaMode::Closed`: every label must have a schema entry — an unknown label (or an
    ///   empty `labels`) is rejected.
    ///
    /// On success `properties` holds the coerced values that should be stored. On failure its
    /// contents are unspecified and must be discarded.
    pub fn validate_vertex(
        schema: &GraphSchema,
        labels: &[CompactString],
        properties: &mut PropertyMap,
    ) -> Result<(), SchemaError> {
        let Some(declarations) = Self::vertex_declarations(schema, labels)? else {
            return Ok(());
        };
        Self::coerce_properties(&declarations, "vertex", properties)?;
        Self::check_properties(&declarations, "vertex", properties, schema.mode)
    }

    /// Validates the stored properties of an existing vertex, without coercing anything.
    ///
    /// Stored data is checked exactly as it is: a string held in a field declared `uuid` is a
    /// [`SchemaError::TypeMismatch`], because reads never coerce.
    pub(crate) fn check_vertex(
        schema: &GraphSchema,
        labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Result<(), SchemaError> {
        let Some(declarations) = Self::vertex_declarations(schema, labels)? else {
            return Ok(());
        };
        Self::check_properties(&declarations, "vertex", properties, schema.mode)
    }

    /// Coerces and validates the properties of an edge being inserted, in place, and — in
    /// `Closed` mode — its endpoint topology.
    ///
    /// On success `properties` holds the coerced values that should be stored. On failure its
    /// contents are unspecified and must be discarded.
    pub fn validate_edge(
        schema: &GraphSchema,
        label: Option<&str>,
        source_labels: &[CompactString],
        target_labels: &[CompactString],
        properties: &mut PropertyMap,
    ) -> Result<(), SchemaError> {
        let Some(declarations) = Self::edge_declarations(schema, label)? else {
            return Ok(());
        };
        Self::check_topology(schema, label, source_labels, target_labels)?;
        Self::coerce_properties(&declarations, "edge", properties)?;
        Self::check_properties(&declarations, "edge", properties, schema.mode)
    }

    /// [`Self::validate_edge`] without the endpoint topology: the check of an edge with a stub
    /// endpoint (`GraphStorage::insert_vertex_stub`), whose labels are not known yet;
    /// [`Self::check_connection`] runs once the stub is completed.
    pub(crate) fn validate_edge_without_topology(
        schema: &GraphSchema,
        label: Option<&str>,
        properties: &mut PropertyMap,
    ) -> Result<(), SchemaError> {
        let Some(declarations) = Self::edge_declarations(schema, label)? else {
            return Ok(());
        };
        Self::coerce_properties(&declarations, "edge", properties)?;
        Self::check_properties(&declarations, "edge", properties, schema.mode)
    }

    /// The endpoint topology part of [`Self::validate_edge`] alone: the check of an existing
    /// edge once its stub endpoint is completed.
    pub(crate) fn check_connection(
        schema: &GraphSchema,
        label: Option<&str>,
        source_labels: &[CompactString],
        target_labels: &[CompactString],
    ) -> Result<(), SchemaError> {
        if Self::edge_declarations(schema, label)?.is_none() {
            return Ok(());
        }
        Self::check_topology(schema, label, source_labels, target_labels)
    }

    /// Validates the stored properties of an existing edge under `label`, without coercing
    /// anything, and — in `Closed` mode — its endpoint topology: the check of a label change
    /// (`GraphStorage::set_edge_label`). Stored data is checked exactly as it is (see
    /// [`Self::check_vertex`]).
    pub(crate) fn check_edge(
        schema: &GraphSchema,
        label: Option<&str>,
        source_labels: &[CompactString],
        target_labels: &[CompactString],
        properties: &PropertyMap,
    ) -> Result<(), SchemaError> {
        let Some(declarations) = Self::edge_declarations(schema, label)? else {
            return Ok(());
        };
        Self::check_topology(schema, label, source_labels, target_labels)?;
        Self::check_properties(&declarations, "edge", properties, schema.mode)
    }

    /// Coerces and validates a single property assignment on an existing element, returning the
    /// value that should be stored.
    ///
    /// `labels` is the element's full label set: a vertex passes all of its labels, an edge a
    /// slice with its single label, or an empty slice when it has none. The declaring-label rule
    /// of [`SchemaValidator`] applies.
    pub fn validate_element_property(
        schema: &GraphSchema,
        element_type: &'static str,
        labels: &[CompactString],
        prop_name: &str,
        value: ElementProperty,
    ) -> Result<ElementProperty, SchemaError> {
        let Some(declarations) = Self::declarations(schema, element_type, labels)? else {
            return Ok(value);
        };
        let value = Self::coerce_property(&declarations, element_type, prop_name, value)?;
        Self::check_property(
            &declarations,
            element_type,
            prop_name,
            &value,
            schema.mode.default_additional_properties(),
        )?;
        Ok(value)
    }

    /// Coerces the leaf of a nested write (`property(jpath(..), value)`) to the node the schema
    /// declares at that location: the first label that declares the property name decides, then
    /// `properties` / `items` / `anyOf` are walked along the path (see `schema/nested.rs`). An
    /// undeclared location, an unlabeled element in `Open` mode and `SchemaMode::None` leave
    /// the value unchanged.
    pub(crate) fn coerce_path_leaf(
        schema: &GraphSchema,
        element_type: &'static str,
        labels: &[CompactString],
        path: &crate::value::JsonPath,
        value: ElementProperty,
    ) -> Result<ElementProperty, SchemaError> {
        let Some(declarations) = Self::declarations(schema, element_type, labels)? else {
            return Ok(value);
        };
        let segments = path.segments();
        let Some(JsonPathSegment::Key(name)) = segments.first() else {
            return Ok(value);
        };
        let Some((label, node)) = declarations
            .iter()
            .find_map(|(lbl, root)| root.properties.get(name.as_str()).map(|node| (*lbl, node)))
        else {
            return Ok(value);
        };
        let ctx = NestedCtx {
            element_type,
            label,
            additional_default: schema.mode.default_additional_properties(),
            full: true,
        };
        let Some(declared) = nested::declared_node_at(&ctx, segments, node, 1, &value)? else {
            return Ok(value);
        };
        coerce_to_declared(value, declared).map_err(|failure| SchemaError::CoercionFailed {
            element_type,
            label: label.to_string(),
            property_name: path.to_string(),
            declared_type: declared.non_null_type_name(),
            actual_value: crate::traversal::step::barrier_steps::numeric::preview(&failure.value)
                .into_boxed_str(),
            reason: failure.reason,
        })
    }

    /// Validates (without coercing) the complete value of one property of an existing element:
    /// the check after a nested write or removal produced `value` for `prop_name`.
    pub(crate) fn check_element_property(
        schema: &GraphSchema,
        element_type: &'static str,
        labels: &[CompactString],
        prop_name: &str,
        value: &ElementProperty,
    ) -> Result<(), SchemaError> {
        let Some(declarations) = Self::declarations(schema, element_type, labels)? else {
            return Ok(());
        };
        Self::check_property(
            &declarations,
            element_type,
            prop_name,
            value,
            schema.mode.default_additional_properties(),
        )
    }

    fn declarations<'a>(
        schema: &'a GraphSchema,
        element_type: &'static str,
        labels: &'a [CompactString],
    ) -> Result<Option<Vec<LabelRoot<'a>>>, SchemaError> {
        if element_type == "vertex" {
            Self::vertex_declarations(schema, labels)
        } else {
            Self::edge_declarations(schema, labels.first().map(|l| l.as_str()))
        }
    }

    /// Resolves the declarations that apply to a vertex with `labels`, in label order.
    ///
    /// `None` means there is nothing to enforce (mode `None`, or an unlabeled vertex in `Open`
    /// mode). In `Closed` mode an unknown or missing label is an error.
    pub(crate) fn vertex_declarations<'a>(
        schema: &'a GraphSchema,
        labels: &'a [CompactString],
    ) -> Result<Option<Vec<LabelRoot<'a>>>, SchemaError> {
        match schema.mode {
            SchemaMode::None => Ok(None),
            SchemaMode::Open => {
                if labels.is_empty() {
                    return Ok(None);
                }
                Ok(Some(
                    labels
                        .iter()
                        .filter_map(|lbl| {
                            schema
                                .vertices
                                .get(lbl.as_str())
                                .map(|v| (lbl.as_str(), &v.schema))
                        })
                        .collect(),
                ))
            }
            SchemaMode::Closed => {
                if labels.is_empty() {
                    return Err(SchemaError::UnknownVertexLabel {
                        label: "<unlabeled>".to_string(),
                        allowed: schema.vertices.keys().cloned().collect(),
                    });
                }
                let mut declarations = Vec::with_capacity(labels.len());
                for lbl in labels {
                    let vertex = schema.vertices.get(lbl.as_str()).ok_or_else(|| {
                        SchemaError::UnknownVertexLabel {
                            label: lbl.to_string(),
                            allowed: schema.vertices.keys().cloned().collect(),
                        }
                    })?;
                    declarations.push((lbl.as_str(), &vertex.schema));
                }
                Ok(Some(declarations))
            }
        }
    }

    /// Resolves the declarations that apply to an edge with `label`.
    ///
    /// `None` means there is nothing to enforce (mode `None`, or an unlabeled edge in `Open`
    /// mode). In `Closed` mode an unknown or missing label is an error.
    pub(crate) fn edge_declarations<'a>(
        schema: &'a GraphSchema,
        label: Option<&'a str>,
    ) -> Result<Option<Vec<LabelRoot<'a>>>, SchemaError> {
        match schema.mode {
            SchemaMode::None => Ok(None),
            SchemaMode::Open => {
                let Some(lbl) = label else {
                    return Ok(None);
                };
                Ok(Some(
                    schema
                        .edges
                        .get(lbl)
                        .map(|e| (lbl, &e.schema))
                        .into_iter()
                        .collect(),
                ))
            }
            SchemaMode::Closed => {
                let lbl = match label {
                    Some(l) if !l.is_empty() => l,
                    _ => {
                        return Err(SchemaError::UnknownEdgeLabel {
                            label: "<unlabeled>".to_string(),
                            allowed: schema.edges.keys().cloned().collect(),
                        });
                    }
                };
                let edge = schema
                    .edges
                    .get(lbl)
                    .ok_or_else(|| SchemaError::UnknownEdgeLabel {
                        label: lbl.to_string(),
                        allowed: schema.edges.keys().cloned().collect(),
                    })?;
                Ok(Some(vec![(lbl, &edge.schema)]))
            }
        }
    }

    /// Checks, in `Closed` mode only, that the edge's endpoints match a declared connection.
    ///
    /// Vertex labels are a set, so the edge is accepted if ANY label of the source and ANY label
    /// of the target matches a declared connection (`*` matches every label). An edge label
    /// without declared connections allows every endpoint.
    pub(crate) fn check_topology(
        schema: &GraphSchema,
        label: Option<&str>,
        source_labels: &[CompactString],
        target_labels: &[CompactString],
    ) -> Result<(), SchemaError> {
        if schema.mode != SchemaMode::Closed {
            return Ok(());
        }
        let Some(lbl) = label else {
            return Ok(());
        };
        let Some(edge) = schema.edges.get(lbl) else {
            return Ok(());
        };
        if edge.connections.is_empty() {
            return Ok(());
        }
        let matches_end = |declared: &str, labels: &[CompactString]| {
            declared == "*" || labels.iter().any(|l| l.as_str() == declared)
        };
        let matching = edge
            .connections
            .iter()
            .any(|c| matches_end(&c.from, source_labels) && matches_end(&c.to, target_labels));
        if matching {
            return Ok(());
        }
        Err(SchemaError::InvalidEdgeTopology {
            source_label: join_labels(source_labels),
            edge_label: lbl.to_string(),
            target_label: join_labels(target_labels),
            allowed: edge
                .connections
                .iter()
                .map(|c| format!("(:{})-[:{lbl}]->(:{})", c.from, c.to))
                .collect(),
        })
    }

    /// Coerces every declared property of `properties` in place.
    fn coerce_properties(
        declarations: &[LabelRoot<'_>],
        element_type: &'static str,
        properties: &mut PropertyMap,
    ) -> Result<(), SchemaError> {
        for (prop_name, slot) in properties.iter_mut() {
            let incoming = std::mem::replace(slot, ElementProperty::Null);
            *slot = Self::coerce_property(declarations, element_type, prop_name, incoming)?;
        }
        Ok(())
    }

    /// Coerces one value by the declaration of the first label that declares `prop_name`.
    fn coerce_property(
        declarations: &[LabelRoot<'_>],
        element_type: &'static str,
        prop_name: &str,
        value: ElementProperty,
    ) -> Result<ElementProperty, SchemaError> {
        let declaring = declarations
            .iter()
            .find_map(|(lbl, root)| root.properties.get(prop_name).map(|node| (*lbl, node)));
        let Some((lbl, node)) = declaring else {
            return Ok(value);
        };
        coerce_to_declared(value, node).map_err(|failure| SchemaError::CoercionFailed {
            element_type,
            label: lbl.to_string(),
            property_name: prop_name.to_string(),
            declared_type: node.non_null_type_name(),
            actual_value: crate::traversal::step::barrier_steps::numeric::preview(&failure.value)
                .into_boxed_str(),
            reason: failure.reason,
        })
    }

    /// Checks every property against every label that declares it, then every label's
    /// `required` keys.
    fn check_properties(
        declarations: &[LabelRoot<'_>],
        element_type: &'static str,
        properties: &PropertyMap,
        mode: SchemaMode,
    ) -> Result<(), SchemaError> {
        let additional_default = mode.default_additional_properties();
        for (prop_name, prop_value) in properties {
            Self::check_property(
                declarations,
                element_type,
                prop_name,
                prop_value,
                additional_default,
            )?;
        }
        if let Some((label, name)) = Self::missing_required(declarations, properties).next() {
            return Err(SchemaError::MissingRequiredProperty {
                element_type,
                label: label.to_string(),
                property_name: name.to_string(),
            });
        }
        Ok(())
    }

    /// Every `(label, key)` a label requires that `properties` lacks.
    pub(crate) fn missing_required<'a>(
        declarations: &'a [LabelRoot<'a>],
        properties: &'a PropertyMap,
    ) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        declarations.iter().flat_map(move |(label, root)| {
            root.required
                .iter()
                .filter(move |name| !properties.contains_key(name.as_str()))
                .map(move |name| (*label, name.as_str()))
        })
    }

    /// Checks that removing the top-level property `prop_name` from an element with `labels`
    /// keeps it valid: a property ANY of the element's labels lists in `required` cannot be
    /// removed, in `Open` and `Closed` mode alike. (Whether the property is present at all is
    /// the caller's business: removing an absent one is a no-op and must not call this.)
    pub(crate) fn check_removal(
        schema: &GraphSchema,
        element_type: &'static str,
        labels: &[CompactString],
        prop_name: &str,
    ) -> Result<(), SchemaError> {
        // Never fails for an unknown label here: that is reported by the write that created it.
        let declarations = Self::declarations(schema, element_type, labels).unwrap_or(None);
        let Some(declarations) = declarations else {
            return Ok(());
        };
        for (lbl, root) in &declarations {
            if root.is_required(prop_name) {
                return Err(SchemaError::RequiredPropertyRemoval {
                    element_type,
                    label: lbl.to_string(),
                    property_name: prop_name.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Checks one value against every label that declares `prop_name`, failing on the first
    /// violation. A property no label declares is [`SchemaError::UndeclaredProperty`] unless one
    /// of the labels admits additional properties.
    pub(crate) fn check_property(
        declarations: &[LabelRoot<'_>],
        element_type: &'static str,
        prop_name: &str,
        value: &ElementProperty,
        additional_default: bool,
    ) -> Result<(), SchemaError> {
        let mut sink = nested::FailFast::default();
        let _ = Self::walk_property(
            declarations,
            element_type,
            prop_name,
            value,
            additional_default,
            &mut sink,
        );
        sink.into_result()
    }

    /// [`Self::check_property`] reporting to `sink`: with a collecting sink, every violation
    /// of the value (every label, every nested key and item) instead of the first.
    pub(crate) fn walk_property<S: nested::Sink>(
        declarations: &[LabelRoot<'_>],
        element_type: &'static str,
        prop_name: &str,
        value: &ElementProperty,
        additional_default: bool,
        sink: &mut S,
    ) -> ControlFlow<()> {
        let mut declared = false;
        for (lbl, root) in declarations {
            if let Some(node) = root.properties.get(prop_name) {
                declared = true;
                let ctx = NestedCtx {
                    element_type,
                    label: lbl,
                    additional_default,
                    full: true,
                };
                let mut path = vec![JsonPathSegment::Key(prop_name.into())];
                nested::walk(&ctx, &mut path, node, value, sink)?;
            }
        }
        if !declared
            && !declarations.is_empty()
            && !declarations
                .iter()
                .any(|(_, root)| root.admits_additional_properties(additional_default))
        {
            return sink.report(SchemaError::UndeclaredProperty {
                element_type,
                // Every label declined it; name the first label in the diagnostic.
                label: declarations
                    .first()
                    .map(|(lbl, _)| lbl.to_string())
                    .unwrap_or_default(),
                property_name: prop_name.to_string(),
            });
        }
        ControlFlow::Continue(())
    }

    /// Validates one value of the vertex property `label.prop_name` against one node with a
    /// mode's defaults: the same check a write runs for a declared property, without coercion.
    #[cfg(test)]
    pub(crate) fn validate_value(
        node: &PropertySchema,
        mode: SchemaMode,
        label: &str,
        prop_name: &str,
        value: &ElementProperty,
    ) -> Result<(), SchemaError> {
        let ctx = NestedCtx {
            element_type: "vertex",
            label,
            additional_default: mode.default_additional_properties(),
            full: true,
        };
        let mut path = vec![JsonPathSegment::Key(prop_name.into())];
        nested::validate_nested(&ctx, &mut path, node, value)
    }
}

/// The node-level value check: exact logical types, nullability and the type names of a mismatch.
#[cfg(test)]
mod value_check_tests {
    use super::SchemaValidator;
    use crate::error::{DiagnosticError, SchemaError};
    use crate::graph::schema::{JsonType, PropertySchema, SchemaMode};
    use crate::value::{ElementProperty, Number};

    fn check(node: &PropertySchema, value: ElementProperty) -> Result<(), SchemaError> {
        SchemaValidator::validate_value(node, SchemaMode::Closed, "person", "p", &value)
    }

    /// Exact logical types, no widening: legitimate conversions happen earlier, as coercion on
    /// write (`schema/coerce.rs`).
    #[test]
    fn test_type_truth_table() {
        let int = ElementProperty::from(1);
        let float = ElementProperty::Number(Number::from_f64(1.0));
        let text = ElementProperty::from("x");
        let uuid = ElementProperty::Uuid(0x550e8400_e29b_41d4_a716_446655440000);

        assert!(check(&PropertySchema::integer(), int.clone()).is_ok());
        assert!(check(&PropertySchema::integer(), float.clone()).is_err());
        assert!(check(&PropertySchema::number(), float.clone()).is_ok());
        // An int64 is coerced to float64 on write, not accepted as stored data.
        assert!(check(&PropertySchema::number(), int.clone()).is_err());
        assert!(check(&PropertySchema::string(), text.clone()).is_ok());
        // A string never satisfies a declared uuid, nor a uuid a declared string.
        assert!(check(&PropertySchema::uuid(), text.clone()).is_err());
        assert!(check(&PropertySchema::string(), uuid.clone()).is_err());
        assert!(check(&PropertySchema::uuid(), uuid.clone()).is_ok());

        // A type array and anyOf: any member.
        let either = PropertySchema::of_types(&[JsonType::Integer, JsonType::String]);
        assert!(check(&either, int.clone()).is_ok());
        assert!(check(&either, text.clone()).is_ok());
        assert!(check(&either, ElementProperty::Boolean(true)).is_err());
        let any_of =
            PropertySchema::any_of(vec![PropertySchema::integer(), PropertySchema::uuid()]);
        assert!(check(&any_of, uuid.clone()).is_ok());
        assert!(check(&any_of, text.clone()).is_err());

        // `{}` admits everything.
        for value in [
            int.clone(),
            text.clone(),
            uuid.clone(),
            ElementProperty::Null,
        ] {
            assert!(check(&PropertySchema::any(), value).is_ok());
        }

        // Arrays compare item by item.
        let ids = PropertySchema::array(PropertySchema::uuid());
        assert!(check(&ids, ElementProperty::Array(vec![uuid.clone()])).is_ok());
        assert!(check(&ids, ElementProperty::Array(vec![text.clone()])).is_err());
        assert!(check(&ids, ElementProperty::Array(Vec::new())).is_ok());
        assert!(check(&ids, uuid).is_err());
    }

    /// `null` is a value like any other: it conforms only where the type admits it; absence is
    /// `required`'s business.
    #[test]
    fn test_null_is_governed_by_the_type() {
        let required_text = PropertySchema::string();
        let nullable_text = PropertySchema::string().nullable();
        assert!(check(&nullable_text, ElementProperty::Null).is_ok());
        let err = check(&required_text, ElementProperty::Null).expect_err("null is not a string");
        match &err {
            SchemaError::TypeMismatch { actual_type, .. } => assert_eq!(actual_type, "null"),
            other => panic!("Expected TypeMismatch, got {other:?}"),
        }
        assert!(err.help().unwrap().contains("does not admit null"));
    }

    /// A type mismatch reports both types through the canonical `Display`, never `Debug`.
    #[test]
    fn test_type_mismatch_renders_canonical_type_names() {
        let err = check(&PropertySchema::integer(), ElementProperty::from("29"))
            .expect_err("a string must not satisfy a declared int64");
        match err {
            SchemaError::TypeMismatch {
                ref expected_type,
                ref actual_type,
                ..
            } => {
                assert_eq!(expected_type, "int64");
                assert_eq!(actual_type, "string");
            }
            ref other => panic!("Expected SchemaError::TypeMismatch, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            "Type mismatch on vertex 'person.p': expected int64, got string"
        );
        let union = PropertySchema::integer().nullable();
        assert_eq!(union.type_name(), "union<int64|null>");
    }
}
