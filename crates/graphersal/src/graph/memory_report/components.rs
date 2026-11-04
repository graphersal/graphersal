//! The process side of the memory report: [`MemoryReport::components`], the named parts the
//! process's tracked heap ([`MemoryReport::tracked_live_bytes`]) goes to, each with its bytes
//! and whether they are exact or estimated (book page "Memory Usage", section "Where the
//! process memory goes").
//!
//! Everything here is computed when a report is asked for, by reading structures that exist
//! anyway: the reported graph's estimate, the embedding models loaded in the process, the
//! script engine's registrations, and every registered [`MemorySource`] (the Store, the
//! playground session, the dev server). Nothing is counted on the read, write, commit or query
//! paths.
//!
//! A host adds its own parts with [`register_memory_source`]: an object implementing
//! [`MemorySource`], held weakly (dropping it unregisters it), asked for its components at
//! every report.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::MemoryReport;

/// One named part of the process memory in a [`MemoryReport`]: what holds it, its bytes and
/// whether they are exact (counted by the tracking allocator, or the exact sizes of plain
/// buffers) or an estimate (sizes computed from the structure, like
/// [`MemoryStats`](crate::graph::memory::MemoryStats)).
///
/// The engine's names: `graph` (the reported graph's own data, [`MemoryReport::structure`]),
/// `workload_statistics`, `embedding_model` (one per model loaded in the process, `detail` its
/// identity), `script_engine` (the DSL's registrations, built once per thread that runs
/// scripts), `store` (a Store's buffers and caches; a store kept in memory counts its files),
/// `store_graph` (a Store's live graph when the report is about another graph, such as a view
/// of a past state). Hosts add their own (`session`, `mcp`, ...).
///
/// ```
/// use graphersal::storage::MemoryComponent;
///
/// let part = MemoryComponent::estimated("session", 4096).with_detail("layouts");
/// assert_eq!((part.name.as_str(), part.bytes, part.exact), ("session", 4096, false));
/// assert_eq!(part.detail.as_deref(), Some("layouts"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryComponent {
    /// What kind of part this is (`graph`, `embedding_model`, `store`, `session`, ...).
    pub name: String,
    /// Which one, when there can be several (a model's identity, a store's location).
    pub detail: Option<String>,
    /// Its bytes.
    pub bytes: u64,
    /// `true`: counted (by the tracking allocator, or the exact length of plain buffers);
    /// `false`: estimated from the structure.
    pub exact: bool,
}

impl MemoryComponent {
    /// A part whose bytes are estimated from its structure.
    pub fn estimated(name: impl Into<String>, bytes: u64) -> Self {
        MemoryComponent {
            name: name.into(),
            detail: None,
            bytes,
            exact: false,
        }
    }

    /// A part whose bytes are counted exactly.
    pub fn exact(name: impl Into<String>, bytes: u64) -> Self {
        MemoryComponent {
            exact: true,
            ..Self::estimated(name, bytes)
        }
    }

    /// With `detail`: which one of its kind it is.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// What a [`MemorySource`] adds its components to, while a memory report is computed.
#[derive(Debug, Default)]
pub struct MemoryComponents {
    list: Vec<MemoryComponent>,
    /// The address of the reported graph's storage, when the report came through a
    /// [`Graph`](crate::graph::Graph): a Store then lists its live graph only when it is
    /// another one.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    reported: Option<usize>,
}

impl MemoryComponents {
    /// Adds a component.
    pub fn push(&mut self, component: MemoryComponent) {
        self.list.push(component);
    }

    /// The address of the reported graph's storage, if known.
    #[cfg_attr(not(feature = "persist"), allow(dead_code))]
    pub(crate) fn reported(&self) -> Option<usize> {
        self.reported
    }
}

/// A holder of process memory that a [`MemoryReport`] should name: asked for its components
/// at every report ([`register_memory_source`]). It must answer quickly, never block on a lock
/// that a running query may hold for long, and never call into a memory report itself.
pub trait MemorySource: Send + Sync {
    /// Adds this holder's parts (bytes and exact/estimated) to `components`.
    fn memory_components(&self, components: &mut MemoryComponents);
}

/// The registered sources (held weakly: a dropped source is skipped and pruned).
fn sources() -> &'static Mutex<Vec<Weak<dyn MemorySource>>> {
    static SOURCES: OnceLock<Mutex<Vec<Weak<dyn MemorySource>>>> = OnceLock::new();
    SOURCES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Registers `source`: every later memory report in this process asks it for its components
/// (as long as it lives: it is held weakly, so dropping the last `Arc` unregisters it). Hosts
/// use it for what they hold beside the engine (a session, a server's client state); the
/// engine registers its own (a Store).
///
/// ```
/// use std::sync::Arc;
/// use graphersal::prelude::*;
/// use graphersal::storage::{
///     AnyGraph, MemoryComponent, MemoryComponents, MemoryReportRequest, MemorySource,
///     register_memory_source,
/// };
///
/// struct Cache(Vec<u8>);
/// impl MemorySource for Cache {
///     fn memory_components(&self, components: &mut MemoryComponents) {
///         components.push(MemoryComponent::exact("my_cache", self.0.capacity() as u64));
///     }
/// }
///
/// let cache: Arc<dyn MemorySource> = Arc::new(Cache(Vec::with_capacity(1 << 20)));
/// register_memory_source(&cache);
/// let graph = Graph::new(TraversalGraph::tinkerpop_modern());
/// let report = AnyGraph::memory_report(&graph, &MemoryReportRequest::new()).unwrap();
/// assert!(report.components.iter().any(|c| c.name == "my_cache" && c.bytes == 1 << 20));
/// ```
pub fn register_memory_source(source: &Arc<dyn MemorySource>) {
    let mut sources = sources().lock().unwrap_or_else(|e| e.into_inner());
    sources.retain(|weak| weak.strong_count() > 0);
    sources.push(Arc::downgrade(source));
}

/// Adds the process's components to `report`: the embedding models loaded in the process,
/// the script engine's registrations and every registered [`MemorySource`]. `reported` is
/// the address of the reported graph's storage when known. Call it without holding a graph
/// lock: a Store's source reads its live graph.
pub(crate) fn collect(report: &mut MemoryReport, reported: Option<usize>) {
    let mut components = MemoryComponents {
        list: Vec::new(),
        reported,
    };
    for model in crate::value::vector::embed::registry::loaded_models() {
        components.push(model);
    }
    #[cfg(feature = "script")]
    if let Some(engine) = crate::script::registration_memory() {
        components.push(engine);
    }
    let live: Vec<Arc<dyn MemorySource>> = {
        let mut sources = sources().lock().unwrap_or_else(|e| e.into_inner());
        sources.retain(|weak| weak.strong_count() > 0);
        sources.iter().filter_map(Weak::upgrade).collect()
    };
    for source in live {
        source.memory_components(&mut components);
    }
    report.components.extend(components.list);
}
