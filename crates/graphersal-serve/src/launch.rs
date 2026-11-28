//! Launching a served graph (EXPERIMENTAL): a source ([`ServedSource`]) loaded into a new
//! session thread ([`launch`]), answered as a [`Launched`] whose
//! [`Launched::context_builder`] is a [`ContextBuilder`] filled with what the load found.
//!
//! The steps every host repeats before it can build a [`crate::Context`]: start the session
//! thread with the presets, load or attach the graph, read its commit position, its statistics
//! and whether it is a layer, run the host's own setup on the session (`extra`, e.g. a reader
//! pool's template), and hand the session to a host queue's first worker (`takeover`). What
//! the host does around it stays the host's: where the graph comes from (open a Store, read a
//! `.gsnap`, apply a schema: [`crate::open_store`] and the other functions of this crate's
//! `open` module help), the listener, the banner, the MCP endpoint, the job ticker.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use graphersal::GraphSource;
use graphersal::exec::CancelToken;
use graphersal::persist::Store;
use graphersal::prelude::Graph;
use graphersal::storage::LayeredGraph;
use graphersal_session::storage::SessionGraph;
use graphersal_session::{Session, SessionPresets};
use serde_json::Value;

use crate::context::{ContextBuilder, Job, Runner, ServeError, spawn_session};
use crate::host_check::HostCheck;

/// The error of a step of the session's setup.
type SetupError = Box<dyn std::error::Error + Send + Sync>;

/// Where the graph a session serves comes from ([`launch`]).
///
/// `#[non_exhaustive]`: further sources may be added.
#[non_exhaustive]
pub enum ServedSource {
    /// A sample graph by its kind (`modern`, `empty`, `large`, `tree`: the kinds of
    /// [`Session::sample_graphs`]), titled as the session titles it.
    Sample(String),
    /// A graph file loaded by the session: a packed snapshot (`.gsnap`, with the embedding
    /// models it carries), GraphSON (`.json`, `.jsonl`, `.graphson`) or else GraphML; titled
    /// with its file name. Data the import skipped is in [`Launched::warnings`].
    File(PathBuf),
    /// An open Store, served as the session's database "main" (the Store panel, history,
    /// checkpoints) and titled with its location; [`Launched::store`] keeps it.
    Store(Arc<Store>),
    /// An open layer store ([`crate::open_layer_store`]): its live graph is served with the
    /// host's title and kind (a layer store is not the session's database: the Store panel
    /// serves plain stores); [`Launched::layer_store`] keeps it for its close, backup and
    /// export.
    LayerStore {
        /// The layer store.
        store: Arc<Store<LayeredGraph>>,
        /// The title the session shows (e.g. the base's).
        title: String,
        /// The graph kind of `GET /api/info` (e.g. `store`, `file`).
        kind: String,
    },
    /// A graph the host loaded and keeps (a `.gsnap` read with its models, a file imported
    /// against a schema, a Store loaded read-only, an in-memory layer over a base, a base
    /// served as an empty layer), served with the host's title and kind.
    Graph {
        /// The graph.
        graph: Arc<dyn SessionGraph>,
        /// The title the session shows.
        title: String,
        /// The graph kind of `GET /api/info` (`file`, `modern`, `store`, ...).
        kind: String,
    },
}

impl ServedSource {
    /// [`ServedSource::Graph`] of `graph` with its title and kind.
    pub fn graph(
        graph: Arc<dyn SessionGraph>,
        title: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self::Graph {
            graph,
            title: title.into(),
            kind: kind.into(),
        }
    }

    /// [`ServedSource::LayerStore`] of `store` with its title and kind.
    pub fn layer_store(
        store: Arc<Store<LayeredGraph>>,
        title: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self::LayerStore {
            store,
            title: title.into(),
            kind: kind.into(),
        }
    }
}

/// A launched session ([`launch`]): its job queue and what the load found.
///
/// `#[non_exhaustive]`: further fields may be added; it is built by [`launch`] only.
#[non_exhaustive]
pub struct Launched {
    /// The session thread's job queue ([`Runner::Session`]). With a `takeover` the session
    /// thread serves the host's queue instead and nothing reads this one.
    pub jobs: Sender<Job>,
    /// The session's cancellation token (cancels its running work; a host's stop signal).
    pub cancel: CancelToken,
    /// The commit position of the loaded graph (the change feed starts there).
    pub commit_seq: u64,
    /// The session's statistics at start ([`Session::statistics_with_process`]: with the
    /// process memory against its ceiling).
    pub statistics: Value,
    /// Whether the served graph is a layer over a frozen base
    /// (`StorageCapabilities::layered`).
    pub layered: bool,
    /// The presets the session runs under, as JSON (`GET /api/info` `presets`).
    pub presets: Value,
    /// The Store of a [`ServedSource::Store`].
    pub store: Option<Arc<Store>>,
    /// The layer store of a [`ServedSource::LayerStore`] (never attached to the session: the
    /// host closes, backs up and exports it).
    pub layer_store: Option<Arc<Store<LayeredGraph>>>,
    /// What the load of a [`ServedSource::Sample`] or [`ServedSource::File`] reported (data an
    /// import skipped, embedding models a `.gsnap` carried that could not be kept), one text
    /// each, for the host to show; empty for the other sources.
    pub warnings: Vec<String>,
}

impl Launched {
    /// A [`ContextBuilder`] whose jobs go to the session thread ([`Runner::Session`]), filled
    /// with the presets, the Store, the commit position and the layer state of the launch.
    pub fn context_builder(&self, check: HostCheck) -> ContextBuilder {
        self.context_builder_with(Runner::Session(self.jobs.clone()), check)
    }

    /// [`Self::context_builder`] with the host's executor policy `runner` (a host queue whose
    /// first worker took the session over through [`launch`]'s `takeover`). The source file of
    /// "Save to file" ([`ContextBuilder::source`]) is never filled: saving over the file a
    /// graph came from is the host's decision.
    pub fn context_builder_with(&self, runner: Runner, check: HostCheck) -> ContextBuilder {
        crate::Context::builder(runner, check)
            .presets(self.presets.clone())
            .store(self.store.clone())
            .commit_seq(self.commit_seq)
            .layered(self.layered)
    }
}

/// Starts a session thread ([`spawn_session`]) with `presets` serving the graph of `source`,
/// then runs the host's `extra` setup on the session (after the load and the statistics,
/// before the layer state is read; its answer comes back beside the [`Launched`]), and then
/// serves the job queue or, with `takeover` (the first worker of the host's
/// [`crate::JobQueue`]), hands the session to it.
///
/// # Errors
///
/// [`ServeError::Setup`] when the graph cannot be loaded or attached (its text is the
/// diagnostic: an unreadable file, `Unknown sample graph: ..`, a session error) or `extra`
/// fails (its error, shown as it is); [`ServeError::Thread`] when the session thread cannot
/// start.
pub fn launch<T, E>(
    source: ServedSource,
    presets: SessionPresets,
    extra: impl FnOnce(&mut Session) -> Result<T, E> + Send + 'static,
    takeover: Option<Job>,
) -> Result<(Launched, T), ServeError>
where
    T: Send + 'static,
    E: Into<SetupError> + Send + 'static,
{
    let presets_json = presets.to_value();
    let (store, layer_store) = match &source {
        ServedSource::Store(store) => (Some(store.clone()), None),
        ServedSource::LayerStore { store, .. } => (None, Some(store.clone())),
        ServedSource::Sample(_) | ServedSource::File(_) | ServedSource::Graph { .. } => {
            (None, None)
        }
    };
    let setup = move |session: &mut Session| -> Result<_, SetupError> {
        let warnings = load(session, source)?;
        let commit_seq = session
            .graph()
            .last_commit_seq()
            .map_err(|err| err.to_string())?;
        let statistics = session
            .statistics_with_process()
            .map_err(|err| err.to_string())?;
        let extra = extra(session).map_err(Into::into)?;
        let layered = session
            .graph()
            .capabilities()
            .map_err(|err| err.to_string())?
            .layered;
        Ok((
            session.cancel_token(),
            commit_seq,
            statistics,
            layered,
            warnings,
            extra,
        ))
    };
    let (jobs, (cancel, commit_seq, statistics, layered, warnings, extra)) =
        spawn_session(presets, setup, takeover)?;
    Ok((
        Launched {
            jobs,
            cancel,
            commit_seq,
            statistics,
            layered,
            presets: presets_json,
            store,
            layer_store,
            warnings,
        },
        extra,
    ))
}

/// Serves the graph of `source` in `session`; answers the load's warnings.
fn load(session: &mut Session, source: ServedSource) -> Result<Vec<String>, SetupError> {
    let info = match source {
        ServedSource::Sample(kind) => session.load_sample(&kind).map_err(String::from)?,
        ServedSource::File(path) => load_file(session, &path)?,
        ServedSource::Store(store) => {
            let label = store.location();
            session
                .attach_store(store, &label)
                .map_err(|err| err.to_string())?;
            return Ok(Vec::new());
        }
        ServedSource::LayerStore { store, title, kind } => {
            let graph: Arc<Graph<LayeredGraph>> = store.graph().clone();
            session
                .load_shared(graph, &title, &kind)
                .map_err(|err| err.to_string())?;
            return Ok(Vec::new());
        }
        ServedSource::Graph { graph, title, kind } => {
            session
                .load_shared(graph, &title, &kind)
                .map_err(|err| err.to_string())?;
            return Ok(Vec::new());
        }
    };
    Ok(info["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|warning| warning.as_str().unwrap_or_default().to_string())
        .collect())
}

/// Loads the graph file `path` into `session` by its extension; answers the graph info. The
/// file is read as it is parsed, never held in memory as a whole.
fn load_file(session: &mut Session, path: &Path) -> Result<Value, SetupError> {
    let file = std::fs::File::open(path)
        .map_err(|err| format!("Error: cannot read '{}': {err}", path.display()))?;
    let reader = std::io::BufReader::with_capacity(128 * 1024, file);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let info = if GraphSource::is_snapshot_path(path) {
        session.load_snapshot_reader(reader, &name)
    } else if GraphSource::is_graphson_path(path) {
        session.load_graphson_reader(reader, &name, None)
    } else {
        session.load_graphml_reader(reader, &name, None)
    };
    Ok(info.map_err(String::from)?)
}
