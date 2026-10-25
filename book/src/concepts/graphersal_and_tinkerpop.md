# Graphersal and TinkerPop

[Apache TinkerPop&trade;](https://tinkerpop.apache.org) defines the property graph model and the
Gremlin language that many graph databases implement. Graphersal is an independent implementation
in Rust: it does not use TinkerPop's Java code, and it is not a Gremlin Server, so a TinkerPop
driver does not connect to it. What it shares with TinkerPop is the language and its meaning.

## Same meaning, checked by TinkerPop's own tests

The rule is simple: a step that Graphersal implements behaves as the TinkerPop specification
says, unless a difference is documented. The specification is TinkerPop's own test suite, a set of
Gherkin scenarios (TinkerPop 3.8.2) that runs with every `cargo test` of Graphersal. A scenario
that passes once is protected from regressing, and the number of supported steps whose results
are wrong is kept at zero. The current numbers are on
[TinkerPop Compliance](../for_developers/tinkerpop_compliance.md).

Not every step exists yet. An unimplemented step is an error that names it, never a silently
different result; the gaps are listed in [Current Limitations](../users_guide/current_limitations.md).

## Deliberate differences

A few behaviours differ on purpose, each for a reason that is written down. The most visible ones:

- **Every traversal is a transaction.** A failing traversal leaves nothing behind
  ([Transactions](../users_guide/transactions.md)).
- **Ids are strings**, so they sort as text ([Predicates](../users_guide/predicates.md#deviations)).
- **One integer and one float type** (`int64`, `float64`) instead of Java's number tower.
- **A removed element stays removed**: reading it through an old reference finds nothing, and
  writing to it is an error ([Dropping Elements](../users_guide/dropping_elements.md)).
- **No `match()`, `io()` or graph-computer algorithms**; files are read and written by the API and
  the command line instead.

Every difference, with its page, is in [TinkerPop Deviations](../for_developers/tinkerpop_deviations.md).

## The query text

TinkerPop's reference language is Gremlin embedded in Groovy or Java. Graphersal's text form is
Gremlin embedded in [Rhai](https://rhai.rs), a small scripting language for Rust. A Groovy line
usually pastes as it is; the differences are small:

| Gremlin Groovy | Graphersal |
|---|---|
| `'single quotes'` | `"double quotes"` |
| `order().by('age', desc)` | `order().by("age", Order.desc)` (token classes are written out) |
| `out()` inside `where(...)`, `repeat(...)` | `__.out()` (anonymous traversals always start with `__.`) |
| `[name: 'ann']` (a map) | `#{name: "ann"}` |
| `def x = ...` | `let x = ...;` |

The full copy-paste table is in [Repeated Labels: select with Pop](../users_guide/select_pop.md#copy-paste-cheat-sheet).
Every TinkerPop step also has a snake_case name (`has_label`, `out_e`) next to its camelCase one
(`hasLabel`, `outE`), and the Rust API uses the snake_case names. Graphersal's own additions
below are snake_case only.

## Additions

Graphersal adds what an embedded engine needs: schemas, saved queries in the graph, path keys into
nested values (`jpath`), multi-label vertices, change capture, permissions and resource limits, a
durable Store, and `profile()` with memory figures. These are extensions: they do not change what
a TinkerPop query means.
