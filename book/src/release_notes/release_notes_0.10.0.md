# Release 0.10.0

A minor release about memory. Importing a graph file no longer needs several times the memory of
the graph it builds: GraphSON and GraphML imports store every edge as soon as they read it, a new
store written from an imported graph no longer holds two graphs, the dev server streams its
`--graph` file instead of reading it whole, the browser playground reads a loaded file in chunks
instead of keeping three copies, and "Save graph" finds its warnings without loading the saved
file back. A 983 MB GraphSON file (500 000 vertices, 4 million edges, a 941 MB graph) now loads
with a peak of 1.05 GB instead of 4.0 GB, and `graphersal store create --from` writes it as a
store with a peak of 1.5 GB instead of 5.1 GB.

One change of behaviour needs a look before upgrading: an edge to a vertex that is neither in the
file nor in the target graph now fails the import (read "Upgrading from 0.9.x"). The public API
changes only by additions; `cargo semver-checks` against 0.9.0 reports no break of `graphersal`,
`graphersal-session`, `graphersal-serve`, `graphersal-mcp` and `graphersal-storage-tests`. No
format changes: stores, snapshots, journals and backups of 0.9.x are read and written exactly as
before. The release also carries the web playground with its WebAssembly engine as an archive, for
hosts that serve a playground running in the browser.

## Upgrading from 0.9.x

**A dangling edge fails the import.** GraphSON skipped an edge whose other endpoint was neither in
the file nor in the target graph and listed it in the import report (`SkippedDanglingEdge`);
GraphML failed with `IOError::Graph(VertexNotFound)` at once. Both now fail at the end of the
import with the new `IOError::DanglingEdges`, which names the first missing vertex ids (up to
five), how many are missing and where the first such edge is (`line 3: edge "e1" from "a" to
"zz"`); a live import is rolled back as a whole, a load into a new graph returns no graph. Fix the
file (add the missing vertices or remove the edges) or import it into a graph that holds them.
`ImportNoteKind::SkippedDanglingEdge` stays for code that matches it but is `#[deprecated]` and
never reported (user decision 2026-11-28)
([Limits and errors](../users_guide/graphson.md#limits-and-errors)).

**GraphSON keeps the copy of an edge read first.** A line-form file lists every edge twice, in the
`outE` of its tail and the `inE` of its head. The copy read first is stored now (it was always the
`outE` copy); the other one is recognized by its id. `EdgeCopiesDisagree` reports a second copy
with the same endpoints and another label; one with other endpoints is another edge with the same
id (`SkippedDuplicateEdge`). Files Graphersal or TinkerPop write have identical copies.

**GraphML accepts edges before their nodes.** An edge may name a node that comes later in the
document, or a vertex already in the target graph (both failed with `VertexNotFound`).

**Additions only.** The new trait methods `GraphStorage::insert_vertex_stub` and
`GraphStorage::complete_vertex_stub` have default implementations (the trait is EXPERIMENTAL), the
new `IOError::DanglingEdges` variant and the new `io::ReloadPreview` enum are
`#[non_exhaustive]`, and the new methods (`Store::create_with_dir`,
`Store::create_layer_with_dir`, `GraphMLExport::reload_preview`, `GraphSONExport::reload_preview`,
`AnyGraph::reload_preview`, the session's `load_*_reader`) are additive. No code change is needed
to build against 0.10.0.

## File imports without an edge buffer

- **Edges are stored as they are read.** GraphSON and GraphML imports kept every edge (GraphSON
  both copies, with their properties, plus a map of the edges stored) until every vertex had been
  read. Each edge is now stored at once; an endpoint whose own record comes later in the file is
  stored as a *stub*, an empty vertex that the record completes with its labels and properties.
  The schema checks a stub when it is completed, with the connections of the edges it already has;
  an edge to a stub has its own properties checked at once. Change capture, the journal and commit
  hooks see a completed stub as one ordinary `AddVertex`. The 983 MB GraphSON file above: 4.0 GB
  peak before, 1.05 GB now; loading takes about 12 % longer
  ([Limits and errors](../users_guide/graphson.md#limits-and-errors)).
- **Stubs for every storage.** `GraphStorage::insert_vertex_stub(id)` and
  `GraphStorage::complete_vertex_stub(vertex, labels, properties)` (EXPERIMENTAL, like the trait)
  have default implementations that write through `insert_vertex`, `add_vertex_label` and
  `set_vertex_property`, which is right for a storage without a schema; `TraversalGraph`
  implements them with the schema rules above. The conformance suite has a new item,
  `a_completed_stub_holds_what_insert_vertex_stores`.

## Stores and servers

- **`Store::create_with` holds one graph.** It wrote the first snapshot and then opened the store,
  which loads the graph back from that snapshot, while the given graph was still alive. It now
  frees the graph first. `graphersal store create --from` of the 983 MB file: 5.06 GB peak in
  0.9.0, 1.51 GB now.
- **Create a store without opening it.** `Store::create_with_dir(dir, graph, options)` and
  `Store::<LayeredGraph>::create_layer_with_dir(dir, &base, layer, options)` write a new store from
  a graph without opening it, so nothing is loaded back: for a host that imports a file into a new
  store and serves it later. `Store::open` opens it as a cleanly closed store
  ([Opening and Closing](../persistence/store_open_close.md#create)).
- **The dev server streams its graph file.** `graphersal --server --graph <file>` read a GraphSON,
  GraphML or `.gsnap` file into memory before parsing it; it now reads it as it parses. The
  session has the reader forms `Session::load_graphson_reader`, `load_graphml_reader` and
  `load_snapshot_reader` (a `Read + Seek`), which the byte forms call. Start of the 983 MB file:
  2.03 GB peak before, 1.04 GB now.

## Web playground

- **A loaded file is read in chunks.** The browser playground held a loaded file three times: in
  the page (for Stop), in the copy posted to the engine's worker, and in the WebAssembly module's
  memory, which never shrinks, so that copy stayed reserved as long as the page was open. The
  worker now gets the `File` itself, a reference to the file on disk, and the module reads it in
  1 MiB slices; Stop restores the graph from that `File` (or from a `Blob` of a saved graph). A
  file changed or removed on disk after loading makes the restore fail with a message.
- **"Save graph" finds its warnings without loading.** It loaded the saved text back into a second
  graph to find what would not survive. The new `io::ReloadPreview`
  (`GraphMLExport::reload_preview`, `GraphSONExport::reload_preview`, `AnyGraph::reload_preview`)
  works out the same answer in one pass over the graph, without the text and without a second
  graph; a test compares it with a real load. GraphSON now also warns about a vector without a
  schema declaration, which loads back as a list of numbers.
- **The playground as a release archive.** `graphersal-playground-v0.10.0.tar.gz` on the GitHub
  Release holds the playground's static files with its WebAssembly engine, for a host that serves
  a playground running the engine in the browser (served as they are; `.wasm` as
  `application/wasm`).

## Fixes

- In the DSL, `property(T.id, __...)` after `add_v()`/`add_e()` stored the text
  "GraphTraversalSource" as the new element's id. The traversal now runs on each traverser that
  reaches the step, and its first result (a string, or an integer as its decimal string) is the
  id; no result, or a value of another type, fails the traversal with a help naming the step.
  `property(T.label, __...)` after `add_v()` sets the vertex label from the traversal; after
  `add_e()` it stays refused. Rust has `property_id_by(traversal)` and
  `property_label_by(traversal)` on `GraphTraversalSource` and `AnonymousTraversal`.
- A traversal passed where a step takes a value (`constant(__...)`, `inject(__...)`,
  `with_sack(__...)`, `with_side_effect(name, __...)`, `fold(__..., Operator)`,
  `with(key, __...)`, `CastPolicy.Default(__...)`, `property_json(...)`, the values of
  `property(map)`, `merge_v(map)`, `merge_e(map)` and `option(Merge.x, map)`, `V(__...)`,
  `E(__...)`, map lookups, saved-query defaults and job arguments, also inside lists and maps) was
  stored or compared as the text "GraphTraversalSource". It is now a script error naming the step,
  with a help showing the value form and the step that runs a traversal. `with_side_effect(name,
  g.v())` (a traversal that is not a bare graph) is refused for the same reason.
- Schema inference kept the connections of an edge label in a list searched for every edge: a
  label over many endpoint label pairs was quadratic (the `large` sample with its unlabeled edges
  saved as GraphSON, where they all become `edge`: 8.7 s, now 0.2 s).
- rustdoc tables escape their pipes, for rustc 1.100's `invalid-markdown-table` lint.
