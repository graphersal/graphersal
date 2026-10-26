//! Backup archives as ZIP (feature `persist-zip`, format spec 12.8): stored entries (the chunks are
//! compressed already), zip64 records always, so any size works; written to any `Write` in one
//! pass and restored from any `Read` in one pass. A minimal implementation of exactly this
//! subset of the ZIP format (APPNOTE 6.3.x), no dependency.

use crate::persist::StoreFailure;
use std::io::{Read, Write};

use super::super::error::PersistError;
use super::files::{
    DirPath, GRAPH_COPY, GRAPH_FILE, MARKS_FILE, SNAPSHOTS_DIR, WAL_DIR, WAL_EXTENSION, sync_dir,
};

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;
/// Version 4.5: zip64.
const VERSION: u16 = 45;
/// General purpose flag bit 11: the name is UTF-8.
const FLAG_UTF8: u16 = 0x0800;
/// 1980-01-01 00:00 in DOS date/time (entries carry no meaningful time).
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = (1 << 5) | 1;

/// CRC-32 (IEEE 802.3, the ZIP checksum; the store's own files use CRC-32C).
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    })
}

#[derive(Clone, Copy)]
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }

    fn update(&mut self, bytes: &[u8]) {
        let table = crc32_table();
        let mut c = self.0;
        for &b in bytes {
            c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

fn io(err: std::io::Error) -> PersistError {
    PersistError::io("writing the backup archive", err)
}

struct Entry {
    name: String,
    crc: u32,
    size: u64,
    offset: u64,
}

/// Writes a stored, zip64 archive in one pass.
pub(crate) struct ZipWriter<W: Write> {
    out: W,
    offset: u64,
    entries: Vec<Entry>,
}

impl<W: Write> ZipWriter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            offset: 0,
            entries: Vec::new(),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), PersistError> {
        self.out.write_all(bytes).map_err(io)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    fn local_header(&mut self, name: &str, crc: u32, size: u64) -> Result<(), PersistError> {
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        h.extend_from_slice(&LOCAL_HEADER.to_le_bytes());
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // stored
        h.extend_from_slice(&DOS_TIME.to_le_bytes());
        h.extend_from_slice(&DOS_DATE.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&u32::MAX.to_le_bytes()); // sizes in the zip64 extra field
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        h.extend_from_slice(&1u16.to_le_bytes()); // zip64 extended information
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        self.put(&h)
    }

    /// Adds an entry from memory.
    pub(crate) fn add_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), PersistError> {
        let mut crc = Crc32::new();
        crc.update(bytes);
        let crc = crc.finish();
        let offset = self.offset;
        self.local_header(name, crc, bytes.len() as u64)?;
        self.put(bytes)?;
        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size: bytes.len() as u64,
            offset,
        });
        Ok(())
    }

    /// Adds the first `len` bytes of a file (read twice: the CRC goes into the local header,
    /// so a one-pass reader can check every entry; the bytes are immutable or append-only).
    pub(crate) fn add_file(
        &mut self,
        name: &str,
        path: &DirPath,
        len: u64,
    ) -> Result<(), PersistError> {
        let mut crc = Crc32::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut file = path.open_read().map_err(io)?.take(len);
        let mut seen = 0u64;
        loop {
            let n = file.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            crc.update(&buf[..n]);
            seen += n as u64;
        }
        if seen != len {
            return Err(PersistError::store(
                "back up the store",
                StoreFailure::ChangedConcurrently,
                format!("{} shrank while it was copied", path.display()),
            ));
        }
        let crc = crc.finish();
        let offset = self.offset;
        self.local_header(name, crc, len)?;
        let mut file = path.open_read().map_err(io)?.take(len);
        let mut check = Crc32::new();
        loop {
            let n = file.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            check.update(&buf[..n]);
            self.out.write_all(&buf[..n]).map_err(io)?;
            self.offset += n as u64;
        }
        if check.finish() != crc || self.offset - offset < len {
            return Err(PersistError::store(
                "back up the store",
                StoreFailure::ChangedConcurrently,
                format!("{} changed while it was copied", path.display()),
            ));
        }
        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size: len,
            offset,
        });
        Ok(())
    }

    /// Writes the central directory and the zip64 end records.
    pub(crate) fn finish(mut self) -> Result<(), PersistError> {
        let central_start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for entry in &entries {
            let mut h = Vec::with_capacity(46 + entry.name.len() + 28);
            h.extend_from_slice(&CENTRAL_HEADER.to_le_bytes());
            h.extend_from_slice(&(VERSION | (3 << 8)).to_le_bytes()); // made by: UNIX
            h.extend_from_slice(&VERSION.to_le_bytes());
            h.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&DOS_TIME.to_le_bytes());
            h.extend_from_slice(&DOS_DATE.to_le_bytes());
            h.extend_from_slice(&entry.crc.to_le_bytes());
            h.extend_from_slice(&u32::MAX.to_le_bytes());
            h.extend_from_slice(&u32::MAX.to_le_bytes());
            h.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            h.extend_from_slice(&28u16.to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // comment
            h.extend_from_slice(&0u16.to_le_bytes()); // disk
            h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            h.extend_from_slice(&(0o100644u32 << 16).to_le_bytes()); // external: rw-r--r--
            h.extend_from_slice(&u32::MAX.to_le_bytes()); // offset in the zip64 field
            h.extend_from_slice(entry.name.as_bytes());
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&24u16.to_le_bytes());
            h.extend_from_slice(&entry.size.to_le_bytes());
            h.extend_from_slice(&entry.size.to_le_bytes());
            h.extend_from_slice(&entry.offset.to_le_bytes());
            self.put(&h)?;
        }
        let central_size = self.offset - central_start;
        let zip64_end = self.offset;
        let count = entries.len() as u64;
        let mut t = Vec::with_capacity(56 + 20 + 22);
        t.extend_from_slice(&ZIP64_END.to_le_bytes());
        t.extend_from_slice(&44u64.to_le_bytes());
        t.extend_from_slice(&(VERSION | (3 << 8)).to_le_bytes());
        t.extend_from_slice(&VERSION.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&count.to_le_bytes());
        t.extend_from_slice(&count.to_le_bytes());
        t.extend_from_slice(&central_size.to_le_bytes());
        t.extend_from_slice(&central_start.to_le_bytes());
        t.extend_from_slice(&ZIP64_LOCATOR.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&zip64_end.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&END_OF_CENTRAL.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&u16::MAX.to_le_bytes());
        t.extend_from_slice(&u16::MAX.to_le_bytes());
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        self.put(&t)?;
        self.out.flush().map_err(io)
    }
}

fn corrupt(offset: u64, reason: impl Into<String>) -> PersistError {
    PersistError::corrupt("backup archive", offset, reason)
}

/// Whether `name` is a file a store backup may contain: `GRAPH`, `GRAPH.copy`, `marks`,
/// `snapshots/<20 digits>/<file>`, `wal/<20 digits>.wal`.
fn allowed(name: &str) -> bool {
    let digits = |s: &str| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit());
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
            && !s.starts_with('.')
    };
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        [single] => [GRAPH_FILE, GRAPH_COPY, MARKS_FILE].contains(single),
        [dir, file] if *dir == WAL_DIR => file
            .strip_suffix(&format!(".{WAL_EXTENSION}"))
            .is_some_and(digits),
        [dir, seq, file] if *dir == SNAPSHOTS_DIR => digits(seq) && plain(file),
        // A model (format spec 4.5, 12.8): `models/<id>/<file>`.
        [dir, id, file] if *dir == super::models::MODELS_DIR => {
            super::models::is_model_id(id)
                && (*file == super::models::MODEL_MANIFEST || super::models::is_file_name(file))
        }
        _ => false,
    }
}

/// Reads exactly `N` bytes; `None` at a clean end of the stream before the first byte.
fn read_array<const N: usize>(source: &mut impl Read, at: u64) -> Result<[u8; N], PersistError> {
    let mut buf = [0u8; N];
    source
        .read_exact(&mut buf)
        .map_err(|_| corrupt(at, "the archive ends inside a header"))?;
    Ok(buf)
}

/// Unpacks a backup archive into `dir` (must not exist or be empty), checking every entry's
/// CRC; a name outside the store layout, a compressed entry or a damaged byte fails it, and
/// what was written is removed again.
pub(crate) fn restore(mut reader: impl Read, dir: &DirPath) -> Result<(), PersistError> {
    let existed = dir.exists();
    if super::files::has_entries(dir) {
        return Err(PersistError::NotAStore {
            path: dir.display().to_string(),
            reason: "the directory to restore into is not empty".into(),
        });
    }
    let result = restore_into(&mut reader, dir);
    if result.is_err() {
        super::files::undo_new_dir(dir, existed);
    }
    result
}

/// The local header of one entry, checked (format spec 12.8: a store name, stored, not
/// encrypted, not streamed, zip64 sizes).
struct EntryHeader {
    name: String,
    crc: u32,
    size: u64,
    /// The bytes of the local header (the entry's data follows them).
    header_len: u64,
}

/// Reads the next local header at `offset`; `None` at the central directory.
fn next_entry(
    reader: &mut impl Read,
    offset: u64,
    seen: &mut std::collections::HashSet<String>,
) -> Result<Option<EntryHeader>, PersistError> {
    let signature = u32::from_le_bytes(read_array::<4>(reader, offset)?);
    if signature == CENTRAL_HEADER {
        return Ok(None);
    }
    if signature != LOCAL_HEADER {
        return Err(corrupt(offset, "not a local file header"));
    }
    let h = read_array::<26>(reader, offset + 4)?;
    let u16_at = |i: usize| u16::from_le_bytes([h[i], h[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let flags = u16_at(2);
    let method = u16_at(4);
    let crc = u32_at(10);
    let (mut stored, mut size) = (u64::from(u32_at(14)), u64::from(u32_at(18)));
    let name_len = u16_at(22) as usize;
    let extra_len = u16_at(24) as usize;
    if method != 0 || flags & 0x0008 != 0 || flags & 0x0001 != 0 {
        return Err(corrupt(
            offset,
            "a compressed, encrypted or streamed entry (not a Graphersal backup)",
        ));
    }
    let mut name = vec![0u8; name_len];
    reader
        .read_exact(&mut name)
        .map_err(|_| corrupt(offset, "the archive ends inside a name"))?;
    let mut extra = vec![0u8; extra_len];
    reader
        .read_exact(&mut extra)
        .map_err(|_| corrupt(offset, "the archive ends inside an extra field"))?;
    let name = String::from_utf8(name).map_err(|_| corrupt(offset, "a name that is not UTF-8"))?;
    if allowed(&name) && !seen.insert(name.clone()) {
        return Err(corrupt(offset, format!("repeated entry {name:?}")));
    }
    if !allowed(&name) {
        // A well-formed name outside this version's layout: a file a newer Graphersal puts
        // into its backups (format spec 16), not damage; this version cannot restore it.
        let plain = name.split('/').all(|part| {
            !part.is_empty()
                && part.len() <= 64
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
        });
        if plain && name.split('/').count() <= 4 {
            return Err(PersistError::unsupported_content(
                "backup archive",
                offset,
                format!(
                    "the entry {name:?} is not part of a store of this version (written by a \
                     newer Graphersal?); restore the backup with that version"
                ),
            ));
        }
        return Err(corrupt(offset, format!("unexpected entry {name:?}")));
    }
    // zip64: the 64-bit sizes in the extended information field.
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[at], extra[at + 1]]);
        let len = u16::from_le_bytes([extra[at + 2], extra[at + 3]]) as usize;
        let body = extra
            .get(at + 4..at + 4 + len)
            .ok_or_else(|| corrupt(offset, "an extra field runs past its block"))?;
        if id == 1 {
            let mut words = body
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c));
            if size == u64::from(u32::MAX) {
                size = words
                    .next()
                    .ok_or_else(|| corrupt(offset, "zip64 size missing"))?;
            }
            if stored == u64::from(u32::MAX) {
                stored = words
                    .next()
                    .ok_or_else(|| corrupt(offset, "zip64 size missing"))?;
            }
        }
        at += 4 + len;
    }
    if stored != size {
        return Err(corrupt(offset, "stored and uncompressed sizes differ"));
    }
    Ok(Some(EntryHeader {
        name,
        crc,
        size,
        header_len: 30 + name_len as u64 + extra_len as u64,
    }))
}

/// The data of one entry as a stream: at most its size, with the CRC-32 of what was read.
struct EntryData<'r, R: Read> {
    inner: &'r mut R,
    left: u64,
    crc: Crc32,
}

impl<R: Read> Read for EntryData<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if want == 0 {
            return Ok(0);
        }
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the archive ends inside an entry",
            ));
        }
        self.crc.update(&buf[..n]);
        self.left -= n as u64;
        Ok(n)
    }
}

/// Reads a backup archive entry by entry (one pass, nothing is written): `check` gets each
/// entry's name and data as a stream and returns its problems (it need not read all of it);
/// every entry's CRC-32 is checked over all of its bytes. Returns the problems in the order
/// found (a structural fault of the archive ends the read).
pub(crate) fn check_entries(
    mut reader: impl Read,
    check: &mut dyn FnMut(&str, &mut dyn Read) -> Vec<String>,
) -> Vec<(String, String)> {
    let mut problems = Vec::new();
    let mut offset = 0u64;
    let mut seen = std::collections::HashSet::new();
    loop {
        let header = match next_entry(&mut reader, offset, &mut seen) {
            Ok(Some(header)) => header,
            Ok(None) => break,
            Err(err) => {
                problems.push((".".to_string(), err.to_string()));
                break;
            }
        };
        let mut data = EntryData {
            inner: &mut reader,
            left: header.size,
            crc: Crc32::new(),
        };
        for reason in check(&header.name, &mut data) {
            problems.push((header.name.clone(), reason));
        }
        if let Err(err) = std::io::copy(&mut data, &mut std::io::sink()) {
            problems.push((header.name.clone(), err.to_string()));
            break;
        }
        if data.crc.finish() != header.crc {
            problems.push((header.name.clone(), "CRC-32 mismatch".into()));
        }
        offset += header.header_len + header.size;
    }
    problems
}

fn restore_into(reader: &mut impl Read, dir: &DirPath) -> Result<(), PersistError> {
    dir.join(SNAPSHOTS_DIR)
        .create_dir_all()
        .and_then(|()| dir.join(WAL_DIR).create_dir_all())
        .map_err(|err| PersistError::io("creating the restored store", err))?;
    let mut offset = 0u64;
    let mut seen = std::collections::HashSet::new();
    let mut buf = vec![0u8; 1 << 16];
    while let Some(header) = next_entry(reader, offset, &mut seen)? {
        let EntryHeader {
            name,
            crc,
            size,
            header_len,
        } = header;
        let path = dir.join(&name);
        if let Some(parent) = path.parent() {
            parent
                .create_dir_all()
                .map_err(|err| PersistError::io("creating the restored store", err))?;
        }
        let mut out = path
            .create(false)
            .map_err(|err| PersistError::io("writing the restored store", err))?;
        let mut left = size;
        let mut check = Crc32::new();
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = reader
                .read(&mut buf[..want])
                .map_err(|err| PersistError::io("reading the backup archive", err))?;
            if n == 0 {
                return Err(corrupt(offset, format!("the archive ends inside {name:?}")));
            }
            check.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|err| PersistError::io("writing the restored store", err))?;
            left -= n as u64;
        }
        if check.finish() != crc {
            return Err(corrupt(offset, format!("{name:?}: CRC-32 mismatch")));
        }
        out.sync()
            .map_err(|err| PersistError::io("writing the restored store", err))?;
        offset += header_len + size;
    }
    if !seen.contains(GRAPH_FILE) {
        return Err(corrupt(offset, "the archive has no GRAPH file"));
    }
    if let Ok(entries) = dir.join(SNAPSHOTS_DIR).list() {
        for (name, _) in entries {
            sync_dir(&dir.join(SNAPSHOTS_DIR).join(&name))?;
        }
    }
    sync_dir(&dir.join(SNAPSHOTS_DIR))?;
    sync_dir(&dir.join(WAL_DIR))?;
    sync_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_reference_value() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    #[test]
    fn only_store_files_are_allowed() {
        assert!(allowed("GRAPH"));
        assert!(allowed("wal/00000000000000000001.wal"));
        assert!(allowed("snapshots/00000000000000000000/v-000000.seg"));
        assert!(!allowed("../GRAPH"));
        assert!(!allowed("snapshots/00000000000000000000/../x"));
        assert!(!allowed("wal/1.wal"));
        assert!(!allowed("LOCK"));
        // The models of a store (format spec 4.5).
        assert!(allowed("models/model2vec-0123456789ab/model.safetensors"));
        assert!(allowed(
            "models/model2vec-0123456789ab/graphersal-embed-model.json"
        ));
        assert!(!allowed("models/model2vec-0123456789ab/.hidden"));
        assert!(!allowed("models/other/model.safetensors"));
        assert!(!allowed("models/model2vec-0123456789ab/a/b"));
    }

    /// A well-formed entry outside this version's layout is a file of a newer version (format
    /// spec 16): refused as unsupported, never damage; a malformed name stays damage.
    #[test]
    fn an_entry_of_a_newer_version_is_unsupported_not_damage() {
        use crate::persist::MemDir;
        use crate::persist::dir::IntoStoreDir;
        let archive = |name: &str| {
            let mut bytes = Vec::new();
            let mut zip = ZipWriter::new(&mut bytes);
            zip.add_bytes(name, b"future").unwrap();
            zip.finish().unwrap();
            bytes
        };
        let dir = DirPath::root(MemDir::new().into_store_dir());
        let err = restore(archive("CATALOG").as_slice(), &dir).unwrap_err();
        assert!(
            matches!(err, PersistError::UnsupportedContent { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("newer Graphersal"), "{err}");
        let dir = DirPath::root(MemDir::new().into_store_dir());
        let err = restore(archive("../GRAPH").as_slice(), &dir).unwrap_err();
        assert!(matches!(err, PersistError::Corrupt { .. }), "{err}");
    }
}
