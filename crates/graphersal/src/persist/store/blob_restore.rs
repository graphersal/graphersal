//! Blob recovery (format spec 4.6, 12.4; book page "Blobs", section "Damage"): the blobs a
//! store references that are missing or damaged are copied back from another copy of them
//! (`Store::restore_blobs_from`, CLI `graphersal store restore-blobs <STORE> --from <SOURCE>`).
//! A blob is named by its hash, so the same name anywhere is the same bytes: a backup, a ZIP
//! backup, a packed snapshot or another store holding it is a valid source. Every restored blob
//! is checked by length, CRC-32C and (for a known algorithm) its hash before it is renamed into
//! place; a healthy blob is never touched.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use super::blobs::{self, BlobError, BlobFault, BlobKey};
use super::closed_commit::closed_store_prelude;
use super::files::DirPath;
use super::{Store, system_journal};
use crate::graph::system_journal::{Event, catalogue};
use crate::persist::PersistError;
use crate::persist::dir::{IntoStoreDir, StoreDir};
use crate::value::BlobRef;

/// Where [`Store::restore_blobs_from`] looks for the blobs.
pub struct BlobSource {
    kind: SourceKind,
    label: String,
}

enum SourceKind {
    Dir(Arc<dyn StoreDir>),
    #[cfg(feature = "persist-zip")]
    Zip(Box<dyn Read + Send>),
    Packed(Box<dyn Read + Send>),
}

impl std::fmt::Debug for BlobSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobSource")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl BlobSource {
    /// Another copy of the store's files: a backup directory (full, or with its increments),
    /// another store (a fork, a repaired copy, a restored backup), a single-file `.gstore`.
    /// Only its blobs are read (no lock: it may be open elsewhere).
    pub fn store(dir: impl IntoStoreDir) -> Self {
        let dir = dir.into_store_dir();
        let label = dir.location();
        Self {
            kind: SourceKind::Dir(dir),
            label,
        }
    }

    /// A ZIP backup (`Store::backup_zip`, CLI `store backup --zip`), read once from start to
    /// end; `label` names it in the report and the journal.
    #[cfg(feature = "persist-zip")]
    pub fn zip(reader: impl Read + Send + 'static, label: impl Into<String>) -> Self {
        Self {
            kind: SourceKind::Zip(Box::new(reader)),
            label: label.into(),
        }
    }

    /// A packed snapshot (`.gsnap`, `store export`), read once from start to end; `label` names
    /// it in the report and the journal.
    pub fn packed(reader: impl Read + Send + 'static, label: impl Into<String>) -> Self {
        Self {
            kind: SourceKind::Packed(Box::new(reader)),
            label: label.into(),
        }
    }

    /// What the report and the journal call the source.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// What [`Store::restore_blobs_from`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RestoreBlobsReport {
    /// The source, as [`BlobSource::label`] names it.
    pub source: String,
    /// The blobs the store's retained history references.
    pub referenced: u64,
    /// The ones that were missing or damaged before the restore (relative names `blobs/..`).
    pub wanted: Vec<String>,
    /// The ones restored from the source.
    pub restored: Vec<String>,
    /// The bytes restored.
    pub bytes_restored: u64,
    /// The ones the source does not hold.
    pub not_found: Vec<String>,
    /// The ones the source holds damaged too (not restored).
    pub damaged_in_source: Vec<String>,
}

impl RestoreBlobsReport {
    /// Whether every blob the store references is intact now.
    pub fn is_complete(&self) -> bool {
        self.not_found.is_empty() && self.damaged_in_source.is_empty()
    }
}

/// The outcome of one blob read from the source.
enum Outcome {
    Restored,
    Damaged,
}

/// Restores `blob` from the staged file `staged` (already written from the source): checked
/// and moved into place, or removed.
fn install(dir: &DirPath, blob: &BlobRef, staged: &DirPath) -> Result<Outcome, PersistError> {
    match blobs::install_staged(dir, blob, staged) {
        Ok(()) => Ok(Outcome::Restored),
        Err(PersistError::Corrupt { .. }) => {
            let _ = staged.remove_file();
            Ok(Outcome::Damaged)
        }
        Err(err) => {
            let _ = staged.remove_file();
            Err(err)
        }
    }
}

impl Store {
    /// Restores the blobs of the CLOSED store in `dir` that are missing or damaged (format spec
    /// 4.6; CLI `graphersal store restore-blobs <dir> --from <source>`) from `source`: every
    /// blob the store's retained history references is checked (length, CRC-32C, hash), each
    /// faulty one is looked up in the source by its algorithm and hash (a blob of an unknown
    /// algorithm by its name), streamed into a temporary file, checked the same way and renamed
    /// into place; a healthy blob is never touched. The writer lock is taken (a store open
    /// elsewhere: [`PersistError::Locked`]); a backup is refused. Every restored blob is
    /// recorded in the system journal (`blob.repaired`, `how` = `restored`). The report lists
    /// what was restored, what the source does not hold and what it holds damaged too
    /// ([`RestoreBlobsReport::is_complete`]).
    pub fn restore_blobs_from(
        dir: impl IntoStoreDir,
        source: BlobSource,
    ) -> Result<RestoreBlobsReport, PersistError> {
        let dir = DirPath::root(dir.into_store_dir());
        let (lock, graph_file) = closed_store_prelude(&dir)?;
        let referenced = blobs::referenced(&dir, &graph_file.chain(), false)?;
        let mut report = RestoreBlobsReport {
            source: source.label.clone(),
            referenced: referenced.blobs.len() as u64,
            ..RestoreBlobsReport::default()
        };
        let mut wanted: BTreeMap<BlobKey, BlobRef> = BTreeMap::new();
        for (key, blob) in &referenced.blobs {
            match blobs::check_blob(&dir, blob, true) {
                Ok(()) => {}
                Err(BlobError::Fault(_)) => {
                    report.wanted.push(blobs::blob_name(blob));
                    wanted.insert(key.clone(), blob.clone());
                }
                Err(BlobError::Io(err)) => return Err(PersistError::io("checking a blob", err)),
                Err(BlobError::DoesNotFit(err)) => return Err(PersistError::Graph(err)),
                Err(BlobError::Unsupported) => {}
            }
        }
        let mut done: BTreeMap<BlobKey, Outcome> = BTreeMap::new();
        if !wanted.is_empty() {
            match source.kind {
                SourceKind::Dir(from) => {
                    let from = DirPath::root(from);
                    for (key, blob) in &wanted {
                        match blobs::check_blob(&from, blob, true) {
                            Ok(()) => {}
                            Err(BlobError::Fault(BlobFault::Missing)) => continue,
                            Err(BlobError::Fault(_)) => {
                                done.insert(key.clone(), Outcome::Damaged);
                                continue;
                            }
                            Err(BlobError::Io(err)) => {
                                return Err(PersistError::io("reading the source's blob", err));
                            }
                            Err(BlobError::DoesNotFit(err)) => {
                                return Err(PersistError::Graph(err));
                            }
                            Err(BlobError::Unsupported) => continue,
                        }
                        match blobs::copy_blob(&from, &dir, blob, true) {
                            Ok(_) => {
                                done.insert(key.clone(), Outcome::Restored);
                            }
                            Err(BlobError::Fault(_)) => {
                                done.insert(key.clone(), Outcome::Damaged);
                            }
                            Err(BlobError::Io(err)) => {
                                return Err(PersistError::io("restoring a blob", err));
                            }
                            Err(BlobError::DoesNotFit(err)) => {
                                return Err(PersistError::Graph(err));
                            }
                            Err(BlobError::Unsupported) => {}
                        }
                    }
                }
                #[cfg(feature = "persist-zip")]
                SourceKind::Zip(reader) => {
                    let by_name: BTreeMap<String, (BlobKey, BlobRef)> = wanted
                        .iter()
                        .map(|(key, blob)| (blobs::blob_name(blob), (key.clone(), blob.clone())))
                        .collect();
                    let mut failure: Option<PersistError> = None;
                    let problems = super::zip::check_entries(reader, &mut |name, _, data| {
                        if failure.is_some() {
                            return Vec::new();
                        }
                        let Some((key, blob)) = by_name.get(name) else {
                            return Vec::new();
                        };
                        let staged = match blobs::stage(&dir) {
                            Ok(staged) => staged,
                            Err(err) => {
                                failure = Some(err);
                                return Vec::new();
                            }
                        };
                        let written = super::snapshot_dir::write_file(&staged, |out| {
                            std::io::copy(data, out)
                                .map(|_| ())
                                .map_err(|err| PersistError::io("reading the backup archive", err))
                        });
                        let outcome = match written {
                            Ok(()) => install(&dir, blob, &staged),
                            Err(err) => {
                                let _ = staged.remove_file();
                                Err(err)
                            }
                        };
                        match outcome {
                            Ok(outcome) => {
                                done.insert(key.clone(), outcome);
                            }
                            Err(err) => failure = Some(err),
                        }
                        Vec::new()
                    });
                    if let Some(err) = failure {
                        return Err(err);
                    }
                    if let Some((_, reason)) = problems.iter().find(|(name, _)| name == ".") {
                        return Err(PersistError::corrupt(&report.source, 0, reason.clone()));
                    }
                    // An entry whose ZIP CRC fails was checked against the blob's own CRC-32C
                    // and hash already: a restored blob is intact whatever the archive says.
                }
                SourceKind::Packed(reader) => {
                    let mut reader = crate::persist::snapshot::PackedReader::open(reader)?;
                    let table = reader.manifest.blobs.clone();
                    for entry in reader.manifest.segments.clone() {
                        let blob = (entry.kind == blobs::PACKED_BLOB_KIND)
                            .then(|| blobs::packed_blob_ref(&entry, &table).ok())
                            .flatten()
                            .filter(|blob| wanted.contains_key(&blobs::key_of(blob)));
                        let Some(blob) = blob else {
                            reader.copy_segment(&mut std::io::sink())?;
                            continue;
                        };
                        let staged = blobs::stage(&dir)?;
                        let written = super::snapshot_dir::write_file(&staged, |out| {
                            reader.copy_segment(out).map(|_| ())
                        });
                        let outcome = match written {
                            Ok(()) => install(&dir, &blob, &staged)?,
                            Err(PersistError::Corrupt { .. }) => {
                                let _ = staged.remove_file();
                                Outcome::Damaged
                            }
                            Err(err) => {
                                let _ = staged.remove_file();
                                return Err(err);
                            }
                        };
                        done.insert(blobs::key_of(&blob), outcome);
                    }
                }
            }
        }
        for (key, blob) in &wanted {
            let name = blobs::blob_name(blob);
            match done.get(key) {
                Some(Outcome::Restored) => {
                    report.bytes_restored += blob.length();
                    report.restored.push(name);
                }
                Some(Outcome::Damaged) => report.damaged_in_source.push(name),
                None => report.not_found.push(name),
            }
        }
        if !report.restored.is_empty() {
            let clock = crate::graph::CommitClock::new();
            let journal = system_journal::open_journal(system_journal::OpenJournal {
                dir: &dir,
                clock: &clock,
                writable: true,
                instance_name: None,
                segment_bytes: system_journal::DEFAULT_JOURNAL_SEGMENT_BYTES,
            });
            for (key, blob) in &wanted {
                if matches!(done.get(key), Some(Outcome::Restored)) {
                    journal.emit(
                        &catalogue::BLOB_REPAIRED,
                        Event::new(format!(
                            "the blob {} was restored from {}",
                            blobs::blob_name(blob),
                            report.source
                        ))
                        .field("hash", blob.hash_hex())
                        .field("how", "restored")
                        .field("source", report.source.as_str()),
                    );
                }
            }
            journal.detach_writer();
        }
        drop(lock);
        Ok(report)
    }
}
