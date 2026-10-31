# Vector Rules

A **vector rule** keeps a vector property in step with the text it is computed from. The schema
declares it on the vector property: beside `embedder` (the model) it names the source property
`from` of the same label. From then on the engine embeds the text of `from` into the vector at
the end of every unit that writes it, in the same unit: the text and its vector commit together,
or neither does.

```json
"emb": {"type": "array", "format": "vector", "element": "f32", "dimension": 256,
        "metric": "cosine", "embedder": "hash-ngram/1", "from": "text"}
```

The preview says what applying the rule fills (on the `modern` sample: the names of the four
people), and applying it embeds every name:

```rhai,graph=modern
g.validate_schema(#{vertices: #{person: #{schema: #{type: "object", properties: #{name: #{type: "string"}, emb: #{type: "array", format: "vector", embedder: "hash-ngram/1", from: "name"}}}}}})["fill"][0]["elements"]   // 4
g.patch_schema(#{vertices: #{person: #{schema: #{type: "object", properties: #{name: #{type: "string"}, emb: #{type: "array", format: "vector", embedder: "hash-ngram/1", from: "name"}}}}}})
```

The vectors are written when the writing unit ends: run a read in the next query. (The
playground and a script run as one unit see them in their next run.)

## The keyword

- `from` needs `format: "vector"` and `embedder` on the same node, and sits on a label's
  top-level property only (like `embedder`).
- `from` names a top-level property of the same label (1 to 128 bytes), never the vector's own
  key and never the vector of another rule of the label: rules are not chained. When the label
  declares `from`, its type must admit a string, or the rule would always remove the vector.
- `embedder` names the built-in `hash-ngram/1` or a model of the store, by its repository
  (`minishlab/potion-base-8M`) or its identity; the schema stores the identity of the store's
  current revision, and `get_schema` shows it. Any other model is refused when the schema is
  set, with the command that imports it (`graphersal embed-model import <repo> --into <store>`).
  An identity with a normalization override (`?normalize=false`) is refused too: a stored
  model is served as it was imported.
- An undeclared `dimension` is declared from the model; a declared one must be the model's.
- A storage without change capture (`StorageCapabilities::change_capture`) refuses rules
  (`Unsupported: vector rules`): the rule pass reads the unit's undo log.
- Inference never infers a rule.
- Two labels may each have a rule for the same key. For a vertex with several labels the FIRST
  label that declares a rule for the key wins (as for coercion).

## When the vector is written

At the end of the outermost unit (a query, a transaction, a whole-script unit, a schema change,
a live import), once per element, however often the unit wrote it:

1. Every element whose `from` the unit wrote, every element it created, and every vertex whose
   labels it changed (a label with a rule added) is looked at.
2. Its text is embedded, in ONE batch per model over the whole unit (chunks of the model's batch
   size).
3. The vector is written as an ordinary write of the unit: the change set, the write-ahead log
   and a rollback see it. Replay, recovery, loads and replicas never call a model; vectors travel
   as data.

Inside the unit a read of the vector after a write of `from` sees the old vector: the new one
appears when the outermost unit ends (`Execution::changes()` of an inner unit does not show it).
A whole script run as one unit (the playground, `eval_value_atomic`) therefore reads the new
vectors in its NEXT run.

**A source without text removes the vector**, in the same unit: `from` removed, set to `null`,
set to `""`, set to a value that is not a string, or a text in which the model finds nothing to
embed (`hash-ngram/1`: separators only).

A failing model, a cancelled execution or the memory ceiling (`memory.limit`, operation
`"vector rule fill"`) rolls the whole unit back: nothing is written
(`GraphError::VectorRuleFailed`, `ResourceLimitExceeded`).

## Direct writes are refused

The vector of a rule is computed, like a generated column of SQL. Writing or removing it
directly (`property("emb", ..)`, `add_v(..).property("emb", ..)`, GQL `SET`, a jpath into it, an
imported file that carries it) fails with `GraphError::DerivedProperty`, whose help names the
source to write instead. `apply_change_set` (the replication form) takes the vector as given,
and so do replay, recovery and loads. To write the vectors yourself, remove `from` from the
declaration: the vectors stay as plain data.

## First fill and model replacement

> **Warning:** adding a rule (or changing its `from`, `embedder`, `element` or `dimension`) over
> existing data embeds EVERY element of the label in the schema-changing unit, also those that
> hold a vector already. While it runs the WHOLE database is locked: every write and every read
> waits until it ends. `hash-ngram/1` embeds about 200,000 short texts per second; a Model2Vec
> model is measured at its load (a batch of 64 sentences). Run it at a quiet time. If it fails,
> nothing is written.

The same holds for a **model replacement** (`Store::import_model` with `replace`): the rules
naming the old revision switch to the new one and re-embed every element of their labels in the
replacing commit. A property whose vectors the old revision made WITHOUT a rule (an `embedder`
but no `from`) refuses the replace (`StoreFailure::ModelInUse`, naming it): there is no text to
re-embed. Add the rule first.

A first fill and a replacement write `vector_fill.started` before the first batch,
`vector_fill.done` after the commit (its `op` is the start record) or `vector_fill.failed`
after the rollback to the [system journal](../persistence/system_journal.md).
`Graph::vector_fill_progress()` (Rust) reports a running fill without taking the lock.

Removing a rule does nothing to the data: the vectors stay as plain values.

## The preview and the one warning

`validate_schema`, `validate_schema_patch` and `diff_schema` report the planned fill before
anything is applied: `fill` lists per rule the label, `from`, `to`, the model, the reason
(`rule_added`, `rule_changed`, `model_replaced`), the elements to embed, the vectors to remove,
the estimated time (`estimate_ms`: the embedding plus the schema check the apply runs under the
same lock, `scan_elements` / `scan_ms`) and the saved queries whose body writes the rule's
property directly (`direct_writes`: such a write is refused once the rule exists; remove it,
the queries are never rewritten); `fill_warning` is the ONE text every front end shows:

```text
Applying this schema fills vectors: 1,204,331 elements of doc.emb from doc.text with the model
minishlab/potion-multilingual-128M (about 1 min).
The whole database is LOCKED while this runs: every write and every read waits until it ends.
Run it at a quiet time. If it fails, nothing is written.
```

and, when saved queries write a filled property directly, a line per property such as
`Saved queries writing doc.emb directly fail once the rule exists (remove that write; they are
not rewritten): load_docs.`

The schema check of a change that keeps the mode (`open` or `closed`) reads only the elements of
the labels whose declaration changed: adding a rule to three labels of a graph of millions of
elements checks those three labels, not the whole graph. A change of the mode checks every
element. `validate_schema` runs the same check and measures it, so its `scan_ms` is the time the
apply's check takes; `diff_schema` reads no data: its counts come from the label statistics
("up to") and its check time from a fixed rate. A fill **needs confirmation** when its estimate
(the check included) is 1 s or more, or, without an estimate, from 10,000 elements or a check of
1 s or more:

- the playground's schema editor asks with a dialog "Apply and lock" / "Cancel" (below the
  threshold a plain note);
- the MCP tools `set_schema` / `patch_schema` refuse it with the text and "Repeat with
  confirm_lock: true" ([Dev Server](../persistence/dev_server.md));
- the CLI prints it as a `note:` before applying `--schema`;
- Python issues `graphersal.VectorFillWarning` before applying.

The diff lists a rule as `vector_rule_added` (compatible on a new property, incompatible when
the property was declared before: its direct writes are refused from then on),
`vector_rule_changed` (incompatible) or `vector_rule_removed` (compatible).

## Permissions

Setting a schema with a rule asks `Update` on the schema. The derived writes ask nothing of their
own (like create-time defaults), and the rule's model runs without `Execute` on the embedder.
