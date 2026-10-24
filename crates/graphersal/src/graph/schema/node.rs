//! One node of the supported JSON Schema 2020-12 subset: [`PropertySchema`] and its parts.
//!
//! A label's `schema` and every property inside it are `PropertySchema`s. The model stays
//! close to the JSON form (one field per keyword), so a schema round-trips through
//! [`GraphSchema::to_value`](crate::graph::schema::GraphSchema::to_value) unchanged, apart from
//! `$ref`/`$defs`, which are inlined when the schema is loaded.

use indexmap::IndexMap;

use crate::graph::schema::SchemaPattern;
use crate::graph::schema::defaults::CreateContext;
use crate::value::temporal::{Temporal, TemporalKind};
use crate::value::{ElementProperty, Number, Vector, VectorElement, VectorMetric};

/// A JSON Schema `type` name of the supported subset.
///
/// The engine's logical types map one to one, except `string`, which with
/// [`SchemaFormat::Uuid`] stands for `uuid`: `integer` = `int64` (a value outside `i64` cannot
/// be stored at all), `number` = `float64` (an `int64` written to it is coerced when exactly
/// representable), `string` = `string`, `boolean`, `null`, `array`, `object`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JsonType {
    /// `null`.
    Null,
    /// `boolean`.
    Boolean,
    /// `integer`: an `int64`.
    Integer,
    /// `number`: a `float64`.
    Number,
    /// `string` (a `uuid` with `format: "uuid"`).
    String,
    /// `array`.
    Array,
    /// `object`.
    Object,
}

impl JsonType {
    /// Every type, in the order diagnostics list them.
    pub const ALL: [JsonType; 7] = [
        JsonType::Null,
        JsonType::Boolean,
        JsonType::Integer,
        JsonType::Number,
        JsonType::String,
        JsonType::Array,
        JsonType::Object,
    ];

    /// The JSON Schema spelling: `null`, `boolean`, `integer`, `number`, `string`, `array`,
    /// `object`.
    pub fn as_str(self) -> &'static str {
        match self {
            JsonType::Null => "null",
            JsonType::Boolean => "boolean",
            JsonType::Integer => "integer",
            JsonType::Number => "number",
            JsonType::String => "string",
            JsonType::Array => "array",
            JsonType::Object => "object",
        }
    }

    /// Parses a JSON Schema type name (case-sensitive, no aliases).
    pub fn from_name(name: &str) -> Option<Self> {
        JsonType::ALL.into_iter().find(|ty| ty.as_str() == name)
    }

    /// The engine's canonical name of this type (`int64`, `float64`, ...), as diagnostics,
    /// `Display` and `GType` spell it.
    pub(crate) fn engine_name(self, format: Option<SchemaFormat>) -> &'static str {
        match self {
            JsonType::Null => "null",
            JsonType::Boolean => "boolean",
            JsonType::Integer => "int64",
            JsonType::Number => "float64",
            JsonType::String => match format {
                Some(format) => format.engine_name(),
                None => "string",
            },
            JsonType::Array => "array",
            JsonType::Object => "object",
        }
    }
}

/// A `format` the engine understands: each one turns `string` into a logical type. Every other
/// `format` is an error when a schema is loaded (a later type must not silently change what an
/// existing schema accepts).
///
/// JSON Schema's `format` is an annotation for other validators, and they ignore a format they
/// do not know: `local-date-time` (no JSON Schema name exists) is safe in a schema shared with
/// them. Note that JSON Schema's own `time` (RFC 3339 `full-time`) requires a UTC offset, while
/// Graphersal's `time` is a local time of day without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SchemaFormat {
    /// `format: "uuid"`: a `string` that is a UUID, stored as the logical `uuid` type.
    Uuid,
    /// `format: "date-time"`: an instant with its UTC offset, the logical `datetime` type.
    DateTime,
    /// `format: "date"`: a calendar day, the logical `date` type.
    Date,
    /// `format: "local-date-time"`: a wall-clock date and time without a zone, the logical
    /// `localdatetime` type (a Graphersal name; JSON Schema has none).
    LocalDateTime,
    /// `format: "time"`: a time of day without a zone, the logical `time` type (JSON Schema's
    /// `time` has an offset; this one has none).
    Time,
    /// `format: "duration"`: months, days and nanoseconds (ISO 8601 text), the logical
    /// `duration` type.
    Duration,
    /// `format: "offset-time"`: a time of day with its UTC offset (`09:30:00+02:00`), the
    /// logical `offsettime` type (a Graphersal name; JSON Schema's `time` is this form, but
    /// Graphersal's `time` has no offset).
    OffsetTime,
}

impl SchemaFormat {
    /// Every format, in the order the documentation lists them.
    pub const ALL: [SchemaFormat; 7] = [
        SchemaFormat::Uuid,
        SchemaFormat::DateTime,
        SchemaFormat::Date,
        SchemaFormat::LocalDateTime,
        SchemaFormat::Time,
        SchemaFormat::OffsetTime,
        SchemaFormat::Duration,
    ];

    /// The keyword value: `uuid`, `date-time`, `date`, `local-date-time`, `time`,
    /// `offset-time`, `duration`.
    pub fn as_str(self) -> &'static str {
        match self {
            SchemaFormat::Uuid => "uuid",
            SchemaFormat::DateTime => "date-time",
            SchemaFormat::Date => "date",
            SchemaFormat::LocalDateTime => "local-date-time",
            SchemaFormat::Time => "time",
            SchemaFormat::Duration => "duration",
            SchemaFormat::OffsetTime => "offset-time",
        }
    }

    /// Parses a keyword value (case-sensitive, no aliases).
    pub fn from_name(name: &str) -> Option<Self> {
        SchemaFormat::ALL
            .into_iter()
            .find(|format| format.as_str() == name)
    }

    /// The temporal type the format declares; `None` for `uuid`.
    pub fn temporal_kind(self) -> Option<TemporalKind> {
        match self {
            SchemaFormat::Uuid => None,
            SchemaFormat::DateTime => Some(TemporalKind::DateTime),
            SchemaFormat::Date => Some(TemporalKind::Date),
            SchemaFormat::LocalDateTime => Some(TemporalKind::LocalDateTime),
            SchemaFormat::Time => Some(TemporalKind::Time),
            SchemaFormat::Duration => Some(TemporalKind::Duration),
            SchemaFormat::OffsetTime => Some(TemporalKind::OffsetTime),
        }
    }

    /// The format that declares a temporal type.
    pub fn of_temporal(kind: TemporalKind) -> Self {
        match kind {
            TemporalKind::DateTime => SchemaFormat::DateTime,
            TemporalKind::Date => SchemaFormat::Date,
            TemporalKind::LocalDateTime => SchemaFormat::LocalDateTime,
            TemporalKind::Time => SchemaFormat::Time,
            TemporalKind::Duration => SchemaFormat::Duration,
            TemporalKind::OffsetTime => SchemaFormat::OffsetTime,
        }
    }

    /// The engine's name of the logical type (`uuid`, `datetime`, `localdatetime`, ...).
    pub(crate) fn engine_name(self) -> &'static str {
        match self.temporal_kind() {
            Some(kind) => kind.name(),
            None => "uuid",
        }
    }

    /// Whether a value is of the logical type the format declares (a `uuid` for `uuid`, a
    /// temporal value of the format's type).
    pub(crate) fn admits(self, value: &ElementProperty) -> bool {
        match (self, value) {
            (SchemaFormat::Uuid, ElementProperty::Uuid(_)) => true,
            (_, ElementProperty::Temporal(temporal)) => {
                self.temporal_kind() == Some(temporal.kind())
            }
            _ => false,
        }
    }
}

/// The vector declaration of an `array` node: `{"type": "array", "format": "vector",
/// "element": "i8", "dimension": 768, "metric": "cosine"}` (book page "Vectors").
///
/// `format: "vector"` keeps the schema a strict subset of JSON Schema with custom formats (as
/// the dates do on `string`): another validator ignores the unknown format and the custom
/// keywords and sees an array, which is what a vector exports as. `element` (default `f32`) and
/// `dimension` (default: any) are checked exactly; `metric` is an annotation for the search
/// steps. `embedder` (a top-level property of a label only) records the model identity of the
/// embedder that produced the vectors (`"hash-ngram/1"`; an
/// [`EmbedderInfo::model`](crate::vector::EmbedderInfo::model), never a path or a registry name):
/// `embed_property` fills it in and checks it, and a text query
/// (`nearest("emb", "graph indexes", 5)`) embeds the text with the registered embedder of that
/// model. A plain write of a vector checks only `element` and `dimension`. `items` is not
/// allowed next to it. A list of numbers written to the property in mode
/// `open`/`closed` is coerced to a vector of the declared element, and a vector of another
/// element is quantized to it: a property can be stored quantized from the start.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct VectorSchema {
    /// `element`: the element kind of the stored vectors.
    pub element: VectorElement,
    /// `dimension`: the number of components; `None` admits any.
    pub dimension: Option<u32>,
    /// `metric`: an annotation, the metric the vectors are meant to be compared with.
    pub metric: Option<VectorMetric>,
    /// `embedder`: the model identity of the embedder that produced the vectors
    /// ([`EmbedderInfo::model`](crate::vector::EmbedderInfo::model)), on a label's top-level
    /// property only. Not checked by a plain write; `embed_property` and the text queries of
    /// `nearest`/`similarity` read it.
    pub embedder: Option<String>,
}

impl VectorSchema {
    /// A declaration of vectors of `element`, any dimension, no metric.
    pub fn new(element: VectorElement) -> Self {
        VectorSchema {
            element,
            dimension: None,
            metric: None,
            embedder: None,
        }
    }

    /// The same declaration with the model identity of its embedder (`"hash-ngram/1"`).
    pub fn with_embedder(mut self, model: impl Into<String>) -> Self {
        self.embedder = Some(model.into());
        self
    }

    /// The same declaration with a dimension.
    pub fn with_dimension(mut self, dimension: u32) -> Self {
        self.dimension = Some(dimension);
        self
    }

    /// The same declaration with a metric annotation.
    pub fn with_metric(mut self, metric: VectorMetric) -> Self {
        self.metric = Some(metric);
        self
    }

    /// Whether a vector has the declared element and dimension.
    pub(crate) fn admits(&self, vector: &Vector) -> bool {
        vector.element() == self.element
            && self
                .dimension
                .is_none_or(|dimension| vector.dimension() == dimension as usize)
    }

    /// The type name of a diagnostic: `vector<i8>`, `vector<i8, 768>`.
    pub(crate) fn type_name(&self) -> String {
        match self.dimension {
            Some(dimension) => format!("vector<{}, {dimension}>", self.element),
            None => format!("vector<{}>", self.element),
        }
    }
}

/// The assertion keywords of a node besides `type`: each one restricts the values of the kind
/// it applies to and ignores the others (JSON Schema semantics): numbers for
/// `minimum`/`maximum`/`exclusive*`, strings for `minLength`/`maxLength`/`pattern`, arrays for
/// `minItems`/`maxItems`, the temporal value of the node's `format` for `formatMinimum`/
/// `formatMaximum`/`formatExclusive*` (the keywords of JSON Schema's format-comparison
/// proposal, which ajv-formats implements), every value for `enum`/`const`.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Constraints {
    /// `minimum`: inclusive lower bound of a number.
    pub minimum: Option<Number>,
    /// `maximum`: inclusive upper bound of a number.
    pub maximum: Option<Number>,
    /// `exclusiveMinimum`: exclusive lower bound of a number.
    pub exclusive_minimum: Option<Number>,
    /// `exclusiveMaximum`: exclusive upper bound of a number.
    pub exclusive_maximum: Option<Number>,
    /// `minLength`: minimum number of characters (code points) of a string.
    pub min_length: Option<u64>,
    /// `maxLength`: maximum number of characters (code points) of a string.
    pub max_length: Option<u64>,
    /// `pattern`: a regular expression a string must match (unanchored, see
    /// [`SchemaPattern`]).
    pub pattern: Option<SchemaPattern>,
    /// `minItems`: minimum number of items of an array.
    pub min_items: Option<u64>,
    /// `maxItems`: maximum number of items of an array.
    pub max_items: Option<u64>,
    /// `enum`: the value must equal one of these (JSON values; a `uuid` compares as its
    /// canonical string, `1` equals `1.0`).
    pub enum_values: Option<Vec<ElementProperty>>,
    /// `const`: the value must equal this one.
    pub const_value: Option<ElementProperty>,
    /// `formatMinimum`: inclusive lower bound of a temporal value, of the type the node's
    /// `format` declares (a `datetime` compares by its instant, a `duration` by its length).
    pub format_minimum: Option<Temporal>,
    /// `formatMaximum`: inclusive upper bound of a temporal value.
    pub format_maximum: Option<Temporal>,
    /// `formatExclusiveMinimum`: exclusive lower bound of a temporal value.
    pub format_exclusive_minimum: Option<Temporal>,
    /// `formatExclusiveMaximum`: exclusive upper bound of a temporal value.
    pub format_exclusive_maximum: Option<Temporal>,
}

impl Constraints {
    /// `true` when no keyword is set.
    pub fn is_empty(&self) -> bool {
        *self == Constraints::default()
    }
}

/// The annotation keywords of a node, plus the two create-time keywords `default` and
/// `defaultFunction`: stored, round-tripped, shown by the renderers, and **never** used for
/// validation.
///
/// # `default` and `defaultFunction`: applied when an element is created
///
/// [`default`](Self::default) has SQL `DEFAULT` semantics on a label's top-level properties:
/// when a vertex or edge is *created* (`add_v`/`add_e`, `mergeV`/`mergeE` on create,
/// [`GraphStorage::add_vertex`](crate::storage::GraphStorage::add_vertex) and its `_with` form)
/// in mode `open` or `closed` without a property whose schema declares a `default`, the default
/// is stored before the element is coerced and validated (so it satisfies `required`). It is
/// never substituted on read, never re-applied when the schema changes, and a new or changed
/// default fills no existing element (no backfill). `default: null` is never stored (a pasted
/// Pydantic schema writes it on every optional field); an explicitly written `null` stays
/// `null`. Imports, change sets, replay and the storage primitive
/// [`GraphStorage::insert_vertex`](crate::storage::GraphStorage::insert_vertex) store exactly
/// what they are given. A `default` below the top level (inside an object value) stays an
/// annotation.
///
/// [`default_function`](Self::default_function) (`defaultFunction`) computes the value instead:
/// a new one for every created element (`uuidv4`, `uuidv7`), or the unit's start time, one
/// value for the whole unit (`now`, `today`; see [`DefaultFunction`]). A property has at most
/// one of the two, and a top-level default must be valid for its property (both checked when
/// the schema is set: `now` needs a property admitting a `date-time`, `today` a `date`).
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Annotations {
    /// `title`.
    pub title: Option<String>,
    /// `description`.
    pub description: Option<String>,
    /// `$comment`.
    pub comment: Option<String>,
    /// `examples`.
    pub examples: Option<Vec<ElementProperty>>,
    /// `deprecated`.
    pub deprecated: Option<bool>,
    /// `default`: stored when an element is created without the property (see the type docs).
    pub default: Option<ElementProperty>,
    /// `defaultFunction`: computes the value stored when an element is created without the
    /// property (see the type docs).
    pub default_function: Option<DefaultFunction>,
}

/// The `defaultFunction` keyword: a value computed for every element created without the
/// property (see [`Annotations`]). The generated value is stored as a plain value (the change
/// set and the journal record it), so replay is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DefaultFunction {
    /// `uuidv4`: a random RFC 9562 version-4 UUID, new for every element.
    Uuidv4,
    /// `uuidv7`: a time-ordered RFC 9562 version-7 UUID (48 bits of Unix milliseconds, then
    /// random bits; increasing within a process), new for every element.
    Uuidv7,
    /// `now`: the `datetime` of the unit's start ([`CreateContext::now`]), the same value for
    /// every element created in one unit (PostgreSQL `now()`: the start of the transaction).
    /// Valid only on a property admitting a `date-time`.
    Now,
    /// `today`: the UTC `date` of [`Self::Now`]'s time. Valid only on a property admitting a
    /// `date`.
    Today,
}

/// What a create stores for an absent property: [`PropertySchema::create_default`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum CreateDefault<'a> {
    /// `default` (never `null`).
    Value(&'a ElementProperty),
    /// `defaultFunction`.
    Function(DefaultFunction),
}

impl CreateDefault<'_> {
    /// The value to store: the default, or a new value of the function at `context`'s time.
    pub(crate) fn value(self, context: &CreateContext) -> ElementProperty {
        match self {
            CreateDefault::Value(value) => value.clone(),
            CreateDefault::Function(function) => function.generate(context),
        }
    }
}

impl DefaultFunction {
    /// The keyword value: `uuidv4`, `uuidv7`, `now`, `today`.
    pub fn as_str(self) -> &'static str {
        match self {
            DefaultFunction::Uuidv4 => "uuidv4",
            DefaultFunction::Uuidv7 => "uuidv7",
            DefaultFunction::Now => "now",
            DefaultFunction::Today => "today",
        }
    }

    /// Parses a keyword value (case-sensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "uuidv4" => Some(DefaultFunction::Uuidv4),
            "uuidv7" => Some(DefaultFunction::Uuidv7),
            "now" => Some(DefaultFunction::Now),
            "today" => Some(DefaultFunction::Today),
            _ => None,
        }
    }

    /// A new value of this function; `now`/`today` take `context`'s time.
    pub(crate) fn generate(self, context: &CreateContext) -> ElementProperty {
        match self {
            DefaultFunction::Uuidv4 => ElementProperty::Uuid(crate::value::uuid::new_v4()),
            DefaultFunction::Uuidv7 => ElementProperty::Uuid(crate::value::uuid::new_v7()),
            DefaultFunction::Now => ElementProperty::Temporal(Temporal::DateTime(context.now())),
            DefaultFunction::Today => ElementProperty::Temporal(Temporal::Date(context.today())),
        }
    }

    /// The temporal format a property must admit for this function (`now`: `date-time`,
    /// `today`: `date`); `None` for the uuid functions.
    pub(crate) fn temporal_format(self) -> Option<SchemaFormat> {
        match self {
            DefaultFunction::Uuidv4 | DefaultFunction::Uuidv7 => None,
            DefaultFunction::Now => Some(SchemaFormat::DateTime),
            DefaultFunction::Today => Some(SchemaFormat::Date),
        }
    }
}

impl Annotations {
    /// `true` when no annotation is set.
    pub fn is_empty(&self) -> bool {
        *self == Annotations::default()
    }
}

/// One JSON Schema node of the supported subset: what a property (or a label's whole property
/// map) may hold.
///
/// Every keyword of the node must hold for a value to be valid, as in JSON Schema:
///
/// - [`types`](Self::types) (`type`): the value's kind must be one of them; `None` (no `type`
///   keyword) admits every kind. `["integer", "null"]` is a nullable integer.
/// - [`format`](Self::format): `uuid`, `date-time`, `date`, `local-date-time`, `time` and
///   `duration` turn `string` into the logical `uuid`, `datetime`, `date`, `localdatetime`,
///   `time` and `duration` types ([`SchemaFormat`]).
/// - [`items`](Self::items): every item of an array value; `None` admits any items.
/// - [`properties`](Self::properties), [`required`](Self::required),
///   [`additional_properties`](Self::additional_properties): the keys of an object value.
///   `required` is independent of nullability: a required key must be present (it may hold
///   `null` when its own schema admits `null`), an optional key may be absent.
///   `additional_properties` `None` takes the graph's default (`Open` mode: `true`, `Closed`:
///   `false`).
/// - [`any_of`](Self::any_of): at least one member must accept the value.
/// - [`constraints`](Self::constraints) and [`annotations`](Self::annotations).
///
/// Build one with the constructors ([`Self::integer`], [`Self::object`], ...) and the `with_*`
/// methods, or load it as part of a [`GraphSchema`](crate::graph::schema::GraphSchema).
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct PropertySchema {
    /// `type`: the admitted kinds; `None` admits every kind.
    pub types: Option<Vec<JsonType>>,
    /// `format`: the logical type of a `string` node ([`SchemaFormat`]).
    pub format: Option<SchemaFormat>,
    /// `items`: the schema of every array item.
    pub items: Option<Box<PropertySchema>>,
    /// `format: "vector"` with `element`/`dimension`/`metric`: the node declares vectors
    /// ([`VectorSchema`]; its `type` is `array`, [`Self::format`] stays `None`).
    pub vector: Option<VectorSchema>,
    /// `properties`: the declared keys of an object.
    pub properties: IndexMap<String, PropertySchema>,
    /// `required`: the keys an object must have.
    pub required: Vec<String>,
    /// `additionalProperties` (`true`/`false`); `None` = the mode's default.
    pub additional_properties: Option<bool>,
    /// `anyOf`: the value must be valid against at least one member.
    pub any_of: Option<Vec<PropertySchema>>,
    /// The assertion keywords besides `type`.
    pub constraints: Option<Box<Constraints>>,
    /// The annotation keywords (`title`, `description`, ...) and the create-time keywords
    /// `default`/`defaultFunction`.
    pub annotations: Option<Box<Annotations>>,
}

impl PropertySchema {
    /// `{}`: admits every value.
    pub fn any() -> Self {
        Self::default()
    }

    /// `{"type": <ty>}`.
    pub fn of(ty: JsonType) -> Self {
        Self {
            types: Some(vec![ty]),
            ..Self::default()
        }
    }

    /// `{"type": [..]}`: a type array (`["integer", "null"]`).
    pub fn of_types(types: &[JsonType]) -> Self {
        Self {
            types: Some(types.to_vec()),
            ..Self::default()
        }
    }

    /// `{"type": "null"}`.
    pub fn null() -> Self {
        Self::of(JsonType::Null)
    }

    /// `{"type": "boolean"}`.
    pub fn boolean() -> Self {
        Self::of(JsonType::Boolean)
    }

    /// `{"type": "integer"}`: an `int64`.
    pub fn integer() -> Self {
        Self::of(JsonType::Integer)
    }

    /// `{"type": "number"}`: a `float64`.
    pub fn number() -> Self {
        Self::of(JsonType::Number)
    }

    /// `{"type": "string"}`.
    pub fn string() -> Self {
        Self::of(JsonType::String)
    }

    /// `{"type": "string", "format": "uuid"}`: a `uuid`.
    pub fn uuid() -> Self {
        Self {
            format: Some(SchemaFormat::Uuid),
            ..Self::of(JsonType::String)
        }
    }

    /// `{"type": "string", "format": <format>}`: the logical type the format declares.
    pub fn with_string_format(format: SchemaFormat) -> Self {
        Self {
            format: Some(format),
            ..Self::of(JsonType::String)
        }
    }

    /// `{"type": "string", "format": "date-time"}`: a `datetime`.
    pub fn datetime() -> Self {
        Self::with_string_format(SchemaFormat::DateTime)
    }

    /// `{"type": "string", "format": "date"}`: a `date`.
    pub fn date() -> Self {
        Self::with_string_format(SchemaFormat::Date)
    }

    /// `{"type": "string", "format": "local-date-time"}`: a `localdatetime`.
    pub fn local_datetime() -> Self {
        Self::with_string_format(SchemaFormat::LocalDateTime)
    }

    /// `{"type": "string", "format": "time"}`: a `time` (a local time of day).
    pub fn time() -> Self {
        Self::with_string_format(SchemaFormat::Time)
    }

    /// `{"type": "string", "format": "offset-time"}`: an `offsettime` (a time of day with its
    /// UTC offset).
    pub fn offset_time() -> Self {
        Self::with_string_format(SchemaFormat::OffsetTime)
    }

    /// `{"type": "string", "format": "duration"}`: a `duration`.
    pub fn duration() -> Self {
        Self::with_string_format(SchemaFormat::Duration)
    }

    /// The node of a temporal type (`{"type": "string", "format": ..}`).
    pub fn temporal(kind: TemporalKind) -> Self {
        Self::with_string_format(SchemaFormat::of_temporal(kind))
    }

    /// `{"type": "array", "format": "vector", "element": .., "dimension": ..}`: a vector of
    /// `element` with `dimension` components (`None`: any dimension).
    pub fn vector(element: VectorElement, dimension: Option<u32>) -> Self {
        Self {
            vector: Some(VectorSchema {
                element,
                dimension,
                metric: None,
                embedder: None,
            }),
            ..Self::of(JsonType::Array)
        }
    }

    /// `{"type": "array", "items": <items>}`.
    pub fn array(items: PropertySchema) -> Self {
        Self {
            items: Some(Box::new(items)),
            ..Self::of(JsonType::Array)
        }
    }

    /// `{"type": "object"}` without declared properties.
    pub fn object() -> Self {
        Self::of(JsonType::Object)
    }

    /// `{"anyOf": [..]}`.
    pub fn any_of(members: Vec<PropertySchema>) -> Self {
        Self {
            any_of: Some(members),
            ..Self::default()
        }
    }

    /// Admits `null` too: adds `null` to `type`, or a `{"type": "null"}` member to `anyOf`
    /// (a node with neither admits `null` already).
    pub fn nullable(mut self) -> Self {
        if let Some(types) = &mut self.types {
            if !types.contains(&JsonType::Null) {
                types.push(JsonType::Null);
            }
        } else if let Some(members) = &mut self.any_of
            && !members.iter().any(PropertySchema::admits_null)
        {
            members.push(PropertySchema::null());
        }
        self
    }

    /// Declares an optional object key.
    pub fn with_property(mut self, name: impl Into<String>, schema: PropertySchema) -> Self {
        self.properties.insert(name.into(), schema);
        self
    }

    /// Declares a required object key.
    pub fn with_required_property(
        mut self,
        name: impl Into<String>,
        schema: PropertySchema,
    ) -> Self {
        let name = name.into();
        if !self.required.contains(&name) {
            self.required.push(name.clone());
        }
        self.properties.insert(name, schema);
        self
    }

    /// Sets `additionalProperties`.
    pub fn with_additional_properties(mut self, allowed: bool) -> Self {
        self.additional_properties = Some(allowed);
        self
    }

    /// Sets the assertion keywords.
    pub fn with_constraints(mut self, constraints: Constraints) -> Self {
        self.constraints = (!constraints.is_empty()).then(|| Box::new(constraints));
        self
    }

    /// Changes the assertion keywords in place.
    pub fn with_constraint(mut self, change: impl FnOnce(&mut Constraints)) -> Self {
        let mut constraints = self.constraints.take().map(|c| *c).unwrap_or_default();
        change(&mut constraints);
        self.with_constraints(constraints)
    }

    /// Changes the annotations in place.
    pub fn with_annotation(mut self, change: impl FnOnce(&mut Annotations)) -> Self {
        let mut annotations = self.annotations.take().map(|a| *a).unwrap_or_default();
        change(&mut annotations);
        self.annotations = (!annotations.is_empty()).then(|| Box::new(annotations));
        self
    }

    /// Sets `description`.
    pub fn with_description(self, description: impl Into<String>) -> Self {
        let description = description.into();
        self.with_annotation(|a| a.description = Some(description))
    }

    /// Sets `default`: the value stored when an element is created without this (top-level)
    /// property (see [`Annotations`]).
    pub fn with_default(self, value: ElementProperty) -> Self {
        self.with_annotation(|a| a.default = Some(value))
    }

    /// Sets `defaultFunction`: the value computed when an element is created without this
    /// (top-level) property (see [`Annotations`]).
    pub fn with_default_function(self, function: DefaultFunction) -> Self {
        self.with_annotation(|a| a.default_function = Some(function))
    }

    /// The `default`/`defaultFunction` of this node: what a create stores when the property is
    /// absent. `default: null` counts as none (never stored).
    pub(crate) fn create_default(&self) -> Option<CreateDefault<'_>> {
        let annotations = self.annotations.as_deref()?;
        if let Some(function) = annotations.default_function {
            return Some(CreateDefault::Function(function));
        }
        match &annotations.default {
            None | Some(ElementProperty::Null) => None,
            Some(value) => Some(CreateDefault::Value(value)),
        }
    }

    /// Whether the node admits a `null` value by its type (`type` contains `null`, no `type`,
    /// or an `anyOf` member does).
    pub fn admits_null(&self) -> bool {
        self.admits_kind(JsonType::Null)
    }

    /// Whether a value of `kind` can pass this node's type checks (through `anyOf` too).
    pub(crate) fn admits_kind(&self, kind: JsonType) -> bool {
        let by_type = self
            .types
            .as_ref()
            .is_none_or(|types| types.contains(&kind));
        let by_any_of = self
            .any_of
            .as_ref()
            .is_none_or(|members| members.iter().any(|m| m.admits_kind(kind)));
        by_type && by_any_of
    }

    /// The kinds the node declares (`(kind, format)`: a `string` with its `format`, so a `uuid`
    /// or a temporal type apart from a plain string), through `anyOf`; `None` when it declares
    /// no type restriction (no `type` and no `anyOf`, or an `anyOf` member without one).
    pub(crate) fn declared_kinds(&self) -> Option<Vec<(JsonType, Option<SchemaFormat>)>> {
        if let Some(types) = &self.types {
            return Some(
                types
                    .iter()
                    .map(|t| {
                        (
                            *t,
                            if *t == JsonType::String {
                                self.format
                            } else {
                                None
                            },
                        )
                    })
                    .collect(),
            );
        }
        let members = self.any_of.as_ref()?;
        let mut kinds = Vec::new();
        for member in members {
            kinds.extend(member.declared_kinds()?);
        }
        Some(kinds)
    }

    /// Whether an object value may hold keys this node does not declare: the node's own
    /// `additionalProperties`, else `mode_default` (the mode's default) for a node typed
    /// `object`, else `true` (a node without `type: object`, such as an `anyOf` wrapper, puts no
    /// constraint on the keys; its members do).
    pub fn admits_additional_properties(&self, mode_default: bool) -> bool {
        match self.additional_properties {
            Some(explicit) => explicit,
            None if self
                .types
                .as_ref()
                .is_some_and(|types| types.contains(&JsonType::Object)) =>
            {
                mode_default
            }
            None => true,
        }
    }

    /// The `format` of a node with `string` in its own `type`: the logical type its strings are
    /// (`uuid`, `datetime`, ...); `None` for a plain string or no string at all.
    pub(crate) fn string_format(&self) -> Option<SchemaFormat> {
        self.format.filter(|_| {
            self.types
                .as_ref()
                .is_some_and(|types| types.contains(&JsonType::String))
        })
    }

    /// The node's type in the engine's canonical spelling, for diagnostics: `int64`,
    /// `array<string>`, `object`, `union<int64|null>`, `any` (no type restriction).
    pub fn type_name(&self) -> String {
        let own: Option<Vec<String>> = self.types.as_ref().map(|types| {
            types
                .iter()
                .map(|ty| match ty {
                    JsonType::Array => match (&self.vector, &self.items) {
                        (Some(vector), _) => vector.type_name(),
                        (None, Some(items)) => format!("array<{}>", items.type_name()),
                        (None, None) => "array".to_string(),
                    },
                    other => other.engine_name(self.format).to_string(),
                })
                .collect()
        });
        let members: Option<Vec<String>> = self
            .any_of
            .as_ref()
            .map(|members| members.iter().map(PropertySchema::type_name).collect());
        let names = match (own, members) {
            (Some(own), _) => own,
            (None, Some(members)) => members,
            (None, None) => return "any".to_string(),
        };
        if names.len() == 1 {
            names.into_iter().next().unwrap_or_default()
        } else {
            format!("union<{}>", names.join("|"))
        }
    }

    /// [`Self::type_name`] without the `null` alternative (`int64` for `["integer", "null"]`):
    /// what diagnostics name as the expected type, since whether `null` is admitted is not what
    /// a mismatch of another value is about.
    pub fn non_null_type_name(&self) -> String {
        if let Some(types) = &self.types
            && types.len() > 1
            && types.contains(&JsonType::Null)
        {
            let mut copy = self.clone();
            copy.types = Some(
                types
                    .iter()
                    .copied()
                    .filter(|t| *t != JsonType::Null)
                    .collect(),
            );
            return copy.type_name();
        }
        if self.types.is_none()
            && let Some(members) = &self.any_of
        {
            let kept: Vec<&PropertySchema> = members
                .iter()
                .filter(|m| m.types.as_deref() != Some(&[JsonType::Null]))
                .collect();
            if kept.len() < members.len() && !kept.is_empty() {
                return match kept.as_slice() {
                    [only] => only.type_name(),
                    many => format!(
                        "union<{}>",
                        many.iter()
                            .map(|m| m.type_name())
                            .collect::<Vec<_>>()
                            .join("|")
                    ),
                };
            }
        }
        self.type_name()
    }

    /// The annotations, if any.
    pub fn annotations(&self) -> Option<&Annotations> {
        self.annotations.as_deref()
    }

    /// The constraints, if any.
    pub fn constraints(&self) -> Option<&Constraints> {
        self.constraints.as_deref()
    }

    /// Whether `key` is in `required`.
    pub fn is_required(&self, key: &str) -> bool {
        self.required.iter().any(|name| name == key)
    }
}
