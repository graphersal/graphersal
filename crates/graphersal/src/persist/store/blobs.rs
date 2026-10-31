//! Blobs (format spec 4.6) and the blob ledger (format spec 10.1): immutable, content-addressed
//! files outside the WAL, referenced from WAL values and catalog payloads by a [`BlobRef`],
//! kept exactly as long as some retained history references them.
//!
//! - `blobs/<2 hex>/<hash hex>`: one blob, its whole content (SHA-256); another algorithm
//!   lives under `blobs/<algo name>/<2 hex>/<hash hex>`. `blobs/tmp-<32 hex>`: a blob being
//!   written (removed at open).
//! - `ledger`: frames `length u32 | CRC u32 | commit_seq u64 | graph_id 16 | blobref | delta
//!   svar`, one per (commit, blob) with a net delta, appended after the commit is durable;
//!   derived (rebuilt from the WAL when missing or damaged), never damage.
//!
//! A blob's damage is never store damage: reads that need it fail with
//! [`StoreFailure::BlobUnavailable`] / [`StoreFailure::BlobDamaged`], everything else goes on.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::{Arc, Mutex};

use super::super::codec::{
    DecodeResult, Reader, crc, crc_append, put_blob_ref, put_svar, put_u32, put_u64, read_blob_ref,
};
use super::super::error::{PersistError, StoreFailure};
use super::super::wal::{JournalReader, JournalRecord};
use super::files::{self, DirPath, EntryKind};
use crate::graph::Definition;
use crate::graph::change::{ChangeSet, Mutation};
use crate::value::BlobRef;
use crate::value::blob::BlobHashAlgo;
use crate::value::vector::embed::sha256::Sha256;

/// The directory of a store's blobs (format spec 4.6).
pub(crate) const BLOBS_DIR: &str = "blobs";
/// The blob ledger (format spec 10.1).
pub(crate) const LEDGER_FILE: &str = "ledger";
/// A blob being written.
const TMP_PREFIX: &str = "tmp-";
/// The copy buffer.
const BUFFER: usize = 1 << 16;
/// The operation a blob read names when it does not fit the memory ceiling.
pub(crate) const BLOB_READ_OPERATION: &str = "blob read";
/// The largest ledger file read whole (frames are small; a larger file is rebuilt).
const MAX_LEDGER_BYTES: u64 = 256 << 20;

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// What is wrong with a blob's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobFault {
    /// The file is missing.
    Missing,
    /// Its length is not the reference's.
    Length,
    /// Its CRC-32C is not the reference's.
    Crc,
    /// Its hash is not its name (verify, foreign input).
    Hash,
}

impl BlobFault {
    /// The `failure` field of the journal event `blob.damaged`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            BlobFault::Missing => "missing",
            BlobFault::Length => "length",
            BlobFault::Crc => "crc",
            BlobFault::Hash => "hash",
        }
    }
}

/// Why a blob could not be read (crate-internal; mapped to a [`PersistError`] only where an
/// operation needs the bytes).
#[derive(Debug)]
pub(crate) enum BlobError {
    /// Missing or damaged.
    Fault(BlobFault),
    /// Written by a newer version's hash algorithm: never read nor verified.
    Unsupported,
    /// Reading it would take the process over its memory ceiling.
    DoesNotFit(crate::error::GraphError),
    /// The backend failed.
    Io(std::io::Error),
}

impl BlobError {
    /// The fault, when the blob itself is missing or damaged.
    pub(crate) fn fault(&self) -> Option<BlobFault> {
        match self {
            BlobError::Fault(fault) => Some(*fault),
            BlobError::Unsupported | BlobError::DoesNotFit(_) | BlobError::Io(_) => None,
        }
    }

    /// The error of an operation that needed the blob's bytes.
    pub(crate) fn into_persist(self, operation: &'static str, blob: &BlobRef) -> PersistError {
        let name = format!("{BLOBS_DIR}/{}", blob.relative_path());
        match self {
            BlobError::Fault(BlobFault::Missing) => PersistError::store(
                operation,
                StoreFailure::BlobUnavailable,
                format!("the blob {name} ({} bytes) is missing", blob.length()),
            ),
            BlobError::Fault(fault) => PersistError::store(
                operation,
                StoreFailure::BlobDamaged,
                format!("the blob {name} is damaged ({} check failed)", fault.name()),
            ),
            BlobError::Unsupported => PersistError::store(
                operation,
                StoreFailure::BlobUnavailable,
                format!(
                    "the blob {name} is named by the hash algorithm {} of a newer version",
                    blob.hash_algo().code()
                ),
            ),
            BlobError::DoesNotFit(err) => PersistError::Graph(err),
            BlobError::Io(err) => PersistError::io("reading a blob", err),
        }
    }
}

impl From<std::io::Error> for BlobError {
    fn from(err: std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::NotFound {
            BlobError::Fault(BlobFault::Missing)
        } else {
            BlobError::Io(err)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------

/// The file of `blob` in the store at `root`.
pub(crate) fn blob_path(root: &DirPath, blob: &BlobRef) -> DirPath {
    root.join(BLOBS_DIR).join(blob.relative_path())
}

/// The name of `blob` relative to the store (`blobs/..`).
pub(crate) fn blob_name(blob: &BlobRef) -> String {
    format!("{BLOBS_DIR}/{}", blob.relative_path())
}

/// Streams `reader` to `out`, computing the SHA-256, the CRC-32C and the length.
fn copy_measured(
    reader: &mut dyn Read,
    out: &mut dyn std::io::Write,
) -> std::io::Result<(u64, u32, [u8; 32])> {
    let mut hash = Sha256::new();
    let mut check = 0u32;
    let mut len = 0u64;
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        hash.update(&buf[..n]);
        check = crc_append(check, &buf[..n]);
        out.write_all(&buf[..n])?;
        len += n as u64;
    }
    note_hash();
    Ok((len, check, hash.finish()))
}

#[cfg(feature = "persist-zip")]
/// Checks a blob's bytes read from a stream (a ZIP entry read back) against its reference:
/// length, CRC-32C and SHA-256. Returns the problems.
pub(crate) fn check_stream(blob: &BlobRef, data: &mut dyn Read) -> Vec<String> {
    let mut sink = std::io::sink();
    match copy_measured(data, &mut sink) {
        Ok((len, _, _)) if len != blob.length() => {
            vec![format!("{len} bytes, the blob has {}", blob.length())]
        }
        Ok((_, check, _)) if check != blob.crc32c() => vec!["CRC-32C mismatch".into()],
        Ok((_, _, sha))
            if blob.hash_algo() == BlobHashAlgo::Sha256 && sha.as_slice() != blob.hash() =>
        {
            vec!["hash mismatch".into()]
        }
        Ok(_) => Vec::new(),
        Err(err) => vec![err.to_string()],
    }
}

/// Streams a file, computing its CRC-32C and length (and the SHA-256 when `hash`).
fn measure(path: &DirPath, hash: bool) -> Result<(u64, u32, Option<[u8; 32]>), BlobError> {
    let mut reader = path.open_read()?;
    let mut sha = hash.then(Sha256::new);
    let mut check = 0u32;
    let mut len = 0u64;
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(BlobError::Io(err)),
        };
        if let Some(sha) = &mut sha {
            sha.update(&buf[..n]);
        }
        check = crc_append(check, &buf[..n]);
        len += n as u64;
    }
    if sha.is_some() {
        note_hash();
    }
    Ok((len, check, sha.map(Sha256::finish)))
}

/// A blob being written: `blobs/tmp-<32 hex>`, removed on drop unless renamed.
struct TmpBlob {
    path: DirPath,
    done: bool,
}

impl Drop for TmpBlob {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.path.remove_file();
        }
    }
}

/// What [`write_blob`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    pub(crate) blob: BlobRef,
    /// The blob was new (`false`: an intact blob of the same hash existed: deduplicated).
    pub(crate) new: bool,
    /// An existing blob of the same hash was damaged and replaced.
    pub(crate) repaired: bool,
}

/// Writes `reader` as a blob into the store at `root` (format spec 4.6): streamed into
/// `blobs/tmp-<random>` with SHA-256, CRC-32C and length (constant memory), synced, then
/// renamed to its name and both directories synced. An existing blob of the same hash is
/// checked by length and CRC: intact, the new copy is dropped (dedup); damaged, it is replaced.
/// `before_rename` runs with the reference before the blob becomes visible (the caller
/// registers it as pending, so no collection removes it before its commit). The referencing
/// commit comes AFTER (the caller's duty).
pub(crate) fn write_blob(
    root: &DirPath,
    reader: &mut dyn Read,
    before_rename: &mut dyn FnMut(&BlobRef),
) -> Result<Written, PersistError> {
    let io = |what: &'static str| move |err: std::io::Error| PersistError::io(what, err);
    let blobs = root.join(BLOBS_DIR);
    blobs
        .create_dir_all()
        .map_err(io("creating the blobs directory"))?;
    let mut tmp = TmpBlob {
        path: blobs.join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7())),
        done: false,
    };
    let mut out = tmp.path.create(true).map_err(io("writing a blob"))?;
    let (len, check, hash) = copy_measured(reader, &mut out).map_err(io("writing a blob"))?;
    out.sync().map_err(io("writing a blob"))?;
    drop(out);
    let blob = BlobRef::new(BlobHashAlgo::Sha256, &hash, len, check)
        .map_err(|err| PersistError::encode("a blob reference", err.to_string()))?;
    before_rename(&blob);
    let target = blob_path(root, &blob);
    let mut repaired = false;
    if target.exists() {
        match measure(&target, false) {
            Ok((found_len, found_crc, _)) if found_len == len && found_crc == check => {
                return Ok(Written {
                    blob,
                    new: false,
                    repaired: false,
                });
            }
            Ok(_) | Err(BlobError::Fault(_)) => repaired = true,
            Err(err) => return Err(err.into_persist("write a blob", &blob)),
        }
    }
    let parent = target.parent().unwrap_or_else(|| blobs.clone());
    parent.create_dir_all().map_err(io("writing a blob"))?;
    if repaired {
        let _ = target.remove_file();
    }
    tmp.path
        .rename_to(&target)
        .map_err(io("moving a blob into place"))?;
    tmp.done = true;
    files::sync_dir(&parent)?;
    files::sync_dir(&blobs)?;
    Ok(Written {
        blob,
        new: !repaired,
        repaired,
    })
}

/// Reads `blob` whole from the store at `root`: the length checked before reading, the
/// memory ceiling checked before allocating, the CRC-32C after.
pub(crate) fn read_blob(root: &DirPath, blob: &BlobRef) -> Result<Vec<u8>, BlobError> {
    if blob.hash_algo() != BlobHashAlgo::Sha256 {
        return Err(BlobError::Unsupported);
    }
    let path = blob_path(root, blob);
    let len = path.len()?;
    if len != blob.length() {
        return Err(BlobError::Fault(BlobFault::Length));
    }
    crate::traversal::memory_budget::ensure_room(BLOB_READ_OPERATION, len, 0)
        .map_err(BlobError::DoesNotFit)?;
    let size = usize::try_from(len).map_err(|_| {
        BlobError::DoesNotFit(crate::traversal::memory_budget::room_error(
            BLOB_READ_OPERATION,
            usize::MAX as u64,
            len,
            len,
        ))
    })?;
    let bytes = if size == 0 {
        Vec::new()
    } else {
        path.read_range(0, size)?
    };
    if bytes.len() as u64 != blob.length() {
        return Err(BlobError::Fault(BlobFault::Length));
    }
    if crc(&bytes) != blob.crc32c() {
        return Err(BlobError::Fault(BlobFault::Crc));
    }
    Ok(bytes)
}

/// Checks `blob` in the store at `root` by streaming it: length and CRC-32C, plus the SHA-256
/// when `hash` (verify, foreign input). A blob of a newer version's algorithm is checked by
/// length and CRC-32C only (its hash is never computed).
pub(crate) fn check_blob(root: &DirPath, blob: &BlobRef, hash: bool) -> Result<(), BlobError> {
    let hash = hash && blob.hash_algo() == BlobHashAlgo::Sha256;
    let path = blob_path(root, blob);
    let (len, check, sha) = measure(&path, hash)?;
    if len != blob.length() {
        return Err(BlobError::Fault(BlobFault::Length));
    }
    if check != blob.crc32c() {
        return Err(BlobError::Fault(BlobFault::Crc));
    }
    if let Some(sha) = sha
        && sha.as_slice() != blob.hash()
    {
        return Err(BlobError::Fault(BlobFault::Hash));
    }
    Ok(())
}

/// Copies `blob` from the store at `from` into the store at `to` (streamed through a tmp file,
/// checked by length and CRC-32C on both sides, plus the SHA-256 when `hash`). An intact copy
/// already in `to` is kept. Returns whether bytes were copied.
pub(crate) fn copy_blob(
    from: &DirPath,
    to: &DirPath,
    blob: &BlobRef,
    hash: bool,
) -> Result<bool, BlobError> {
    if check_blob(to, blob, false).is_ok() {
        return Ok(false);
    }
    if blob.hash_algo() != BlobHashAlgo::Sha256 {
        // A newer version's algorithm: copied BY NAME (format spec 4.6), checked by length and
        // CRC-32C on both sides, never hashed.
        check_blob(from, blob, false)?;
        let staged =
            stage(to).map_err(|err| BlobError::Io(std::io::Error::other(err.to_string())))?;
        let mut tmp = TmpBlob {
            path: staged,
            done: false,
        };
        let mut reader = blob_path(from, blob).open_read()?;
        let mut out = tmp.path.create(true)?;
        copy_measured(&mut reader, &mut out)?;
        out.sync()?;
        drop(out);
        let target = blob_path(to, blob);
        let parent = target.parent().unwrap_or_else(|| to.join(BLOBS_DIR));
        parent.create_dir_all()?;
        let _ = target.remove_file();
        tmp.path.rename_to(&target)?;
        tmp.done = true;
        let sync = |dir: &DirPath| {
            files::sync_dir(dir)
                .map_err(|err| BlobError::Io(std::io::Error::other(err.to_string())))
        };
        sync(&parent)?;
        sync(&to.join(BLOBS_DIR))?;
        check_blob(to, blob, false)?;
        return Ok(true);
    }
    let mut reader = blob_path(from, blob).open_read()?;
    let written = write_blob(to, &mut reader, &mut |_| {}).map_err(|err| match err {
        PersistError::Io { source, .. } => BlobError::Io(source),
        other => BlobError::Io(std::io::Error::other(other.to_string())),
    });
    match written {
        Ok(written) if &written.blob == blob => {}
        Ok(written) => {
            // The source's bytes are not the blob: drop the copy unless the target holds the
            // other content legitimately (another reference's).
            let fault = if written.blob.length() != blob.length() {
                BlobFault::Length
            } else if written.blob.crc32c() != blob.crc32c() {
                BlobFault::Crc
            } else {
                BlobFault::Hash
            };
            let _ = written;
            return Err(BlobError::Fault(fault));
        }
        Err(err) => return Err(err),
    }
    check_blob(to, blob, hash)?;
    Ok(true)
}

/// One file under `blobs/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlobFile {
    /// The name relative to the store (`blobs/ab/ab..`).
    pub(crate) name: String,
    /// The hash text (the file name), and the algorithm's directory for a non-SHA-256 blob.
    pub(crate) key: BlobKey,
    pub(crate) len: u64,
}

/// A blob's identity by name: `(algorithm code, hash bytes)`.
pub(crate) type BlobKey = (u8, Vec<u8>);

/// The identity of a reference.
pub(crate) fn key_of(blob: &BlobRef) -> BlobKey {
    (blob.hash_algo().code(), blob.hash().to_vec())
}

/// What lies under `blobs/`: the blob files, the tmp files and the entries that are neither
/// (verify reports those as problems).
#[derive(Debug, Default)]
pub(crate) struct BlobListing {
    pub(crate) blobs: Vec<BlobFile>,
    pub(crate) tmp: Vec<String>,
    pub(crate) strays: Vec<String>,
}

/// Lists `blobs/` by names and lengths only (no hashing). A missing directory is empty.
pub(crate) fn list_blobs(root: &DirPath) -> std::io::Result<BlobListing> {
    let mut out = BlobListing::default();
    let blobs = root.join(BLOBS_DIR);
    if blobs.try_kind()? != Some(EntryKind::Dir) {
        return Ok(out);
    }
    for (name, kind) in blobs.list()? {
        let path = blobs.join(&name);
        match kind {
            EntryKind::File if name.starts_with(TMP_PREFIX) => {
                out.tmp.push(format!("{BLOBS_DIR}/{name}"));
            }
            EntryKind::Dir if is_prefix(&name) => list_prefix(&path, 1, &name, &mut out)?,
            EntryKind::Dir => match algo_dir(&name) {
                Some(code) => {
                    for (prefix, kind) in path.list()? {
                        let inner = path.join(&prefix);
                        if kind == EntryKind::Dir && is_prefix(&prefix) {
                            list_prefix(&inner, code, &prefix, &mut out)?;
                        } else {
                            out.strays.push(format!("{BLOBS_DIR}/{name}/{prefix}"));
                        }
                    }
                }
                None => out.strays.push(format!("{BLOBS_DIR}/{name}")),
            },
            _ => out.strays.push(format!("{BLOBS_DIR}/{name}")),
        }
    }
    out.blobs.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

#[cfg(feature = "persist-zip")]
/// Whether `prefix`/`hash` name a blob of algorithm `code` (lowercase hex, the prefix the
/// hash's first two digits; SHA-256: 64 digits).
pub(crate) fn is_blob_name(code: u8, prefix: &str, hash: &str) -> bool {
    is_prefix(prefix)
        && hash.starts_with(prefix)
        && hash.len() >= 2
        && hash.len() <= 2 * crate::value::blob::MAX_BLOB_HASH_BYTES
        && hash.len().is_multiple_of(2)
        && (code != 1 || hash.len() == 64)
        && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_prefix(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The code of an algorithm directory `algo-<n>` (n >= 2).
fn algo_dir(name: &str) -> Option<u8> {
    let code = name.strip_prefix("algo-")?.parse::<u8>().ok()?;
    (code >= 2).then_some(code)
}

fn list_prefix(
    dir: &DirPath,
    code: u8,
    prefix: &str,
    out: &mut BlobListing,
) -> std::io::Result<()> {
    for (name, kind) in dir.list()? {
        let path = dir.join(&name);
        let relative = path.name().to_string();
        let hash = (kind == EntryKind::File && name.starts_with(prefix))
            .then(|| BlobRef::parse_hex(&name).ok())
            .flatten()
            .filter(|hash| {
                name.bytes().all(|b| !b.is_ascii_uppercase())
                    && !hash.is_empty()
                    && hash.len() <= crate::value::blob::MAX_BLOB_HASH_BYTES
                    && (code != 1 || hash.len() == 32)
            });
        match hash {
            Some(hash) => out.blobs.push(BlobFile {
                len: path.len()?,
                name: relative,
                key: (code, hash),
            }),
            None => out.strays.push(relative),
        }
    }
    Ok(())
}

/// Whether the store at `root` holds any blob file (a cheap listing: no blob, no reference
/// work for a copy).
pub(crate) fn holds_blobs(root: &DirPath) -> Result<bool, PersistError> {
    Ok(!list_blobs(root)
        .map_err(|err| PersistError::io("listing the blobs", err))?
        .blobs
        .is_empty())
}

/// Removes the `blobs/tmp-*` leftovers of an interrupted write (the open of a writable store).
pub(crate) fn remove_tmp(root: &DirPath) -> usize {
    let Ok(listing) = list_blobs(root) else {
        return 0;
    };
    listing
        .tmp
        .iter()
        .filter(|name| root.join(name).remove_file().is_ok())
        .count()
}

// ---------------------------------------------------------------------------------------------
// References: catalog definitions, change sets, the blob table
// ---------------------------------------------------------------------------------------------

/// The blob table of a state (format spec 6.2 item 5): every blob its catalog definitions
/// reference, with the number of references, sorted by `(algorithm, hash)`. Element
/// properties never hold a blob reference in this version (refused by the write checks).
pub(crate) fn blob_table<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
) -> Vec<(BlobRef, u64)> {
    let mut refs = Vec::new();
    for definition in definitions {
        definition.blob_refs(&mut refs);
    }
    let mut table: BTreeMap<BlobKey, (BlobRef, u64)> = BTreeMap::new();
    for blob in refs {
        table
            .entry(key_of(&blob))
            .and_modify(|(_, count)| *count += 1)
            .or_insert((blob, 1));
    }
    table.into_values().collect()
}

/// The net blob deltas of a commit: every blob reference in the after images (+1) and the
/// before images (-1) of its definition changes, nonzero only, sorted by `(algorithm, hash)`.
pub(crate) fn commit_deltas(changes: &ChangeSet) -> Vec<(BlobRef, i64)> {
    let mut deltas: BTreeMap<BlobKey, (BlobRef, i64)> = BTreeMap::new();
    let mut refs = Vec::new();
    for mutation in changes.mutations() {
        if let Mutation::SetDefinition { before, after, .. } = mutation {
            for (image, sign) in [(before, -1i64), (after, 1)] {
                if let Some(definition) = image {
                    refs.clear();
                    definition.blob_refs(&mut refs);
                    for blob in refs.drain(..) {
                        deltas
                            .entry(key_of(&blob))
                            .and_modify(|(_, delta)| *delta += sign)
                            .or_insert((blob, sign));
                    }
                }
            }
        }
    }
    deltas
        .into_values()
        .filter(|(_, delta)| *delta != 0)
        .collect()
}

/// Encodes a blob table (format spec 6.2 item 5): `uvar count | (blobref | refs uvar)*`.
pub(crate) fn put_table(out: &mut Vec<u8>, table: &[(BlobRef, u64)]) {
    super::super::codec::put_uvar(out, table.len() as u64);
    for (blob, refs) in table {
        put_blob_ref(out, blob);
        super::super::codec::put_uvar(out, *refs);
    }
}

/// Decodes a blob table; rows must be strictly increasing by `(algorithm, hash)`, counts >= 1.
pub(crate) fn read_table(r: &mut Reader<'_>) -> DecodeResult<Vec<(BlobRef, u64)>> {
    let at = r.offset();
    // Each row takes at least 1 + 1 + 1 + 8 + 4 + 1 bytes.
    let count = r.count(16)?;
    let mut table: Vec<(BlobRef, u64)> = Vec::with_capacity(count);
    for _ in 0..count {
        let row_at = r.offset();
        let blob = read_blob_ref(r, row_at)?;
        let refs = r.uvar()?;
        if refs == 0 {
            return Err(super::super::codec::DecodeError::new(
                row_at,
                "a blob table row with zero references",
            ));
        }
        if let Some((last, _)) = table.last()
            && key_of(last) >= key_of(&blob)
        {
            return Err(super::super::codec::DecodeError::new(
                row_at,
                "blob table rows out of order",
            ));
        }
        table.push((blob, refs));
    }
    let _ = at;
    Ok(table)
}

// ---------------------------------------------------------------------------------------------
// The ledger (format spec 10.1)
// ---------------------------------------------------------------------------------------------

/// One frame of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LedgerFrame {
    pub(crate) commit_seq: u64,
    pub(crate) graph_id: u128,
    pub(crate) blob: BlobRef,
    pub(crate) delta: i64,
}

impl LedgerFrame {
    fn encode(&self, out: &mut Vec<u8>) {
        let mut payload = Vec::with_capacity(80);
        put_u64(&mut payload, self.commit_seq);
        payload.extend_from_slice(&self.graph_id.to_be_bytes());
        put_blob_ref(&mut payload, &self.blob);
        put_svar(&mut payload, self.delta);
        put_u32(out, payload.len() as u32);
        put_u32(out, crc(&payload));
        out.extend_from_slice(&payload);
    }

    fn decode(payload: &[u8]) -> DecodeResult<LedgerFrame> {
        let mut r = Reader::new(payload);
        let commit_seq = r.u64()?;
        let graph_id = r.u128_be()?;
        let at = r.offset();
        let blob = read_blob_ref(&mut r, at)?;
        let delta = r.svar()?;
        if !r.is_empty() {
            return Err(super::super::codec::DecodeError::new(
                r.offset(),
                "trailing bytes in a ledger frame",
            ));
        }
        Ok(LedgerFrame {
            commit_seq,
            graph_id,
            blob,
            delta,
        })
    }

    /// Whether the frame counts on a lineage `chain` (`(graph_id, last valid commit)`, newest
    /// first): its lineage is on the chain and its commit within that lineage's part.
    pub(crate) fn on_chain(&self, chain: &[(u128, u64)]) -> bool {
        chain
            .iter()
            .any(|(id, valid)| *id == self.graph_id && self.commit_seq <= *valid)
    }
}

/// The frames of one commit.
pub(crate) fn commit_frames(changes: &ChangeSet, graph_id: u128) -> Vec<LedgerFrame> {
    commit_deltas(changes)
        .into_iter()
        .map(|(blob, delta)| LedgerFrame {
            commit_seq: changes.commit_seq(),
            graph_id,
            blob,
            delta,
        })
        .collect()
}

/// The bytes of `frames`.
pub(crate) fn ledger_bytes(frames: &[LedgerFrame]) -> Vec<u8> {
    let mut out = Vec::new();
    for frame in frames {
        frame.encode(&mut out);
    }
    out
}

/// Decodes a ledger file; `None` on any damage (rebuild it).
pub(crate) fn decode_ledger(bytes: &[u8]) -> Option<Vec<LedgerFrame>> {
    let mut frames = Vec::new();
    let mut r = Reader::new(bytes);
    while !r.is_empty() {
        let len = r.u32().ok()? as usize;
        let check = r.u32().ok()?;
        let payload = r.bytes(len).ok()?;
        if crc(payload) != check {
            return None;
        }
        frames.push(LedgerFrame::decode(payload).ok()?);
    }
    Some(frames)
}

/// Appends `frames` to the ledger of the store at `root` (one append, synced).
pub(crate) fn append_frames(root: &DirPath, frames: &[LedgerFrame]) -> std::io::Result<()> {
    root.join(LEDGER_FILE).append(&ledger_bytes(frames))
}

/// Rewrites the ledger with `frames` (atomically).
pub(crate) fn write_ledger(root: &DirPath, frames: &[LedgerFrame]) -> Result<(), PersistError> {
    files::write_atomic(root, LEDGER_FILE, &ledger_bytes(frames))
}

/// Reads the ledger: `Ok(None)` when it is missing or damaged (rebuild it from the WAL).
pub(crate) fn read_ledger(root: &DirPath) -> Result<Option<Vec<LedgerFrame>>, PersistError> {
    let path = root.join(LEDGER_FILE);
    let bytes = match files::read_bounded(&path, MAX_LEDGER_BYTES) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::OutOfMemory => {
            return Err(PersistError::io("reading the blob ledger", err));
        }
        Err(_) => return Ok(None),
    };
    Ok(decode_ledger(&bytes))
}

/// The ledger frames of the commits in WAL `segments` (each frame under the lineage of its
/// segment). Stops quietly at a torn tail or at damage (`verify` reports the damage).
pub(crate) fn scan_frames(segments: &[(u64, DirPath)]) -> Vec<LedgerFrame> {
    let mut frames = Vec::new();
    for (index, (_, path)) in segments.iter().enumerate() {
        let Ok(source) = super::wal_files::open_segment_reader(path, None) else {
            continue;
        };
        let Ok(mut reader) =
            JournalReader::open(source, index, crate::value::depth::DEFAULT_MAX_VALUE_DEPTH)
        else {
            continue;
        };
        let graph_id = reader.graph_id();
        while let Ok(Some((_, _, record))) = reader.next_known_record(&mut |_| {}) {
            if let JournalRecord::Commit(changes) = record {
                frames.extend(commit_frames(&changes, graph_id));
            }
        }
    }
    frames
}

/// Merges `extra` frames into `frames` (idempotent by `(graph_id, commit_seq, blob)`), in
/// order.
pub(crate) fn merge_frames(frames: &mut Vec<LedgerFrame>, extra: Vec<LedgerFrame>) -> bool {
    let known: BTreeSet<(u128, u64, BlobKey)> = frames
        .iter()
        .map(|f| (f.graph_id, f.commit_seq, key_of(&f.blob)))
        .collect();
    let mut changed = false;
    for frame in extra {
        if !known.contains(&(frame.graph_id, frame.commit_seq, key_of(&frame.blob))) {
            frames.push(frame);
            changed = true;
        }
    }
    changed
}

// ---------------------------------------------------------------------------------------------
// Pending writes and the referenced set
// ---------------------------------------------------------------------------------------------

/// The blobs written but not yet referenced by a durable commit, shared by a Store's
/// [`PendingBlob`](crate::blob::PendingBlob) handles and its collections (format spec 12.4: a
/// running import's blobs are referenced).
#[derive(Debug, Default)]
pub(crate) struct BlobGate {
    pending: Mutex<BTreeMap<BlobKey, usize>>,
}

impl BlobGate {
    /// Registers a pending blob (before it becomes visible).
    pub(crate) fn hold(&self, blob: &BlobRef) {
        if let Ok(mut pending) = self.pending.lock() {
            *pending.entry(key_of(blob)).or_insert(0) += 1;
        }
    }

    /// Releases one hold of `blob`.
    pub(crate) fn release(&self, blob: &BlobRef) {
        if let Ok(mut pending) = self.pending.lock() {
            let key = key_of(blob);
            if let Some(count) = pending.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    pending.remove(&key);
                }
            }
        }
    }

    /// Runs `f` with the pending set locked (a collection: no write registers meanwhile).
    pub(crate) fn with_pending<T>(
        &self,
        f: impl FnOnce(&BTreeSet<BlobKey>) -> T,
    ) -> Result<T, PersistError> {
        let pending = self.pending.lock().map_err(|_| {
            PersistError::store(
                "collect blobs",
                StoreFailure::LockPoisoned,
                "the blob gate's lock is poisoned",
            )
        })?;
        let keys: BTreeSet<BlobKey> = pending.keys().cloned().collect();
        Ok(f(&keys))
    }
}

/// A written blob not yet referenced by a durable commit: while the handle lives, no prune,
/// attic removal or rollback removes it. Reference it in a catalog definition, commit, then
/// drop the handle (a blob dropped unreferenced is an orphan the next prune removes).
#[derive(Debug)]
pub struct PendingBlob {
    blob: BlobRef,
    gate: Arc<BlobGate>,
    new: bool,
}

impl PendingBlob {
    pub(crate) fn new(blob: BlobRef, gate: Arc<BlobGate>, new: bool) -> Self {
        PendingBlob { blob, gate, new }
    }

    /// The blob's reference (put it into a catalog definition's payload).
    pub fn blob(&self) -> &BlobRef {
        &self.blob
    }

    /// Whether the blob was new (`false`: the store held these bytes already).
    pub fn is_new(&self) -> bool {
        self.new
    }
}

impl Drop for PendingBlob {
    fn drop(&mut self) {
        self.gate.release(&self.blob);
    }
}

/// Whether a shareable copy (a packed snapshot, a ZIP backup) carries the store's models and
/// blobs (format spec 4.5, 4.6). [`ModelCopy`](crate::persist::ModelCopy) is its 0.4 name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BlobCopy {
    /// The copy carries every model with its card and licence files and every blob its
    /// history references (the default): whoever receives it can use them, and passing it on
    /// redistributes them.
    #[default]
    Include,
    /// The copy leaves the models and blobs out: its catalog still references the blobs, so
    /// the receiver has them missing until they are added again (models by the identities the
    /// schema records: `graphersal embed-model download <repo> --into <store>`).
    Exclude,
}

/// The blobs a store holds by name ([`StoreInfo::blobs`](super::StoreInfo::blobs)): a listing
/// by names and lengths, no hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BlobUsage {
    /// The blob files.
    pub count: u64,
    /// Their total length in bytes.
    pub bytes: u64,
    /// Blob files no retained history references (removed by the next prune); `0` when the
    /// referenced set could not be read, and in [`Store::quick_info`](super::Store::quick_info)
    /// (not counted).
    pub unreferenced: u64,
}

/// What [`referenced`] found: every blob a retained history of the store references.
#[derive(Debug, Default)]
pub(crate) struct Referenced {
    pub(crate) blobs: BTreeMap<BlobKey, BlobRef>,
}

impl Referenced {
    fn add(&mut self, blob: &BlobRef) {
        self.blobs
            .entry(key_of(blob))
            .or_insert_with(|| blob.clone());
    }

    pub(crate) fn contains(&self, key: &BlobKey) -> bool {
        self.blobs.contains_key(key)
    }
}

/// The blobs of `table`s of every snapshot in `snapshots`.
fn add_tables(out: &mut Referenced, snapshots: &[DirPath]) -> Result<(), PersistError> {
    for snapshot in snapshots {
        let (_, manifest) = super::snapshot_dir::read_manifest(snapshot)?;
        for (blob, _) in &manifest.blobs {
            out.add(blob);
        }
    }
    Ok(())
}

/// The referenced set R of the store at `root` (format spec 12.4): the blob tables of every
/// snapshot of the store and of its attic entries, the store ledger's frames on the current
/// lineage `chain` (rebuilt from the WAL when the ledger is missing or damaged, or when
/// `ledger_stale`), and every blob the attic entries' own WAL references. Fails when anything
/// cannot be read (a collection then removes nothing).
pub(crate) fn referenced(
    root: &DirPath,
    chain: &[(u128, u64)],
    ledger_stale: bool,
) -> Result<Referenced, PersistError> {
    referenced_from(root, chain, ledger_stale, 0)
}

/// The blobs the blob tables of every snapshot of the store and of its attic entries
/// reference (no ledger, no WAL).
pub(crate) fn referenced_by_tables(root: &DirPath) -> Result<Referenced, PersistError> {
    let mut out = Referenced::default();
    let snapshots: Vec<DirPath> = files::list_snapshots(root)?
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    add_tables(&mut out, &snapshots)?;
    add_tables(&mut out, &super::rollback::attic_snapshot_dirs(root)?)?;
    Ok(out)
}

/// [`referenced`] of the history from `kept_from` on: the snapshots at or after it, the frames
/// after it.
fn referenced_from(
    root: &DirPath,
    chain: &[(u128, u64)],
    ledger_stale: bool,
    kept_from: u64,
) -> Result<Referenced, PersistError> {
    let mut out = Referenced::default();
    let snapshots: Vec<DirPath> = files::list_snapshots(root)?
        .into_iter()
        .filter(|(seq, _)| *seq >= kept_from)
        .map(|(_, path)| path)
        .collect();
    add_tables(&mut out, &snapshots)?;
    add_tables(&mut out, &super::rollback::attic_snapshot_dirs(root)?)?;
    let frames = match (ledger_stale, read_ledger(root)?) {
        (false, Some(frames)) => frames,
        _ => scan_frames(&files::list_wal(root)?),
    };
    for frame in frames
        .iter()
        .filter(|f| f.on_chain(chain) && (kept_from == 0 || f.commit_seq > kept_from))
    {
        out.add(&frame.blob);
    }
    for entry in super::rollback::attic_list(root)? {
        let (_, segments) = super::rollback::attic_segments(root, &entry.id)?;
        for frame in scan_frames(&segments) {
            out.add(&frame.blob);
        }
    }
    Ok(out)
}

/// The blob files a prune up to `kept_from` would remove (count, bytes): those the history
/// from `kept_from` on does not reference (estimated without pending writes; `(0, 0)` when the
/// referenced set does not read). Reads manifests and the ledger only when blobs exist.
pub(crate) fn estimate_unreferenced(
    root: &DirPath,
    kept_from: u64,
    attic: Option<&AtticBlobs>,
) -> (u64, u64) {
    let Ok(listing) = list_blobs(root) else {
        return (0, 0);
    };
    if listing.blobs.is_empty() {
        return (0, 0);
    }
    let Ok(file) = files::read_graph_file(root) else {
        return (0, 0);
    };
    let Ok(unreferenced) =
        unreferenced_files(root, &file.file.chain(), kept_from, &listing.blobs, attic)
    else {
        return (0, 0);
    };
    unreferenced
        .iter()
        .fold((0, 0), |(count, bytes), b| (count + 1, bytes + b.len))
}

/// The files of `blobs` that the history from `kept_from` on does not reference: the same
/// answer as filtering by [`referenced_from`], but the sources are read cheapest first (the
/// snapshots' blob tables, then the ledger, then the attic entries' own WAL, entry by entry)
/// and reading stops as soon as every file is found referenced. The common case (every blob
/// in use by the current state) reads no WAL at all; an open store remembers what each attic
/// entry's WAL references (`attic`), so a blob only an attic entry or nothing references costs
/// one read of that WAL per process, not one per call: the Store menu's info and the
/// compaction advice stay independent of the history's length. Fails like [`referenced_from`].
fn unreferenced_files<'a>(
    root: &DirPath,
    chain: &[(u128, u64)],
    kept_from: u64,
    blobs: &'a [BlobFile],
    attic: Option<&AtticBlobs>,
) -> Result<Vec<&'a BlobFile>, PersistError> {
    let mut left: Vec<&BlobFile> = blobs.iter().collect();
    if left.is_empty() {
        return Ok(left);
    }
    let mut found = Referenced::default();
    let snapshots: Vec<DirPath> = files::list_snapshots(root)?
        .into_iter()
        .filter(|(seq, _)| *seq >= kept_from)
        .map(|(_, path)| path)
        .collect();
    add_tables(&mut found, &snapshots)?;
    add_tables(&mut found, &super::rollback::attic_snapshot_dirs(root)?)?;
    left.retain(|b| !found.contains(&b.key));
    if left.is_empty() {
        return Ok(left);
    }
    let frames = match read_ledger(root)? {
        Some(frames) => frames,
        None => scan_frames(&files::list_wal(root)?),
    };
    for frame in frames
        .iter()
        .filter(|f| f.on_chain(chain) && (kept_from == 0 || f.commit_seq > kept_from))
    {
        found.add(&frame.blob);
    }
    left.retain(|b| !found.contains(&b.key));
    for entry in super::rollback::attic_list(root)? {
        if left.is_empty() {
            break;
        }
        let (_, segments) = super::rollback::attic_segments(root, &entry.id)?;
        for blob in AtticBlobs::of_entry(attic, &entry.id, &segments) {
            found.add(&blob);
        }
        left.retain(|b| !found.contains(&b.key));
    }
    Ok(left)
}

/// What the WAL of each attic entry references, remembered by an open store (`Shared`): an
/// entry's WAL never changes while it is in the attic (a restore moves it out, a removal
/// deletes it), and the remembered blobs are used only while the entry's segments have the
/// same names and lengths.
#[derive(Default)]
pub(crate) struct AtticBlobs {
    /// Entry id -> (its segments' first commits and lengths, the blobs their frames reference).
    entries: Mutex<BTreeMap<String, AtticEntryBlobs>>,
}

/// What [`AtticBlobs`] keeps of one attic entry: its WAL segments' first commits and lengths
/// (the key that tells a changed entry), and the blobs their frames reference.
type AtticEntryBlobs = (Vec<(u64, u64)>, Vec<BlobRef>);

impl AtticBlobs {
    /// The blobs the frames of attic entry `id` (WAL `segments`) reference: remembered in
    /// `cache` when given, else read.
    fn of_entry(cache: Option<&Self>, id: &str, segments: &[(u64, DirPath)]) -> Vec<BlobRef> {
        let read = || -> Vec<BlobRef> {
            scan_frames(segments)
                .into_iter()
                .map(|frame| frame.blob)
                .collect()
        };
        let Some(cache) = cache else {
            return read();
        };
        let Some(shape) = segments
            .iter()
            .map(|(first, path)| path.len().ok().map(|len| (*first, len)))
            .collect::<Option<Vec<_>>>()
        else {
            return read();
        };
        if let Ok(entries) = cache.entries.lock()
            && let Some((known, blobs)) = entries.get(id)
            && *known == shape
        {
            return blobs.clone();
        }
        let blobs = read();
        if let Ok(mut entries) = cache.entries.lock() {
            entries.insert(id.to_string(), (shape, blobs.clone()));
        }
        blobs
    }
}

#[cfg(feature = "persist-zip")]
/// Checks the blobs of a store that came from outside (a restored ZIP archive): every blob
/// file's SHA-256 against its name, and the length and CRC-32C of every one the history
/// references against its reference (format spec 12.8: a foreign blob is hostile input).
/// Returns the first problem as `(path, reason)`.
pub(crate) fn check_foreign_dir(root: &DirPath) -> Result<(), (String, String)> {
    let listing = list_blobs(root).map_err(|err| (BLOBS_DIR.to_string(), err.to_string()))?;
    if let Some(stray) = listing.strays.first() {
        return Err((stray.clone(), "not a blob".into()));
    }
    for file in &listing.blobs {
        if file.key.0 != 1 {
            continue;
        }
        let path = root.join(&file.name);
        match measure(&path, true) {
            Ok((_, _, Some(sha))) if sha.as_slice() == file.key.1.as_slice() => {}
            Ok(_) => {
                return Err((
                    file.name.clone(),
                    "its content does not match its hash".into(),
                ));
            }
            Err(_) => return Err((file.name.clone(), "unreadable".into())),
        }
    }
    let Ok(file) = files::read_graph_file(root) else {
        return Ok(());
    };
    let referenced = referenced(root, &file.file.chain(), false)
        .map_err(|err| (BLOBS_DIR.to_string(), err.to_string()))?;
    for blob in referenced.blobs.values() {
        match check_blob(root, blob, false) {
            Ok(()) | Err(BlobError::Fault(BlobFault::Missing)) | Err(BlobError::Unsupported) => {}
            Err(err) => {
                return Err((
                    blob_name(blob),
                    format!(
                        "its {} does not match its reference",
                        err.fault().map_or("content", BlobFault::name)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Copies into the store (or backup) `to` every blob its history references that it lacks,
/// from the store `from` (checked by length and CRC-32C on both sides). Returns the names
/// copied and the names that could not be carried (missing or damaged in `from`).
pub(crate) fn sync_into(
    from: &DirPath,
    to: &DirPath,
) -> Result<(Vec<String>, Vec<String>), PersistError> {
    if !holds_blobs(from)? {
        return Ok((Vec::new(), Vec::new()));
    }
    let chain = files::read_graph_file(to)?.file.chain();
    let wanted = referenced(to, &chain, true)?;
    copy_set(from, to, wanted.blobs.values())
}

/// Copies `blobs` from `from` into `to` (see [`copy_blob`]): names copied and names not
/// carried.
pub(crate) fn copy_set<'a>(
    from: &DirPath,
    to: &DirPath,
    blobs: impl IntoIterator<Item = &'a BlobRef>,
) -> Result<(Vec<String>, Vec<String>), PersistError> {
    let mut copied = Vec::new();
    let mut missing = Vec::new();
    for blob in blobs {
        match copy_blob(from, to, blob, false) {
            Ok(true) => copied.push(blob_name(blob)),
            Ok(false) => {}
            Err(BlobError::Io(err)) => return Err(PersistError::io("copying a blob", err)),
            Err(BlobError::DoesNotFit(err)) => return Err(PersistError::Graph(err)),
            Err(BlobError::Fault(_) | BlobError::Unsupported) => missing.push(blob_name(blob)),
        }
    }
    Ok((copied, missing))
}

/// Copies into the new store `to` the blobs its newest snapshot's table references, from the
/// store `from` (a fork, an attic fork, a repair: format spec 12.5, 13.4), checked by length
/// and CRC-32C (and SHA-256 when `hash`). Returns the names copied and the names not carried
/// (missing or damaged in `from`); an I/O failure is the error.
pub(crate) fn carry_state(
    from: &DirPath,
    to: &DirPath,
    hash: bool,
) -> Result<(Vec<String>, Vec<String>), PersistError> {
    let Some((_, snapshot)) = files::list_snapshots(to)?.pop() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (_, manifest) = super::snapshot_dir::read_manifest(&snapshot)?;
    let mut copied = Vec::new();
    let mut missing = Vec::new();
    for (blob, _) in &manifest.blobs {
        match copy_blob(from, to, blob, hash) {
            Ok(true) => copied.push(blob_name(blob)),
            Ok(false) => {}
            Err(BlobError::Io(err)) => return Err(PersistError::io("copying a blob", err)),
            Err(BlobError::DoesNotFit(err)) => return Err(PersistError::Graph(err)),
            Err(BlobError::Fault(_) | BlobError::Unsupported) => missing.push(blob_name(blob)),
        }
    }
    Ok((copied, missing))
}

/// The segment-table kind of a blob in a packed snapshot (format spec 7.5, 8): IGNORABLE, so a
/// reader without blobs checks its length and CRC and otherwise skips it. The row's path is
/// the hash hex (64 digits for SHA-256), its first id the algorithm's name (`sha256`), its last
/// id empty, its element count 0; its length and CRC-32C are the blob's.
pub(crate) const PACKED_BLOB_KIND: u8 = 4;

/// The blobs a packed export of the snapshot `snapshot` carries: the rows of its blob table,
/// each checked in the store (length, CRC-32C); a missing or damaged one refuses the export.
pub(crate) fn packed_files(
    root: &DirPath,
    snapshot: &DirPath,
) -> Result<Vec<super::models::PackedModelFile>, PersistError> {
    let (_, manifest) = super::snapshot_dir::read_manifest(snapshot)?;
    let mut out = Vec::new();
    for (blob, _) in &manifest.blobs {
        // A newer version's algorithm is carried by name too (checked by length and CRC-32C).
        check_blob(root, blob, false)
            .map_err(|err| err.into_persist("export the snapshot", blob))?;
        out.push(super::models::PackedModelFile {
            kind: PACKED_BLOB_KIND,
            name: blob.hash_hex(),
            id: blob.hash_algo().name(),
            file: String::new(),
            source: super::models::PackedSource::File(blob_path(root, blob)),
            len: blob.length(),
            crc: blob.crc32c(),
        });
    }
    Ok(out)
}

/// The reference of a packed snapshot's blob row: by the hash its path names, found in the
/// manifest's blob table (length and CRC-32C must match the row's).
pub(crate) fn packed_blob_ref(
    entry: &super::super::manifest::SegmentEntry,
    table: &[(BlobRef, u64)],
) -> Result<BlobRef, PersistError> {
    let bad = |reason: &str| {
        PersistError::corrupt(
            "packed snapshot",
            0,
            format!("blob row {:?}: {reason}", entry.path),
        )
    };
    if !entry.last_id.is_empty() || entry.count != 0 {
        return Err(bad("not a blob row"));
    }
    let hash = BlobRef::parse_hex(&entry.path).map_err(|_| bad("its name is not a hash"))?;
    let blob = table
        .iter()
        .map(|(blob, _)| blob)
        .find(|blob| blob.hash_algo().name() == entry.first_id && blob.hash() == hash.as_slice())
        .ok_or_else(|| bad("the snapshot's blob table does not name it"))?;
    if blob.length() != entry.byte_len || blob.crc32c() != entry.crc {
        return Err(bad("its length or CRC-32C is not the blob table's"));
    }
    Ok(blob.clone())
}

/// A new staging file `blobs/tmp-<32 hex>` (the directory created).
pub(crate) fn stage(root: &DirPath) -> Result<DirPath, PersistError> {
    let blobs = root.join(BLOBS_DIR);
    blobs
        .create_dir_all()
        .map_err(|err| PersistError::io("creating the blobs directory", err))?;
    Ok(blobs.join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7())))
}

/// Moves the staged file `staged` into place as `blob` after checking its SHA-256, length and
/// CRC-32C (foreign input); a mismatch removes it and is damage of the input. An intact blob
/// of the same hash already present is kept.
pub(crate) fn install_staged(
    root: &DirPath,
    blob: &BlobRef,
    staged: &DirPath,
) -> Result<(), PersistError> {
    let mut tmp = TmpBlob {
        path: staged.clone(),
        done: false,
    };
    let known = blob.hash_algo() == BlobHashAlgo::Sha256;
    let (len, check, sha) =
        measure(staged, known).map_err(|err| err.into_persist("import a blob", blob))?;
    // A newer version's algorithm is checked by length and CRC-32C only.
    if len != blob.length()
        || check != blob.crc32c()
        || (known && sha.as_ref().map(|s| s.as_slice()) != Some(blob.hash()))
    {
        return Err(PersistError::corrupt(
            "packed snapshot",
            0,
            format!("the blob {} does not match its reference", blob.hash_hex()),
        ));
    }
    if check_blob(root, blob, false).is_ok() {
        return Ok(());
    }
    let target = blob_path(root, blob);
    let parent = target.parent().unwrap_or_else(|| root.join(BLOBS_DIR));
    parent
        .create_dir_all()
        .map_err(|err| PersistError::io("importing a blob", err))?;
    let _ = target.remove_file();
    tmp.path
        .rename_to(&target)
        .map_err(|err| PersistError::io("moving a blob into place", err))?;
    tmp.done = true;
    files::sync_dir(&parent)?;
    files::sync_dir(&root.join(BLOBS_DIR))
}

/// What a collection removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Collected {
    pub(crate) count: u64,
    pub(crate) bytes: u64,
}

/// Removes every blob file of the store at `root` that no retained history references
/// (format spec 12.4, 12.5; idempotent), holding `gate`'s pending set (a store's running
/// writes) for the whole removal. Removes nothing when the referenced set cannot be read.
pub(crate) fn collect(
    root: &DirPath,
    chain: &[(u128, u64)],
    ledger_stale: bool,
    gate: Option<&BlobGate>,
) -> Result<Collected, PersistError> {
    let listing = list_blobs(root).map_err(|err| PersistError::io("listing the blobs", err))?;
    if listing.blobs.is_empty() {
        return Ok(Collected::default());
    }
    let run = |pending: &BTreeSet<BlobKey>| -> Result<Collected, PersistError> {
        let referenced = referenced(root, chain, ledger_stale)?;
        let mut collected = Collected::default();
        let mut dirs = BTreeSet::new();
        for file in &listing.blobs {
            if referenced.contains(&file.key) || pending.contains(&file.key) {
                continue;
            }
            let path = root.join(&file.name);
            path.remove_file()
                .map_err(|err| PersistError::io("removing an unreferenced blob", err))?;
            if let Some(parent) = path.parent() {
                dirs.insert(parent.name().to_string());
            }
            collected.count += 1;
            collected.bytes += file.len;
        }
        for dir in dirs {
            let path = root.join(&dir);
            if !files::has_entries(&path) {
                let _ = path.remove();
            }
        }
        if collected.count > 0 {
            files::sync_dir(&root.join(BLOBS_DIR))?;
        }
        Ok(collected)
    };
    match gate {
        Some(gate) => gate.with_pending(run)?,
        None => run(&BTreeSet::new()),
    }
}

/// The blob usage of the store at `root` (names and lengths; the unreferenced count from the
/// referenced set when it reads).
/// [`usage`] without the unreferenced count: the listing only (no table, ledger or WAL read).
pub(crate) fn listing_usage(root: &DirPath) -> Result<BlobUsage, PersistError> {
    let listing = list_blobs(root).map_err(|err| PersistError::io("listing the blobs", err))?;
    Ok(BlobUsage {
        count: listing.blobs.len() as u64,
        bytes: listing.blobs.iter().map(|b| b.len).sum(),
        unreferenced: 0,
    })
}

pub(crate) fn usage(
    root: &DirPath,
    chain: &[(u128, u64)],
    attic: Option<&AtticBlobs>,
) -> Result<BlobUsage, PersistError> {
    let listing = list_blobs(root).map_err(|err| PersistError::io("listing the blobs", err))?;
    let mut usage = BlobUsage {
        count: listing.blobs.len() as u64,
        bytes: listing.blobs.iter().map(|b| b.len).sum(),
        unreferenced: 0,
    };
    if usage.count > 0
        && let Ok(unreferenced) = unreferenced_files(root, chain, 0, &listing.blobs, attic)
    {
        usage.unreferenced = unreferenced.len() as u64;
    }
    Ok(usage)
}

/// Rewrites the ledger after a prune (format spec 10.1): without frames off the current chain
/// (they live on in attic entries' WAL) and without frames at or before `kept_from` (the oldest
/// kept snapshot, whose table holds them). A missing or damaged ledger is rebuilt from the WAL
/// left.
pub(crate) fn rewrite_after_prune(
    root: &DirPath,
    chain: &[(u128, u64)],
    kept_from: u64,
) -> Result<(), PersistError> {
    let frames = match read_ledger(root)? {
        Some(frames) => frames,
        None => scan_frames(&files::list_wal(root)?),
    };
    let kept: Vec<LedgerFrame> = frames
        .iter()
        .filter(|f| f.on_chain(chain) && f.commit_seq > kept_from)
        .cloned()
        .collect();
    if kept.len() != frames.len() || !root.join(LEDGER_FILE).exists() {
        write_ledger(root, &kept)?;
    }
    Ok(())
}

/// Every blob the copied history references: the tables of `snapshots` and the frames of the
/// commits in `segments` up to `up_to` (a backup's content).
pub(crate) fn referenced_by_copy(
    snapshots: &[DirPath],
    segments: &[(u64, DirPath)],
    up_to: u64,
) -> Result<Vec<BlobRef>, PersistError> {
    let mut out = Referenced::default();
    add_tables(&mut out, snapshots)?;
    for frame in scan_frames(segments) {
        if frame.commit_seq <= up_to {
            out.add(&frame.blob);
        }
    }
    Ok(out.blobs.into_values().collect())
}

// ---------------------------------------------------------------------------------------------
// Test hook: hash counter
// ---------------------------------------------------------------------------------------------

static HASHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn note_hash() {
    HASHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// How many blob SHA-256 computations ran in this process (a test hook: the hash is computed
/// only at write, import and verify).
#[doc(hidden)]
pub fn blob_hash_count() -> u64 {
    HASHES.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::definition::{DefinitionFlags, DefinitionKind};
    use crate::persist::codec::{BLOB_FORMAT_VERSION, InlineKeys, put_value, read_value};
    use crate::persist::{MemDir, Store, StoreOptions};
    use crate::value::{ElementProperty, PropertyMap};

    fn blob(n: u8) -> BlobRef {
        BlobRef::new(
            BlobHashAlgo::Sha256,
            &[n; 32],
            u64::from(n) * 100,
            u32::from(n),
        )
        .unwrap()
    }

    fn host(name: &str, blobs: &[BlobRef]) -> Definition {
        let mut payload = PropertyMap::default();
        payload.insert(
            "b",
            ElementProperty::Array(
                blobs
                    .iter()
                    .map(|b| ElementProperty::Blob(Box::new(b.clone())))
                    .collect(),
            ),
        );
        Definition::opaque(
            DefinitionKind::host(200).unwrap(),
            DefinitionFlags::NONE,
            name,
            crate::persist::encode_payload(&payload).unwrap(),
        )
    }

    #[test]
    fn tag_21_round_trips_and_is_damage_before_version_4() {
        for value in [
            ElementProperty::Blob(Box::new(blob(7))),
            ElementProperty::Blob(Box::new(
                BlobRef::new(BlobHashAlgo::Unknown(9), &[1, 2, 3], 5, 6).unwrap(),
            )),
        ] {
            let mut out = Vec::new();
            put_value(&mut out, &value, &mut InlineKeys).unwrap();
            assert_eq!(out[0], 21);
            let mut reader = Reader::new(&out);
            assert_eq!(read_value(&mut reader, &InlineKeys, 8).unwrap(), value);
            let mut old = Reader::new(&out).with_version(BLOB_FORMAT_VERSION - 1);
            assert!(read_value(&mut old, &InlineKeys, 8).is_err());
        }
        // Invalid payloads: algorithm 0, an empty or too long hash, a SHA-256 of 31 bytes.
        for bad in [
            vec![21u8, 0, 1, 0],
            vec![21u8, 1, 0],
            vec![21u8, 2, 65],
            [vec![21u8, 1, 31], vec![0; 31], vec![0; 12]].concat(),
        ] {
            let mut reader = Reader::new(&bad);
            assert!(read_value(&mut reader, &InlineKeys, 8).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ledger_frames_round_trip_and_damage_is_detected() {
        let frames = vec![
            LedgerFrame {
                commit_seq: 3,
                graph_id: 9,
                blob: blob(1),
                delta: 1,
            },
            LedgerFrame {
                commit_seq: 5,
                graph_id: 9,
                blob: blob(1),
                delta: -1,
            },
        ];
        let bytes = ledger_bytes(&frames);
        assert_eq!(decode_ledger(&bytes), Some(frames.clone()));
        for at in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[at] ^= 0x10;
            assert_ne!(decode_ledger(&flipped), Some(frames.clone()), "byte {at}");
        }
        assert_eq!(decode_ledger(&bytes[..bytes.len() - 1]), None);
        assert!(frames[0].on_chain(&[(9, u64::MAX)]));
        assert!(!frames[1].on_chain(&[(9, 4)]));
        assert!(!frames[0].on_chain(&[(8, u64::MAX)]));
    }

    #[test]
    fn the_blob_table_counts_references_and_round_trips_in_order() {
        let defs = [host("a", &[blob(2), blob(1)]), host("b", &[blob(2)])];
        let table = blob_table(defs.iter());
        assert_eq!(table, vec![(blob(1), 1), (blob(2), 2)]);
        let mut out = Vec::new();
        put_table(&mut out, &table);
        assert_eq!(read_table(&mut Reader::new(&out)).unwrap(), table);
        // Out of order or a zero count: damage.
        let mut swapped = Vec::new();
        put_table(&mut swapped, &[(blob(2), 1), (blob(1), 1)]);
        assert!(read_table(&mut Reader::new(&swapped)).is_err());
        let mut zero = Vec::new();
        put_table(&mut zero, &[(blob(1), 0)]);
        assert!(read_table(&mut Reader::new(&zero)).is_err());
    }

    #[test]
    fn commit_deltas_are_net_per_blob() {
        let kind = DefinitionKind::host(200).unwrap();
        let mut builder = ChangeSet::builder(4, 0);
        builder.push(Mutation::set_definition(
            kind,
            "m",
            Some(host("m", &[blob(1), blob(2)])),
            Some(host("m", &[blob(2), blob(3), blob(3)])),
        ));
        let changes = builder.build().unwrap();
        assert_eq!(commit_deltas(&changes), vec![(blob(1), -1), (blob(3), 2)]);
    }

    /// Hostile bytes never panic, never allocate beyond the input: tag 21 payloads, ledger
    /// frames and blob table rows, random and mutated from valid ones.
    #[test]
    fn hostile_blob_bytes_are_refused_without_a_panic() {
        let mut state = 0x5eed_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        let mut valid_value = Vec::new();
        put_value(
            &mut valid_value,
            &ElementProperty::Blob(Box::new(blob(3))),
            &mut InlineKeys,
        )
        .unwrap();
        let valid_ledger = ledger_bytes(&[LedgerFrame {
            commit_seq: 1,
            graph_id: 2,
            blob: blob(3),
            delta: -4,
        }]);
        let mut valid_table = Vec::new();
        put_table(&mut valid_table, &[(blob(1), 2), (blob(5), 1)]);
        for case in 0..20_000 {
            let base: &[u8] = match case % 3 {
                0 => &valid_value,
                1 => &valid_ledger,
                _ => &valid_table,
            };
            let mut bytes = if case % 7 == 0 {
                (0..(next() % 80))
                    .map(|_| next() as u8)
                    .collect::<Vec<u8>>()
            } else {
                base.to_vec()
            };
            for _ in 0..(1 + next() % 4) {
                if bytes.is_empty() {
                    break;
                }
                let at = next() as usize % bytes.len();
                bytes[at] = next() as u8;
            }
            if next() % 5 == 0 {
                bytes.truncate(next() as usize % (bytes.len() + 1));
            }
            let _ = read_value(&mut Reader::new(&bytes), &InlineKeys, 8);
            let _ = decode_ledger(&bytes);
            let _ = read_table(&mut Reader::new(&bytes));
        }
    }

    /// The merged checkpoint computes the blob table from its whole catalog: the same table a
    /// full encode of the live graph writes.
    #[test]
    fn a_merged_table_equals_the_full_encode_table() {
        let tables = |merge_bytes: u64| {
            let dir = MemDir::new();
            let store = Store::create(
                &dir,
                StoreOptions::new()
                    .with_checkpoint_after_bytes(0)
                    .with_checkpoint_merge_bytes(merge_bytes),
            )
            .unwrap();
            let set = |d: Definition| {
                crate::graph::GraphStorage::set_definition(&mut *store.graph().write(), d).unwrap();
            };
            set(host("a", &[blob(1), blob(2)]));
            store.checkpoint(None).unwrap();
            set(host("b", &[blob(2), blob(3)]));
            set(host("a", &[blob(4)]));
            let info = store.checkpoint(None).unwrap();
            let root = DirPath::root(std::sync::Arc::new(dir));
            let (_, newest) = files::list_snapshots(&root).unwrap().pop().unwrap();
            assert_eq!(newest.file_name(), files::snapshot_name(info.commit_seq));
            super::super::snapshot_dir::read_manifest(&newest)
                .unwrap()
                .1
                .blobs
        };
        let merged = tables(u64::MAX);
        let encoded = tables(0);
        assert_eq!(merged, vec![(blob(2), 1), (blob(3), 1), (blob(4), 1)]);
        assert_eq!(merged, encoded);
    }
}
