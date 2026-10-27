//! The merge checkpoint through the public API (format spec 12.3; the test matrix of the CP2
//! task, sunny and rainy day): a checkpoint built from the newest snapshot and the WAL after
//! it loads to EXACTLY the graph a load of that snapshot plus a replay of the WAL builds (the
//! oracle: `Store::open_read_only` before the checkpoint), and where every segment is
//! rewritten its segment files are byte for byte what the streaming encoder writes for that
//! graph. Damage in any input is refused with nothing visible written; a failing directory at
//! every write step leaves a store that opens; commits go on during a merge.
//!
//! Crate tests (`persist::store::merge_tests`) cover what needs the internals: segment and
//! chunk boundaries, crafted WAL records (divergence, gaps), the exact bound, unknown kinds.
#![cfg(all(feature = "persist", feature = "script"))]

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use graphersal::catalog::DefinitionFlags;
use graphersal::catalog::{Definition, DefinitionKind, QueryDefinition};
use graphersal::persist::{
    self, CheckpointReport, Codec, DirEntry, DirLockGuard, EntryKind, LockKind, MemDir,
    PersistError, ReadOptions, RecoveryTarget, RollbackMode, Store, StoreDir, StoreFile,
    StoreOptions, WriteOptions,
};
use graphersal::prelude::*;
use graphersal::storage::PersistentStorage;

use crate::script_host::eval_value;

// ---------------------------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------------------------

/// Everything a graph holds, in a canonical text: identity, position, time, auto-id sequences,
/// schema, catalog (unknown definitions with their bytes), and every vertex and
/// edge with its labels in order and its properties in order (nested values and types through
/// their `Debug` form). The id index is checked on the way.
pub(crate) fn exact(graph: &TraversalGraph) -> Vec<String> {
    let info = persist::write_snapshot(graph, io::sink(), &WriteOptions::new()).unwrap();
    let mut out = vec![
        format!(
            "graph {:032x} at {} time {} sequences {}/{}",
            graph.graph_id(),
            graph.last_commit_seq(),
            graph.last_committed_at(),
            info.node_sequence,
            info.edge_sequence
        ),
        format!(
            "schema {}",
            graph
                .get_schema()
                .unwrap()
                .map(|s| s.to_json())
                .unwrap_or_default()
        ),
    ];
    for definition in graph.definitions().unwrap() {
        out.push(format!("definition {definition:?}"));
    }
    let mut elements = Vec::new();
    for v in graph.vertices() {
        let id = graph.vertex_id(v).unwrap().to_string();
        assert_eq!(graph.get_vertex_by_id(&id), Some(v), "index of {id}");
        let properties: Vec<String> = graph
            .vertex_properties(v)
            .map(|p| p.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        elements.push(format!(
            "v {id:?} {:?} {properties:?}",
            graph.vertex_labels(v)
        ));
    }
    for e in graph.edges() {
        let id = graph.edge_id(e).unwrap().to_string();
        assert_eq!(graph.get_edge_by_id(&id), Some(e), "index of {id}");
        let (from, to) = graph.edge_endpoints(e).unwrap();
        let properties: Vec<String> = graph
            .edge_properties(e)
            .map(|p| p.iter().map(|(k, v)| format!("{k}={v:?}")).collect())
            .unwrap_or_default();
        elements.push(format!(
            "e {id:?} {:?} {:?}->{:?} {properties:?}",
            graph.edge_label(e),
            graph.vertex_id(from).unwrap(),
            graph.vertex_id(to).unwrap()
        ));
    }
    elements.sort();
    out.extend(elements);
    out
}

/// `a` and `b` hold exactly the same: [`exact`] and the statistics.
pub(crate) fn same(a: &TraversalGraph, b: &TraversalGraph, label: &str) {
    assert_eq!(exact(a), exact(b), "{label}");
    assert_eq!(
        a.statistics().unwrap(),
        b.statistics().unwrap(),
        "{label}: statistics"
    );
}

/// The oracle of a store's files: the newest snapshot loaded and the WAL after it replayed.
pub(crate) fn oracle(dir: &Arc<dyn StoreDir>) -> TraversalGraph {
    Store::open_read_only(dir, RecoveryTarget::Latest)
        .unwrap()
        .graph
}

/// The newest snapshot of the store, loaded ALONE (no WAL), and its packed form.
pub(crate) fn newest_alone(dir: &Arc<dyn StoreDir>) -> (TraversalGraph, Vec<u8>) {
    let mut packed = Vec::new();
    Store::export_packed_dir(dir, None, &mut packed).unwrap();
    let graph = persist::read_snapshot(packed.as_slice(), &ReadOptions::new()).unwrap();
    (graph, packed)
}

/// The sections of a packed snapshot after the manifest: `schema.json` and the segments.
fn data_sections(packed: &[u8]) -> Vec<Vec<u8>> {
    let mut at = 16;
    let mut out = Vec::new();
    while at + 9 <= packed.len() - 4 {
        let kind = packed[at];
        let len = u64::from_le_bytes(packed[at + 1..at + 9].try_into().unwrap()) as usize;
        if kind != 1 {
            out.push(packed[at + 9..at + 9 + len].to_vec());
        }
        at += 9 + len;
    }
    out
}

/// Checks the newest snapshot against `expected` (the oracle): the same graph exactly, and,
/// when the merge rewrote every segment, the encoder's bytes for that graph.
pub(crate) fn assert_merged(
    dir: &Arc<dyn StoreDir>,
    expected: &TraversalGraph,
    report: &CheckpointReport,
    options: &StoreOptions,
    write: &WriteOptions,
    label: &str,
) {
    let _ = options;
    let (alone, packed) = newest_alone(dir);
    same(&alone, expected, label);
    assert_eq!(
        report.snapshot.commit_seq,
        expected.last_commit_seq(),
        "{label}"
    );
    if report.merged && report.segments_reused == 0 && report.segments_copied == 0 {
        let mut encoded = Vec::new();
        persist::write_snapshot(expected, &mut encoded, write).unwrap();
        assert!(
            data_sections(&packed) == data_sections(&encoded),
            "{label}: the merged segments differ from the encoder's"
        );
    }
}

/// Small chunks and segments: snapshots of many files, merges that reuse some of them.
pub(crate) fn small(codec: Codec) -> (StoreOptions, WriteOptions) {
    (
        StoreOptions::new()
            .with_checkpoint_after_bytes(0)
            .with_codec(codec)
            .with_chunk_bytes(300)
            .with_snapshot_segment_bytes(1500),
        WriteOptions::new()
            .with_codec(codec)
            .with_chunk_bytes(300)
            .with_segment_bytes(1500),
    )
}

pub(crate) fn shared(dir: &MemDir) -> Arc<dyn StoreDir> {
    Arc::new(dir.clone())
}

/// A checkpoint of the closed store in `dir`, checked against the oracle taken before it.
pub(crate) fn checkpoint_closed(
    dir: &Arc<dyn StoreDir>,
    options: &StoreOptions,
    write: &WriteOptions,
    label: &str,
) -> CheckpointReport {
    let expected = oracle(dir);
    let report = Store::checkpoint_dir(dir, options.clone(), None)
        .unwrap_or_else(|err| panic!("{label}: {err}"));
    assert_merged(dir, &expected, &report, options, write, label);
    report
}

/// A checkpoint of the open store, checked against its live graph (the same history).
pub(crate) fn checkpoint_live(store: &Store, label: &str) -> persist::SnapshotInfo {
    let snapshot = store
        .checkpoint(None)
        .unwrap_or_else(|err| panic!("{label}: {err}"));
    let expected = store.graph().read();
    assert_eq!(snapshot.commit_seq, expected.last_commit_seq(), "{label}");
    let (alone, _) = newest_alone(store.dir());
    same(&alone, &expected, label);
    snapshot
}

// ---------------------------------------------------------------------------------------------
// Histories
// ---------------------------------------------------------------------------------------------

/// SplitMix64.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

pub(crate) fn run(graph: &Arc<Graph>, script: &str) {
    let _ = eval_value(graph.clone(), script).unwrap_or_else(|err| panic!("{script}: {err}"));
}

/// Fake people (with a nested address) and companies, and edges between them: ids `p<i>`,
/// `c<i>`, `k<i>`.
pub(crate) fn fake_data(graph: &Arc<Graph>, seed: u64, people: usize) {
    run(
        graph,
        &format!(
            r#"let f = Fake({seed});
            for i in 0..{people} {{
                g.add_v(f.pick(["person", "employee"])).property(T.id, "p" + i)
                    .property("name", f.full_name()).property("age", f.int(18, 90))
                    .property("email", f.email()).property("score", f.float(0, 10))
                    .property("id", f.uuid())
                    .property("addr", #{{city: f.city(), zip: f.zip_code(), tags: [f.word(), f.word()]}})
                    .to_list();
            }}
            for i in 0..{companies} {{
                g.add_v("company").property(T.id, "c" + i).property("name", f.company()).to_list();
            }}
            for i in 0..{people} {{
                g.add_e("works_at").from(__.v("p" + i)).to(__.v("c" + (i % {companies})))
                    .property(T.id, "k" + i).property("since", f.int(1990, 2026)).to_list();
            }}"#,
            companies = people / 5 + 1
        ),
    );
}

fn vertex_ids(graph: &TraversalGraph) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .vertices()
        .filter_map(|v| graph.vertex_id(v).map(str::to_string))
        .collect();
    ids.sort();
    ids
}

fn edge_ids(graph: &TraversalGraph) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .edges()
        .filter_map(|e| graph.edge_id(e).map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// The mutation kinds of the matrix (row 2), each one a committed unit (or, `fail`, a unit
/// that rolls back after taking an auto id).
pub(crate) const KINDS: [&str; 18] = [
    "add_vertex",
    "add_edge",
    "drop_vertex",
    "drop_edge",
    "set_property",
    "remove_property",
    "jpath_write",
    "add_label",
    "remove_label",
    "set_edge_label",
    "set_schema",
    "define",
    "replace_definition",
    "drop_definition",
    "unknown_definition",
    "drop_and_readd",
    "auto_id_gap",
    "temporal",
];

/// Applies one mutation of `kind` (picking its element with `rng`); failures of the unit are
/// fine (a unit that fails changes nothing).
pub(crate) fn mutate(graph: &Arc<Graph>, kind: &str, rng: &mut Rng, round: usize) {
    let pick = |ids: Vec<String>, rng: &mut Rng| -> Option<String> {
        (!ids.is_empty()).then(|| ids[rng.below(ids.len())].clone())
    };
    let vertex = pick(vertex_ids(&graph.read()), rng);
    let edge = pick(edge_ids(&graph.read()), rng);
    let other = pick(vertex_ids(&graph.read()), rng);
    let seed = rng.next() % 10_000;
    let tx = |body: &mut dyn FnMut(&mut TraversalGraph) -> Result<(), GraphError>| {
        let _ = graph.write().transaction(|g| body(g));
    };
    match kind {
        "add_vertex" => run(
            graph,
            &format!(
                r#"let f = Fake({seed}); g.add_v("person").property(T.id, "n{round}-{seed}")
                   .property("name", f.full_name()).property("nested", #{{a: [1, 2.5, f.word()], b: #{{c: f.uuid()}}}}).to_list()"#
            ),
        ),
        "add_edge" => {
            if let (Some(a), Some(b)) = (vertex, other) {
                let _ = eval_value(
                    graph.clone(),
                    &format!(
                        r#"g.add_e("knows").from(__.v("{a}")).to(__.v("{b}")).property(T.id, "x{round}-{seed}").property("w", {seed}).to_list()"#
                    ),
                );
            }
        }
        "drop_vertex" => {
            if let Some(a) = vertex {
                run(graph, &format!(r#"g.v("{a}").drop().to_list()"#));
            }
        }
        "drop_edge" => {
            if let Some(e) = edge {
                run(graph, &format!(r#"g.e("{e}").drop().to_list()"#));
            }
        }
        "set_property" => {
            if let Some(a) = vertex {
                run(
                    graph,
                    &format!(r#"g.v("{a}").property("p{}", {seed}).to_list()"#, seed % 4),
                );
            }
            if let Some(e) = edge {
                run(
                    graph,
                    &format!(r#"g.e("{e}").property("w", "v{seed}").to_list()"#),
                );
            }
        }
        "remove_property" => {
            if let Some(a) = vertex {
                run(
                    graph,
                    &format!(r#"g.v("{a}").remove_property("name").to_list()"#),
                );
            }
        }
        "jpath_write" => {
            if let Some(a) = vertex {
                let _ = eval_value(
                    graph.clone(),
                    &format!(r#"g.v("{a}").property(jpath("addr.city"), "City {seed}").to_list()"#),
                );
            }
        }
        "add_label" | "remove_label" => {
            if let Some(a) = vertex {
                let label = ["admin", "person", "vip"][seed as usize % 3];
                let add = kind == "add_label";
                tx(&mut |g| {
                    let Some(v) = g.get_vertex_by_id(&a) else {
                        return Ok(());
                    };
                    if add {
                        g.add_vertex_label(v, label.into())?;
                    } else {
                        g.remove_vertex_label(v, label)?;
                    }
                    Ok(())
                });
            }
        }
        "set_edge_label" => {
            if let Some(e) = edge {
                let label = ["knows", "likes", "rated"][seed as usize % 3];
                tx(&mut |g| {
                    if let Some(edge) = g.get_edge_by_id(&e) {
                        g.set_edge_label(edge, label.into())?;
                    }
                    Ok(())
                });
            }
        }
        "set_schema" => {
            let schema = if seed.is_multiple_of(3) {
                GraphSchema::default()
            } else {
                GraphSchema::from_json(&format!(
                    r#"{{"mode": "none", "meta": {{"v": {seed}}}, "vertices": {{"person": {{"schema": {{"type": "object", "properties": {{"name": {{"type": "string"}}}}}}}}}}}}"#
                ))
                .unwrap()
            };
            let _ = graph.write().set_schema(schema);
        }
        "define" | "replace_definition" => {
            let name = format!("q{}", seed % 2);
            let source = format!("fn {name}() {{ g.v().count() + {seed} }}");
            tx(&mut |g| {
                g.set_definition(Definition::query(
                    name.clone(),
                    QueryDefinition::new(source.clone()),
                ))
            });
        }
        "drop_definition" => {
            let name = format!("q{}", seed % 2);
            tx(&mut |g| {
                g.remove_definition(DefinitionKind::QUERY, &name)
                    .map(|_| ())
            });
        }
        "unknown_definition" => {
            let payload = vec![1, 1, b'k', 3, (seed % 100) as u8];
            tx(&mut |g| {
                g.set_definition(Definition::opaque(
                    DefinitionKind::from_code(200),
                    DefinitionFlags::NONE,
                    format!("u{}", seed % 2),
                    payload.clone(),
                ))
            });
        }
        "drop_and_readd" => {
            if let Some(a) = vertex {
                run(
                    graph,
                    &format!(
                        r#"g.v("{a}").drop().to_list(); g.add_v("readded").property(T.id, "{a}").property("again", true).to_list()"#
                    ),
                );
            }
        }
        "auto_id_gap" => {
            // A unit that takes an auto id and fails: the id is never reused.
            let _ = graph.write().transaction(|g| -> Result<(), Abort> {
                g.add_vertex(None, vec!["ghost".into()], PropertyMap::default())?;
                Err(Abort)
            });
            run(graph, r#"g.add_v("auto").to_list()"#);
        }
        "temporal" => {
            // Every temporal kind of file format version 3: digits below the microsecond, a
            // zone name, an offset time, the longest durations; nested too.
            let ns = seed * 100_003 % 1_000_000_000;
            let values = [
                format!(r#"localdatetime("2024-05-06T07:08:09.{ns:09}")"#),
                format!(r#"datetime("1969-12-31T23:59:59.{ns:09}-05:30")"#),
                format!(r#"datetime("2024-03-31T03:30:00.{ns:09}+02:00[Europe/Berlin]")"#),
                format!(r#"time("23:59:59.{ns:09}")"#),
                format!(r#"offsettime("10:00:00.{ns:09}+05:45")"#),
                format!(r#"duration("P{}M-{seed}DT{seed}.{ns:09}S")"#, seed % 13),
                r#"duration("PT9223372036854775807.999999999S")"#.to_string(),
                r#"duration("-PT9223372036854775808S")"#.to_string(),
                format!(r#"date("{:04}-02-28")"#, seed % 9999),
                format!(
                    r#"[datetime("9999-12-30T12:00:00.{ns:09}[Asia/Tokyo]"), #{{at: offsettime("00:00:00.{ns:09}-18:00")}}]"#
                ),
            ];
            let value = &values[seed as usize % values.len()];
            if let Some(a) = vertex {
                run(
                    graph,
                    &format!(r#"g.v("{a}").property("t{}", {value}).to_list()"#, seed % 3),
                );
            }
            if let Some(e) = edge {
                run(
                    graph,
                    &format!(r#"g.e("{e}").property("when", {value}).to_list()"#),
                );
            }
        }
        other => panic!("unknown kind {other}"),
    }
}

/// The error of a unit a test rolls back on purpose.
pub(crate) struct Abort;

impl From<GraphError> for Abort {
    fn from(_: GraphError) -> Self {
        Abort
    }
}

/// A random history of `rounds` mutations of every kind.
pub(crate) fn history(graph: &Arc<Graph>, rng: &mut Rng, rounds: usize, start: usize) {
    for round in start..start + rounds {
        let kind = KINDS[rng.below(KINDS.len())];
        mutate(graph, kind, rng, round);
    }
}

// ---------------------------------------------------------------------------------------------
// A directory with hooks: failures and a gate
// ---------------------------------------------------------------------------------------------

/// What a hooked operation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Dir,
    Create,
    Write,
    Sync,
    SetLen,
    Append,
    Replace,
    Rename,
    Truncate,
    Remove,
    SyncDir,
}

pub(crate) trait Hook: Send + Sync {
    fn on(&self, op: Op, name: &str) -> io::Result<()>;
}

/// A [`MemDir`] whose every changing operation (and every write, sync and cut of an open file)
/// passes `hook` first.
#[derive(Clone)]
pub(crate) struct Hooked {
    pub(crate) inner: MemDir,
    pub(crate) hook: Arc<dyn Hook>,
}

impl std::fmt::Debug for Hooked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hooked")
    }
}

struct HookedFile {
    inner: Box<dyn StoreFile>,
    hook: Arc<dyn Hook>,
    name: String,
}

impl io::Write for HookedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hook.on(Op::Write, &self.name)?;
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl StoreFile for HookedFile {
    fn sync(&mut self) -> io::Result<()> {
        self.hook.on(Op::Sync, &self.name)?;
        self.inner.sync()
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.hook.on(Op::SetLen, &self.name)?;
        self.inner.set_len(len)
    }
}

impl Hooked {
    fn file(&self, name: &str, inner: Box<dyn StoreFile>) -> Box<dyn StoreFile> {
        Box::new(HookedFile {
            inner,
            hook: self.hook.clone(),
            name: name.to_string(),
        })
    }
}

impl StoreDir for Hooked {
    fn location(&self) -> String {
        "memory:hooked".into()
    }
    fn entry_kind(&self, name: &str) -> io::Result<Option<EntryKind>> {
        self.inner.entry_kind(name)
    }
    fn list(&self, dir: &str) -> io::Result<Vec<DirEntry>> {
        self.inner.list(dir)
    }
    fn file_len(&self, name: &str) -> io::Result<u64> {
        self.inner.file_len(name)
    }
    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        self.inner.read(name, max)
    }
    fn read_range(&self, name: &str, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.inner.read_range(name, offset, len)
    }
    fn open_read(&self, name: &str) -> io::Result<Box<dyn io::Read + Send>> {
        self.inner.open_read(name)
    }
    fn create_dir_all(&self, name: &str) -> io::Result<()> {
        self.hook.on(Op::Dir, name)?;
        self.inner.create_dir_all(name)
    }
    fn create(&self, name: &str, exclusive: bool) -> io::Result<Box<dyn StoreFile>> {
        self.hook.on(Op::Create, name)?;
        Ok(self.file(name, self.inner.create(name, exclusive)?))
    }
    fn open_write(&self, name: &str, offset: u64) -> io::Result<Box<dyn StoreFile>> {
        self.hook.on(Op::Create, name)?;
        Ok(self.file(name, self.inner.open_write(name, offset)?))
    }
    fn append(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.hook.on(Op::Append, name)?;
        self.inner.append(name, bytes)
    }
    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.hook.on(Op::Replace, name)?;
        self.inner.write_atomic(name, bytes)
    }
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.hook.on(Op::Rename, from)?;
        self.inner.rename(from, to)
    }
    fn truncate(&self, name: &str, len: u64) -> io::Result<()> {
        self.hook.on(Op::Truncate, name)?;
        self.inner.truncate(name, len)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.hook.on(Op::Remove, name)?;
        self.inner.remove_file(name)
    }
    fn remove_tree(&self, name: &str) -> io::Result<()> {
        self.hook.on(Op::Remove, name)?;
        self.inner.remove_tree(name)
    }
    fn sync_dir(&self, name: &str) -> io::Result<()> {
        self.hook.on(Op::SyncDir, name)?;
        self.inner.sync_dir(name)
    }
    fn lock(&self, kind: LockKind) -> io::Result<Option<DirLockGuard>> {
        self.inner.lock(kind)
    }
}

/// Fails the operation number `at` (counted from arming): `crash` makes every later one fail
/// too (a machine that died there); otherwise only that one fails (a full disk for a moment,
/// `kind` = `StorageFull`, or an I/O error).
pub(crate) struct Faults {
    pub(crate) armed: AtomicBool,
    pub(crate) count: AtomicU64,
    pub(crate) at: AtomicU64,
    pub(crate) crash: bool,
    pub(crate) kind: io::ErrorKind,
    pub(crate) fired: AtomicBool,
    pub(crate) ops: Mutex<Vec<Op>>,
}

impl Faults {
    pub(crate) fn new(crash: bool, kind: io::ErrorKind) -> Arc<Self> {
        Arc::new(Self {
            armed: AtomicBool::new(false),
            count: AtomicU64::new(0),
            at: AtomicU64::new(u64::MAX),
            crash,
            kind,
            fired: AtomicBool::new(false),
            ops: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn arm(&self, at: u64) {
        self.count.store(0, Ordering::SeqCst);
        self.fired.store(false, Ordering::SeqCst);
        self.at.store(at, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
    }

    pub(crate) fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Hook for Faults {
    fn on(&self, op: Op, _: &str) -> io::Result<()> {
        if !self.armed.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Ok(mut ops) = self.ops.lock() {
            ops.push(op);
        }
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        let at = self.at.load(Ordering::SeqCst);
        if n == at || (self.crash && n > at) {
            self.fired.store(true, Ordering::SeqCst);
            return Err(io::Error::new(
                self.kind,
                format!("injected failure at {op:?}"),
            ));
        }
        Ok(())
    }
}

/// Stops a merge while it writes its snapshot: the first file created in `snapshots/tmp-*`
/// waits until the gate is opened.
#[derive(Default)]
pub(crate) struct Gate {
    state: Mutex<(bool, bool)>,
    signal: Condvar,
}

impl Gate {
    pub(crate) fn close(&self) {
        *self.state.lock().unwrap() = (true, false);
    }

    pub(crate) fn open(&self) {
        self.state.lock().unwrap().0 = false;
        self.signal.notify_all();
    }

    /// Waits until a merge is held at the gate.
    pub(crate) fn wait_held(&self) -> bool {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .signal
            .wait_timeout_while(state, Duration::from_secs(30), |s| !s.1)
            .unwrap();
        !timeout.timed_out() && state.1
    }
}

impl Hook for Gate {
    fn on(&self, op: Op, name: &str) -> io::Result<()> {
        if op == Op::Create && name.starts_with("snapshots/tmp-") {
            let mut state = self.state.lock().unwrap();
            if state.0 {
                state.1 = true;
                self.signal.notify_all();
                let _held = self.signal.wait_while(state, |s| s.0).unwrap();
            }
        }
        Ok(())
    }
}

/// The names under `snapshots/` that are temporary directories.
pub(crate) fn leftovers(dir: &MemDir) -> Vec<String> {
    dir.file_names()
        .into_iter()
        .filter(|n| n.contains("/tmp-"))
        .collect()
}

/// Every file of a store in memory with its bytes, sorted.
pub(crate) fn listing(dir: &MemDir) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = dir
        .file_names()
        .into_iter()
        .map(|n| {
            let bytes = dir.file(&n).unwrap_or_default();
            (n, bytes)
        })
        .collect();
    out.sort();
    out
}

/// A store with Fake data and a history, closed: `(dir, options, write)`.
fn closed_store(codec: Codec, seed: u64, rounds: usize) -> (MemDir, StoreOptions, WriteOptions) {
    let (options, write) = small(codec);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), seed, 25);
    store.checkpoint(None).unwrap();
    history(store.graph(), &mut Rng(seed), rounds, 0);
    store.close().unwrap();
    (dir, options, write)
}

// ---------------------------------------------------------------------------------------------
// Sunny day
// ---------------------------------------------------------------------------------------------

/// Row 1: nothing committed after the newest snapshot: no new snapshot, on the live and on the
/// closed store; the files stay as they are.
#[test]
fn row01_an_empty_delta_writes_nothing() {
    let (options, _) = small(Codec::Lz4);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 1, 10);
    let first = store.checkpoint(None).unwrap();
    let again = store.checkpoint(None).unwrap();
    assert_eq!(first, again);
    assert_eq!(store.snapshots().unwrap().len(), 2);
    store.close().unwrap();
    let before = listing(&dir);
    let report = Store::checkpoint_dir(&dir, options, None).unwrap();
    assert!(!report.written && !report.merged);
    assert_eq!(report.snapshot.commit_seq, first.commit_seq);
    assert_eq!(listing(&dir), before);
}

/// Row 2: every mutation kind alone, then all of them mixed, on the closed store and on the
/// live one: the merged snapshot is the oracle.
#[test]
fn row02_every_mutation_kind_alone_and_mixed() {
    for (index, kind) in KINDS.iter().enumerate() {
        for live in [false, true] {
            let (options, write) = small(Codec::Lz4);
            let dir = MemDir::new();
            let store = Store::create(&dir, options.clone()).unwrap();
            fake_data(store.graph(), index as u64, 20);
            run(
                store.graph(),
                r#"g.v("p1").property("name", "named").to_list()"#,
            );
            store
                .graph()
                .write()
                .transaction(|g| {
                    g.set_definition(Definition::query(
                        "q0",
                        QueryDefinition::new("fn q0() { 1 }"),
                    ))
                })
                .unwrap();
            store.checkpoint(None).unwrap();
            let mut rng = Rng(index as u64);
            for round in 0..4 {
                mutate(store.graph(), kind, &mut rng, round);
            }
            let label = format!("{kind} (live {live})");
            if live {
                checkpoint_live(&store, &label);
            } else {
                store.close().unwrap();
                let report = checkpoint_closed(&shared(&dir), &options, &write, &label);
                // (A mutation that changed nothing committed nothing: no new snapshot.)
                assert!(report.merged || !report.written, "{label}");
            }
        }
    }
    // Mixed: every kind several times, in one delta.
    let (options, write) = small(Codec::Lz4);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 99, 30);
    store.checkpoint(None).unwrap();
    let mut rng = Rng(99);
    for round in 0..3 {
        for kind in KINDS {
            mutate(store.graph(), kind, &mut rng, round * 100);
        }
    }
    store.close().unwrap();
    checkpoint_closed(&shared(&dir), &options, &write, "mixed");
}

/// Row 4: a WAL of several segments with marks and checkpoint records inside, and a torn tail
/// at its end (a crash image): merged up to the last complete record.
#[test]
fn row04_a_wal_of_many_segments_marks_checkpoint_records_and_a_torn_tail() {
    let (options, write) = small(Codec::Lz4);
    let options = options.with_wal_segment_bytes(700);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 4, 15);
    let mut rng = Rng(4);
    for round in 0..6 {
        history(store.graph(), &mut rng, 5, round * 10);
        store.mark(&format!("m{round}")).unwrap();
        if round == 2 {
            store.checkpoint(None).unwrap();
        }
    }
    history(store.graph(), &mut rng, 3, 100);
    assert!(
        dir.file_names()
            .iter()
            .filter(|n| n.starts_with("wal/"))
            .count()
            > 4
    );
    // A crash image: the last record cut in the middle.
    let image = dir.deep_copy();
    drop(store);
    let mut wal: Vec<String> = image
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with("wal/"))
        .collect();
    wal.sort();
    let last = wal.pop().unwrap();
    let mut bytes = image.file(&last).unwrap();
    let cut = bytes.len() - 3;
    bytes.truncate(cut);
    image.put_file(&last, bytes);
    // The image was not closed cleanly (copied while open).
    let report = checkpoint_closed(&shared(&image), &options, &write, "torn tail");
    assert!(report.merged);
    // And the complete WAL of the closed store.
    let report = checkpoint_closed(&shared(&dir), &options, &write, "many segments");
    assert!(report.merged && report.commits_merged > 10, "{report:?}");
}

/// Row 5: incremental chains over several generations, prune keeping every referenced file
/// and removing the rest, backups (full, increment, ZIP), restore and fork of a store whose
/// newest snapshot is incremental; every snapshot loads to the state it was taken at.
#[test]
fn row05_incremental_chains_prune_backup_restore_fork() {
    let (options, write) = small(Codec::Lz4);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 5, 60);
    store.checkpoint(None).unwrap();
    store.close().unwrap();
    let mut states = Vec::new();
    let mut reused = 0;
    let backup = MemDir::new();
    for generation in 0..6 {
        // A few touched ids: most segments are referenced.
        let store = Store::open(&dir, options.clone()).unwrap();
        let id = format!("p{}", generation * 7);
        run(
            store.graph(),
            &format!(r#"g.v("{id}").property("gen", {generation}).to_list()"#),
        );
        store.close().unwrap();
        let report = checkpoint_closed(&shared(&dir), &options, &write, "generation");
        assert!(report.merged);
        reused += report.segments_reused;
        states.push((
            report.snapshot.commit_seq,
            exact(&newest_alone(&shared(&dir)).0),
        ));
        let reopened = Store::open(&dir, options.clone()).unwrap();
        if generation == 1 {
            reopened.backup(&backup).unwrap();
        }
        if generation == 3 {
            let report = reopened.backup(&backup).unwrap();
            assert!(report.incremental);
        }
        // Prune now and then: referenced files stay.
        if generation % 2 == 1 {
            let last = reopened.snapshots().unwrap().pop().unwrap().commit_seq;
            reopened.prune(last).unwrap();
            assert!(reopened.verify().unwrap().is_ok());
        }
        reopened.close().unwrap();
        // Every snapshot still listed loads to its state.
        for snapshot in Store::snapshots_dir(&dir).unwrap() {
            if let Some((_, state)) = states.iter().find(|(seq, _)| *seq == snapshot.commit_seq) {
                let mut packed = Vec::new();
                Store::export_packed_dir(&dir, Some(snapshot.commit_seq), &mut packed).unwrap();
                let alone = persist::read_snapshot(packed.as_slice(), &ReadOptions::new()).unwrap();
                assert_eq!(&exact(&alone), state);
            }
        }
    }
    assert!(reused > 10, "{reused}");
    let store = Store::open(&dir, options.clone()).unwrap();
    let live = exact(&store.graph().read());
    // A full backup and a restore of it.
    let full = MemDir::new();
    store.backup(&full).unwrap();
    let increment = store.backup(&backup).unwrap();
    assert!(increment.incremental);
    #[cfg(feature = "persist-zip")]
    {
        let mut zip = std::io::Cursor::new(Vec::new());
        store.backup_zip(&mut zip).unwrap();
        let zip = zip.into_inner();
        let unpacked = MemDir::new();
        Store::restore_zip(std::io::Cursor::new(zip), &unpacked).unwrap();
        let restored = Store::open(&unpacked, StoreOptions::new()).unwrap();
        assert_eq!(exact(&restored.graph().read()), live);
    }
    for copy in [&full, &backup] {
        assert!(Store::verify_dir(copy).unwrap().is_ok());
        Store::restore_backup(copy).unwrap();
        let restored = Store::open(copy, StoreOptions::new()).unwrap();
        assert_eq!(exact(&restored.graph().read()), live);
    }
    // A fork of the store (its newest snapshot is incremental).
    let fork = MemDir::new();
    store.fork(RecoveryTarget::Latest, &fork).unwrap();
    let forked = Store::open(&fork, StoreOptions::new()).unwrap();
    assert_eq!(exact(&forked.graph().read())[1..], live[1..]);
}

/// Row 6: lineage: a merge after an in-place rollback (base of the ancestor lineage), after an
/// attic restore, with incremental snapshots moved into the attic and a prune in between; and a
/// merge in a fork.
#[test]
fn row06_lineage_rollback_attic_restore_and_fork() {
    let (options, write) = small(Codec::Lz4);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 6, 40);
    store.checkpoint(None).unwrap();
    let mut rng = Rng(6);
    history(store.graph(), &mut rng, 10, 0);
    store.checkpoint(None).unwrap();
    let target = store.graph().read().last_commit_seq();
    for round in 0..3 {
        history(store.graph(), &mut rng, 8, 100 + round * 10);
        checkpoint_live(&store, "before the rollback");
    }
    let rolled = store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap();
    let attic = rolled.attic.clone().unwrap();
    // The attic's snapshots reference files in the store: a prune keeps them.
    let last = store.snapshots().unwrap().pop().unwrap().commit_seq;
    store.prune(last).unwrap();
    assert!(store.verify().unwrap().is_ok());
    // Restore the attic entry (nothing was committed since): its snapshots load again.
    store.attic_restore(&attic).unwrap();
    assert!(store.verify().unwrap().is_ok());
    history(store.graph(), &mut rng, 8, 300);
    checkpoint_live(&store, "after the attic restore");
    // Roll back again and continue on the new lineage: the merge's base is of the ancestor.
    store
        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
        .unwrap();
    history(store.graph(), &mut rng, 8, 400);
    checkpoint_live(&store, "new lineage");
    store.close().unwrap();
    let reopened = Store::open(&dir, options.clone()).unwrap();
    history(reopened.graph(), &mut rng, 5, 500);
    reopened.close().unwrap();
    checkpoint_closed(&shared(&dir), &options, &write, "new lineage, closed");
    // A fork, and a merge in it.
    let fork = MemDir::new();
    Store::fork_dir(&dir, RecoveryTarget::Latest, &fork).unwrap();
    let forked = Store::open(&fork, options.clone()).unwrap();
    history(forked.graph(), &mut rng, 8, 600);
    forked.close().unwrap();
    checkpoint_closed(&shared(&fork), &options, &write, "fork");
}

/// Row 7: commits go on while a merge writes its snapshot (it holds no graph lock) and land
/// after its position; the next checkpoint picks them up. Also the automatic checkpoint on its
/// background thread.
#[test]
fn row07_commits_go_on_during_a_merge() {
    let (options, write) = small(Codec::Lz4);
    let gate = Arc::new(Gate::default());
    let inner = MemDir::new();
    let dir: Arc<dyn StoreDir> = Arc::new(Hooked {
        inner: inner.clone(),
        hook: gate.clone(),
    });
    let store = Arc::new(Store::create(&dir, options.clone()).unwrap());
    fake_data(store.graph(), 7, 30);
    let position = store.graph().read().last_commit_seq();
    gate.close();
    let checkpointing = {
        let store = store.clone();
        std::thread::spawn(move || store.checkpoint(None))
    };
    assert!(gate.wait_held(), "the merge never reached the gate");
    // The merge is held while writing: commits must not wait for it.
    let (done, finished) = std::sync::mpsc::channel();
    {
        let graph = store.graph().clone();
        std::thread::spawn(move || {
            for i in 0..5 {
                run(
                    &graph,
                    &format!(r#"g.add_v("during").property(T.id, "d{i}").to_list()"#),
                );
            }
            let _ = done.send(());
        });
    }
    assert!(
        finished.recv_timeout(Duration::from_secs(30)).is_ok(),
        "commits waited for the merge"
    );
    gate.open();
    let snapshot = checkpointing.join().unwrap().unwrap();
    assert_eq!(
        snapshot.commit_seq, position,
        "the merge is at its starting position"
    );
    assert_eq!(store.graph().read().last_commit_seq(), position + 5);
    checkpoint_live(&store, "picks up the commits");

    // The automatic checkpoint (a background thread) merges too.
    let auto = MemDir::new();
    let store = Store::create(&auto, options.clone().with_checkpoint_after_bytes(2_000)).unwrap();
    fake_data(store.graph(), 8, 40);
    for _ in 0..200 {
        if store.snapshots().unwrap().len() > 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(store.snapshots().unwrap().len() > 1);
    assert!(store.last_auto_checkpoint_error().unwrap().is_none());
    store.close().unwrap();
    let report = checkpoint_closed(&shared(&auto), &options, &write, "after automatic");
    let _ = report;
}

/// Row 8: every codec and every backend, live and closed.
#[test]
fn row08_every_codec_and_backend() {
    #[allow(unused_mut)]
    let mut codecs = vec![Codec::None, Codec::Lz4];
    #[cfg(feature = "persist-zstd")]
    codecs.push(Codec::Zstd);
    let base = std::env::temp_dir().join(format!("graphersal-merge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    for codec in codecs {
        let (options, write) = small(codec);
        let backends: Vec<Arc<dyn StoreDir>> = vec![
            Arc::new(MemDir::new()),
            Arc::new(persist::FsDir::new(base.join(format!("dir-{codec:?}")))),
            Arc::new(persist::SingleFileDir::new(
                base.join(format!("{codec:?}.gstore")),
            )),
        ];
        for dir in backends {
            let label = format!("{codec:?} {}", dir.location());
            let store = Store::create(&dir, options.clone()).unwrap();
            fake_data(store.graph(), 8, 30);
            store.checkpoint(None).unwrap();
            history(store.graph(), &mut Rng(8), 30, 0);
            checkpoint_live(&store, &label);
            history(store.graph(), &mut Rng(9), 30, 100);
            store.close().unwrap();
            let report = checkpoint_closed(&dir, &options, &write, &label);
            assert!(report.merged, "{label}");
            assert!(Store::verify_dir(&dir).unwrap().is_ok(), "{label}");
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Row 9 (the library side of `graphersal store checkpoint`): refused while the store is open.
#[test]
fn row09_a_closed_store_only() {
    let (options, _) = small(Codec::Lz4);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 9, 5);
    let err = Store::checkpoint_dir(&dir, options.clone(), None).unwrap_err();
    assert!(matches!(err, PersistError::Locked { .. }), "{err}");
    store.close().unwrap();
    assert!(
        Store::checkpoint_dir(&dir, options, Some("nightly"))
            .unwrap()
            .written
    );
    assert_eq!(
        Store::snapshots_dir(&dir).unwrap().pop().unwrap().name,
        "nightly"
    );
}

// ---------------------------------------------------------------------------------------------
// Rainy day
// ---------------------------------------------------------------------------------------------

/// The input files of a merge of the closed store in `dir`: the newest snapshot's files, the
/// older files it references, and the WAL segments after it.
pub(crate) fn inputs(dir: &MemDir) -> Vec<String> {
    let names = dir.file_names();
    let newest = Store::snapshots_dir(dir).unwrap().pop().unwrap().commit_seq;
    let newest_dir = format!("snapshots/{newest:020}/");
    let mut out: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with(&newest_dir))
        .cloned()
        .collect();
    // Referenced files: in older snapshot directories (they are read when the new snapshot is
    // verified).
    let manifest = dir.file(&format!("{newest_dir}manifest")).unwrap();
    for name in names
        .iter()
        .filter(|n| n.starts_with("snapshots/") && !n.starts_with(&newest_dir))
    {
        let relative = name.trim_start_matches("snapshots/");
        if manifest
            .windows(relative.len())
            .any(|w| w == relative.as_bytes())
        {
            out.push(name.clone());
        }
    }
    let mut wal: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("wal/"))
        .cloned()
        .collect();
    wal.sort();
    // The replay set: the last segment named at or before newest + 1, and every later one.
    let start = wal
        .iter()
        .rposition(|n| {
            n.trim_start_matches("wal/")
                .trim_end_matches(".wal")
                .parse::<u64>()
                .is_ok_and(|first| first <= newest + 1)
        })
        .unwrap_or(0);
    out.extend(wal[start..].iter().cloned());
    out
}

/// Row 10: every byte of every input file flipped (one bit, rotating): refused, nothing
/// visible written (the files unchanged byte for byte); the only exception is a flip inside
/// one copy of a manifest's fixed part, which is redundancy (format spec 13.2): merged exactly.
#[test]
fn row10_every_flipped_bit_in_every_input_is_refused() {
    let (dir, options, write) = closed_store(Codec::Lz4, 10, 6);
    let expected = exact(&oracle(&shared(&dir)));
    let files = inputs(&dir);
    assert!(files.iter().any(|f| f.starts_with("wal/")));
    assert!(files.iter().filter(|f| f.ends_with(".seg")).count() > 2);
    let mut refused = 0;
    for file in &files {
        let len = dir.file(file).unwrap().len();
        for at in 0..len {
            let image = dir.deep_copy();
            let mut bytes = image.file(file).unwrap();
            bytes[at] ^= 1 << (at % 8);
            image.put_file(file, bytes);
            let before = listing(&image);
            let result = Store::checkpoint_dir(&image, options.clone(), None);
            let redundant = file.ends_with("/manifest") && (at < 4096 || at >= len - 4096);
            match result {
                Err(_) => {
                    refused += 1;
                    assert_eq!(listing(&image), before, "{file} byte {at}: files changed");
                }
                Ok(report) => {
                    assert!(redundant, "{file} byte {at}: damage went unnoticed");
                    let (alone, _) = newest_alone(&shared(&image));
                    assert_eq!(exact(&alone), expected, "{file} byte {at}");
                    let _ = (&report, &write);
                }
            }
        }
    }
    assert!(refused > 1000, "{refused}");
}

/// Row 11: every cut of the last WAL record of a crash image is a torn tail (merged up to the
/// record before it); of a cleanly closed store, a cut WAL is damage (its close recorded the
/// last commit); damage in the middle of the WAL, with valid records after it, is refused.
#[test]
fn row11_cuts_of_the_last_record_and_damage_in_the_middle() {
    let (options, write) = small(Codec::Lz4);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 11, 10);
    store.checkpoint(None).unwrap();
    history(store.graph(), &mut Rng(11), 6, 0);
    let mut wal: Vec<String> = dir
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with("wal/"))
        .collect();
    wal.sort();
    let last = wal.pop().unwrap();
    let complete = dir.file(&last).unwrap().len();
    run(
        store.graph(),
        r#"g.add_v("last").property("x", "a longer value for the record").to_list()"#,
    );
    let crash = dir.deep_copy();
    store.close().unwrap();
    let full = crash.file(&last).unwrap().len();
    for cut in complete..full {
        let image = crash.deep_copy();
        let mut bytes = image.file(&last).unwrap();
        bytes.truncate(cut);
        image.put_file(&last, bytes);
        let report = checkpoint_closed(&shared(&image), &options, &write, &format!("cut {cut}"));
        assert_eq!(
            report.snapshot.commit_seq,
            oracle(&shared(&crash)).last_commit_seq() - 1
        );
        // The cleanly closed store with the same cut: refused.
        let clean = dir.deep_copy();
        let mut bytes = clean.file(&last).unwrap();
        bytes.truncate(cut);
        clean.put_file(&last, bytes);
        assert!(
            Store::checkpoint_dir(&clean, options.clone(), None).is_err(),
            "clean cut {cut}"
        );
    }
    // A record in the middle damaged, valid ones after it: refused, never skipped.
    let image = dir.deep_copy();
    let mut bytes = image.file(&last).unwrap();
    bytes[64 + 30] ^= 0x40;
    image.put_file(&last, bytes);
    let before = listing(&image);
    assert!(Store::checkpoint_dir(&image, options, None).is_err());
    assert_eq!(listing(&image), before);
}

/// Row 12: a missing referenced file, a segment of the wrong length, an unreadable base
/// manifest, damaged GRAPH copies, a missing WAL segment (a gap), a segment under another name,
/// a WAL segment of another lineage: refused with nothing written.
#[test]
fn row12_missing_wrong_and_foreign_inputs_are_refused() {
    // A store whose newest snapshot is incremental, and a WAL of several segments after it.
    let (options, _) = small(Codec::Lz4);
    let options = options.with_wal_segment_bytes(600);
    let dir = MemDir::new();
    let store = Store::create(&dir, options.clone()).unwrap();
    fake_data(store.graph(), 12, 40);
    store.checkpoint(None).unwrap();
    // The first merge copies the unchanged segments (no twin yet, format spec 12.3), the
    // second references the twins.
    run(store.graph(), r#"g.v("p3").property("x", 1).to_list()"#);
    store.checkpoint(None).unwrap();
    run(store.graph(), r#"g.v("p3").property("x", 2).to_list()"#);
    store.checkpoint(None).unwrap();
    history(store.graph(), &mut Rng(12), 20, 0);
    store.close().unwrap();
    let files = inputs(&dir);
    let referenced: Vec<&String> = files
        .iter()
        .filter(|f| f.ends_with(".seg"))
        .filter(|f| {
            let newest = Store::snapshots_dir(&dir)
                .unwrap()
                .pop()
                .unwrap()
                .commit_seq;
            !f.starts_with(&format!("snapshots/{newest:020}/"))
        })
        .collect();
    assert!(!referenced.is_empty());
    let own_segment = files
        .iter()
        .find(|f| f.ends_with(".seg") && !referenced.contains(f))
        .unwrap()
        .clone();
    let manifest = files
        .iter()
        .find(|f| f.ends_with("/manifest"))
        .unwrap()
        .clone();
    let wal: Vec<&String> = files.iter().filter(|f| f.starts_with("wal/")).collect();
    assert!(wal.len() > 2, "{wal:?}");

    let other = MemDir::new();
    let other_store = Store::create(&other, options.clone()).unwrap();
    fake_data(other_store.graph(), 13, 40);
    history(other_store.graph(), &mut Rng(13), 40, 0);
    other_store.close().unwrap();

    type Damage = Box<dyn Fn(&MemDir)>;
    let remove = |name: String| -> Damage {
        Box::new(move |d: &MemDir| {
            d.remove_file(&name).unwrap();
        })
    };
    let edit = |name: String, f: fn(&mut Vec<u8>)| -> Damage {
        Box::new(move |d: &MemDir| {
            let mut bytes = d.file(&name).unwrap();
            f(&mut bytes);
            d.put_file(&name, bytes);
        })
    };
    let middle = wal[1].clone();
    let renamed_from = wal[wal.len() - 1].clone();
    let foreign_name = wal[wal.len() - 1].clone();
    let cases: Vec<(&str, Damage)> = vec![
        (
            "a referenced segment missing",
            remove(referenced[0].clone()),
        ),
        ("an own segment missing", remove(own_segment.clone())),
        (
            "a referenced segment too long",
            edit(referenced[0].clone(), |b| b.push(0)),
        ),
        (
            "an own segment too short",
            edit(own_segment.clone(), |b| {
                b.pop();
            }),
        ),
        (
            "the base manifest unreadable",
            edit(manifest.clone(), |b| b.fill(7)),
        ),
        (
            "the base schema missing",
            remove(manifest.replace("manifest", "schema.json")),
        ),
        (
            "both GRAPH copies damaged",
            Box::new(|d: &MemDir| {
                for name in ["GRAPH", "GRAPH.copy"] {
                    let mut bytes = d.file(name).unwrap();
                    bytes[20] ^= 1;
                    d.put_file(name, bytes);
                }
            }),
        ),
        ("a WAL segment missing (a gap)", remove(middle.clone())),
        (
            "a WAL segment under another name",
            Box::new(move |d: &MemDir| {
                let bytes = d.file(&renamed_from).unwrap();
                d.remove_file(&renamed_from).unwrap();
                d.put_file("wal/00000000000000099999.wal", bytes);
            }),
        ),
        (
            "a WAL segment of another lineage",
            Box::new(move |d: &MemDir| {
                let mut foreign: Vec<String> = other
                    .file_names()
                    .into_iter()
                    .filter(|n| n.starts_with("wal/"))
                    .collect();
                foreign.sort();
                let bytes = other.file(&foreign[0]).unwrap();
                d.put_file(&foreign_name, bytes);
            }),
        ),
    ];
    for (name, damage) in cases {
        let image = dir.deep_copy();
        damage(&image);
        let before = listing(&image);
        let result = Store::checkpoint_dir(&image, options.clone(), None);
        assert!(result.is_err(), "{name}: {result:?}");
        assert_eq!(listing(&image), before, "{name}: files changed");
        assert!(leftovers(&image).is_empty(), "{name}");
    }
}

/// Row 13: a directory that fails at every write step in turn (a crash: everything after
/// fails too; a transient failure: only that step; I/O error and a full disk), for the live
/// checkpoint and for the closed one: the store reopens with every commit, no partial snapshot
/// is visible, `tmp-*` is gone, and the next checkpoint succeeds and is the oracle.
#[test]
fn row13_a_failing_directory_at_every_write_step() {
    for (crash, kind) in [
        (true, io::ErrorKind::Other),
        (false, io::ErrorKind::StorageFull),
        (false, io::ErrorKind::Other),
    ] {
        for live in [true, false] {
            for at in 0.. {
                let (options, write) = small(Codec::Lz4);
                let inner = MemDir::new();
                let faults = Faults::new(crash, kind);
                let dir: Arc<dyn StoreDir> = Arc::new(Hooked {
                    inner: inner.clone(),
                    hook: faults.clone(),
                });
                let store = Store::create(&dir, options.clone()).unwrap();
                fake_data(store.graph(), 13, 20);
                store.checkpoint(None).unwrap();
                history(store.graph(), &mut Rng(at), 8, 0);
                let expected = exact(&store.graph().read());
                let snapshots = store.snapshots().unwrap().len();
                let label = format!("crash {crash} {kind:?} live {live} step {at}");
                let result = if live {
                    faults.arm(at);
                    let result = store.checkpoint(None).map(|_| ());
                    faults.disarm();
                    result
                } else {
                    store.shutdown().unwrap();
                    faults.arm(at);
                    let result = Store::checkpoint_dir(&dir, options.clone(), None).map(|_| ());
                    faults.disarm();
                    result
                };
                if !faults.fired() {
                    result.unwrap_or_else(|err| panic!("{label}: {err}"));
                    assert!(at > 5, "{label}: a merge takes many writes");
                    break;
                }
                let failed_op = faults
                    .ops
                    .lock()
                    .unwrap()
                    .get(faults.at.load(Ordering::SeqCst) as usize)
                    .copied();
                if result.is_ok() {
                    // The one failure a checkpoint tolerates: rewriting GRAPH to record the
                    // newest WAL segment after a rotation (format spec 9.8: a failed write
                    // leaves the older value, which stays valid).
                    assert_eq!(
                        failed_op,
                        Some(Op::Replace),
                        "{label}: a failure went unnoticed"
                    );
                }
                if live && !crash {
                    // The open store carries on: commits work (or the journal refuses them
                    // after its own failed write until reopened), the next checkpoint works.
                    if store.checkpoint(None).is_ok() {
                        let (alone, _) = newest_alone(&dir);
                        assert_eq!(exact(&alone), expected, "{label}");
                    }
                }
                drop(store);
                // The image (what is on the disk): opens with every commit, no partial
                // snapshot, and checkpoints again.
                let image = shared(&inner);
                let reopened = Store::open(&image, options.clone())
                    .unwrap_or_else(|err| panic!("{label}: reopen: {err}"));
                assert_eq!(
                    exact(&reopened.graph().read())[1..],
                    expected[1..],
                    "{label}"
                );
                assert!(leftovers(&inner).is_empty(), "{label}");
                let count = reopened.snapshots().unwrap().len();
                assert!(count == snapshots || count == snapshots + 1, "{label}");
                assert!(reopened.verify().unwrap().is_ok(), "{label}");
                reopened.close().unwrap();
                checkpoint_closed(&image, &options, &write, &label);
            }
        }
    }
}

/// Row 14: concurrency. A second checkpoint while one runs waits for it (checkpoints are
/// serialized) and then finds the snapshot written; a close during a background merge waits
/// for it; a rollback requested during a merge waits for it. The store is consistent after
/// each.
#[test]
fn row14_concurrent_checkpoint_close_and_rollback() {
    let (options, write) = small(Codec::Lz4);
    for action in ["checkpoint", "close", "rollback"] {
        let gate = Arc::new(Gate::default());
        let inner = MemDir::new();
        let dir: Arc<dyn StoreDir> = Arc::new(Hooked {
            inner: inner.clone(),
            hook: gate.clone(),
        });
        let store = Arc::new(Store::create(&dir, options.clone()).unwrap());
        fake_data(store.graph(), 14, 20);
        store.checkpoint(None).unwrap();
        let target = store.graph().read().last_commit_seq();
        history(store.graph(), &mut Rng(14), 10, 0);
        let position = store.graph().read().last_commit_seq();
        let expected = exact(&store.graph().read());
        gate.close();
        let first = {
            let store = store.clone();
            std::thread::spawn(move || store.checkpoint(None))
        };
        assert!(gate.wait_held(), "{action}");
        let started = Arc::new(AtomicBool::new(false));
        let second = {
            let store = store.clone();
            let started = started.clone();
            let action = action.to_string();
            std::thread::spawn(move || {
                started.store(true, Ordering::SeqCst);
                match action.as_str() {
                    "checkpoint" => store.checkpoint(None).map(|_| ()),
                    "close" => store.shutdown(),
                    _ => store
                        .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
                        .map(|_| ()),
                }
            })
        };
        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(!second.is_finished(), "{action} did not wait for the merge");
        gate.open();
        let merged = first.join().unwrap();
        second
            .join()
            .unwrap()
            .unwrap_or_else(|err| panic!("{action}: {err}"));
        let merged = merged.unwrap();
        assert_eq!(merged.commit_seq, position, "{action}");
        drop(store);
        let image = shared(&inner);
        let reopened = Store::open(&image, options.clone()).unwrap();
        assert!(reopened.verify().unwrap().is_ok(), "{action}");
        if action == "rollback" {
            assert_eq!(reopened.graph().read().last_commit_seq(), target);
        } else {
            assert_eq!(exact(&reopened.graph().read()), expected, "{action}");
        }
        reopened.close().unwrap();
        let _ = &write;
    }
}

/// Row 17: a store in maintenance mode and a backup refuse a checkpoint, with the reason.
#[test]
fn row17_maintenance_and_backup_refuse() {
    let (options, _) = small(Codec::Lz4);
    let (dir, _, _) = closed_store(Codec::Lz4, 17, 5);
    // A backup.
    let backup = MemDir::new();
    {
        let store = Store::open(&dir, options.clone()).unwrap();
        store.backup(&backup).unwrap();
    }
    let err = Store::checkpoint_dir(&backup, options.clone(), None).unwrap_err();
    assert!(matches!(err, PersistError::IsBackup { .. }), "{err}");
    let opened = Store::open_backup(&backup, options.clone()).unwrap();
    assert!(matches!(
        opened.checkpoint(None),
        Err(PersistError::IsBackup { .. })
    ));
    // Maintenance mode: a damaged WAL record.
    let mut wal: Vec<String> = dir
        .file_names()
        .into_iter()
        .filter(|n| n.starts_with("wal/"))
        .collect();
    wal.sort();
    let last = wal.pop().unwrap();
    let mut bytes = dir.file(&last).unwrap();
    bytes[80] ^= 1;
    dir.put_file(&last, bytes);
    let store = Store::open(&dir, options.clone()).unwrap();
    assert!(store.maintenance().is_some());
    assert!(matches!(
        store.checkpoint(None),
        Err(PersistError::Maintenance { .. })
    ));
    drop(store);
    assert!(Store::checkpoint_dir(&dir, options, None).is_err());
}

/// Row 14: a close while the AUTOMATIC checkpoint (its background thread) merges waits for the
/// merge; the store reopens with every commit and verifies.
#[test]
fn row14_close_during_a_background_merge() {
    let (options, _) = small(Codec::Lz4);
    let gate = Arc::new(Gate::default());
    let inner = MemDir::new();
    let dir: Arc<dyn StoreDir> = Arc::new(Hooked {
        inner: inner.clone(),
        hook: gate.clone(),
    });
    let store = Arc::new(Store::create(&dir, options.clone()).unwrap());
    fake_data(store.graph(), 15, 20);
    gate.close();
    drop(store);
    let store =
        Arc::new(Store::open(&dir, options.clone().with_checkpoint_after_bytes(1_000)).unwrap());
    history(store.graph(), &mut Rng(15), 10, 0);
    let expected = exact(&store.graph().read());
    assert!(gate.wait_held(), "the automatic merge never started");
    let closing = {
        let store = store.clone();
        std::thread::spawn(move || store.shutdown())
    };
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !closing.is_finished(),
        "the close did not wait for the merge"
    );
    gate.open();
    closing.join().unwrap().unwrap();
    drop(store);
    let reopened = Store::open(shared(&inner), options).unwrap();
    assert_eq!(exact(&reopened.graph().read()), expected);
    assert!(reopened.verify().unwrap().is_ok());
    assert!(leftovers(&inner).is_empty());
}

/// Row 6: a checkpoint right after a rollback, before any commit on the new lineage (whose WAL
/// segment holds no commit yet): the snapshot is of the NEW lineage at the target, live and
/// closed (found by the randomized test: the replay stopped before the new lineage's segment).
#[test]
fn row06_a_checkpoint_right_after_a_rollback() {
    for live in [true, false] {
        let (options, write) = small(Codec::Lz4);
        let options = options.with_wal_segment_bytes(400);
        let dir = MemDir::new();
        let store = Store::create(&dir, options.clone()).unwrap();
        fake_data(store.graph(), 61, 10);
        let target = store.graph().read().last_commit_seq() - 4;
        store
            .rollback_to(RecoveryTarget::CommitSeq(target), RollbackMode::Move)
            .unwrap();
        let lineage = store.graph().read().graph_id();
        if live {
            let snapshot = checkpoint_live(&store, "live after a rollback");
            assert_eq!(snapshot.graph_id, lineage);
        } else {
            store.shutdown().unwrap();
            let report = checkpoint_closed(&shared(&dir), &options, &write, "closed");
            assert_eq!(report.snapshot.graph_id, lineage);
        }
        drop(store);
        let reopened = Store::open(&dir, options).unwrap();
        assert_eq!(reopened.graph().read().graph_id(), lineage);
        assert!(reopened.verify().unwrap().is_ok());
    }
}
