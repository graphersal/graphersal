# The Command Line

The `graphersal` binary runs Gremlin queries from a terminal. It has three modes: a one-shot
query runner for shell scripts, an interactive REPL, and a local server (next chapter).

Install it from a prebuilt binary or with `cargo install graphersal-cli`
([Installation](installation.md)), or build it in a clone of the repository:

```bash
cargo build -p graphersal-cli --release      # target/release/graphersal
```

## One-shot queries

Pass a query with `-e`; the result goes to stdout and nothing else does:

```text
$ graphersal -e 'g.v().has("name", "marko").out("knows").values("name")'
╭───────╮
│ value │
├───────┤
│ josh  │
│ vadas │
╰───────╯
```

Without `--graph` it runs on the TinkerPop "modern" sample. `--graph` also takes `empty`, `large`
(about 110,000 elements), `tree`, a GraphSON or GraphML file, a snapshot or a Store directory:

```bash
graphersal --graph people.json -e 'g.v().count()'
echo 'g.v().values("age").max().next()' | graphersal --graph modern     # the query from stdin
graphersal -e 'g.v().has_label("person").values("name")' --format json  # also markdown, mermaid, ...
```

A query that fails prints its diagnostic on stderr and exits with `1`, so a shell script or a CI
job can check a graph:

```bash
# fails (exit 1) when a server has no owner
graphersal --graph export.json -e 'g.v().has_label("server").has_not("owner").fail("a server without an owner")'
```

## The REPL

Without a query, `graphersal` starts an interactive shell with history, Tab completion of steps
and tokens, and the documentation of every function:

```text
$ graphersal
graphersal> g.v().has_label("person").values("name").limit(2);
╭───────╮
│ value │
├───────┤
│ marko │
│ vadas │
╰───────╯
graphersal> let n = g.v().count().next();
graphersal> n * 10;
60
graphersal> /help values
values  (step, terminal or source method of a traversal)
  values()
  values(arg1: any)
  ...
Yields the values of the given (or all) properties; a jpath() key reads a nested value.

Example: `g.v(1).values("name", "age").to_list()` returns `["marko", 29]`
```

Variables live across lines: a script is ordinary Rhai code around Gremlin traversals, so loops,
functions and maps work as well.

## See the plan

`profile()` prints the plan the optimizer made, with timings and traverser counts:

```text
$ graphersal -e 'g.v().has_label("person").out("created").count().profile()'
Traversal Metrics
Step                                                         Call      In     Out       Time    % Dur
=====================================================================================================
v(labels: ["person"])                                           1       0       4    5.667µs     8.66
out("created")                                                  1       4       4    5.125µs     7.83
count()                                                         1       4       1      125ns     0.19
                                                      TOTAL:             execute:   65.417µs    16.69
=====================================================================================================
Optimizer rules applied: source_filter_pushdown
```

## Keep a graph on disk

`graphersal store` creates and manages a Store, a graph on disk with a write-ahead log, and every
mode opens one with `--graph <dir>`:

```bash
graphersal store create data --from people.json    # a new store from a graph file
graphersal --graph data -e 'g.add_v("person").property("name", "ann").iterate()'   # durable
graphersal store info data
```

More: [The graphersal store Command](../persistence/store_cli.md).

## Embedding models

`embed()` computes vectors from text with an embedder; built in is `hash-ngram/1` (lexical only).
`graphersal embed-model download <repo>` fetches a real Model2Vec model from Hugging Face once
(the only network access of the tool), and `--embedder NAME=REPO` (or `NAME=<directory>`) loads it
for any mode, `--default-embedder NAME` makes it the default:

```bash
graphersal embed-model download minishlab/potion-base-8M     # into ~/.cache/graphersal/embed-models
graphersal embed-model download minishlab/potion-multilingual-128M   # 101 languages (about 510 MB)
graphersal embed-model list                                  # repository, size, model identity
graphersal --embedder potion=minishlab/potion-base-8M \
  -e 'g.v().has_label("person").property("emb", __.embed("name", #{embedder: "potion"})).iterate();
      let model = g.embedders().filter(|e| e.name == "potion")[0].model;
      g.set_schema(g.infer_schema(), SchemaMode.open);
      g.patch_schema(#{vertices: #{person: #{schema: #{properties: #{emb: #{embedder: model}}}}}});
      g.v().has_label("person").nearest("emb", "Marco", 2).values("name").to_list()'
```

`embedder: "potion"` picks the embedder by the name given to `--embedder` (without it the default
runs: `--default-embedder`, else `hash-ngram/1`); `g.embedders()` lists the names with their
model's full identity, `model2vec/minishlab/potion-base-8M@fac4fa854e68`, the one `embed-model
list` shows. The schema records it for `person.emb`, so the text query embeds with that model. A
model imported into a Store (`--into`) can fill the property by itself through a
[vector rule](../users_guide/vector_rules.md).

The models directory is `$GRAPHERSAL_EMBED_MODELS` (or `--dir`), else the user cache. A model
that does not load stops the command with exit code 2 and the reason. More:
[Embeddings and Vector Search](embeddings.md#real-models-model2vec).

Every option of the tool is in
`graphersal --help` and on the [Command Cheat Sheet](../users_guide/command_cheat_sheet.md).
