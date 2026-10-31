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
A real model serves a database only once it is imported into it: `graphersal embed-model
download <repo>` fetches a Model2Vec model from Hugging Face once into the download cache (the
only network access of the tool), and `graphersal embed-model import <repo> --into <store>`
puts it into a store (`--default` makes it the model `embed()` uses without a name):

```bash
graphersal embed-model download minishlab/potion-base-8M     # into ~/.cache/graphersal/embed-models
graphersal embed-model download minishlab/potion-multilingual-128M   # 101 languages (about 510 MB)
graphersal embed-model list                                  # repository, size, model identity
graphersal store create data
graphersal embed-model import minishlab/potion-base-8M --into data --default
graphersal --graph data \
  -e 'g.add_v("person").property("name", "Marco").iterate();
      g.add_v("person").property("name", "Mary").iterate();
      g.patch_schema(#{vertices: #{person: #{schema: #{type: "object", properties: #{emb: #{type: "array",
        format: "vector", embedder: "minishlab/potion-base-8M", from: "name"}}}}}});
      g.v().has_label("person").nearest("emb", "Marco", 2).values("name").to_list()'
```

The vector rule (`from: "name"`) fills `person.emb` with the store's model at every write
([Vector Rules](../users_guide/vector_rules.md)); `embed(text)` and `g.v().embed("name",
#{embedder: "minishlab/potion-base-8M"})` name a model by its repository; `g.embedders()` lists
the built-in and the database's models with their full identity,
`model2vec/minishlab/potion-base-8M@fac4fa854e68`, the one `embed-model list` shows. A `.gsnap`
file carries its models too: `--graph file.gsnap` serves them without a store.

Changed in 0.5.0: the flags `--embedder NAME=...` and `--default-embedder` are removed (a command
line that still passes them stops with exit code 2 and the import command); a model is imported
into the database instead.

The download cache is `$GRAPHERSAL_EMBED_MODELS` (or `--dir`), else the user cache. A model
that does not load stops the command and gives the reason. More:
[Embeddings and Vector Search](embeddings.md#real-models-model2vec).

Every option of the tool is in
`graphersal --help` and on the [Command Cheat Sheet](../users_guide/command_cheat_sheet.md).
