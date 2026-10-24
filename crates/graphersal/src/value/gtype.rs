use std::fmt;

use crate::graph::schema::ValueType;

/// A kind of stream value that the property type vocabulary deliberately does not cover.
///
/// [`ValueType`] describes *property values*; a vertex, an edge and a path are not property values
/// and are never declarable in a schema (design decision D1). They are still observable in a
/// stream, so a runtime type check has to be able to name them — that is what this enum is for,
/// and its spelling matches the already stable stream vocabulary of
/// [`TraverserValue::type_name`](crate::value::TraverserValue::type_name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamKind {
    /// A vertex.
    Vertex,
    /// An edge.
    Edge,
    /// A traversal path.
    Path,
    /// A graph value (the `cap()` of a `subgraph()`).
    Graph,
}

impl fmt::Display for StreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamKind::Vertex => f.write_str("vertex"),
            StreamKind::Edge => f.write_str("edge"),
            StreamKind::Path => f.write_str("path"),
            StreamKind::Graph => f.write_str("graph"),
        }
    }
}

/// The Apache TinkerPop `GType` type-token vocabulary.
///
/// `GType` is a *runtime type check* vocabulary, not a second type system: every token that
/// names a property type carries the corresponding [`ValueType`] verbatim in [`GType::Value`]. The
/// remaining variants exist because vanilla `GType` covers two things [`ValueType`] cannot express
/// on purpose:
///
/// - [`GType::Number`], [`GType::List`] and [`GType::Map`] are *families* — vanilla `NUMBER`
///   matches any number, while [`ValueType`] only ever names one concrete type;
/// - [`GType::Stream`] names a vertex, an edge or a path, none of which is a property type (D1).
///
/// Vanilla members Graphersal cannot represent (for example `BYTE` or `SET`) have no token
/// at all.
///
/// The associated constants use the exact vanilla member spelling, so a Gremlin query using
/// `GType.STRING` transfers unchanged.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GType {
    /// Exactly the given property type.
    Value(ValueType),
    /// Any numeric type — [`ValueType::Int64`] or [`ValueType::Float64`].
    Number,
    /// Any array, whatever its element type.
    List,
    /// Any map, whatever its fields.
    Map,
    /// Any vector, whatever its element kind and dimension (a Graphersal extension).
    Vector,
    /// A graph element kind.
    Stream(StreamKind),
}

impl GType {
    /// `GType.LONG` — a 64-bit signed integer.
    pub const LONG: GType = GType::Value(ValueType::Int64);
    /// `GType.INT` — an integer; Graphersal stores every integer as [`ValueType::Int64`].
    pub const INT: GType = GType::Value(ValueType::Int64);
    /// `GType.DOUBLE` — a 64-bit float.
    pub const DOUBLE: GType = GType::Value(ValueType::Float64);
    /// `GType.FLOAT` — a float; Graphersal stores every float as [`ValueType::Float64`].
    pub const FLOAT: GType = GType::Value(ValueType::Float64);
    /// `GType.STRING` — a UTF-8 string.
    pub const STRING: GType = GType::Value(ValueType::String);
    /// `GType.BOOLEAN` — a boolean.
    pub const BOOLEAN: GType = GType::Value(ValueType::Boolean);
    /// `GType.NULL` — the absence of a value.
    pub const NULL: GType = GType::Value(ValueType::Null);
    /// `GType.UUID` — a UUID.
    pub const UUID: GType = GType::Value(ValueType::Uuid);
    /// `GType.DATETIME` — an instant with its UTC offset (TinkerPop's date type).
    pub const DATETIME: GType = GType::Value(ValueType::DateTime);
    /// `GType.DURATION` — a length of time (TinkerPop `DURATION`; months, days and
    /// nanoseconds here).
    pub const DURATION: GType = GType::Value(ValueType::Duration);
    /// `GType.DATE` — a calendar day (a Graphersal extension).
    pub const DATE: GType = GType::Value(ValueType::Date);
    /// `GType.LOCALDATETIME` — a wall-clock date and time without a zone (a Graphersal
    /// extension).
    pub const LOCALDATETIME: GType = GType::Value(ValueType::LocalDateTime);
    /// `GType.TIME` — a time of day (a Graphersal extension).
    pub const TIME: GType = GType::Value(ValueType::Time);
    /// `GType.VECTOR` — any vector, whatever its element kind (a Graphersal extension); as a
    /// conversion target (`as_vector()`) an f32 vector.
    pub const VECTOR: GType = GType::Vector;
    /// `GType.NUMBER` — any numeric type.
    pub const NUMBER: GType = GType::Number;
    /// `GType.LIST` — any array.
    pub const LIST: GType = GType::List;
    /// `GType.MAP` — any map.
    pub const MAP: GType = GType::Map;
    /// `GType.VERTEX` — a vertex.
    pub const VERTEX: GType = GType::Stream(StreamKind::Vertex);
    /// `GType.EDGE` — an edge.
    pub const EDGE: GType = GType::Stream(StreamKind::Edge);
    /// `GType.PATH` — a traversal path.
    pub const PATH: GType = GType::Stream(StreamKind::Path);
    /// `GType.GRAPH` — a graph value.
    pub const GRAPH: GType = GType::Stream(StreamKind::Graph);

    /// The DSL token of this type (`GType.LONG`, `GType.STRING`, ...), for the canonical step
    /// rendering. A declared container type without a token of its own renders as the token of
    /// its kind (`GType.LIST`, `GType.MAP`), a union as its type name.
    pub(crate) fn token(&self) -> String {
        let name = match self {
            GType::Value(ValueType::Null) => "NULL",
            GType::Value(ValueType::Boolean) => "BOOLEAN",
            GType::Value(ValueType::Int64) => "LONG",
            GType::Value(ValueType::Float64) => "DOUBLE",
            GType::Value(ValueType::String) => "STRING",
            GType::Value(ValueType::Uuid) => "UUID",
            GType::Value(ValueType::DateTime) => "DATETIME",
            GType::Value(ValueType::Duration) => "DURATION",
            GType::Value(ValueType::Date) => "DATE",
            GType::Value(ValueType::LocalDateTime) => "LOCALDATETIME",
            GType::Value(ValueType::Time) => "TIME",
            GType::Value(ValueType::Vector(_)) | GType::Vector => "VECTOR",
            GType::Value(ValueType::Array(_)) => "LIST",
            GType::Value(ValueType::Object) => "MAP",
            GType::Value(union @ ValueType::Union(_)) => return union.to_string(),
            GType::Number => "NUMBER",
            GType::List => "LIST",
            GType::Map => "MAP",
            GType::Stream(StreamKind::Vertex) => "VERTEX",
            GType::Stream(StreamKind::Edge) => "EDGE",
            GType::Stream(StreamKind::Path) => "PATH",
            GType::Stream(StreamKind::Graph) => "GRAPH",
        };
        format!("GType.{name}")
    }

    /// The type names `P.typeOf(name)` accepts (TinkerPop 3.8 resolves a name through its
    /// registered-type cache, whose entries are the Java simple class names), in the order a
    /// diagnostic lists them.
    pub const TYPE_NAMES: &'static [&'static str] = &[
        "String",
        "Integer",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "UUID",
        "OffsetDateTime",
        "Duration",
        "LocalDate",
        "LocalDateTime",
        "LocalTime",
        "Vector",
        "Number",
        "List",
        "Map",
        "Vertex",
        "Edge",
        "Path",
        "Graph",
    ];

    /// The token a TinkerPop type name stands for in `P.typeOf(name)`: `"String"` is
    /// [`GType::STRING`], `"Integer"`/`"Long"` are [`GType::LONG`] (Graphersal stores every
    /// integer as `int64`), `"Double"`/`"Float"` [`GType::DOUBLE`], and so on through
    /// [`TYPE_NAMES`](Self::TYPE_NAMES). The match is exact (case-sensitive); any other name is
    /// `None`, which `P.typeOf` rejects, as TinkerPop rejects an unregistered name.
    pub fn from_type_name(name: &str) -> Option<GType> {
        Some(match name {
            "String" => GType::STRING,
            "Integer" | "Long" => GType::LONG,
            "Double" | "Float" => GType::DOUBLE,
            "Boolean" => GType::BOOLEAN,
            "UUID" => GType::UUID,
            "OffsetDateTime" => GType::DATETIME,
            "Duration" => GType::DURATION,
            "LocalDate" => GType::DATE,
            "LocalDateTime" => GType::LOCALDATETIME,
            "LocalTime" => GType::TIME,
            "Vector" => GType::VECTOR,
            "Number" => GType::NUMBER,
            "List" => GType::LIST,
            "Map" => GType::MAP,
            "Vertex" => GType::VERTEX,
            "Edge" => GType::EDGE,
            "Path" => GType::PATH,
            "Graph" => GType::GRAPH,
            _ => return None,
        })
    }

    /// Returns whether an observed value type satisfies this token.
    ///
    /// [`GType::Value`] compares for equality: this is a check on the type a value *has*, so a
    /// [`ValueType::Union`] target is matched literally and never member-wise — a union is a schema
    /// declaration, and no value ever observes one. The family tokens are the only ones that
    /// match more than a single type.
    pub fn matches(&self, observed: &ValueType) -> bool {
        match self {
            GType::Value(declared) => declared == observed,
            GType::Number => matches!(observed, ValueType::Int64 | ValueType::Float64),
            GType::List => matches!(observed, ValueType::Array(_)),
            GType::Map => matches!(observed, ValueType::Object),
            GType::Vector => matches!(observed, ValueType::Vector(_)),
            GType::Stream(_) => false,
        }
    }

    /// Returns the property type this token names, or `None` for a token that names a family,
    /// or a graph element.
    ///
    /// This is what a conversion target has to resolve to: `as_number(GType::LONG)` converts to
    /// `int64`, while `as_number(GType::NUMBER)` names no single target and is handled by the
    /// step itself.
    pub fn as_type(&self) -> Option<&ValueType> {
        match self {
            GType::Value(declared) => Some(declared),
            _ => None,
        }
    }
}

impl From<ValueType> for GType {
    fn from(value: ValueType) -> Self {
        GType::Value(value)
    }
}

impl From<&ValueType> for GType {
    fn from(value: &ValueType) -> Self {
        GType::Value(value.clone())
    }
}

impl From<StreamKind> for GType {
    fn from(value: StreamKind) -> Self {
        GType::Stream(value)
    }
}

impl fmt::Display for GType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GType::Value(declared) => write!(f, "{declared}"),
            GType::Number => f.write_str("number"),
            GType::List => f.write_str("list"),
            GType::Map => f.write_str("map"),
            GType::Vector => f.write_str("vector"),
            GType::Stream(kind) => write!(f, "{kind}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_constants_map_onto_the_type_vocabulary() {
        assert_eq!(GType::LONG.as_type(), Some(&ValueType::Int64));
        assert_eq!(GType::INT.as_type(), Some(&ValueType::Int64));
        assert_eq!(GType::DOUBLE.as_type(), Some(&ValueType::Float64));
        assert_eq!(GType::FLOAT.as_type(), Some(&ValueType::Float64));
        assert_eq!(GType::STRING.as_type(), Some(&ValueType::String));
        assert_eq!(GType::BOOLEAN.as_type(), Some(&ValueType::Boolean));
        assert_eq!(GType::NULL.as_type(), Some(&ValueType::Null));
        assert_eq!(GType::UUID.as_type(), Some(&ValueType::Uuid));
    }

    #[test]
    fn family_tokens_match_more_than_one_type() {
        assert!(GType::NUMBER.matches(&ValueType::Int64));
        assert!(GType::NUMBER.matches(&ValueType::Float64));
        assert!(!GType::NUMBER.matches(&ValueType::String));

        assert!(GType::LIST.matches(&ValueType::Array(Box::new(ValueType::String))));
        assert!(GType::LIST.matches(&ValueType::Array(Box::new(ValueType::Null))));
        assert!(!GType::LIST.matches(&ValueType::String));

        assert!(GType::MAP.matches(&ValueType::Object));
        assert!(!GType::MAP.matches(&ValueType::Array(Box::new(ValueType::String))));
    }

    #[test]
    fn value_tokens_match_exactly() {
        assert!(GType::STRING.matches(&ValueType::String));
        assert!(!GType::STRING.matches(&ValueType::Uuid));
        assert!(!GType::LONG.matches(&ValueType::Float64));
    }

    #[test]
    fn a_union_target_is_matched_literally() {
        let union = ValueType::Union(vec![ValueType::Int64, ValueType::String]);
        assert!(GType::from(union.clone()).matches(&union));
        assert!(!GType::from(union).matches(&ValueType::Int64));
    }

    #[test]
    fn element_tokens_never_match_a_value_type() {
        assert!(!GType::VERTEX.matches(&ValueType::Object));
        assert!(!GType::EDGE.matches(&ValueType::Object));
        assert!(!GType::PATH.matches(&ValueType::Array(Box::new(ValueType::Null))));
    }

    #[test]
    fn display_is_the_stable_rendering() {
        assert_eq!(GType::LONG.to_string(), "int64");
        assert_eq!(GType::NUMBER.to_string(), "number");
        assert_eq!(GType::LIST.to_string(), "list");
        assert_eq!(GType::MAP.to_string(), "map");
        assert_eq!(GType::VERTEX.to_string(), "vertex");
    }

    #[test]
    fn every_type_name_resolves_and_token_names_do_not() {
        for name in GType::TYPE_NAMES {
            assert!(GType::from_type_name(name).is_some(), "{name}");
        }
        assert_eq!(GType::from_type_name("Graph"), Some(GType::GRAPH));
        // TinkerPop registers Java simple class names, not the token names.
        assert_eq!(GType::from_type_name("LONG"), None);
        assert_eq!(GType::from_type_name("GRAPH"), None);
    }

    /// The token form and the name form select the same values, in both DSL spellings.
    #[test]
    fn type_of_takes_tokens_and_names_alike() {
        crate::test_dual!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g
                .e(None)
                .has_label("knows")
                .subgraph("sg")
                .cap("sg")
                .is(crate::P::type_of(GType::GRAPH))
                .count()
                .to_list(),
            rhai: [
                r#"g.E().hasLabel("knows").subgraph("sg").cap("sg").is(P.typeOf(GType.GRAPH)).count()"#,
                r#"g.E().hasLabel("knows").subgraph("sg").cap("sg").is(P.typeOf("Graph")).count()"#,
                r#"g.e().has_label("knows").subgraph("sg").cap("sg").is(P::typeOf(GType::GRAPH)).count()"#,
                r#"g.e().has_label("knows").subgraph("sg").cap("sg").is(typeOf("Graph")).count()"#,
            ],
            assert: |res| assert_eq!(res, &[1.into()]),
        );
        crate::test_dual!(
            graph: crate::GraphSource::tinkerpop_modern(),
            rust: |g| g.v(None).values("age").is(crate::P::type_of(GType::LONG)).count().to_list(),
            rhai: [
                r#"g.V().values("age").is(P.typeOf(GType.LONG)).count()"#,
                r#"g.V().values("age").is(P.typeOf("Long")).count()"#,
                r#"g.v().values("age").is(P::typeOf(GType::INT)).count()"#,
                r#"g.v().values("age").is(typeOf("Integer")).count()"#,
            ],
            assert: |res| assert_eq!(res, &[4.into()]),
        );
    }
}
