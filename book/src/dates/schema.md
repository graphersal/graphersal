# Dates in a Schema

A [schema](../users_guide/schema_enforcement.md) declares a date property as a JSON Schema
`string` with a `format`, and bounds it with the `format*` keywords. The full reference (every
keyword, the errors and why, inference and diffs) is on the Schemas page; this page is the
overview.

| `format` | Type | Example value |
|---|---|---|
| `date-time` | `datetime` | `2024-05-01T09:30:00+02:00` |
| `local-date-time` | `localdatetime` | `2024-05-01T09:30:00` (a Graphersal name: JSON Schema has none) |
| `date` | `date` | `2024-05-01` |
| `time` | `time` | `09:30:00` (local; JSON Schema's own `time` requires an offset) |
| `offset-time` | `offsettime` | `09:30:00+02:00` (a Graphersal name: JSON Schema's `time` is this form, but Graphersal's `time` is local) |
| `duration` | `duration` | `P1Y2M3DT4H` |

JSON Schema validators treat `format` as an annotation and ignore one they do not know, so a
schema with these formats stays usable with other tools; Pydantic writes `date-time`, `date`,
`time` and `duration` for `datetime`, `date`, `time` and `timedelta` fields.

## Coercion on write

In `open` and `closed` mode a **string** written to a declared date property becomes the date:
the declaration says the property is a date, so nothing is guessed. The text must be one the
type reads ([Text Formats](text_formats.md)); a `date-time` text needs its UTC offset or a zone
(`2024-05-01T09:30` is refused, never read as UTC). A date value is stored as it is; a value of
another date type is refused (a `date` is not a `datetime`). Reads never convert: a string stored
before the schema was set does not conform to the format, and `set_schema` reports it.

```rhai,graph=empty,script
g.set_schema(#{vertices: #{event: #{schema: #{type: "object", properties: #{
    at: #{type: "string", format: "date-time"},
    on: #{type: "string", format: "date", formatMinimum: "2000-01-01"}
}}}}}, SchemaMode.open);
g.add_v("event").property("at", "2024-05-01T09:30:00+02:00").property("on", date("2024-05-01")).iterate();
g.v().has_label("event").values("at").next().offset   // 7200: a datetime, not a string
```

```rhai,graph=empty
{
    g.set_schema(#{vertices: #{event: #{schema: #{type: "object", properties: #{
        at: #{type: "string", format: "date-time"}
    }}}}}, SchemaMode.open);
    g.add_v("event").property("at", "2024-05-01T09:30").iterate()
}   // refused: the text has no offset
```

## Bounds

`formatMinimum`, `formatMaximum`, `formatExclusiveMinimum` and `formatExclusiveMaximum` (the
keywords of JSON Schema's format-comparison proposal, which ajv-formats implements) bound a date
property. Their value is the text of a value of the node's type, and a value is compared as
[the type compares](comparing.md): a `datetime` by its instant, a `duration` by its length.
`minimum` and `maximum` stay number keywords.

```json
{ "type": "string", "format": "date-time", "formatMinimum": "2020-01-01T00:00:00Z" }
{ "type": "string", "format": "duration", "formatExclusiveMaximum": "P1D" }
```

## Inference

`infer_schema()` reports a stored date with its format (`{"type": "string", "format":
"date-time"}`), and a position that holds strings and dates as an `anyOf`, so freezing the
structure you have (`g.set_schema(g.infer_schema(), SchemaMode.closed)`) keeps the dates dates.
The playground's schema editor offers the date formats as types and their bounds as constraints.
