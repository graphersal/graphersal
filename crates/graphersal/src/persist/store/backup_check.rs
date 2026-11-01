//! The checks of a backup (format spec 12.6-12.8): every checksum of what a backup copies is
//! read in the SOURCE before the copy (`GRAPH`, `marks`, the snapshot files, every WAL record
//! with its header and body CRC, commit continuity, the WAL segment set of 9.8) and again in
//! the COPY after it was written. Damage stops the backup with a report in the form of
//! [`VerifyProblem`]s; damage of redundant metadata (one `GRAPH` copy, one manifest fixed part,
//! the derived `marks` file) is a warning, as in the damage scan (format spec 13.2).

use super::super::error::{BackupDamage, DamageSide, PersistError};
use super::super::manifest::{FIXED_BYTES, ManifestFixed};
use super::super::wal::{JournalReader, JournalRecord, WAL_HEADER_BYTES};
use super::files::{
    DirPath, GRAPH_COPY, GRAPH_FILE, GraphFile, MARKS_FILE, WAL_DIR, read_bounded, wal_name,
};
use super::maintenance::VerifyProblem;
use super::snapshot_dir::{MANIFEST_FILE, verify_snapshot_dir};
use super::wal_files::{decode_marks, open_segment_reader};

/// The problems and warnings a check collected.
#[derive(Debug, Default)]
pub(super) struct Findings {
    pub(super) problems: Vec<VerifyProblem>,
    pub(super) warnings: Vec<String>,
}

impl Findings {
    pub(super) fn problem(&mut self, path: impl Into<String>, offset: Option<u64>, reason: String) {
        self.problems.push(VerifyProblem {
            path: path.into(),
            offset,
            reason,
        });
    }

    pub(super) fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }

    /// The error of a backup that stopped on these problems.
    pub(super) fn into_error(
        self,
        side: DamageSide,
        store: &DirPath,
        backup: String,
        from_memory: bool,
    ) -> PersistError {
        PersistError::BackupDamaged(Box::new(BackupDamage {
            side,
            store: store.display(),
            backup,
            problems: self.problems,
            from_memory,
        }))
    }
}

/// `GRAPH` and `GRAPH.copy` of the source: the valid one (the longer lineage, as a load takes
/// it); one damaged copy is a warning (the backup writes two fresh ones), both damaged is
/// damage. Neither valid but one of an older store format: [`PersistError::OlderFormat`] (a
/// store this build does not read, not damage).
pub(super) fn check_graph_files(
    dir: &DirPath,
    findings: &mut Findings,
) -> Result<Option<GraphFile>, PersistError> {
    Ok(match super::files::decode_graph_copies(dir)? {
        (Ok(main), Ok(copy)) => Some(if copy.lineage.len() >= main.lineage.len() {
            copy
        } else {
            main
        }),
        (Ok(file), Err(err)) | (Err(err), Ok(file)) => {
            findings.warnings.push(
                crate::graph::note_text::NoteText::new(format!(
                    "one copy of GRAPH is not usable ({err}); the other one was used and the \
                     backup has two intact copies (a repair or the next read-write open of the \
                     store rewrites it)"
                ))
                .command("list it", "graphersal store verify <dir>")
                .finish(),
            );
            Some(file)
        }
        (Err(main), Err(copy)) => {
            findings.problem(GRAPH_FILE, None, format!("not usable: {main}"));
            findings.problem(GRAPH_COPY, None, format!("not usable: {copy}"));
            None
        }
    })
}

/// The source's `marks` file: its bytes when it is intact (`None`: missing). A damaged one is
/// a warning: it is derived from the WAL (format spec 10), and the backup gets one rebuilt
/// from the WAL it copies.
pub(super) fn check_marks(dir: &DirPath, findings: &mut Findings) -> Option<Vec<u8>> {
    let path = dir.join(MARKS_FILE);
    if !path.exists() {
        return None;
    }
    match read_bounded(&path, 256 << 20) {
        Ok(bytes) if decode_marks(&bytes).is_some() => Some(bytes),
        Ok(_) => {
            findings.warnings.push(
                "the store's marks file is damaged (it is derived from the WAL): the backup's \
                 is rebuilt from the WAL it holds"
                    .into(),
            );
            None
        }
        Err(err) => {
            findings.warnings.push(format!(
                "the store's marks file cannot be read ({err}): the backup's is rebuilt from \
                 the WAL it holds"
            ));
            None
        }
    }
}

/// A snapshot directory: every checksum (manifest, `schema.json`, every segment and chunk with
/// the chunk index, referenced files included) and both copies of the manifest's fixed part.
///
/// In the STORE (`heal`), one damaged copy of the fixed part is a warning: the load uses the
/// other one, and the backup gets the manifest with the intact copy in both places (byte for
/// byte what the writer wrote), returned here. In a COPY (`heal` false) it is damage.
pub(super) fn check_snapshot(
    dir: &DirPath,
    snapshot: &DirPath,
    heal: bool,
    findings: &mut Findings,
) -> Option<Vec<u8>> {
    let relative = snapshot.relative_to(dir);
    if let Err(err) = verify_snapshot_dir(snapshot) {
        let root = dir.display();
        match &err {
            PersistError::Corrupt {
                location,
                offset,
                reason,
            } => {
                let file = location
                    .strip_prefix(&root)
                    .map(|rest| rest.trim_start_matches('/').to_string())
                    .unwrap_or_else(|| relative.clone());
                findings.problem(file, Some(*offset), reason.clone());
            }
            other => findings.problem(relative, None, other.to_string()),
        }
        return None;
    }
    let path = snapshot.join(MANIFEST_FILE);
    let bytes = match read_bounded(&path, 256 << 20) {
        Ok(bytes) if bytes.len() >= 2 * FIXED_BYTES => bytes,
        Ok(_) => {
            findings.problem(
                path.relative_to(dir),
                None,
                "shorter than its two fixed parts".into(),
            );
            return None;
        }
        Err(err) => {
            findings.problem(path.relative_to(dir), None, err.to_string());
            return None;
        }
    };
    let tail_at = bytes.len() - FIXED_BYTES;
    let head = ManifestFixed::decode(&bytes[..FIXED_BYTES]);
    let tail = ManifestFixed::decode(&bytes[tail_at..]);
    let name = path.relative_to(dir);
    match (head, tail) {
        (Ok(h), Ok(t)) if h == t => None,
        // One copy damaged: the other one is the one the load uses (the head first).
        (Ok(_), Err(err)) | (Err(err), Ok(_)) if heal => {
            let mut healed = bytes.clone();
            if ManifestFixed::decode(&bytes[..FIXED_BYTES]).is_ok() {
                healed[tail_at..].copy_from_slice(&bytes[..FIXED_BYTES]);
            } else {
                healed[..FIXED_BYTES].copy_from_slice(&bytes[tail_at..]);
            }
            findings.warnings.push(format!(
                "{name}: one copy of the fixed part is damaged ({}); the intact one was used \
                 and the backup holds it in both places: plan a repair of the store",
                err.reason
            ));
            Some(healed)
        }
        (Ok(_), Ok(_)) => {
            findings.problem(name, None, "the two copies of the fixed part differ".into());
            None
        }
        (Err(err), _) => {
            findings.problem(name, Some(err.offset), format!("head copy: {}", err.reason));
            None
        }
        (_, Err(err)) => {
            findings.problem(
                name,
                Some(tail_at as u64 + err.offset),
                format!("tail copy: {}", err.reason),
            );
            None
        }
    }
}

/// What a verified read of one WAL segment found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SegmentCheck {
    /// The end of the last complete record read: the bytes a copy takes.
    pub(super) end: u64,
    /// Where the last record starts (`None`: no record).
    pub(super) last_record: Option<u64>,
    /// `(record start, commit_seq)` of every Commit record.
    pub(super) commits: Vec<(u64, u64)>,
}

/// Reads WAL segment `first` (`path`) up to `limit` bytes (the whole file without one),
/// checking its header against its name and the lineage chain (`chain`, when given), every
/// record's header and body CRC and that every commit lies at or after the segment's first
/// commit. `torn_ok`: an incomplete last record is the torn tail of a segment still being
/// written (the copy ends before it); otherwise it is damage. `Err` holds the problems.
pub(super) fn check_segment(
    dir: &DirPath,
    first: u64,
    path: &DirPath,
    limit: Option<u64>,
    torn_ok: bool,
    chain: Option<&GraphFile>,
    findings: &mut Findings,
) -> Option<SegmentCheck> {
    let relative = path.relative_to(dir);
    match open_segment_reader(path, limit) {
        Ok(source) => check_segment_stream(&relative, first, source, torn_ok, chain, findings),
        Err(err) => {
            let (offset, reason) = located(err);
            findings.problem(relative, offset, reason);
            None
        }
    }
}

/// The offset and reason of a read error.
fn located(err: PersistError) -> (Option<u64>, String) {
    match err {
        PersistError::Corrupt { offset, reason, .. } => (Some(offset), reason),
        other => (None, other.to_string()),
    }
}

/// [`check_segment`] of a segment read as a stream (`relative`: its name for the problems).
pub(super) fn check_segment_stream(
    relative: &str,
    first: u64,
    source: impl std::io::Read,
    torn_ok: bool,
    chain: Option<&GraphFile>,
    findings: &mut Findings,
) -> Option<SegmentCheck> {
    let relative = relative.to_string();
    let mut reader =
        match JournalReader::open(source, 0, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH) {
            Ok(reader) => reader,
            Err(err) => {
                let (offset, reason) = located(err);
                findings.problem(relative, offset, reason);
                return None;
            }
        };
    let before = findings.problems.len();
    if reader.first_commit_seq() != first {
        findings.problem(
            relative.clone(),
            Some(28),
            format!(
                "its header says first commit {}, its name {first}: another segment's bytes",
                reader.first_commit_seq()
            ),
        );
    }
    if let Some(chain) = chain
        && !chain.on_chain(reader.graph_id())
    {
        findings.problem(
            relative.clone(),
            Some(12),
            "a lineage not on the store's chain".into(),
        );
    }
    let mut check = SegmentCheck {
        end: WAL_HEADER_BYTES as u64,
        last_record: None,
        commits: Vec::new(),
    };
    loop {
        match reader.next_record() {
            Ok(Some((start, _, record))) => {
                check.last_record = Some(start);
                check.end = reader.offset();
                if let JournalRecord::Commit(changes) = record {
                    let seq = changes.commit_seq();
                    if seq < first {
                        findings.problem(
                            relative.clone(),
                            Some(start),
                            format!("commit {seq} before the segment's first commit {first}"),
                        );
                    }
                    check.commits.push((start, seq));
                }
            }
            Ok(None) => break,
            // Content of a newer version behind valid checksums (format spec 16): the copy
            // takes its bytes as they are; not damage.
            Err(PersistError::UnsupportedContent { .. }) => {
                check.end = reader.offset();
                if let Some((start, seq)) = reader.unsupported_seq() {
                    check.last_record = Some(start);
                    check.commits.push((start, seq));
                }
            }
            Err(err) => {
                let (offset, reason) = located(err);
                findings.problem(relative.clone(), offset, reason);
                break;
            }
        }
    }
    if let Some(torn) = reader.torn_tail()
        && !torn_ok
    {
        findings.problem(
            relative,
            Some(torn.offset),
            format!(
                "an incomplete record ({} bytes) before the end the backup copies",
                torn.bytes
            ),
        );
    }
    (findings.problems.len() == before).then_some(check)
}

/// Checks that the commits of the WAL a backup copies follow each other without a gap or an
/// overlap: every commit is the previous one + 1, and the first commit after `after` (the
/// snapshot or the backup's position; `None`: not checked) is `after + 1`. `segments` are the
/// copied segments in order with their checks.
pub(super) fn check_continuity(
    segments: &[(u64, &SegmentCheck)],
    after: Option<u64>,
    findings: &mut Findings,
) {
    let mut previous: Option<u64> = None;
    for (first, check) in segments {
        for (start, seq) in &check.commits {
            let expected = match previous {
                Some(prev) => Some(prev.saturating_add(1)),
                None => after
                    .filter(|after| *seq > *after)
                    .map(|after| after.saturating_add(1)),
            };
            if let Some(expected) = expected
                && *seq != expected
            {
                findings.problem(
                    format!("{WAL_DIR}/{}", wal_name(*first)),
                    Some(*start),
                    format!("commit {seq} where commit {expected} must follow (a gap in the WAL)"),
                );
            }
            previous = Some(*seq);
        }
    }
}
