# Step - glob_path

`glob_path(pattern)` walks the tree below each incoming vertex along a filesystem-style glob
pattern and emits every vertex the pattern matches:

```rhai,graph=tree
g.v("root").glob_path("**/author/*/some/*.rs").by("name").by(["directory","file"])
```

It is a **Gremlin extension**: TinkerPop has no such step, and a query that uses it does not run
on other Gremlin servers. It makes nothing newly possible (see [Equivalents](#equivalents)), but
it turns a common tree query into one short step. The step prunes every branch as soon as the
pattern can no longer match there, emits each match once, and stops on cycles.

- `pattern`: `/`-separated segments; each segment matches one level below the incoming vertex.
- First `by()` (required): the **match key**, which a named segment is compared with: a property
  key such as `by("name")`, the vertex id or the vertex labels.
- Second `by()` (optional): a list of **vertex labels**; the walk only visits vertices that carry
  one of them.

Every example on this page runs on the `file_tree` sample graph and shows the output of
`graphersal --graph tree`. The step's output order is unspecified, so the examples sort with
`order().by(T.id)` where there is more than one result.

## Quick start

Start the CLI on the sample tree:

```bash
graphersal --graph tree
```

The graph is a small filesystem. Vertex ids are paths from `root`, the labels are `directory`,
`file` and `symlink`, the `name` property holds the file name, and `contains` edges point from a
directory to its entries. Two `points_to` edges add a cycle and a second route to `root/author`:

```text
root                                   directory
├── author
│   ├── x
│   │   └── some
│   │       ├── f.rs                   file
│   │       └── readme.md              file
│   └── q
│       └── author
│           └── w
│               └── some
│                   └── h.rs           file
├── y
│   ├── author
│   │   └── z
│   │       └── some
│   │           └── g.rs               file
│   └── loop                           symlink ──points_to──▶ root        (a cycle)
├── src
│   ├── main.rs  lib.rs  test_a.rs  test_b.rs
│   └── util
│       ├── mod.rs
│       └── io.rs
├── docs
│   └── guide.md  notes.txt  data[1].csv  résumé.md
├── #unnamed                           directory without a name property
│   └── orphan.rs
├── #42                                directory whose name is the int64 42
│   └── answer.rs
├── #slash                             directory named "a/b"
│   └── c.rs
└── latest                             symlink ──points_to──▶ root/author (a diamond)
```

Every unmarked entry with children is a `directory`, every other one a `file`. The id of each
vertex is the path of names from `root`, for example `root/src/util/io.rs`; the three
directories marked `#` have the ids `root/#unnamed`, `root/#42` and `root/#slash`.

Find every Rust file two levels below any `author` directory, where the level in between is
`some`:

```rhai,graph=tree
g.v("root").glob_path("**/author/*/some/*.rs").by("name").by(["directory","file"]).order().by(T.id).id().to_list()
```

```text
"root/author/q/author/w/some/h.rs"
"root/author/x/some/f.rs"
"root/y/author/z/some/g.rs"
```

`**` crosses any number of levels, so the nested `root/author/q/author/...` is found too. The
second `by()` keeps the walk on directories and files: it never follows a symlink.

The step can be chained, and used in child traversals like any other step:

```rhai,graph=tree
g.v("root").glob_path("src").by("name").glob_path("util/*").by("name").order().by(T.id).id().to_list()
```

```text
"root/src/util/io.rs"
"root/src/util/mod.rs"
```

```rhai,graph=tree
g.v("root").glob_path("*").by("name").by(["directory"]).where(__.glob_path("*.md").by("name")).id().to_list()
```

```text
"root/docs"
```

The camelCase spelling `globPath(...)` is the same step.

## Pattern syntax

A pattern is a list of segments separated by `/`. Segment `k` matches the vertices `k` levels
below the incoming vertex. A segment with at least one character that is not `*` is a **named**
segment: it reads the match key of the vertex and compares it with the segment. `*` and `**`
alone never read the key.

Each row below is the query
`g.v("root").glob_path(PATTERN).by("name").order().by(T.id).id().to_list()` with the pattern of that row:

| Construct | Meaning | Pattern | Matches on `file_tree` |
|---|---|---|---|
| literal | exactly this value | `src/main.rs` | `root/src/main.rs` |
| `*` | any value, including the empty one | `src/*` | `root/src/lib.rs`, `root/src/main.rs`, `root/src/test_a.rs`, `root/src/test_b.rs`, `root/src/util` |
| `*` inside a segment | any sequence of characters | `src/util/*.rs` | `root/src/util/io.rs`, `root/src/util/mod.rs` |
| `?` | exactly one character (a Unicode character, not a byte) | `src/test_?.rs` | `root/src/test_a.rs`, `root/src/test_b.rs` |
| `?` with non-ASCII | `é` is one character | `docs/r?sum?.md` | `root/docs/résumé.md` |
| leading `**` | zero or more levels | `**/src` | `root/src` |
| leading `**` | zero or more levels | `**/util` | `root/src/util` |
| inner `**` | zero or more levels | `author/**/h.rs` | `root/author/q/author/w/some/h.rs` |
| trailing `**` | **one** or more levels: everything below, not the level itself | `src/**` | `root/src/lib.rs`, `root/src/main.rs`, `root/src/test_a.rs`, `root/src/test_b.rs`, `root/src/util`, `root/src/util/io.rs`, `root/src/util/mod.rs` |
| `[..]` | one character of the set | `src/test_[ab].rs` | `root/src/test_a.rs`, `root/src/test_b.rs` |
| `[!..]` | one character not in the set | `src/test_[!a].rs` | `root/src/test_b.rs` |
| `[a-z]` | one character of the range | `src/[a-m]*.rs` | `root/src/lib.rs`, `root/src/main.rs` |
| `{a,b}` | one of the alternatives | `src/{main,lib}.rs` | `root/src/lib.rs`, `root/src/main.rs` |
| several `{..}` | every combination | `docs/{guide,notes}.{md,txt}` | `root/docs/guide.md`, `root/docs/notes.txt` |
| empty alternative | the group may match nothing | `docs/guide{,s}.md` | `root/docs/guide.md` |
| `{..}` per segment | groups choose within their own segment | `{src,docs}/{m,g}*` | `root/docs/guide.md`, `root/src/main.rs` |
| `\` | the next character is literal | `docs/data\[1\].csv` | `root/docs/data[1].csv` |
| `\/` | a `/` inside one value, not a separator | `a\/b/c.rs` | `root/#slash/c.rs` |
| case | matching is case-sensitive | `docs/*.MD` | nothing |

Details:

- `**` must be a whole segment; `a**` or `**b` is an error. A run such as `**/**` counts as one
  `**`.
- A `]` right after `[` or `[!` is a literal member of the set, so `[]]` matches `]`. A stray `]`,
  `}` or `,` outside a class or alternatives is an ordinary character.
- Alternatives cannot be nested and cannot contain `/`. The `{..}` groups of one segment expand to
  their product, at most **256** alternatives.
- A pattern has at most **63** segments, after collapsing `**/**`.
- `.` and `..` as whole segments are reserved (see
  [Limitations and reserved extensions](#limitations-and-reserved-extensions)); `\.` matches a
  value that is literally `.`.
- A pattern cannot be empty, start or end with `/`, or contain `//`.
- In a Rhai or Rust string literal the backslash itself must be escaped: the pattern
  `docs/data\[1\].csv` is written `"docs/data\\[1\\].csv"` (or `r"docs/data\[1\].csv"` in Rust):

```rhai,graph=tree
g.v("root").glob_path("docs/data\\[1\\].csv").by("name").id().to_list()
```

```text
"root/docs/data[1].csv"
```

Every violation fails with a positioned error; see [Errors](#errors).

## The `by()` modulators

The `by()` modulators of `glob_path()` are **positional**, like `group().by(key).by(value)`:

| Position | Required | Accepts |
|---|---|---|
| 1st `by()` | yes | the match key: a property key (`by("name")`), the vertex id (`by(T.id)`) or the vertex labels (`by(T.label)`) |
| 2nd `by()` | no | a non-empty list of vertex labels (`by(["directory","file"])`) |

In Rust the tokens are `By::Id` and `By::Label`.

**Match on the id.** Here a segment is compared with the full vertex id. Inside one segment `*`
also matches `/`, so an id's `/` is written `\/` in the pattern:

```rhai,graph=tree
g.v("root").glob_path("**/*\\/util\\/*").by(T.id).order().by(T.id).id().to_list()
```

```text
"root/src/util/io.rs"
"root/src/util/mod.rs"
```

**Match on the label.** A named segment matches when any of the vertex's labels matches:

```rhai,graph=tree
g.v("root").glob_path("**/symlink").by(T.label).order().by(T.id).id().to_list()
```

```text
"root/latest"
"root/y/loop"
```

**Restrict the walk by label.** With a second `by()`, every vertex the walk visits below the
start must carry one of the listed labels. A vertex without one is neither emitted nor walked
through:

```rhai,graph=tree
g.v("root").glob_path("src/*").by("name").by(["directory"]).id().to_list()
```

```text
"root/src/util"
```

**Invalid combinations.** Anything else fails when the traversal runs, with
`InvalidModulator` naming the position of the offending `by()`:

| Written | Error (`Caused by:` line) | Fix |
|---|---|---|
| `glob_path("*")` | `` `glob_path()` cannot use its by() #1: glob_path() needs a first by() naming the match key `` | Add the match key as the first by(). |
| `glob_path("*").by(["directory"])` | `` `glob_path()` cannot use its by() #1: the first by() is the match key; the vertex-label list goes into the second by() `` | The first by() is the key and the label list goes second: put a key such as by("name") before the label list. |
| `glob_path("*").by(__.values("name"))` | `` `glob_path()` cannot use its by() #1: the match key must be a property key, T.id or T.label `` | A traversal, an order, a JSON path, Value or Count cannot be the match key. |
| `glob_path("*").by("name").by("name")` | `` `glob_path()` cannot use its by() #2: the second by() must be a list of vertex labels `` | The second by() only filters by vertex label: pass a list such as by(["directory", "file"]), or drop it to visit every vertex. |
| `glob_path("*").by("name").by([])` | `` `glob_path()` cannot use its by() #2: the vertex-label list is empty `` | An empty label list would admit no vertex: list at least one label, or drop the second by() to visit every vertex. |
| `glob_path("*").by("name").by(["file"]).by(T.id)` | `` `glob_path()` cannot use its by() #3: glob_path() takes at most two by() modulators `` | There is no third by(): drop it. |

## Options

`glob_path(pattern, #{...})` takes a map of options after the pattern. Each option is independent
and optional; `glob_path(pattern)` is the same as `glob_path(pattern, #{})`. The `by()`
modulators stay the same.

| Option | Value | Effect |
|---|---|---|
| `max_depth` | an integer from 1 | never visit a vertex more than this many levels below the start vertex |
| `prune` | an anonymous traversal | do not descend into a visited vertex for which it yields a result |

In Rust the options are a `GlobPathOptions` built with `max_depth(n)` and `prune(traversal)`,
passed to `glob_path_with(pattern, options)`; see [Rust API](#rust-api).

**`max_depth`.** The start vertex is level 0, its children are level 1. With `max_depth: n` a
vertex deeper than level `n` is not visited at all: it is not emitted, not walked through, and its
key is never read. The bound applies to the whole walk, not only to `**`. `max_depth: 1` visits
exactly the children of the start vertex:

```rhai,graph=tree
g.v("root").glob_path("**", #{max_depth: 1}).by("name").order().by(T.id).id().to_list()
```

```text
"root/#42"
"root/#slash"
"root/#unnamed"
"root/author"
"root/docs"
"root/latest"
"root/src"
"root/y"
```

The Rust files at most two levels down:

```rhai,graph=tree
g.v("root").glob_path("**/*.rs", #{max_depth: 2}).by("name").order().by(T.id).id().to_list()
```

```text
"root/#42/answer.rs"
"root/#slash/c.rs"
"root/#unnamed/orphan.rs"
"root/src/lib.rs"
"root/src/main.rs"
"root/src/test_a.rs"
"root/src/test_b.rs"
```

A vertex that several routes reach, such as `root/author` (directly and through `root/latest`),
counts at the depth of its shortest route within the bound, so no match within the bound is lost
whatever order the walk takes.

**`prune`.** The child traversal starts from each visited vertex; when it yields at least one
result, the walk does not descend below that vertex, like `find -prune`. Nothing below the three
`author` directories:

```rhai,graph=tree
g.v("root").glob_path("**/*.rs", #{prune: __.has("name", "author")}).by("name").order().by(T.id).id().to_list()
```

```text
"root/#42/answer.rs"
"root/#slash/c.rs"
"root/#unnamed/orphan.rs"
"root/src/lib.rs"
"root/src/main.rs"
"root/src/test_a.rs"
"root/src/test_b.rs"
"root/src/util/io.rs"
"root/src/util/mod.rs"
```

Pruning never suppresses the vertex itself: a pruned vertex that matches the pattern is still
emitted. Here the nested `root/author/q/author` is not found, because the walk stops below
`root/author`:

```rhai,graph=tree
g.v("root").glob_path("**/author", #{prune: __.has("name", "author")}).by("name").order().by(T.id).id().to_list()
```

```text
"root/author"
"root/y/author"
```

The child starts from a vertex that carries the path of the incoming traverser (the walk records
no intermediate levels), so it can read a step label set before `glob_path`. Here `select("a")`
is the start vertex, so every child is emitted and none is descended into:

```rhai,graph=tree
g.v("root/src").as("a").glob_path("**", #{prune: __.select("a").has("name", "src")}).by("name").order().by(T.id).id().to_list()
```

```text
"root/src/lib.rs"
"root/src/main.rs"
"root/src/test_a.rs"
"root/src/test_b.rs"
"root/src/util"
```

The child can be any traversal, a `glob_path` included (`__.glob_path("*.md").by("name")` prunes
every directory that holds a Markdown file). An error it raises fails the whole traversal.

**The label filter is the cheap prune.** A second `by()` already prunes: a vertex without one of
the listed labels is neither emitted nor walked through, and checking a label costs no child
traversal. Use `prune` for conditions a label cannot express.

The options combine with each other and with the label filter; the checks for each visited vertex
run in this order: label filter, depth bound, pattern, emission, prune.

```rhai,graph=tree
g.v("root").glob_path("**/*.rs", #{max_depth: 3, prune: __.has("name", "author")}).by("name").by(["directory","file"]).count().next()
```

```text
9
```

Any other key fails when the step is built, so a misspelled option is never silently ignored. The
keys `edges`, `path` and `case_insensitive` are reserved for later options (see
[Limitations and reserved extensions](#limitations-and-reserved-extensions)).

## Semantics

**The start vertex is the root.** The pattern describes the path *below* the incoming vertex: the
first segment matches its children. The start vertex itself is never checked against the pattern
or the label filter, and it is never emitted, even when the walk comes back to it through a
cycle:

```rhai,graph=tree
g.v("root").glob_path("root").by("name").count().next()
```

```text
0
```

```rhai,graph=tree
g.v("root").glob_path("**/root").by("name").count().next()
```

```text
0
```

`root/latest` is a `symlink`, but as the start it passes a `["directory"]` filter:

```rhai,graph=tree
g.v("root/latest").glob_path("*").by("name").by(["directory"]).id().to_list()
```

```text
"root/author"
```

**All outgoing edges.** The walk follows every outgoing edge, whatever its label. Here it goes
through the `points_to` edge of `root/latest`; with the label filter it does not enter the
symlink at all:

```rhai,graph=tree
g.v("root").glob_path("latest/*").by("name").order().by(T.id).id().to_list()
```

```text
"root/author"
```

```rhai,graph=tree
g.v("root").glob_path("latest/*").by("name").by(["directory","file"]).count().next()
```

```text
0
```

The filter applies to every visited vertex, not only to emitted ones, which is what keeps a walk
on a real filesystem tree:

```rhai,graph=tree
g.v("root").glob_path("**/loop").by("name").order().by(T.id).id().to_list()
```

```text
"root/y/loop"
```

```rhai,graph=tree
g.v("root").glob_path("**/loop").by("name").by(["directory","file"]).count().next()
```

```text
0
```

**Multiplicity.** Each matched vertex is emitted at most once per incoming traverser, even when
several routes or several splits of `**` reach it. `root/author` is reachable directly and through
`root/latest`, yet it appears once:

```rhai,graph=tree
g.v("root").glob_path("**/author").by("name").order().by(T.id).id().to_list()
```

```text
"root/author"
"root/author/q/author"
"root/y/author"
```

```rhai,graph=tree
g.v("root").glob_path("**/author/**/*.rs").by("name").order().by(T.id).id().to_list()
```

```text
"root/author/q/author/w/some/h.rs"
"root/author/x/some/f.rs"
"root/y/author/z/some/g.rs"
```

Distinct incoming traversers are independent, like `out()`: each one emits its own matches.

```rhai,graph=tree
g.v(["root","root/author"]).glob_path("**/f.rs").by("name").id().to_list()
```

```text
"root/author/x/some/f.rs"
"root/author/x/some/f.rs"
```

**Path.** The step extends the traverser path by exactly one element, the matched vertex. The
intermediate levels are not recorded:

```rhai,graph=tree
g.v("root").glob_path("src/util/io.rs").by("name").path().by(T.id).to_list()
```

```text
["root", "root/src/util/io.rs"]
```

**Output order is unspecified.** Sort with `order()` (or `order().by(...)`) when the order
matters.

**Missing and non-string values.** A named segment works like `has()`: a vertex whose match key
is missing, or holds a value that is not a string, does not match. The value is never converted
to a string, and no error is raised. `*` and `**` never read the key, so such vertices are still
walked through:

```rhai,graph=tree
g.v("root").glob_path("*/orphan.rs").by("name").id().to_list()
```

```text
"root/#unnamed/orphan.rs"
```

```rhai,graph=tree
g.v("root").glob_path("?*/orphan.rs").by("name").count().next()
```

```text
0
```

`?*` matches every non-empty string, but `root/#unnamed` has no `name`. `root/#42` has the int64
name `42`, which the segment `42` does not match:

```rhai,graph=tree
g.v("root").glob_path("42/*").by("name").count().next()
```

```text
0
```

```rhai,graph=tree
g.v("root").glob_path("*/answer.rs").by("name").id().to_list()
```

```text
"root/#42/answer.rs"
```

**Case-sensitive.** `docs/*.MD` matches nothing (see the syntax table).

**No hidden-file rule.** Unlike a shell, `*` and `?` also match a value that starts with `.`.
There are no special names.

## Performance

- The pattern is compiled once, when the traversal is built, into an automaton whose live states
  fit in one 64-bit mask. The walk is an iterative depth-first search, so deep trees cannot
  overflow the stack.
- **Pruning.** A branch is abandoned as soon as no pattern state is alive in it. A pattern
  without `**`, such as `src/util/*.rs`, reads only the levels it names; a leading `**` has to
  visit the whole subtree, but still skips everything the label filter excludes.
- **Trees and graphs.** On a tree every vertex is visited at most once per incoming traverser. A
  vertex reachable over several routes (a diamond, a cycle) is walked again only when it arrives
  with pattern states it has not seen yet, so the walk always ends, but it may do more than one
  pass over a shared subtree. Use the label filter to keep the walk on the tree edges when the
  graph has shortcuts such as symlinks: without it the walk below counts every vertex once,
  including the symlinks; with it, the 34 directories and files.

```rhai,graph=tree
g.v("root").glob_path("**").by("name").count().next()
```

```text
36
```

```rhai,graph=tree
g.v("root").glob_path("**").by("name").by(["directory","file"]).count().next()
```

```text
34
```

- **Depth bound.** `max_depth` stops the walk at a fixed level, so a `**` walk over a deep or
  huge tree costs only the levels it is allowed to see. On a vertex that several routes reach, the
  walk keeps the smallest depth per pattern state and walks the vertex again only when it arrives
  at a smaller depth, which keeps it exact and finite.
- **Prune cost.** `prune` runs its child traversal once for every visited vertex the walk would
  descend into, so it pays off when it cuts large subtrees; prefer the label filter where a
  label is enough.
- **Counting.** A `count()` directly after the step counts the matches without creating a
  traverser for each one, also with `max_depth`. With `prune`, the child traversal may allocate
  for every vertex it is evaluated on:

```rhai,graph=tree
g.v("root").glob_path("**/*.rs").by("name").by(["directory","file"]).count().next()
```

```text
12
```

- **Output size.** Graphersal executes a traversal one step at a time, so a step with N matches
  holds N traversers in memory before the next step runs, exactly like `out()`. Put a
  `limit()` after the step to shorten the result, not to shorten the walk.
- A long walk respects `evaluationTimeout`; see [Query Limits](../../users_guide/query_limits.md).
- `.profile()` shows the step with its modulators and the options that are set, for example
  `glob_path("**/*.rs", {max_depth: 3, prune: __.has("name", "author")}).by("name")`, with its
  traverser counts; the `prune` child appears as a nested row with how often it ran.

## Errors

The pattern and `by()` errors are raised when the traversal runs. `graphersal` shows the failing step,
the cause and a help text. For example:

```rhai
g.v("root").glob_path("src/a**").by("name").to_list()
```

```text
Error: Step #1 'glob_path("src/a**").by("name")' execution failed
  at #1: v("root").glob_path("src/a**").by("name")
                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
Caused by: Invalid glob pattern 'src/a**' at position 5: '**' must be a whole segment
Help: '**' spans whole levels, so it must stand alone between slashes. Write glob_path("src/a*/**") instead, or plain glob_path("**") to match every level below the start vertex. A literal '*' is written '\*'.
      A pattern is a '/'-separated list of segments matched level by level below the start vertex: '*' (any value), '?' (one character), '**' (any number of levels, a whole segment only), '[a-z]' / '[!a-z]' (character classes), '{a,b}' (alternatives within one segment) and '\' to escape any of them, for example glob_path("**/src/*.{rs,toml}").
```

### Invalid glob pattern

`InvalidGlobPattern` reports the pattern, the 0-based character position of the problem and the
reason. Each row is `g.v("root").glob_path(PATTERN).by("name").to_list()`; the Fix column quotes the
help text, which `graphersal` shows in full together with a summary of the pattern syntax:

| Pattern | Error (`Caused by:` line) | Fix |
|---|---|---|
| empty: `""` | `Invalid glob pattern '' at position 0: the pattern is empty` | Pass a non-empty pattern, for example `glob_path("*")` for the children of the start vertex or `glob_path("**")` for all of its descendants. |
| `/src` | `Invalid glob pattern '/src' at position 0: a pattern cannot start with '/'` | The pattern is relative to the start vertex: drop the leading and trailing `'/'` and the empty segments |
| `src/` | `Invalid glob pattern 'src/' at position 3: a pattern cannot end with '/'` | The pattern is relative to the start vertex: drop the leading and trailing `'/'` and the empty segments |
| `src//a` | `Invalid glob pattern 'src//a' at position 4: empty segment between two '/'` | The pattern is relative to the start vertex: drop the leading and trailing `'/'` and the empty segments |
| `src/..` | `Invalid glob pattern 'src/..' at position 4: '.' and '..' are reserved as whole segments` | The start vertex is already the current level: `glob_path("docs/*")` matches its children directly. To walk back up use `.in()` or `repeat(__.in())` before the step, for example `g.v("x").in().glob_path("docs/*")`. A value that is literally `'.'` is matched with `'\.'`. |
| `src/a**` | `Invalid glob pattern 'src/a**' at position 5: '**' must be a whole segment` | Write `glob_path("src/a*/**")` instead, or plain `glob_path("**")` to match every level below the start vertex. A literal `'*'` is written `'\*'`. |
| `src\` | `Invalid glob pattern 'src\' at position 3: a trailing '\' escapes nothing` | `'\'` escapes the character after it; match a literal backslash with `'\\'` |
| `src/[ab` | `Invalid glob pattern 'src/[ab' at position 4: unclosed character class '['` | Close the character class with `']'` in the same segment, for example `glob_path("file[0-9].txt")`; match a literal `'['` with `'\['`. |
| `src/[]` | `Invalid glob pattern 'src/[]' at position 4: empty character class` | A character class needs at least one character; a `']'` right after `'['` or `'[!'` is taken literally, so `glob_path("[]]")` matches `']'` and `glob_path("[!]]")` anything else. |
| `src/[z-a]` | `Invalid glob pattern 'src/[z-a]' at position 5: inverted character range` | A range goes from the lower to the higher character, for example `glob_path("[a-z]*")` instead of `glob_path("[z-a]*")`. |
| `src/{a,b` | `Invalid glob pattern 'src/{a,b' at position 4: unclosed alternatives '{'` | Close the alternatives with `'}'`, for example `glob_path("*.{rs,toml}")`; match a literal `'{'` with `'\{'`. |
| `src/{a,{b}}` | `Invalid glob pattern 'src/{a,{b}}' at position 7: alternatives '{..}' cannot be nested` | Alternatives cannot be nested; flatten them into one group, for example `glob_path("{a,b1,b2}")` instead of `glob_path("{a,b{1,2}}")`, or place two groups side by side: `glob_path("{a,b}{1,2}")`. |
| `src/{a/b,c}` | `Invalid glob pattern 'src/{a/b,c}' at position 6: alternatives '{..}' cannot contain '/'` | Split the alternative into two queries, for example `glob_path("src/a")` and `glob_path("lib/b")` instead of `glob_path("{src/a,lib/b}")`, or use `'**'` to span levels, for example `glob_path("**/{a,b}")`. A value that itself contains `'/'` is matched with `'\/'`. |
| `{a,b}` nine times | `Invalid glob pattern '{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}' at position 40: a segment expands to more than 256 alternatives` | Replace a group with a class or a wildcard, for example `glob_path("file[0-9][0-9]")` instead of `glob_path("file{0,1,...,9}{0,1,...,9}")`, or split the query. |
| `a` 64 times, joined by `/` | `Invalid glob pattern 'a/a/.../a' at position 126: the pattern has more than 63 segments` | Split the walk into two consecutive steps, each matching below the vertices the previous one emits, for example `glob_path("a/b/c").glob_path("d/e/f")` instead of `glob_path("a/b/c/d/e/f")`. |

In Rhai the backslash pattern is written `"src\\"`, and the last row builds its pattern in a loop:

```rhai
let p = "a"; for i in 0..63 { p += "/a"; } g.v("root").glob_path(p).by("name").to_list()
```

### Invalid modulator

`InvalidModulator` names the step, the position of the `by()` and the reason. The cases are listed
under [Invalid combinations](#the-by-modulators). Every help text ends with a corrected query:
`g.v("root").glob_path("**/*.rs").by("name").by(["directory","file"])`.

### Invalid options

A `max_depth` below 1 or above 4294967295 fails when the traversal runs, after the pattern and
`by()` checks, with `InvalidOption` for the key `glob_path.max_depth`. The help repeats the query
with a valid value:

```rhai
g.v("root").glob_path("**", #{max_depth: 0}).by("name").to_list()
```

```text
Error: Step #1 'glob_path("**", {max_depth: 0}).by("name")' execution failed
  at #1: v("root").glob_path("**", {max_depth: 0}).by("name")
                   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
Caused by: Invalid value for option 'glob_path.max_depth': must be at least 1, got 0
Help: max_depth is the deepest level below the start vertex that glob_path() visits: the start vertex is level 0 and its children are level 1, so max_depth is an integer from 1 to 4294967295. Leave it out to walk without a bound. For example g.v("root").glob_path("**", #{max_depth: 1}).by("name").
```

An unknown or reserved key, or a value of the wrong type (`max_depth: "3"`, `prune: 5`), fails in
Rhai as soon as the step is built, with an argument mismatch:

```rhai
g.v("root").glob_path("**", #{edges: ["contains"]}).by("name").to_list()
```

```text
Error: Argument mismatch for step 'glob_path': expected an options map with the keys max_depth, prune, got the reserved key `edges`, which is not implemented yet
Help: glob_path(pattern, #{...}) accepts the options max_depth (an integer from 1: the deepest level below the start vertex the walk visits) and prune (an anonymous traversal: a visited vertex for which it yields a result is not descended into, but is still emitted when it matches); the keys edges, path and case_insensitive are reserved. For example g.v("root").glob_path("**/*.rs", #{max_depth: 3, prune: __.has("name", "target")}).by("name").
```

An error inside the `prune` child traversal fails the traversal with that error.

## Equivalents

The step makes a common query compact and fast; it makes nothing newly possible. The quick-start
query reads as follows elsewhere.

**Vanilla Gremlin.** `emit().repeat(...)` covers the leading `**` (zero or more levels), one
`out()` per segment follows, and `dedup()` removes the duplicates of several routes. In Graphersal
(`TextP.endingWith(".rs")` stands in for `*.rs`, which only differs for a name containing `/`):

```rhai,graph=tree
g.V("root").emit().repeat(__.out().hasLabel("directory","file")).out().hasLabel("directory","file").has("name","author").out().hasLabel("directory","file").out().hasLabel("directory","file").has("name","some").out().hasLabel("directory","file").has("name", TextP.endingWith(".rs")).dedup().order().by(T.id).id().to_list()
```

```text
"root/author/q/author/w/some/h.rs"
"root/author/x/some/f.rs"
"root/y/author/z/some/g.rs"
```

In TinkerPop's Groovy syntax, with an exact regex for `*.rs`:

```groovy
g.V('root').emit().repeat(out().hasLabel('directory','file'))
  .out().hasLabel('directory','file').has('name','author')
  .out().hasLabel('directory','file')
  .out().hasLabel('directory','file').has('name','some')
  .out().hasLabel('directory','file').has('name', regex('^[^/]*\\.rs$'))
  .dedup()
```

`emit().repeat(...)` needs no `until()`: the loop ends when no vertex is left, and `emit()` before
`repeat()` also covers zero levels. Do **not** write `**` as `repeat(...).until(has("name",
"author"))`: it stops at the first `author` on each branch and loses the nested
`author/.../author/...` match, here `h.rs`:

```rhai,graph=tree
g.V("root").repeat(__.out().hasLabel("directory","file")).until(__.has("name","author")).out().hasLabel("directory","file").out().hasLabel("directory","file").has("name","some").out().hasLabel("directory","file").has("name", TextP.endingWith(".rs")).dedup().order().by(T.id).id().to_list()
```

```text
"root/author/x/some/f.rs"
"root/y/author/z/some/g.rs"
```

The vanilla form grows with every `**`, `{a,b}` or character class, and it expands the whole
subtree before it filters. On a cycle TinkerPop loops forever; Graphersal stops after
`repeat.max_loops` iterations (see [Recursive Traversals](../../users_guide/recursive_traversals.md)).

**Cypher.** A classic variable-length relationship cannot constrain the labels of the vertices in
between without a `WHERE` over the whole path:

```cypher
MATCH p = (r {id:'root'})-[*0..]->(a:directory|file {name:'author'})
          -->(:directory|file)-->(s:directory|file {name:'some'})-->(f:directory|file)
WHERE all(n IN nodes(p)[1..] WHERE n:directory OR n:file) AND f.name =~ '[^/]*\\.rs'
RETURN DISTINCT f
```

**GQL** (ISO/IEC 39075) and Neo4j 5 quantified path patterns express the `**` directly:

```text
(r) ( ()-->(:directory|file) ){0,} (a:directory|file {name:'author'}) --> ...
```

TigerGraph GSQL (pattern syntax v2) can express it with `-(>)-*0..` and `LIKE` or regex
conditions, but verbosely.

## Limitations and reserved extensions

- Only outgoing edges are followed, whatever their label. An **edge-label filter** is reserved.
- The path grows by one element. Recording the **full path** of intermediate vertices is reserved.
- Matching is case-sensitive. **Case-insensitive matching** is reserved.
- `..` as "go to the parent" is reserved; today `.` and `..` as whole segments are an error.

The first three will be further keys of the options map (`edges`, `path: "full"`,
`case_insensitive`; today these keys are rejected), never extra positional `by()`s, and the
defaults never change: a one-element path, all outgoing edges, case-sensitive matching. `max_depth`
and `prune` are available now; see [Options](#options).

## Rust API

The step is `glob_path(pattern)` on `GraphTraversalSource`, followed by `by()`; `__::glob_path`
builds it in a child traversal. `TraversalGraph::file_tree()` builds the sample graph
(`GraphSource::file_tree()` wraps it in a lock):

```rust
use graphersal::prelude::*;
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let graph = TraversalGraph::file_tree();

let rust_files = graph
    .traversal()
    .v("root")
    .glob_path("**/author/*/some/*.rs")
    .by("name")
    .by(["directory", "file"])
    .id()
    .order()
    .to_list()?;
assert_eq!(
    rust_files,
    vec![
        ElementProperty::from("root/author/q/author/w/some/h.rs"),
        ElementProperty::from("root/author/x/some/f.rs"),
        ElementProperty::from("root/y/author/z/some/g.rs"),
    ]
);

let symlinks = graph.traversal().v("root").glob_path("**/symlink").by(By::Label).count().next()?;
assert_eq!(symlinks.and_then(|count| count.as_i64()), Some(2));

let docs_with_markdown = graph
    .traversal()
    .v("root")
    .glob_path("*")
    .by("name")
    .by(["directory"])
    .where_t(__::glob_path("*.md").by("name"))
    .id()
    .to_list()?;
assert_eq!(docs_with_markdown, vec![ElementProperty::from("root/docs")]);

let outside_author = graph
    .traversal()
    .v("root")
    .glob_path_with(
        "**/*.rs",
        GlobPathOptions::default().max_depth(3).prune(__::has("name", "author")),
    )
    .by("name")
    .by(["directory", "file"])
    .count()
    .next()?;
assert_eq!(outside_author.and_then(|count| count.as_i64()), Some(9));
# Ok(())
# }
```

The options are `glob_path_with(pattern, GlobPathOptions)`, on `GraphTraversalSource` and in child
traversals (`__::glob_path_with`). `GlobPathOptions::default()` sets nothing, `max_depth(n)` takes
a `u32` and `prune(traversal)` an anonymous traversal; `glob_path(p)` is
`glob_path_with(p, GlobPathOptions::default())`.

An invalid pattern, `by()` or `max_depth` surfaces as a `TraverserError` from the terminal step:
`ValueError::InvalidGlobPattern` (under `TraverserError::Value`),
`TraverserError::InvalidModulator` or `TraverserError::InvalidOption`.
