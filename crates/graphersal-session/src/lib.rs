//! `graphersal-session`: the session logic of the Graphersal playground, shared by the
//! WebAssembly build (`graphersal-wasm`) and the dev server of `graphersal-cli`
//! (`graphersal --server`).
//!
//! **Internal, no stability guarantees.** The API is shaped by the playground's engine boundary
//! (`playground/js/engine/engine.js`) and changes with it in any release. It is published only
//! because `graphersal-cli` depends on it; applications use the `graphersal` crate.
//!
//! [`Session`] holds one graph and the Rhai engine that runs scripts against it; every operation
//! takes and returns JSON (text or [`serde_json::Value`]), so a front end passes it on without
//! conversion glue.
//!
//! # Serving your own storage
//!
//! The session works over any [`GraphStorage`](graphersal::storage::GraphStorage), not only
//! [`TraversalGraph`](graphersal::TraversalGraph) ([`Session::new`], what the playground and the
//! dev server serve). It holds its graph as `Arc<dyn` [`SessionGraph`]`>` and its stores as
//! `Arc<dyn` [`SessionStore`]`>` (both blanket-implemented, for every `Graph<S>` and every
//! `Store<S>`), and builds every graph through the host's [`GraphFactory`]:
//!
//! ```
//! use std::sync::Arc;
//! use graphersal::prelude::*;
//! use graphersal_session::{RunOptions, Session, StorageKind};
//!
//! // `StorageKind::<S>::persistent()` for a `PersistentStorage` (graphs, `.gsnap` loads and
//! // stores); `StorageKind::<S>::new()` for any other storage (graphs only).
//! let mut session = Session::with_storage(Arc::new(StorageKind::<TraversalGraph>::persistent()));
//! session.load_sample("modern").unwrap();
//! let answer = session.execute("g.v().count().next()", &RunOptions::default());
//! assert!(answer.contains("\"ok\":true"));
//!
//! // A graph the host already holds (of any storage) is served as it is.
//! let shared = Arc::new(Graph::new(TraversalGraph::tinkerpop_modern()));
//! let info = session.load_shared(shared, "host graph", "host");
//! assert_eq!(info["vertexCount"], 6);
//! ```
//!
//! Every answer is the same JSON for every storage; a storage shows only what it really lacks:
//! a storage without compressed properties keeps compression rules but reports no statistics
//! and refuses `recompress`, one without property indexes keeps index definitions but refuses
//! to create one, one without persistence loads no `.gsnap` and keeps no store (the
//! Store menu says so). See the [`storage`] module.

#![deny(missing_docs)]

pub mod compression;
pub mod create;
pub mod display;
pub mod edit;
pub mod filter;
pub mod indexes;
pub mod jobs;
pub mod layout;
pub mod process;
pub mod queries;
pub mod selection;
pub mod session;
pub mod storage;
pub mod store;
pub mod views;

pub use session::{
    RunOptions, Session, SessionPresets, playground_authorizer, read_only_authorizer,
};
pub use storage::{
    GraphFactory, IntoSessionGraph, SessionGraph, SessionStore, StorageKind, StoreView,
};
pub use store::{ForkInto, StoreError};
