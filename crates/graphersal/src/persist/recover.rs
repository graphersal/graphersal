//! Recovery over streams (format spec 12.1): a packed snapshot plus the journals written
//! after it, replayed up to a target.

use std::io::Read;

use super::error::PersistError;
use super::snapshot::{ReadOptions, read_snapshot_into};
use super::wal::{JournalReader, JournalRecord, TornTail};
use crate::graph::PersistentStorage;
use crate::graph::TraversalGraph;
use crate::graph::hooks::Mark;

/// Where a recovery stops.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RecoveryTarget {
    /// The end of the journals.
    #[default]
    Latest,
    /// Right after this commit (`commit_seq` of a [`ChangeSet`](crate::graph::ChangeSet)).
    CommitSeq(u64),
    /// Right after the last commit whose time is at or before this time (microseconds since
    /// the Unix epoch, UTC). The end of the journals counts as reached.
    Time(i64),
    /// At the mark with this name ([`GraphStorage::mark`](crate::graph::GraphStorage::mark)): every commit up to the one it
    /// points at.
    Mark(String),
}

impl std::fmt::Display for RecoveryTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoveryTarget::Latest => f.write_str("latest"),
            RecoveryTarget::CommitSeq(seq) => write!(f, "commit {seq}"),
            RecoveryTarget::Time(time) => write!(f, "time {}", time_text(*time)),
            RecoveryTarget::Mark(name) => write!(f, "mark {name:?}"),
        }
    }
}

/// The civil UTC date and time of `micros` (microseconds since the Unix epoch):
/// `(year, month, day, hour, minute, second)` (Howard Hinnant's algorithm).
pub(crate) fn civil(micros: i64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = micros.div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rem / 3600, rem / 60 % 60, rem % 60)
}

/// `micros` as RFC 3339 in UTC (`2026-10-08T02:43:00Z`; with `.ffffff` when it is not a whole
/// second): the form the CLI's `--at-time` accepts back.
pub(crate) fn time_text(micros: i64) -> String {
    let (year, month, day, hour, minute, second) = civil(micros);
    let fraction = micros.rem_euclid(1_000_000);
    if fraction == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{fraction:06}Z")
    }
}

/// What a recovery did.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RecoveryReport {
    /// The snapshot's position (`0` without a snapshot).
    pub snapshot_commit_seq: u64,
    /// The position reached: the last commit replayed (or the snapshot's).
    pub commit_seq: u64,
    /// The time of that commit.
    pub committed_at: i64,
    /// How many journal commits were replayed onto the snapshot.
    pub commits_replayed: u64,
    /// Every mark read from the journals (before the stop), in journal order; pass the names to
    /// [`Journal::with_known_marks`](super::Journal::with_known_marks) of the next journal.
    pub marks: Vec<Mark>,
    /// The incomplete record the last journal ended in (an interrupted write, ignored).
    pub torn_tail: Option<TornTail>,
    /// `true` when the journals were read to their end; `false` when the target stopped the
    /// recovery earlier (a past state: attach a journal to it only as a new branch).
    pub reached_end: bool,
}

/// A recovered graph and its report.
#[non_exhaustive]
pub struct Recovered<S: PersistentStorage = TraversalGraph> {
    /// The graph, unpublished and without hooks: attach a new journal before sharing it.
    pub graph: S,
    /// What the recovery did.
    pub report: RecoveryReport,
}

/// A recovery: an optional packed snapshot, the journals written since (in order) and a
/// target ([`recover`] is the short form).
///
/// The journals must continue the snapshot without a gap: commits up to the snapshot's
/// position are skipped, the first one after it must be the next number, and every journal
/// must belong to the snapshot's lineage ([`PersistentStorage::graph_id`](crate::graph::PersistentStorage::graph_id)). Only the LAST journal
/// may end in an incomplete record (a torn tail, reported and ignored); a complete record with
/// a bad checksum anywhere is damage and fails the recovery with its location.
///
/// The recovered graph is a [`TraversalGraph`] ([`Recovery::new`]) or any
/// [`PersistentStorage`] ([`Recovery::new_in`] with an empty instance): the snapshot is
/// loaded through the storage's load sink ([`read_snapshot_into`]) and every journal commit is
/// applied by [`PersistentStorage::replay`].
#[must_use]
pub struct Recovery<'a, S: PersistentStorage = TraversalGraph> {
    snapshot: Option<Box<dyn Read + 'a>>,
    /// The empty storage the snapshot (or the first journal) is loaded into.
    empty: S,
    /// A graph to start from instead of a snapshot (a merge checkpoint's partial base state).
    start: Option<S>,
    /// Whether every mutation's before image is checked against the state it is replayed onto.
    verify_before_images: bool,
    journals: Vec<Box<dyn Read + 'a>>,
    target: RecoveryTarget,
    options: ReadOptions,
    /// A Store's lineage chain, newest first: `(graph_id, last commit it may hold)`. With it,
    /// journals of an ancestor lineage are accepted up to their branch point and a journal of
    /// a descendant continues the replay under its id (an in-place rollback, format spec 5.4, 12.5).
    chain: Option<Vec<(u128, u64)>>,
}

impl<S: PersistentStorage> std::fmt::Debug for Recovered<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recovered")
            .field("vertex_count", &self.graph.vertex_count())
            .field("edge_count", &self.graph.edge_count())
            .field("report", &self.report)
            .finish()
    }
}

impl Default for Recovery<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl Recovery<'_> {
    /// A recovery into a new [`TraversalGraph`], from nothing (an empty graph) to
    /// [`RecoveryTarget::Latest`].
    pub fn new() -> Self {
        Self::new_in(TraversalGraph::new())
    }
}

impl<'a, S: PersistentStorage> Recovery<'a, S> {
    /// A recovery into `empty`, an EMPTY storage of any [`PersistentStorage`] type (a new
    /// instance, or one emptied by [`PersistentStorage::clear`]), from nothing to
    /// [`RecoveryTarget::Latest`]. A storage that holds elements fails the run with
    /// [`PersistError::Graph`] holding
    /// [`GraphError::Unsupported`](crate::error::GraphError::Unsupported).
    pub fn new_in(empty: S) -> Self {
        Self {
            snapshot: None,
            empty,
            start: None,
            verify_before_images: false,
            journals: Vec::new(),
            target: RecoveryTarget::Latest,
            options: ReadOptions::default(),
            chain: None,
        }
    }

    /// The lineage chain of a Store (newest first, with each lineage's last valid commit).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn chain(mut self, chain: Vec<(u128, u64)>) -> Self {
        self.chain = Some(chain);
        self
    }

    /// The packed snapshot to start from. Without one the recovery starts from an empty graph
    /// and the first journal must start at commit 1.
    pub fn snapshot(mut self, reader: impl Read + 'a) -> Self {
        self.snapshot = Some(Box::new(reader));
        self
    }

    /// A loaded graph to start from instead of a snapshot: its position, time and lineage id
    /// play the snapshot's part (a merge checkpoint replays the WAL onto the elements of the
    /// base snapshot it touches, `store::merge`).
    pub(crate) fn start_graph(mut self, graph: S) -> Self {
        self.start = Some(graph);
        self
    }

    /// Checks every mutation's before image against the state it is replayed onto: a mismatch
    /// (the WAL does not continue that state) is damage (`PersistentStorage::replay` with `verify`).
    pub(crate) fn verify_before_images(mut self) -> Self {
        self.verify_before_images = true;
        self
    }

    /// Adds the next journal stream.
    pub fn journal(mut self, reader: impl Read + 'a) -> Self {
        self.journals.push(Box::new(reader));
        self
    }

    /// Where to stop.
    pub fn target(mut self, target: RecoveryTarget) -> Self {
        self.target = target;
        self
    }

    /// The snapshot read options (memory limit, value depth; the depth applies to journal
    /// records too).
    pub fn options(mut self, options: ReadOptions) -> Self {
        self.options = options;
        self
    }

    /// Runs the recovery.
    pub fn run(self) -> Result<Recovered<S>, PersistError> {
        let Recovery {
            snapshot,
            empty,
            start,
            verify_before_images,
            journals,
            target,
            options,
            chain,
        } = self;
        // `based`: the graph holds a base state (a start graph, a snapshot, or the first
        // journal's lineage); before that it is the empty storage.
        let (mut graph, based) = match (start, snapshot) {
            (Some(start), _) => (start, true),
            (None, Some(reader)) => (read_snapshot_into(reader, &options, empty)?, true),
            (None, None) => {
                if empty.vertex_count() != 0 || empty.edge_count() != 0 {
                    return Err(PersistError::Graph(crate::error::GraphError::unsupported(
                        "recovering into a storage that holds elements",
                    )));
                }
                (empty, false)
            }
        };
        let report = replay_journals(
            &mut graph,
            based,
            journals,
            target,
            &options,
            chain.as_deref(),
            verify_before_images,
        )?;
        Ok(Recovered { graph, report })
    }
}

/// A recovery INTO a borrowed storage (a Store's reload in place keeps the instance and its
/// hooks): `graph` must be empty (new, or emptied by [`PersistentStorage::clear`]); the packed
/// `snapshot` is loaded through its load sink, then `journals` are replayed up to `target`.
/// On error the storage is half-loaded: clear it.
pub(crate) fn recover_into<'a, S: PersistentStorage + ?Sized>(
    graph: &mut S,
    snapshot: impl Read,
    journals: Vec<Box<dyn Read + 'a>>,
    target: RecoveryTarget,
    options: &ReadOptions,
    chain: Option<&[(u128, u64)]>,
) -> Result<RecoveryReport, PersistError> {
    super::snapshot::load_snapshot(snapshot, options, graph)?;
    replay_journals(graph, true, journals, target, options, chain, false)
}

/// Replays `journals` onto `graph` up to `target` (the rules of [`Recovery`]); `based`: the
/// graph holds a base state (a snapshot or a start graph) whose position the journals continue.
fn replay_journals<'a, S: PersistentStorage + ?Sized>(
    graph: &mut S,
    mut based: bool,
    journals: Vec<Box<dyn Read + 'a>>,
    target: RecoveryTarget,
    options: &ReadOptions,
    chain: Option<&[(u128, u64)]>,
    verify_before_images: bool,
) -> Result<RecoveryReport, PersistError> {
    {
        let not_reached = |reason: String| PersistError::TargetNotReached {
            target: target.to_string(),
            reason,
        };
        let snapshot_seq = if based { graph.last_commit_seq()? } else { 0 };
        let snapshot_time = if based { graph.last_committed_at() } else { 0 };
        match &target {
            RecoveryTarget::CommitSeq(seq) if *seq < snapshot_seq => {
                return Err(not_reached(format!(
                    "the snapshot is at commit {snapshot_seq}, after the target"
                )));
            }
            RecoveryTarget::Time(time) if snapshot_seq > 0 && *time < snapshot_time => {
                return Err(not_reached(format!(
                    "the snapshot's last commit is at {snapshot_time}, after the target"
                )));
            }
            _ => {}
        }
        let mut report = RecoveryReport {
            snapshot_commit_seq: snapshot_seq,
            commit_seq: snapshot_seq,
            committed_at: snapshot_time,
            commits_replayed: 0,
            marks: Vec::new(),
            torn_tail: None,
            reached_end: true,
        };
        let mut stopped = matches!(target, RecoveryTarget::CommitSeq(seq) if seq == snapshot_seq);
        let mut mark_found = false;
        let journal_count = journals.len();
        let depth = options.max_value_depth();
        for (index, source) in journals.into_iter().enumerate() {
            if stopped {
                break;
            }
            let mut reader = JournalReader::open(source, index, depth)?
                .with_newer_schema(options.newer_schema.clone());
            let lineage = if based {
                graph.graph_id()
            } else {
                if reader.first_commit_seq() > 1 {
                    return Err(PersistError::Gap {
                        journal: index,
                        offset: 0,
                        expected: 1,
                        found: reader.first_commit_seq(),
                    });
                }
                graph.set_graph_id(reader.graph_id())?;
                based = true;
                graph.graph_id()
            };
            // The lineage limit of this journal's commits (a Store's chain: an ancestor is
            // valid up to its branch point; a descendant continues the graph under its id).
            let mut limit = u64::MAX;
            if reader.graph_id() != lineage {
                let position =
                    |id: u128| chain.and_then(|chain| chain.iter().position(|(g, _)| *g == id));
                match (position(reader.graph_id()), position(lineage)) {
                    // A descendant: the replay continues under the new lineage id.
                    (Some(journal_at), Some(graph_at)) if journal_at < graph_at => {
                        graph.set_graph_id(reader.graph_id())?;
                    }
                    // An ancestor: only its commits the snapshot already holds may appear.
                    (Some(_), Some(_)) => limit = snapshot_seq,
                    _ => {
                        return Err(PersistError::LineageMismatch {
                            journal: index,
                            expected: crate::value::uuid::format_canonical(lineage).to_string(),
                            found: crate::value::uuid::format_canonical(reader.graph_id())
                                .to_string(),
                        });
                    }
                }
            }
            if let Some(chain) = chain
                && let Some((_, valid)) = chain.iter().find(|(g, _)| *g == reader.graph_id())
            {
                limit = limit.min(*valid);
            }
            while let Some((offset, _, record)) = reader.next_record()? {
                match record {
                    JournalRecord::Commit(changes) => {
                        let seq = changes.commit_seq();
                        if seq <= snapshot_seq {
                            continue;
                        }
                        if seq > limit {
                            return Err(PersistError::corrupt(
                                format!("journal {index}"),
                                offset,
                                format!(
                                    "commit {seq} lies beyond the branch point (commit {limit}) \
                                     of its lineage"
                                ),
                            ));
                        }
                        let expected = report.commit_seq + 1;
                        if seq != expected {
                            return Err(PersistError::Gap {
                                journal: index,
                                offset,
                                expected,
                                found: seq,
                            });
                        }
                        let beyond = match &target {
                            RecoveryTarget::CommitSeq(target_seq) => seq > *target_seq,
                            RecoveryTarget::Time(time) => changes.committed_at() > *time,
                            _ => false,
                        };
                        if beyond {
                            stopped = true;
                            break;
                        }
                        let verify = verify_before_images;
                        PersistentStorage::replay(graph, &changes, verify).map_err(|reason| {
                            let reason = if verify {
                                format!(
                                    "commit {seq} does not continue the state it is \
                                         replayed onto: {reason}"
                                )
                            } else {
                                format!("commit {seq} cannot be replayed: {reason}")
                            };
                            PersistError::corrupt(format!("journal {index}"), offset, reason)
                        })?;
                        report.commit_seq = seq;
                        report.committed_at = changes.committed_at();
                        report.commits_replayed += 1;
                        if matches!(target, RecoveryTarget::CommitSeq(target_seq) if target_seq == seq)
                        {
                            stopped = true;
                            break;
                        }
                    }
                    JournalRecord::Mark(mark) => {
                        let hit =
                            matches!(&target, RecoveryTarget::Mark(name) if name == mark.name());
                        let mark_seq = mark.commit_seq();
                        report.marks.push(mark);
                        if hit {
                            if mark_seq < snapshot_seq {
                                return Err(not_reached(format!(
                                    "the mark points at commit {mark_seq}, before the snapshot \
                                     (commit {snapshot_seq})"
                                )));
                            }
                            if mark_seq != report.commit_seq {
                                return Err(PersistError::corrupt(
                                    format!("journal {index}"),
                                    offset,
                                    format!(
                                        "the mark points at commit {mark_seq}, but the journal \
                                         is at commit {}",
                                        report.commit_seq
                                    ),
                                ));
                            }
                            mark_found = true;
                            stopped = true;
                            break;
                        }
                    }
                    JournalRecord::Checkpoint { .. } => {}
                }
            }
            if let Some(torn) = reader.torn_tail() {
                if index + 1 != journal_count {
                    return Err(PersistError::corrupt(
                        format!("journal {index}"),
                        torn.offset,
                        "incomplete record in a journal that is not the last one",
                    ));
                }
                report.torn_tail = Some(torn);
            }
        }
        report.reached_end = !stopped;
        match &target {
            RecoveryTarget::CommitSeq(seq) if report.commit_seq < *seq => {
                return Err(not_reached(format!(
                    "the journals end at commit {}",
                    report.commit_seq
                )));
            }
            RecoveryTarget::Mark(_) if !mark_found => {
                return Err(not_reached("no such mark in the journals".to_string()));
            }
            _ => {}
        }
        // The replayed elements grew the slot vectors past the snapshot's exact reservation:
        // release the spare capacity, as the end of a load does.
        if report.commits_replayed > 0 {
            graph.shrink_to_fit()?;
        }
        Ok(report)
    }
}

/// Recovers a graph from a packed snapshot and the journals written after it, up to `target`
/// (see [`Recovery`] for the rules and for a recovery without a snapshot).
pub fn recover<S: Read, J: Read>(
    snapshot: S,
    journals: impl IntoIterator<Item = J>,
    target: RecoveryTarget,
) -> Result<Recovered, PersistError> {
    let mut recovery = Recovery::new().snapshot(snapshot).target(target);
    for journal in journals {
        recovery = recovery.journal(journal);
    }
    recovery.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_target_displays_as_rfc_3339() {
        assert_eq!(
            RecoveryTarget::Time(1_760_000_000_000_000).to_string(),
            "time 2025-10-09T08:53:20Z"
        );
        assert_eq!(
            time_text(1_760_000_000_000_042),
            "2025-10-09T08:53:20.000042Z"
        );
        assert_eq!(time_text(0), "1970-01-01T00:00:00Z");
        assert_eq!(time_text(-1), "1969-12-31T23:59:59.999999Z");
    }

    /// The generic replay (`PersistentStorage::replay`'s default, through the trait's write
    /// methods; the test-only `PersistentStorage for Box<TraversalGraph>` keeps it) recovers a
    /// recorded WAL to the state `TraversalGraph`'s raw override reaches, verified or not.
    #[test]
    fn the_default_replay_recovers_a_recorded_wal_like_the_override() {
        use crate::error::GraphError;
        use crate::graph::definition::{Definition, QueryDefinition};
        use crate::graph::schema::{GraphSchema, PropertySchema};
        use crate::graph::{GraphStorage, Transactional};
        use crate::persist::encoder::tests::fingerprint;
        use crate::persist::snapshot::{WriteOptions, write_packed};
        use crate::persist::{Durability, Journal, SharedBuffer};
        use crate::value::PropertyMap;

        let mut live = TraversalGraph::tinkerpop_modern();
        let mut snapshot = Vec::new();
        write_packed(&live, &mut snapshot, &WriteOptions::new()).unwrap();
        let wal = SharedBuffer::new();
        Journal::new(wal.clone(), Durability::EveryCommit)
            .attach(&mut live)
            .unwrap();
        live.transaction(|g| {
            g.set_schema(GraphSchema::new().with_vertex("person", PropertySchema::object()))?;
            g.set_definition(Definition::query(
                "all",
                QueryDefinition::new("fn all() { g.v() }"),
            ))?;
            let marko = g
                .get_vertex_by_id("1")
                .ok_or(GraphError::unsupported("marko"))?;
            let lop = g
                .get_vertex_by_id("3")
                .ok_or(GraphError::unsupported("lop"))?;
            g.set_vertex_property(marko, "age".into(), 30i64.into())?;
            g.add_vertex_label(marko, "friend".into())?;
            let ann = g.insert_vertex(None, vec!["person".into()], PropertyMap::default())?;
            g.insert_edge(None, Some("likes".into()), ann, lop, PropertyMap::default())?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
        live.transaction(|g| {
            let josh = g
                .get_vertex_by_id("4")
                .ok_or(GraphError::unsupported("josh"))?;
            g.drop_vertex(josh)?;
            let created = g.get_edge_by_id("1").ok_or(GraphError::unsupported("e1"))?;
            g.drop_edge_property("weight", created)?;
            Ok::<_, GraphError>(())
        })
        .unwrap();
        let wal = wal.contents();

        let state = |graph: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = Vec::new();
            graph(&mut bytes);
            fingerprint(&bytes)
        };
        let expected = state(&|out| {
            write_packed(&live, out, &WriteOptions::new()).unwrap();
        });
        for verify in [false, true] {
            let run = |recovery: Recovery<'_, Box<TraversalGraph>>| {
                let recovery = recovery
                    .snapshot(snapshot.as_slice())
                    .journal(wal.as_slice());
                let recovery = if verify {
                    recovery.verify_before_images()
                } else {
                    recovery
                };
                recovery.run().unwrap()
            };
            let generic = run(Recovery::new_in(Box::new(TraversalGraph::new())));
            let mut own = Recovery::new()
                .snapshot(snapshot.as_slice())
                .journal(wal.as_slice());
            if verify {
                own = own.verify_before_images();
            }
            let own = own.run().unwrap();
            assert_eq!(generic.report, own.report, "verify {verify}");
            assert_eq!(generic.report.commits_replayed, 2);
            let generic_state = state(&|out| {
                write_packed(&*generic.graph, out, &WriteOptions::new()).unwrap();
            });
            let own_state = state(&|out| {
                write_packed(&own.graph, out, &WriteOptions::new()).unwrap();
            });
            assert_eq!(generic_state, expected, "verify {verify}");
            assert_eq!(own_state, expected, "verify {verify}");
            assert_eq!(
                generic.graph.get_schema().unwrap(),
                live.get_schema().unwrap()
            );
            assert_eq!(generic.graph.definitions().unwrap().count(), 1);
            assert_eq!(
                generic.graph.last_commit_seq().unwrap(),
                live.last_commit_seq()
            );
            assert_eq!(generic.graph.id_sequences(), live.id_sequences());
        }
    }
}
