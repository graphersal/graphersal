"""Type stubs of the `graphersal` extension module (Python bindings of the Graphersal graph
traversal engine). Shipped in the wheel by maturin together with a `py.typed` marker.
"""

import datetime
import os
import uuid
from typing import Any, Callable, Literal, Mapping, Optional, Sequence, Union

__all__ = [
    "Graph",
    "AccessPolicy",
    "DryRunResult",
    "Store",
    "Vertex",
    "Edge",
    "Interval",
    "Vector",
    "Embedder",
    "Model2Vec",
    "GraphersalError",
    "PermissionDeniedError",
    "CommitRejectedError",
    "EmbedPropertyError",
    "ReturnedClosureError",
]

#: A value a script parameter may take (a ``Vertex``/``Edge`` by its id and label). The date and
#: time values map to the engine's ``datetime`` (an aware ``datetime.datetime``),
#: ``localdatetime`` (a naive one), ``date``, ``time`` (a naive ``datetime.time``), ``offsettime``
#: (a ``datetime.time`` with a ``datetime.timezone``) and
#: ``duration`` (a ``timedelta`` or an ``Interval``); a ``pandas.Timestamp`` / ``pandas.Timedelta``
#: (subclasses of ``datetime`` / ``timedelta``) passes with its nanoseconds. A one-dimensional
#: ``float32``/``float64`` buffer (a ``numpy.ndarray``) is accepted as an f32 ``Vector`` too.
#:
#: A returned value with digits below the microsecond is a ``pandas.Timestamp`` (a ``datetime`` or
#: ``localdatetime``) or ``pandas.Timedelta`` (a ``duration`` without months within an int64 of
#: nanoseconds, else an ``Interval``) and needs the extra ``graphersal[pandas]`` (else
#: ``ImportError``); a ``time`` or ``offsettime`` with such digits raises ``ValueError``.
_ParamValue = Union[
    int,
    float,
    str,
    bool,
    None,
    uuid.UUID,
    datetime.datetime,
    datetime.date,
    datetime.time,
    datetime.timedelta,
    "Interval",
    "Vector",
    "Vertex",
    "Edge",
]

class Interval:
    """A ``duration`` with months (PostgreSQL ``interval``), which ``datetime.timedelta`` cannot
    hold because a month has no fixed length. A query returns a duration without months as a
    ``timedelta`` (a ``pandas.Timedelta`` with nanoseconds), and one with months, or one longer
    than those hold, as an ``Interval``; all are accepted as parameters.
    Immutable; equal when all three fields are equal (``Interval("P1M") != Interval(0, 30)``, as
    in the engine). ``repr`` is ``Interval('P1Y2M')``, ``str`` the ISO 8601 text."""

    def __init__(self, months: Union[int, str], days: int = 0, micros: int = 0) -> None:
        """``Interval(months, days=0, micros=0)`` (int32 months and days, int64 microseconds) or
        ``Interval("P1Y2M3DT4H")`` from ISO 8601 text (up to nine fraction digits)."""
    @property
    def months(self) -> int:
        """The months (a year is 12)."""
    @property
    def days(self) -> int:
        """The days (a day is not always 24 hours in a time zone)."""
    @property
    def micros(self) -> int:
        """The time part in microseconds, exact over the whole range (up to an int64 of
        seconds, beyond an int64 of microseconds). ``ValueError`` when the time part has digits
        below the microsecond (read ``nanos``): never cut."""
    @property
    def nanos(self) -> int:
        """The time part in nanoseconds, exact over the whole range."""
    def to_iso(self) -> str:
        """The ISO 8601 text (``P1Y2M3DT4H``), the engine's canonical form."""
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Vector:
    """A vector (an embedding): a fixed-length list of numbers stored as one value, with the
    element kind ``"f32"`` (default), ``"f16"``, ``"i8"`` (symmetric scalar quantization: one
    code per component and one scale per vector) or ``"bit"`` (one sign bit per component). A
    vector property read back is a ``Vector``; a ``Vector`` passed as a parameter is a vector in
    the script (``g.v().has("emb", q)``). Immutable; equal when element, dimension and
    components are equal. Embeddings are computed outside the engine (book page "Vectors")."""

    def __init__(self, values: Any, element: Literal["f32", "f16", "i8", "bit"] = "f32") -> None:
        """``Vector(values, element="f32")``: ``values`` is a list (or tuple) of numbers or any
        one-dimensional ``float32``/``float64`` buffer (a ``numpy.ndarray``, ``array.array``),
        read without a ``numpy`` dependency. Another element quantizes the values. A NaN, an
        infinity, an empty list or more than 65536 components raise ``ValueError``."""
    @property
    def element(self) -> str:
        """The element kind: ``"f32"``, ``"f16"``, ``"i8"`` or ``"bit"``."""
    @property
    def dimension(self) -> int:
        """The number of components (also ``len(v)``)."""
    def tolist(self) -> list[float]:
        """The components as floats (dequantized: an ``i8`` component is ``code * scale``, a
        ``bit`` one ``1.0`` or ``-1.0``); ``numpy.array(v.tolist(), dtype="float32")`` gives an
        array."""
    def quantize(self, element: Literal["f32", "f16", "i8", "bit"]) -> "Vector":
        """This vector in another element kind (``quantize("f32")`` dequantizes)."""
    def __len__(self) -> int: ...
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

#: What an ``Embedder``'s callable returns for N texts: N rows (each a list of floats, a
#: one-dimensional ``float32``/``float64`` buffer or a ``Vector``) or ONE two-dimensional
#: ``float32``/``float64`` buffer of shape ``(N, dimension)`` (a ``numpy.ndarray``).
_EmbedOutput = Any

class Embedder:
    """An embedder: turns texts into vectors for the ``embed`` step, ``embed(text)``,
    ``Graph.embed_property`` and text queries (``nearest("emb", "a question", 5)``). Register it
    with ``Graph(embedders=[..])`` or ``graph.register_embedder``. Immutable."""

    def __init__(
        self,
        name: str,
        fn: Callable[[list[str]], _EmbedOutput],
        *,
        dimension: int,
        element: Literal["f32", "f16", "i8", "bit"] = "f32",
        metric: Literal["cosine", "dot", "l2", "hamming"] = "cosine",
        max_batch: int = 64,
        deterministic: bool = False,
        local: bool = True,
        model: Optional[str] = None,
    ) -> None:
        """``fn(texts)`` gets a ``list[str]`` of at most ``max_batch`` texts and returns one row
        per text (see ``_EmbedOutput``). A wrong number of rows or of values per row raises
        ``GraphersalError`` ("returned a wrong answer"); an exception ``fn`` raises (also
        ``KeyboardInterrupt``) fails the call with its type and message. ``model`` (default:
        ``name``) is the model identity the schema records for the vectors the embedder
        produces: keep it stable across runs and hosts and change it whenever the model
        changes. An invalid name, a reserved one (``"hash-ngram"``, ``"hash-ngram/*"``), a
        dimension outside 1..65536 or ``max_batch`` 0 raise ``ValueError``."""
    @property
    def name(self) -> str:
        """The registry name."""
    @property
    def model(self) -> str:
        """The model identity the schema records."""
    @property
    def dimension(self) -> int:
        """The number of components of every vector."""
    @property
    def element(self) -> str:
        """The element kind of the vectors."""
    @property
    def metric(self) -> str:
        """The metric the vectors are meant for."""
    @property
    def max_batch(self) -> int:
        """The most texts per call."""
    @property
    def deterministic(self) -> bool:
        """Whether the same text always gives the same bits."""
    @property
    def local(self) -> bool:
        """Whether the embedder runs locally (sends no text anywhere)."""
    def embed(self, texts: Sequence[str]) -> list[Vector]:
        """The vectors of ``texts``, checked like inside the engine; raises
        ``GraphersalError``."""

class Model2Vec(Embedder):
    """A Model2Vec static embedding model loaded from a LOCAL directory; the library never
    downloads (fetch the files first, e.g. ``huggingface_hub.snapshot_download``)."""

    def __init__(
        self,
        name: str,
        path: Union[str, os.PathLike[str]],
        *,
        model: Optional[str] = None,
        max_tokens: int = 512,
        normalize: Optional[bool] = None,
        max_batch: int = 256,
    ) -> None:
        """``path`` holds ``config.json``, ``tokenizer.json`` and ``model.safetensors`` (or the
        sentence-transformers layout). ``model`` is the Hugging Face repository the files come
        from (``"minishlab/potion-base-8M"``): the model identity becomes
        ``model2vec/<model>@<12 hex>``, the same as ``graphersal embed-model download`` of the
        same files. Without it the directory's ``graphersal-embed-model.json`` names it, else
        the identity is ``model2vec@<12 hex>``; a ``model`` differing from that file raises
        ``GraphersalError``, as does a directory holding no readable model. ``normalize``
        overrides the config's L2 normalization (and becomes part of the identity). Loading
        releases the GIL."""
    # The members of ``Embedder``, with what they are for a Model2Vec.
    @property
    def name(self) -> str:
        """The registry name."""
    @property
    def model(self) -> str:
        """``model2vec/<repo>@<12 hex>`` or ``model2vec@<12 hex>`` (a hash of the three model
        files, never a path: a copied directory keeps it)."""
    @property
    def dimension(self) -> int:
        """The model's dimension."""
    @property
    def element(self) -> str:
        """``"f32"``."""
    @property
    def metric(self) -> str:
        """``"cosine"``."""
    @property
    def max_batch(self) -> int:
        """The most texts per call (``max_batch=``)."""
    @property
    def deterministic(self) -> bool:
        """``False``: the bits may differ between Graphersal builds."""
    @property
    def local(self) -> bool:
        """``True``: the model runs in the process."""
    def embed(self, texts: Sequence[str]) -> list[Vector]:
        """The vectors of ``texts``."""

class Vertex:
    """A vertex a query returned (detached: id, labels and a snapshot of its properties), like
    gremlin-python's ``Vertex``. ``id``, ``label`` and ``labels`` are the vertex's own;
    ``properties`` and ``v["name"]`` are its properties only, so a property named ``label`` is
    ``v["label"]`` and never hides ``v.label``. Equal to (and hashed like) another vertex of the
    same id; ``repr`` is ``v[1]``. Can be passed back as a parameter (``g.V(v)``)."""

    @property
    def id(self) -> str: ...
    @property
    def label(self) -> Optional[str]:
        """The primary label, ``None`` for a vertex without a label."""
    @property
    def labels(self) -> list[str]:
        """The full label set, primary label first."""
    @property
    def properties(self) -> dict[str, Any]:
        """The properties (never the id or the label)."""
    def __getitem__(self, key: str) -> Any:
        """The property ``key``; ``KeyError`` when the vertex has none of that name."""
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Edge:
    """An edge a query returned (detached: id, label, endpoints and a snapshot of its
    properties), like gremlin-python's ``Edge``. ``out_v``/``in_v`` are vertices without
    properties; ``properties`` and ``e["weight"]`` are the edge's properties only. Equal to (and
    hashed like) another edge of the same id; ``repr`` is ``e[0][1-knows->2]``."""

    @property
    def id(self) -> str: ...
    @property
    def label(self) -> Optional[str]:
        """The edge's label, ``None`` for an edge without a label."""
    @property
    def out_v(self) -> Vertex:
        """The outgoing vertex (id and label only)."""
    @property
    def in_v(self) -> Vertex:
        """The incoming vertex (id and label only)."""
    @property
    def properties(self) -> dict[str, Any]:
        """The properties (never the id, the label or the endpoints)."""
    def __getitem__(self, key: str) -> Any:
        """The property ``key``; ``KeyError`` when the edge has none of that name."""
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

#: The name of an action kind of an ``AccessPolicy`` rule (``"*"``: every one).
_Action = Literal["read", "create", "update", "delete", "execute", "custom", "*"]

#: The name of a resource kind of an ``AccessPolicy`` rule (``"*"``: every one).
_Resource = Literal["data", "schema", "file", "option", "definition", "custom", "embedder", "*"]

class GraphersalError(Exception):
    """Raised for any script/engine error: parse errors, traversal failures, GraphSON/GraphML import
    errors. Subclasses name the failures a caller handles on purpose."""

    #: The engine's guidance text for this error, or ``None`` when it has none.
    help: Optional[str]
    #: Why a ``Store`` operation was refused (``"name_taken"``, ``"backup_running"``,
    #: ``"closed"``, ...: the names of ``graphersal::persist::StoreFailure``), or ``None`` for
    #: every other error.
    cause: Optional[str]

class PermissionDeniedError(GraphersalError):
    """A script made a request (an action on a resource) its ``policy`` does not allow, or
    used a file path outside the policy's file root. The denied traversal did not run."""

class CommitRejectedError(GraphersalError):
    """A commit hook of the graph vetoed the commit; everything the unit changed was rolled
    back. Only raised for graphs whose hooks were registered on the Rust side."""

class EmbedPropertyError(GraphersalError):
    """``Graph.embed_property`` failed. The pages before the failure stay committed, the
    failing one was rolled back; a second run continues where it stopped."""

    #: The job's report up to the failure (the dict ``embed_property`` returns).
    report: dict[str, Any]

class ReturnedClosureError(GraphersalError):
    """A script run as one unit (``atomic=True``, ``Graph.dry_run``) returned a closure or
    function pointer. Everything the script changed was rolled back."""

class AccessPolicy:
    """Which actions a script may perform on which kinds of resource, plus an optional file
    root that confines file access to one directory. Immutable and hashable;
    ``allow``/``deny``/``with_file_root`` return a new value. ``"*"`` stands for every action
    or every resource."""

    def __init__(
        self,
        *rules: tuple[_Action, _Resource],
        file_root: Union[str, os.PathLike[str], None] = None,
    ) -> None:
        """Allows exactly ``rules`` (``(action, resource)`` tuples); ``file_root`` confines
        file access to that directory (it does not allow file access by itself). An unknown
        name raises ``ValueError``."""
    @staticmethod
    def allow_all() -> AccessPolicy:
        """Every request, no file root: what ``Graph.execute`` uses without ``policy``."""
    @staticmethod
    def deny_all() -> AccessPolicy:
        """Nothing allowed: a script can only compute with its own values."""
    @staticmethod
    def read_only() -> AccessPolicy:
        """``"read"`` on ``"data"``, ``"schema"`` and ``"definition"``, ``"execute"`` on
        ``"definition"`` (calling a saved query) and on ``"embedder"`` (running an embedder)."""
    def allow(self, action: _Action, resource: _Resource) -> AccessPolicy:
        """A copy that also allows ``action`` on ``resource``."""
    def deny(self, action: _Action, resource: _Resource) -> AccessPolicy:
        """A copy that no longer allows ``action`` on ``resource``."""
    def with_file_root(self, path: Union[str, os.PathLike[str], None]) -> AccessPolicy:
        """A copy whose file access is confined to the directory ``path`` (``None`` removes
        the root). Relative script paths resolve under it; paths leaving it are denied."""
    def allows(self, action: _Action, resource: _Resource) -> bool:
        """Whether ``action`` on ``resource`` is allowed."""
    @property
    def rules(self) -> frozenset[tuple[str, str]]:
        """The allowed ``(action, resource)`` pairs."""
    @property
    def file_root(self) -> Optional[str]:
        """The file root, or ``None``."""
    def __eq__(self, other: object) -> bool: ...
    def __hash__(self) -> int: ...

class Graph:
    """A Graphersal graph. Scripts in the project's Gremlin dialect run against it through
    ``execute``/``query`` (and ``dry_run`` to preview their changes); results come back as
    native Python values."""

    def __init__(
        self,
        *,
        memory_limit: Union[int, Literal["auto"], None] = None,
        memory_headroom: Optional[int] = None,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> None:
        """A new, empty graph.

        ``embedders``: the ``Embedder``s (also ``Model2Vec``) its scripts and
        ``embed_property`` call, beside the built-in ``"hash-ngram/1"``; ``default_embedder``
        names the one used when a call names none (default ``"hash-ngram/1"``). A duplicate or
        reserved name raises ``ValueError``. Every constructor and loader takes both keywords.

        ``memory_limit``: the memory budget of every traversal on this graph, in bytes, or
        ``"auto"`` (the cgroup memory limit of the process minus ``memory_headroom`` bytes;
        no budget without a cgroup limit). A traversal that would exceed it raises
        ``GraphersalError`` and is rolled back. ``None`` (the default): no budget.

        The budget is compared with the exact live Rust heap of the extension (it installs a
        tracking allocator), which does not count Python objects: with ``"auto"`` choose a
        ``memory_headroom`` that also covers the Python side of the process."""
    @staticmethod
    def tinkerpop_modern(
        *,
        memory_limit: Union[int, Literal["auto"], None] = None,
        memory_headroom: Optional[int] = None,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Graph:
        """The standard TinkerPop "modern" toy graph (6 vertices, 6 edges); the keywords are
        those of ``Graph()``."""
    @staticmethod
    def from_graphml(
        path: str,
        *,
        schema: Union[Mapping[str, Any], str, None] = None,
        mode: Optional[Literal["none", "open", "closed"]] = None,
        memory_limit: Union[int, Literal["auto"], None] = None,
        memory_headroom: Optional[int] = None,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Graph:
        """Loads a graph from a GraphML file (see also ``from_graphson``). GraphML carries no
        schema: ``schema`` (a mapping or JSON text in the schema format; ``mode`` replaces its
        own mode) is set on the new graph first, then the file is imported as one unit, so
        declared ``uuid``/``array``/``object`` properties come back typed and in
        ``open``/``closed`` a violation fails the load. ``mode`` without ``schema`` raises
        ``ValueError``. The other keywords are those of ``Graph()``."""
    @staticmethod
    def from_graphson(
        path: str,
        *,
        schema: Union[Mapping[str, Any], str, None] = None,
        mode: Optional[Literal["none", "open", "closed"]] = None,
        memory_limit: Union[int, Literal["auto"], None] = None,
        memory_headroom: Optional[int] = None,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Graph:
        """Loads a graph from a GraphSON 3.0 file (one vertex per line, TinkerPop's default, or
        one ``{"vertices": [...]}`` document; 2.0 and untyped 1.0 read too). Skipped data
        (dates, types Graphersal cannot hold, edges to missing vertices) is reported in one
        ``UserWarning``. ``schema``/``mode`` as in ``from_graphml``; the other keywords are
        those of ``Graph()``."""
    def import_graphml(self, path: str) -> None:
        """Imports a GraphML file into this live graph as one unit (all or nothing); the stored
        schema restores declared ``uuid``/``array``/``object`` properties."""
    def import_graphson(self, path: str) -> None:
        """Imports a GraphSON file into this live graph as one unit (all or nothing); skipped
        data is reported in one ``UserWarning``."""
    def export_graphson(self, path: str) -> None:
        """Writes this graph out as GraphSON 3.0 (line form, typed values), which TinkerGraph's
        ``g.io(path).read()`` loads. The file carries no schema."""
    def export_graphml(self, path: str) -> None:
        """Writes this graph out as GraphML. GraphML carries no schema: save it separately and
        set it on the target before importing to restore declared types (uuid, array, object)."""
    def save(self, path: str, *, name: Optional[str] = None) -> dict[str, int]:
        """Writes this graph as a packed snapshot (lossless binary, ``.gsnap``): data, schema,
        lineage id, commit position and auto-id sequences. Returns ``{"commit_seq",
        "vertex_count", "edge_count"}``."""
    @staticmethod
    def load(
        path: str,
        *,
        memory_limit: Union[int, Literal["auto"], None] = None,
        memory_headroom: Optional[int] = None,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Graph:
        """Reads a packed snapshot written by ``save`` (no schema validation; damage raises
        ``GraphersalError``). A numeric ``memory_limit`` also refuses a snapshot whose size
        estimate does not fit; the keywords are those of ``Graph()``."""
    def start_journal(
        self, path: str, *, durability: Union[Literal["every_commit", "os"], float, None] = None
    ) -> None:
        """Attaches a journal (write-ahead log) writing to a new file at ``path``: every commit
        is written (and fsynced per ``durability``) before it becomes final; a failed write rolls
        it back. Save a snapshot at the same position first so ``recover`` has a base."""
    def mark(self, name: str) -> dict[str, Any]:
        """Sets a named mark in the graph's journal at the current position; returns
        ``{"name", "commit_seq", "time"}``. Needs a journal; names are unique per journal."""
    @staticmethod
    def recover(
        snapshot: Optional[str],
        journals: list[str],
        *,
        commit_seq: Optional[int] = None,
        time: Optional[int] = None,
        mark: Optional[str] = None,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> tuple[Graph, dict[str, Any]]:
        """Loads ``snapshot`` (or an empty graph) and replays ``journals`` up to the end or the
        one given target; returns ``(graph, report)``. The graph has no journal attached."""
    def set_limits(
        self,
        *,
        max_operations: Optional[int] = None,
        max_string_size: Optional[int] = None,
        max_array_size: Optional[int] = None,
        max_map_size: Optional[int] = None,
        max_value_depth: Optional[int] = None,
        unlimited: bool = False,
    ) -> None:
        """Changes the resource limits of the scripts this graph runs (``0`` = unlimited;
        ``max_value_depth``, the deepest nesting of lists and maps in one value, is never
        unlimited: ``0`` means its default, 128)."""
    def get_schema(self, *, policy: Optional[AccessPolicy] = None) -> Optional[dict[str, Any]]:
        """The stored schema (the schema format as a ``dict``), or ``None`` without one. Never
        inferred."""
    def set_schema(
        self,
        schema: Union[Mapping[str, Any], str],
        mode: Optional[Literal["none", "open", "closed"]] = None,
        *,
        policy: Optional[AccessPolicy] = None,
    ) -> None:
        """Replaces the stored schema (a ``dict`` or JSON text in the schema format); ``mode``
        replaces the schema's own mode, so ``graph.set_schema(graph.infer_schema(),
        mode="closed")`` freezes the structure the graph has. Existing data is validated first;
        on violations nothing changes and ``GraphersalError`` carries the report. ``{}`` removes
        the schema."""
    def patch_schema(
        self, patch: Union[Mapping[str, Any], str], *, policy: Optional[AccessPolicy] = None
    ) -> dict[str, Any]:
        """Applies a JSON Merge Patch (RFC 7396) to the stored schema and stores the result
        like ``set_schema``; returns the new schema."""
    def validate_schema(
        self,
        schema: Union[Mapping[str, Any], str],
        mode: Optional[Literal["none", "open", "closed"]] = None,
        *,
        policy: Optional[AccessPolicy] = None,
    ) -> dict[str, Any]:
        """A dry run of ``set_schema(schema, mode)``: ``{"violations": {...}, "diff": [...],
        "compatible": bool}``. Nothing is applied."""
    def validate_schema_patch(
        self, patch: Union[Mapping[str, Any], str], *, policy: Optional[AccessPolicy] = None
    ) -> dict[str, Any]:
        """``validate_schema`` of the stored schema patched with ``patch``."""
    def infer_schema(self, *, policy: Optional[AccessPolicy] = None) -> dict[str, Any]:
        """The schema the data satisfies (mode ``"none"``); stores nothing."""
    def diff_schema(
        self, schema: Union[Mapping[str, Any], str], *, policy: Optional[AccessPolicy] = None
    ) -> dict[str, Any]:
        """``{"changes": [...], "compatible": bool}`` from the stored schema to ``schema``."""
    def queries(
        self, folder: Optional[str] = None, *, policy: Optional[AccessPolicy] = None
    ) -> list[dict[str, Any]]:
        """The saved queries (in ``folder`` and below it): ``name``, ``folder``,
        ``description``, ``params``, ``status`` and ``error``, ``writes``, ``effects`` and
        ``denied_writes`` (an effect ``{"action", "element", "label", "text"}``), ``language``
        (``"rhai"`` or ``"gql"``) and ``dialect`` each."""
    def get_query(
        self, name: str, *, policy: Optional[AccessPolicy] = None
    ) -> Optional[dict[str, Any]]:
        """The saved query ``name`` with its ``body``, ``dialect`` and ``meta``, or ``None``."""
    def define_query(
        self, spec: Mapping[str, Any], *, policy: Optional[AccessPolicy] = None
    ) -> None:
        """Stores (or replaces) a saved query: ``{"name", "body", "language"?, "folder"?,
        "description"?, "params"?, "meta"?}`` as for the DSL's ``g.define_query``; with
        ``"language": "gql"`` the body is one plain GQL query whose ``$name`` parameters are
        exactly the declared ones (``"rhai"``, the default: one Rhai function)."""
    def drop_query(self, name: str, *, policy: Optional[AccessPolicy] = None) -> bool:
        """Removes the saved query; ``False`` when there is none."""
    def define_compression(
        self,
        name: str,
        label: str,
        path: str,
        element: str = "vertex",
        min_bytes: Optional[int] = None,
        dictionary: Optional[bool] = None,
        codec: Optional[str] = None,
        *,
        policy: Optional[AccessPolicy] = None,
    ) -> None:
        """Stores (or replaces) a compression rule: strings of ``element``s labelled ``label``
        at ``path`` are kept compressed in memory (``codec="lz4"``, default: ``min_bytes`` 256,
        a dictionary) or stored once per distinct value (``codec="dedup"``: ``min_bytes`` 25,
        no dictionary) and read plain."""
    def compressions(self, *, policy: Optional[AccessPolicy] = None) -> list[dict[str, Any]]:
        """The compression rules with their current stats; ``served`` is ``False`` (with a
        ``reason``) for a rule this version keeps but does not apply."""
    def drop_compression(self, name: str, *, policy: Optional[AccessPolicy] = None) -> bool:
        """Removes the compression rule and decompresses its values; ``False`` when there is
        none."""
    def recompress(self, name: str, *, policy: Optional[AccessPolicy] = None) -> None:
        """Rebuilds the rule's dictionary from the current values and re-encodes them."""
    def memory_report(
        self,
        *,
        sample_every: Optional[int] = None,
        max_elements: Optional[int] = None,
        max_rows: Optional[int] = None,
        max_labels: Optional[int] = None,
        policy: Optional[AccessPolicy] = None,
    ) -> dict[str, Any]:
        """Where the graph's memory goes per label and top-level property (rows, labels,
        shapes, indexes, compression rules, structure); changes nothing. ``max_elements`` /
        ``sample_every`` sample, ``max_rows`` / ``max_labels`` keep the largest rows."""
    def estimate_compression(
        self,
        label: str,
        path: str,
        element: str = "vertex",
        min_bytes: Optional[int] = None,
        dictionary: Optional[bool] = None,
        codec: Optional[Union[str, list[str]]] = None,
        *,
        policy: Optional[AccessPolicy] = None,
    ) -> dict[str, Any]:
        """What compression rules for ``element``s labelled ``label`` at ``path`` would hold
        over the current values (candidates, best, current rule, warnings); changes nothing."""
    def create_index(
        self,
        label: str,
        key: str,
        element: str = "vertex",
        name: Optional[str] = None,
        type: str = "memory-ordered",
        *,
        transform: Optional[str] = None,
        policy: Optional[AccessPolicy] = None,
    ) -> None:
        """Declares (or replaces) a property index: the ``element``s labelled ``label``,
        ordered by their top-level property ``key`` (name: ``<label>_<key>`` by default, with
        ``_to_lower`` appended for that transform) in an index of ``type``
        (``"memory-ordered"``, the only type this version knows). ``transform="to_lower"``
        indexes the lowered text of string values for ``has(key, __.to_lower().is(..))``. An
        unknown type or transform, or a key the schema declares as values the index cannot
        hold, is refused."""
    def indexes(self, *, policy: Optional[AccessPolicy] = None) -> list[dict[str, Any]]:
        """The property indexes with their type, whether they are served (``reason`` when
        not) and their current stats."""
    def drop_index(self, name: str, *, policy: Optional[AccessPolicy] = None) -> bool:
        """Drops the property index; ``False`` when there is none."""
    def register_embedder(self, embedder: Embedder, *, default: bool = False) -> None:
        """Adds ``embedder`` to this graph's embedders (``default=True`` makes it the default).
        A running execution keeps the embedders it started with. A name already registered,
        or a reserved one, raises ``ValueError``."""
    def embedders(self) -> list[dict[str, Any]]:
        """This graph's embedders, the default first: ``name``, ``model``, ``dimension``,
        ``element``, ``metric``, ``max_batch``, ``deterministic``, ``local``, ``default``."""
    def embed_property(
        self,
        *,
        label: str,
        source: str,
        target: str,
        element: Literal["vertex", "edge"] = "vertex",
        embedder: Optional[str] = None,
        only_missing: bool = True,
        batch: Optional[int] = None,
    ) -> dict[str, Any]:
        """Embeds the text property ``source`` of every ``element`` labelled ``label`` into its
        vector property ``target``, page by page (the embedder runs without the graph's lock;
        each page is one unit). Needs an attached schema (mode ``open``/``closed``), where the
        job records the embedder's model for ``target``. Returns the report (``embedded``,
        ``skipped_no_text``, ``not_text``, ``skipped_changed``, ``skipped_removed``, ``units``,
        ``schema_changed``, ``elapsed_ms``, ``embedder``, ``model``); a failure raises
        ``EmbedPropertyError`` with ``report``. Releases the GIL; Ctrl+C cancels between pages
        (the ``KeyboardInterrupt`` carries ``report`` too)."""
    def call_query(
        self,
        name: str,
        params: Optional[Mapping[str, _ParamValue]] = None,
        *,
        policy: Optional[AccessPolicy] = None,
    ) -> Any:
        """Calls the saved query ``name`` with named arguments (``g.query(name, #{..})``) and
        returns its result like ``execute``. A query whose body writes data (``writes`` in
        ``queries()``) changes only its inferred ``effects``, as one transaction; a query
        written in GQL returns its rows as ``dict``s like ``g.gql()``."""
    def execute(
        self,
        script: str,
        params: Optional[Mapping[str, _ParamValue]] = None,
        *,
        atomic: bool = False,
        policy: Optional[AccessPolicy] = None,
    ) -> Any:
        """Evaluates ``script`` and returns its result as native Python values.

        ``atomic=False``: every traversal is its own unit (a failing one rolls back only
        itself). ``atomic=True``: the whole script is one unit (a failing script leaves
        nothing behind). ``policy=None`` allows everything (a Python program is a trusted local
        host)."""
    def query(
        self,
        script: str,
        params: Optional[Mapping[str, _ParamValue]] = None,
        *,
        atomic: bool = False,
        policy: Optional[AccessPolicy] = None,
    ) -> Any:
        """Identical to ``execute``; a read-oriented alias (it does not restrict the script by
        itself: pass ``policy=AccessPolicy.read_only()`` for that)."""
    def dry_run(
        self,
        script: str,
        params: Optional[Mapping[str, _ParamValue]] = None,
        *,
        policy: Optional[AccessPolicy] = None,
    ) -> DryRunResult:
        """Runs ``script`` as one unit and always rolls it back. Returns a ``DryRunResult``:
        ``.result`` is what ``execute`` would have returned, ``.changes`` the would-be changes
        as plain data (``{"commit_seq": 0, "committed_at": 0, "node_sequence": ..,
        "edge_sequence": .., "mutations": [{"op": ..., ...}, ...]}``, uuids tagged as
        ``{"@type": "g:UUID", "@value": "..."}``; ``commit_seq``/``committed_at`` are ``0``,
        never committed; the sequences are the auto-id sequences after the run). The graph is
        left as it was and no commit hook runs. A returned closure raises ``ReturnedClosureError``."""

class DryRunResult:
    """What ``Graph.dry_run`` returns. Immutable; more fields may be added later."""

    @property
    def result(self) -> Any:
        """What ``execute`` would have returned, as native Python values."""
    @property
    def changes(self) -> dict[str, Any]:
        """The would-be change set as plain data: ``{"commit_seq": 0, "committed_at": 0,
        "node_sequence": .., "edge_sequence": .., "mutations": [...]}``."""
    def __repr__(self) -> str: ...

class Store:
    """One graph kept in one directory (the Store of the book's "Persistence" part): every commit
    of ``store.graph`` is written to the store's write-ahead log before it returns. Open
    read-write with its lock until ``close()`` (also a context manager). The
    ``embedders``/``default_embedder`` keywords of every opener give ``store.graph`` its
    embedders (as ``Graph()``); every ``store.graph`` shares them."""

    @staticmethod
    def create(
        path: str,
        *,
        on_damage: str = "maintenance",
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Store:
        """Creates a store for an empty graph in ``path`` (missing or empty) and opens it.
        ``on_damage`` (``"maintenance"`` or ``"continue"``) is the damage policy, fixed for the
        store's life."""
    @staticmethod
    def open(
        path: str,
        *,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Store:
        """Opens the store in ``path`` read-write and runs its recovery; a second writer raises."""
    @staticmethod
    def create_file(
        path: str,
        *,
        on_damage: str = "maintenance",
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Store:
        """Creates a single-file store (the whole store in the one file ``path``) and opens it;
        ``Store.create`` of a path ending in ``.gstore`` does the same."""
    @staticmethod
    def open_file(
        path: str,
        *,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Store:
        """Opens the single-file store ``path`` read-write (``Store.open`` of an existing file
        does the same)."""
    @staticmethod
    def convert(source: str, target: str, *, single_file: bool = False) -> dict[str, Any]:
        """Copies the closed store in ``source`` into ``target`` (a new directory, or a single
        file: a ``.gstore`` path or ``single_file=True``), verified: ``{"files", "bytes"}``."""
    @staticmethod
    def upgrade(
        source: str,
        target: str | None = None,
        *,
        in_place: bool = False,
        single_file: bool = False,
    ) -> dict[str, Any]:
        """Converts a store (or backup, or ``.gstore`` file) written by an older Graphersal
        (0.1.x) to the current format, without the older Graphersal: into a new store in
        ``target`` (``source`` unchanged), or ``in_place=True``. Everything is carried; the
        source is verified first (a damaged one raises with ``err.cause ==
        "damaged_source"``). ``{"from_version", "to_version", "already_current", "in_place",
        "backup", "files", "bytes"}``."""
    def prune(self, up_to: int) -> dict[str, Any]:
        """Removes snapshots and WAL only needed for targets before ``up_to`` (the last two
        snapshots stay): ``{"kept_from", "snapshots_removed", "segments_removed",
        "marks_unreachable"}`` (the marks whose history is gone now; ``marks()`` leaves them
        out). It does not ask first (unlike the CLI, which refuses without ``--drop-marks``):
        ``compaction_advice()["marks_unreachable"]`` names the marks a prune to now
        would make unreachable."""
    def compact(self) -> dict[str, Any]:
        """A prune up to the current commit, then (a single file) the file rewritten without its
        garbage: ``{"snapshots_removed", "segments_removed", "marks_unreachable", "compacted",
        "bytes_before", "bytes_after", "bytes_freed"}``. It does not ask first: check
        ``compaction_advice()["marks_unreachable"]`` before, as the CLI and the
        playground do."""
    def compaction_advice(
        self,
        *,
        min_ratio: Optional[float] = None,
        min_reclaimable: Optional[int] = None,
        min_size: Optional[int] = None,
    ) -> dict[str, Any]:
        """Whether ``compact()`` is worth it now (cheap: no data is read): ``{"recommended",
        "reason", "decided_by", "total_bytes", "live_bytes", "garbage_bytes", "prune_bytes",
        "prune_first", "reclaimable_bytes", "space_needed", "free_space", "marks_unreachable",
        ...}`` (``marks_unreachable``: the marks a prune to now would make unreachable)."""
    @property
    def graph(self) -> Graph:
        """The store's graph; every commit of it is durable before it returns."""
    def open_report(self) -> dict[str, Any]:
        """What the open did: ``{"clean_close", "commit_seq", "commits_replayed",
        "torn_tail_bytes"}``."""
    def workload_report(self, history: Optional[str] = None) -> dict[str, Any]:
        """The workload statistics of the store's graph: ``{"level", "queries",
        "properties", "locks", "jobs"}``, rows with their ``channel`` and the windows
        ``last_hour`` / ``last_two_days`` / ``last_ninety_days``; ``history`` (``"minutes"``,
        ``"hours"``, ``"days"``) adds every row's buckets of that tier."""
    def checkpoint(self, name: Optional[str] = None) -> dict[str, Any]:
        """Writes a snapshot of the current state; returns its listing."""
    def mark(self, name: str) -> dict[str, Any]:
        """Names the current position (unique in the store's history)."""
    def snapshots(self) -> list[dict[str, Any]]:
        """The snapshots, oldest first."""
    def marks(self) -> list[dict[str, Any]]:
        """The marks a target can reach, in order (a mark before the oldest snapshot is left
        out: a prune removed its history)."""
    def verify(self) -> dict[str, Any]:
        """Scrubs every checksum: ``{"ok", "problems", "notes"}``."""
    def fork(
        self,
        path: str,
        *,
        commit_seq: Optional[int] = None,
        time: Optional[int] = None,
        mark: Optional[str] = None,
    ) -> dict[str, Any]:
        """A new store in ``path`` from the state at the target, with a new lineage id; the
        snapshot dict plus ``"marks_not_carried"`` (``[{"name", "commit_seq"}]``: a fork starts
        without marks)."""
    def backup(
        self, path: str, *, full: bool = False, from_memory: bool = False
    ) -> dict[str, Any]:
        """A backup without blocking writers: full into an empty directory, incremental into an
        existing backup of this store (``full=True``: always full, the directory must be empty).
        Every checksum is read in the store and in the copy; damage raises and nothing is kept.
        ``from_memory=True`` (only after damage was found): the graph in memory is written, the
        store's files are not touched. The backup opens read-only until
        ``Store.restore_backup``."""
    @property
    def damage_policy(self) -> str:
        """``"maintenance"`` or ``"continue"``: fixed when the store was created."""
    @property
    def store_id(self) -> str:
        """The store's identity (a UUID string), fixed when it was created; a fork, repair or
        conversion gets a new one, a restored backup keeps it. An incremental backup requires
        the same id."""
    @property
    def damage_found(self) -> list[dict[str, Any]]:
        """Damage found on disk while the store is open: ``[{"found_by", "at", "problems"}]``."""
    @property
    def frozen_by_damage(self) -> bool:
        """Whether the store turned read-only because damage was found while it is open."""
    @staticmethod
    def open_backup(
        path: str,
        *,
        embedders: Optional[Sequence[Embedder]] = None,
        default_embedder: Optional[str] = None,
    ) -> Store:
        """Opens a backup read-only (listing, verify, fork work; writes raise)."""
    @staticmethod
    def restore_backup(path: str) -> dict[str, Any]:
        """Makes the backup the live store in place (same graph id); returns the cleared
        marker ``{"commit_seq", "taken_at"}``."""
    @property
    def backup_marker(self) -> Optional[dict[str, Any]]:
        """``None`` for a store; ``{"commit_seq", "taken_at"}`` for a backup opened read-only."""
    @property
    def maintenance(self) -> Optional[str]:
        """``None`` when healthy; the damage report (text) when the store opened read-only in
        maintenance mode."""
    def rollback(
        self,
        *,
        commit_seq: Optional[int] = None,
        time: Optional[int] = None,
        mark: Optional[str] = None,
        delete: bool = False,
    ) -> dict[str, Any]:
        """Rolls the store back in place; the history after the target moves to the attic."""
    def attic(self) -> list[dict[str, Any]]:
        """The attic entries: ``[{"id", "target", "last_commit_seq", "time"}]``."""
    def attic_restore(self, id: str) -> int:
        """Undoes the rollback of an attic entry (only while nothing was committed since)."""
    def repair_to(self, path: str) -> dict[str, Any]:
        """A repaired copy in a new directory: ``{"commit_seq", "lossless",
        "marks_not_carried", "lineage_approximate_before", "report"}``."""
    def close(self) -> None:
        """A clean close; the graph refuses commits afterwards."""
    def __enter__(self) -> Store: ...
    def __exit__(self, *args: Any) -> bool: ...
