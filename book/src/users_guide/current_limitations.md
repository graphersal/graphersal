# Current Limitations

What Graphersal 0.2.0 does not do, in one place. Each item links to the page with the details;
the deliberate differences from Apache TinkerPop are listed one by one in
[TinkerPop Deviations](../for_developers/tinkerpop_deviations.md), and the steps and scenarios that
are not implemented yet in [TinkerPop Compliance](../for_developers/tinkerpop_compliance.md).

## Data model

- **Dates keep an offset, not a zone.** A `datetime` stores its UTC offset; a time zone is an
  argument of the operations that need one, never part of a value, and the zone database is the
  one bundled with the release ([Time Zones](../dates/time_zones.md)). GraphSON's `gx:Year`,
  `gx:YearMonth`, `gx:MonthDay`, `gx:OffsetTime` and `gx:ZoneOffset` are skipped on import
  ([GraphSON](graphson.md)).
- **One value per property key.** Only `Cardinality.single` is written; store several values as
  one array ([Upserts](upserts.md)). There are no meta-properties: a GraphSON import folds them
  into the `_meta` property.
- **Ids are strings.** A numeric id becomes its text, and ids order as strings (`"10"` before
  `"9"`; see [Id Arguments](id_arguments.md) and [Predicates](predicates.md)).
- **Edges have one label**; vertices have a label set ([Multi-Label Vertices](multi_label_vertices.md)).
- **Property indexes are declared, top-level and per label.** A `has()` on a property reads an
  index only when one is declared with `g.create_index(..)` for the start step's label and the key
  ([Property Indexes](property_indexes.md)); nested values (jpath keys), `has()` without
  `has_label()` and `order()` scan.
- **Vector search is exact, without an index.** `nearest()` scores every incoming element (about
  13 ms per 100 000 vectors of 384 f32 components); there is no approximate (ANN, HNSW) vector
  index yet. Filter before the search to score fewer ([Vectors](vectors.md#exact-search-nearest)).
- **The graph lives in memory.** A [Store](../persistence/store.md) makes it durable, but the
  whole graph is loaded into memory when the store is opened.

## Query language

- **The DSL is Rhai, not Groovy.** Queries read like Gremlin in either spelling (`hasLabel` or
  `has_label`), but maps are written `#{..}`, child traversals need `__.` (`where(__.out())`) and
  tokens need their class (`Order.desc`, `Scope.local`). A Gremlin text query is not parsed as it
  is.
- **No `withStrategies()` / `withoutStrategies()`.** Execution options are set with
  `g.with(key, value)` ([Execution Options Reference](execution_options_reference.md)).
- **No `g.tx()`.** Every traversal is one unit of work; a host can run a whole script as one unit
  ([Transactions](transactions.md)).
- **GQL reads only.** [`g.gql()`](../gql/index.md) runs ISO GQL read queries (Cypher spellings
  as aliases) as a start step; composite queries (`UNION`, `NEXT`), subqueries, `FOR`, shortest
  paths and quantified parenthesized paths are not supported yet, and GQL writes are refused (use
  the Gremlin mutation steps).
- **Saved queries only read** and are called at the start of a traversal (`g.query(..)`), not on
  incoming traversers ([Saved Queries](saved_queries.md)).

## Schema

- No map type (`additionalProperties` with a schema), no `oneOf`/`allOf`/`not`, no tuples, and no
  `format` other than `uuid`, the date and time formats (`date-time`, `date`, `local-date-time`,
  `time`, `duration`) and `vector` ([Schemas](schema_enforcement.md#what-is-an-error-and-why)).
- `default`/`defaultFunction` fill only a label's top-level properties, only when an element is
  created, and only in mode `open`/`closed`; `defaultFunction` knows `uuidv4`, `uuidv7`, `now`
  and `today`; see
  [Defaults on create](schema_enforcement.md#defaults-on-create).

## Security and serving

- Read security per element or label does not exist yet; an authorizer decides per request kind
  and label as the plan states it ([Permissions](permissions.md)).
- The dev server (`graphersal --server`, also MCP and AI Chat) is a local development tool:
  unstable, bound to the loopback, without authentication
  ([Dev Server and MCP](../persistence/dev_server.md)).
- **Not a Gremlin Server.** There is no network protocol for remote Gremlin drivers
  (gremlin-python, the Java driver); Graphersal is embedded as a library, the CLI or the Python
  package ([Graphersal and TinkerPop](../concepts/graphersal_and_tinkerpop.md)).

## Concurrency and packaging

- **Readers wait for the writer.** A graph has one writer at a time and reads wait while a unit
  writes; a layered storage with snapshot reads is planned ([Transactions](transactions.md#not-covered-yet)).
- **No npm package.** The WebAssembly engine runs the [web playground](web_playground.md); its
  JavaScript API is internal to it, and a published npm package is planned.

## Other

- Compression rules cover strings at a path of names; values inside arrays are not compressed
  ([Compressed Properties](compressed_properties.md)).
- [Generated test data](generating_test_data.md) is English only.
