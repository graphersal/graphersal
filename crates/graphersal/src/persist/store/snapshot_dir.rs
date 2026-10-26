//! Snapshot directories of a Store (format spec sections 4 and 6): writing one (temporary
//! directory, fsync, verify, rename), listing from the manifests' fixed parts, verifying, and
//! reading one as a packed stream (the packed sections are byte-identical to the files).

use crate::persist::StoreFailure;
use std::collections::VecDeque;
use std::io::{Read, Write};

use super::super::StoreFile;
use super::super::codec::{FORMAT_VERSION, crc, crc_append};
use super::super::encoder::{EncodeStats, SegmentOut, SnapshotEncoder};
use super::super::error::PersistError;
use super::super::manifest::{FIXED_BYTES, Manifest, ManifestFixed, is_element_kind};
use super::super::segment::{KIND_EDGE, KIND_VERTEX};
use super::super::snapshot::{
    COPY_BUFFER, PACKED_MAGIC, PackedReader, SECTION_MANIFEST, SECTION_SCHEMA, SnapshotInfo,
    WriteOptions, check_segment_stream, extra_file_prefix, section_kind,
};
use super::files::{
    DirPath, SNAPSHOTS_DIR, TMP_PREFIX, parse_seq, read_bounded, safe_relative, snapshot_name,
    sync_dir,
};
use crate::graph::PersistentStorage;

pub(crate) const MANIFEST_FILE: &str = "manifest";
pub(crate) const SCHEMA_FILE: &str = "schema.json";

/// The largest manifest or `schema.json` file read (as for packed snapshots).
const MAX_META_FILE_BYTES: u64 = 256 << 20;

/// The path of a segment named in a manifest (format spec 6.2): a plain file name lives in the
/// snapshot's own directory, `<seq>/<file>` (an incremental snapshot's reference) in the
/// directory of the older snapshot `<seq>` next to it. A snapshot moved into an attic entry
/// finds a referenced file that did not move with it in the store's own `snapshots/`.
pub(crate) fn segment_path(snapshot: &DirPath, relative: &str) -> Option<DirPath> {
    let Some((holder, file)) = relative.split_once('/') else {
        return safe_relative(snapshot, relative);
    };
    if parse_seq(holder).is_none() || file.contains('/') {
        return None;
    }
    let parent = snapshot.parent()?;
    let near = safe_relative(&parent, relative)?;
    if parent.name() != SNAPSHOTS_DIR && !near.exists() {
        let store = DirPath::root(snapshot.store_dir().clone()).join(SNAPSHOTS_DIR);
        return safe_relative(&store, relative);
    }
    Some(near)
}

/// The manifest bytes of a snapshot as a self-contained packed snapshot carries them: an
/// incremental manifest (format spec 6.2, flag bit 0) gets plain file names for its references,
/// `v-NNNNNN.seg`/`e-NNNNNN.seg` numbered per kind in table order, and flag bit 0 cleared (the
/// segments' bytes are unchanged); any other manifest is returned as it is.
pub(crate) fn self_contained_manifest(bytes: Vec<u8>, manifest: &Manifest) -> Vec<u8> {
    if !manifest
        .segments
        .iter()
        .any(|s| is_element_kind(s.kind) && s.path.contains('/'))
    {
        return bytes;
    }
    let mut flat = manifest.clone();
    flat.fixed.flags &= !super::super::manifest::FLAG_INCREMENTAL;
    let (mut vertices, mut edges) = (0usize, 0usize);
    for segment in &mut flat.segments {
        let (prefix, counter) = match segment.kind {
            KIND_VERTEX => ("v", &mut vertices),
            KIND_EDGE => ("e", &mut edges),
            _ => continue,
        };
        segment.path = format!("{prefix}-{:06}.seg", *counter);
        *counter += 1;
    }
    flat.encode()
}

/// A snapshot directory being written (format spec 12.3): its files go into
/// `snapshots/tmp-<random>/`, which [`NewSnapshotDir::finish`] syncs, verifies by reading it
/// back and renames to `snapshots/<commit_seq>`. Dropped unfinished (an error), the temporary
/// directory is removed; a crash leaves it behind, and the next open removes it.
pub(crate) struct NewSnapshotDir {
    root: DirPath,
    tmp: DirPath,
    final_dir: DirPath,
    done: bool,
}

impl NewSnapshotDir {
    /// Starts the snapshot at `commit_seq` of the store in `store`: fails when one exists.
    pub(crate) fn begin(store: &DirPath, commit_seq: u64) -> Result<Self, PersistError> {
        let root = store.join(SNAPSHOTS_DIR);
        let final_dir = root.join(snapshot_name(commit_seq));
        if final_dir.exists() {
            return Err(PersistError::store(
                "write the snapshot",
                StoreFailure::SnapshotExists,
                format!("a snapshot at commit {commit_seq} already exists"),
            ));
        }
        let tmp = root.join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7()));
        tmp.create_dir_all()
            .map_err(|err| PersistError::io("creating a snapshot directory", err))?;
        Ok(Self {
            root,
            tmp,
            final_dir,
            done: false,
        })
    }

    /// The temporary directory the files go into.
    pub(crate) fn tmp(&self) -> &DirPath {
        &self.tmp
    }

    /// Syncs the temporary directory, verifies every file against the manifest, renames it to
    /// its final name and syncs `snapshots/`. Returns the new directory.
    pub(crate) fn finish(mut self) -> Result<(DirPath, SnapshotInfo), PersistError> {
        sync_dir(&self.tmp)?;
        // A new snapshot counts only once it is verified (format spec 12.3).
        let info = verify_snapshot_dir(&self.tmp)?;
        self.tmp
            .rename_to(&self.final_dir)
            .map_err(|err| PersistError::io("renaming the new snapshot", err))?;
        self.done = true;
        sync_dir(&self.root)?;
        Ok((self.final_dir.clone(), info))
    }
}

impl Drop for NewSnapshotDir {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.tmp.remove();
        }
    }
}

/// A file of a new snapshot written piece by piece (the emit pass of a merge checkpoint):
/// created, written through a buffer, synced by [`NewFile::finish`]. Dropped unfinished, it
/// stays incomplete in the temporary directory, which is removed with it.
pub(crate) struct NewFile(FileOut);

impl NewFile {
    pub(crate) fn create(path: &DirPath) -> Result<Self, PersistError> {
        let file = path
            .create(true)
            .map_err(|err| PersistError::io("writing a snapshot file", err))?;
        Ok(Self(FileOut(std::io::BufWriter::with_capacity(
            COPY_BUFFER,
            file,
        ))))
    }

    pub(crate) fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        self.0.put(bytes)
    }

    pub(crate) fn finish(self) -> Result<(), PersistError> {
        let mut file = (self.0)
            .0
            .into_inner()
            .map_err(|err| PersistError::io("writing a snapshot file", err.into_error()))?;
        file.sync()
            .map_err(|err| PersistError::io("writing a snapshot file", err))
    }
}

/// A file of a new snapshot: created, written through a buffer, synced.
pub(crate) fn write_file(
    path: &DirPath,
    fill: impl FnOnce(&mut FileOut) -> Result<(), PersistError>,
) -> Result<(), PersistError> {
    let file = path
        .create(true)
        .map_err(|err| PersistError::io("writing a snapshot file", err))?;
    let mut out = FileOut(std::io::BufWriter::with_capacity(COPY_BUFFER, file));
    fill(&mut out)?;
    let mut file = out
        .0
        .into_inner()
        .map_err(|err| PersistError::io("writing a snapshot file", err.into_error()))?;
    file.sync()
        .map_err(|err| PersistError::io("writing a snapshot file", err))
}

/// The buffered writer of one new snapshot file.
pub(crate) struct FileOut(std::io::BufWriter<Box<dyn StoreFile>>);

impl SegmentOut for FileOut {
    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        self.0
            .write_all(bytes)
            .map_err(|err| PersistError::io("writing a snapshot file", err))
    }
}

impl Write for FileOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Writes `graph` as the files of a snapshot directory into `tmp` with the streaming encoder
/// (`persist::encoder`): every segment file as soon as its plan is complete, chunk by chunk,
/// then `schema.json`, then the manifest. Memory: the sorted id index and one chunk. The
/// caller holds the graph's read access for the whole call and nothing longer: the files are
/// synced here, the directory is synced, verified and renamed by [`NewSnapshotDir::finish`]
/// without the graph.
pub(crate) fn write_graph_snapshot<S: PersistentStorage + ?Sized>(
    tmp: &DirPath,
    graph: &S,
    options: &WriteOptions,
) -> Result<EncodeStats, PersistError> {
    let encoder = SnapshotEncoder::new(graph, options);
    let mut index = Vec::new();
    let mut property_counts = [0u64; 2];
    for (slot, kind) in [KIND_VERTEX, KIND_EDGE].into_iter().enumerate() {
        let totals = encoder.plan_kind(kind, &mut |plan| {
            write_file(&tmp.join(&plan.entry.path), |out| encoder.emit(&plan, out))?;
            index.push(plan.index());
            Ok(())
        })?;
        property_counts[slot] = totals.property_count;
    }
    let schema = encoder.schema();
    let (_, manifest, _) =
        encoder.manifest(&index, property_counts[0], property_counts[1], &schema)?;
    write_file(&tmp.join(SCHEMA_FILE), |out| out.put(&schema))?;
    write_file(&tmp.join(MANIFEST_FILE), |out| out.put(&manifest))?;
    Ok(encoder.stats())
}

/// Writes the snapshot of a packed stream as the files of a snapshot directory, copying each
/// section through a bounded buffer (format spec 8: the sections are the files). Returns the
/// new snapshot, verified and renamed. Nothing is written when the stream's head is bad.
pub(crate) fn import_packed(
    store: &DirPath,
    packed: impl Read,
) -> Result<(DirPath, SnapshotInfo), PersistError> {
    let mut reader = PackedReader::open(packed)?;
    let snapshot = NewSnapshotDir::begin(store, reader.manifest.fixed.commit_seq)?;
    // Model files (format spec 4.5, 8) go to a staging directory per model under models/, not
    // into the snapshot; the snapshot's manifest is written without their rows.
    let mut staged: Vec<(String, DirPath)> = Vec::new();
    let mut model_rows = Vec::new();
    for (number, entry) in reader.manifest.segments.clone().into_iter().enumerate() {
        if entry.path.contains('/') {
            return Err(PersistError::corrupt(
                "packed snapshot",
                0,
                "an incremental segment reference cannot be imported",
            ));
        }
        if entry.kind == super::models::PACKED_MODEL_KIND
            && let Some((id, file)) = super::models::parse_packed_name(&entry.path)
        {
            let dir = match staged.iter().find(|(staged_id, _)| staged_id == id) {
                Some((_, dir)) => dir.clone(),
                None => {
                    let dir = super::models::models_dir(store)
                        .join(format!("{TMP_PREFIX}{:032x}", crate::value::uuid::new_v7()));
                    dir.create_dir_all()
                        .map_err(|err| PersistError::io("creating a model directory", err))?;
                    staged.push((id.to_string(), dir.clone()));
                    dir
                }
            };
            write_file(&dir.join(file), |out| reader.copy_segment(out).map(|_| ()))?;
            model_rows.push(number);
            continue;
        }
        let path = safe_relative(snapshot.tmp(), &entry.path)
            .ok_or_else(|| PersistError::corrupt("packed snapshot", 0, "unsafe segment path"))?;
        write_file(&path, |out| reader.copy_segment(out).map(|_| ()))?;
    }
    let schema = std::mem::take(&mut reader.schema);
    let mut manifest = std::mem::take(&mut reader.manifest_bytes);
    if !model_rows.is_empty() {
        let mut without = reader.manifest.clone();
        let mut number = 0;
        without.segments.retain(|_| {
            number += 1;
            !model_rows.contains(&(number - 1))
        });
        manifest = without.encode();
    }
    reader.finish()?;
    write_file(&snapshot.tmp().join(SCHEMA_FILE), |out| out.put(&schema))?;
    write_file(&snapshot.tmp().join(MANIFEST_FILE), |out| {
        out.put(&manifest)
    })?;
    let result = (|| {
        for (id, dir) in &staged {
            super::models::install_staged(store, id, dir)?;
        }
        snapshot.finish()
    })();
    if result.is_err() {
        for (_, dir) in &staged {
            let _ = super::files::remove_path(dir);
        }
    }
    result
}

/// Reads a snapshot's manifest file.
pub(crate) fn read_manifest(snapshot: &DirPath) -> Result<(Vec<u8>, Manifest), PersistError> {
    let location = format!("{}/{MANIFEST_FILE}", snapshot.display());
    let bytes = read_bounded(&snapshot.join(MANIFEST_FILE), MAX_META_FILE_BYTES)
        .map_err(|err| PersistError::io("reading a snapshot manifest", err))?;
    let manifest = Manifest::decode(&bytes).map_err(|err| err.at(&location, 0))?;
    Ok((bytes, manifest))
}

/// The listing fields of a snapshot: only the 4096-byte fixed part of its manifest is read
/// (the tail copy when the head is damaged). `meta` is not part of it (empty).
pub(crate) fn read_listing(snapshot: &DirPath) -> Result<SnapshotInfo, PersistError> {
    let location = format!("{}/{MANIFEST_FILE}", snapshot.display());
    let file = snapshot.join(MANIFEST_FILE);
    let len = file
        .len()
        .map_err(|err| PersistError::io("reading a snapshot manifest", err))?;
    let head_result = file
        .read_range(0, FIXED_BYTES)
        .map_err(|err| err.to_string())
        .and_then(|head| ManifestFixed::decode(&head).map_err(|err| err.reason));
    let fixed = match head_result {
        Ok(fixed) => fixed,
        Err(head_err) => {
            let tail = len
                .checked_sub(FIXED_BYTES as u64)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
                .and_then(|at| file.read_range(at, FIXED_BYTES))
                .map_err(|_| PersistError::corrupt(&location, 0, head_err.clone()))?;
            ManifestFixed::decode(&tail)
                .map_err(|_| PersistError::corrupt(&location, 0, head_err))?
        }
    };
    Ok(SnapshotInfo::from_fixed(&fixed))
}

/// The length and CRC of a stream, read through a bounded buffer up to `max + 1` bytes (a
/// longer stream shows as a longer length).
fn crc_of_stream(reader: &mut dyn Read, max: u64) -> std::io::Result<(u64, u32)> {
    let mut buf = vec![0u8; COPY_BUFFER];
    let (mut len, mut value) = (0u64, 0u32);
    let limit = max.saturating_add(1);
    while len < limit {
        let want = buf
            .len()
            .min(usize::try_from(limit - len).unwrap_or(usize::MAX));
        let n = match reader.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        value = crc_append(value, &buf[..n]);
        len += n as u64;
    }
    Ok((len, value))
}

/// Checks every file of a snapshot directory against its manifest: the manifest's own CRCs,
/// `schema.json` (length, CRC), every segment (length, CRC of the file, header, every chunk's
/// CRC, footer, the chunk index). Decodes no element; segment files are read as streams, one
/// chunk at a time.
pub(crate) fn verify_snapshot_dir(snapshot: &DirPath) -> Result<SnapshotInfo, PersistError> {
    let (_, manifest) = read_manifest(snapshot)?;
    let schema = read_bounded(&snapshot.join(SCHEMA_FILE), MAX_META_FILE_BYTES)
        .map_err(|err| PersistError::io("reading a snapshot's schema.json", err))?;
    if schema.len() as u64 != manifest.fixed.schema_len || crc(&schema) != manifest.fixed.schema_crc
    {
        return Err(PersistError::corrupt(
            format!("{}/{SCHEMA_FILE}", snapshot.display()),
            0,
            "length or checksum differs from the manifest",
        ));
    }
    for (number, entry) in manifest.segments.iter().enumerate() {
        let location = format!("{}/{}", snapshot.display(), entry.path);
        let path = segment_path(snapshot, &entry.path)
            .ok_or_else(|| PersistError::corrupt(&location, 0, "unsafe segment path"))?;
        let mut reader = path
            .open_read()
            .map_err(|err| PersistError::io("reading a snapshot segment", err))?;
        if !is_element_kind(entry.kind) {
            // A file of another kind (format spec 7.5): its length and CRC.
            let (len, file_crc) = crc_of_stream(&mut reader, entry.byte_len)
                .map_err(|err| PersistError::io("reading a snapshot segment", err))?;
            if len != entry.byte_len || file_crc != entry.crc {
                return Err(PersistError::corrupt(
                    &location,
                    0,
                    "length or checksum differs from the manifest",
                ));
            }
            continue;
        }
        check_segment_stream(&manifest, number, &mut reader, &location)?;
    }
    Ok(SnapshotInfo::from_manifest(&manifest))
}

/// Checks one file of a snapshot read as a stream (an entry of a backup archive) against the
/// snapshot's manifest, as [`verify_snapshot_dir`] checks it: `schema.json` (length, CRC), or
/// the file the manifest names `path` (a segment: every chunk, the chunk index, the footer,
/// the file's length and CRC; a file of another kind: its length and CRC).
#[cfg_attr(not(feature = "persist-zip"), allow(dead_code))]
pub(crate) fn check_snapshot_file(
    manifest: &Manifest,
    path: &str,
    reader: &mut dyn Read,
    location: &str,
) -> Result<(), PersistError> {
    let mismatch =
        || PersistError::corrupt(location, 0, "length or checksum differs from the manifest");
    if path == SCHEMA_FILE {
        let (len, value) = crc_of_stream(reader, MAX_META_FILE_BYTES)
            .map_err(|err| PersistError::io("reading a snapshot's schema.json", err))?;
        if len != manifest.fixed.schema_len || value != manifest.fixed.schema_crc {
            return Err(mismatch());
        }
        return Ok(());
    }
    let Some(number) = manifest.segments.iter().position(|s| s.path == path) else {
        return Err(PersistError::corrupt(
            location,
            0,
            "a file the snapshot's manifest does not name",
        ));
    };
    let entry = &manifest.segments[number];
    if !is_element_kind(entry.kind) {
        let (len, value) = crc_of_stream(reader, entry.byte_len)
            .map_err(|err| PersistError::io("reading a snapshot segment", err))?;
        if len != entry.byte_len || value != entry.crc {
            return Err(mismatch());
        }
        return Ok(());
    }
    check_segment_stream(manifest, number, reader, location)
}

/// One piece of a packed stream built from a snapshot directory.
enum Part {
    Bytes(Vec<u8>),
    File(DirPath),
    /// A file of which exactly this many bytes are read (its length was checked when the
    /// manifest row was made; a file that grew since is cut there, one that shrank fails the
    /// trailer).
    Sized(DirPath, u64),
}

/// A snapshot directory read as the packed snapshot it is equivalent to (format spec 8: the
/// sections are the files), so the stream reader loads it and `export_packed` copies it. The
/// trailer CRC is computed while streaming.
pub(crate) struct DirSnapshotReader {
    parts: VecDeque<Part>,
    current: Option<Box<dyn Read + Send>>,
    crc: u32,
    trailer: Option<std::io::Cursor<[u8; 4]>>,
    done: bool,
}

impl DirSnapshotReader {
    pub(crate) fn open(snapshot: &DirPath) -> Result<Self, PersistError> {
        Self::open_with_models(snapshot, Vec::new())
    }

    /// The packed stream of `snapshot` carrying `models` (the store's model files, format spec
    /// 4.5) as named extra files of the ignorable segment kind
    /// [`PACKED_MODEL_KIND`](super::models::PACKED_MODEL_KIND), after the snapshot's own rows.
    pub(crate) fn open_with_models(
        snapshot: &DirPath,
        models: Vec<super::models::PackedModelFile>,
    ) -> Result<Self, PersistError> {
        let (manifest_bytes, manifest) = read_manifest(snapshot)?;
        // A packed snapshot is self-contained: references become plain names (the segment
        // sections are the referenced files' bytes).
        let manifest_bytes = self_contained_manifest(manifest_bytes, &manifest);
        let (manifest_bytes, manifest, models) = if models.is_empty() {
            (manifest_bytes, manifest, models)
        } else {
            let mut with = Manifest::decode(&manifest_bytes)
                .map_err(|err| err.at(format!("{}/{MANIFEST_FILE}", snapshot.display()), 0))?;
            // A snapshot imported from a packed file by a Graphersal without models keeps their
            // files as its own rows (format spec 4.5): those names are not added twice.
            let own: std::collections::HashSet<String> =
                with.segments.iter().map(|s| s.path.clone()).collect();
            let models: Vec<super::models::PackedModelFile> = models
                .into_iter()
                .filter(|f| !own.contains(&f.name))
                .collect();
            for file in &models {
                with.segments.push(super::super::manifest::SegmentEntry {
                    kind: super::models::PACKED_MODEL_KIND,
                    path: file.name.clone(),
                    first_id: file.id.as_str().into(),
                    last_id: file.file.as_str().into(),
                    count: 0,
                    byte_len: file.len,
                    crc: file.crc,
                });
            }
            let bytes = with.encode();
            (bytes, with, models)
        };
        let own_rows = manifest.segments.len() - models.len();
        let mut models = models.into_iter();
        let mut parts = VecDeque::new();
        let mut head = Vec::with_capacity(16);
        head.extend_from_slice(PACKED_MAGIC);
        head.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        head.extend_from_slice(&(2 + manifest.segments.len() as u32).to_le_bytes());
        let section = |kind: u8, len: u64| {
            let mut out = Vec::with_capacity(9);
            out.push(kind);
            out.extend_from_slice(&len.to_le_bytes());
            out
        };
        head.extend_from_slice(&section(SECTION_MANIFEST, manifest_bytes.len() as u64));
        parts.push_back(Part::Bytes(head));
        parts.push_back(Part::Bytes(manifest_bytes));
        let schema_path = snapshot.join(SCHEMA_FILE);
        let schema_len = schema_path
            .len()
            .map_err(|err| PersistError::io("reading a snapshot's schema.json", err))?;
        parts.push_back(Part::Bytes(section(SECTION_SCHEMA, schema_len)));
        parts.push_back(Part::File(schema_path));
        for (number, entry) in manifest.segments.iter().enumerate() {
            if number >= own_rows {
                // A model file (format spec 4.5): its bytes from the store's models/.
                let Some(file) = models.next() else { break };
                let prefix = extra_file_prefix(&entry.path);
                let mut bytes = section(section_kind(entry.kind), prefix.len() as u64 + file.len);
                bytes.extend_from_slice(&prefix);
                parts.push_back(Part::Bytes(bytes));
                parts.push_back(match file.source {
                    super::models::PackedSource::Bytes(bytes) => Part::Bytes(bytes),
                    super::models::PackedSource::File(path) => Part::Sized(path, file.len),
                });
                continue;
            }
            let location = format!("{}/{}", snapshot.display(), entry.path);
            let path = segment_path(snapshot, &entry.path)
                .ok_or_else(|| PersistError::corrupt(&location, 0, "unsafe segment path"))?;
            let len = path
                .len()
                .map_err(|err| PersistError::io("reading a snapshot segment", err))?;
            if is_element_kind(entry.kind) {
                parts.push_back(Part::Bytes(section(section_kind(entry.kind), len)));
            } else {
                // A named extra file: the name precedes the file's bytes (format spec 8).
                let prefix = extra_file_prefix(&entry.path);
                let mut bytes = section(section_kind(entry.kind), prefix.len() as u64 + len);
                bytes.extend_from_slice(&prefix);
                parts.push_back(Part::Bytes(bytes));
            }
            parts.push_back(Part::File(path));
        }
        Ok(Self {
            parts,
            current: None,
            crc: 0,
            trailer: None,
            done: false,
        })
    }
}

impl Read for DirSnapshotReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(trailer) = &mut self.trailer {
                return trailer.read(buf);
            }
            if self.done {
                return Ok(0);
            }
            if let Some(current) = &mut self.current {
                let n = current.read(buf)?;
                if n > 0 {
                    self.crc = crc_append(self.crc, &buf[..n]);
                    return Ok(n);
                }
                self.current = None;
            }
            match self.parts.pop_front() {
                Some(Part::Bytes(bytes)) => {
                    self.current = Some(Box::new(std::io::Cursor::new(bytes)))
                }
                Some(Part::File(path)) => self.current = Some(path.open_read()?),
                Some(Part::Sized(path, len)) => {
                    self.current = Some(Box::new(path.open_read()?.take(len)))
                }
                None => {
                    self.done = true;
                    self.trailer = Some(std::io::Cursor::new(self.crc.to_le_bytes()));
                }
            }
        }
    }
}
