//! The model tar (format spec 4.5 "Embedding models"): ONE blob holding every file of an
//! embedding model, a deterministic POSIX ustar archive. Same files, same bytes, same blob hash.
//!
//! - One regular-file entry (`typeflag '0'`) per file, entries sorted by name bytewise, `mode
//!   0000644`, `uid`/`gid` 0, `uname`/`gname` empty, `mtime` 0, `devmajor`/`devminor` 0, `prefix`
//!   empty; the data padded to 512 bytes; the end is two zero blocks and nothing after them. No
//!   directories, links, pax or GNU headers.
//! - The files: the model's own (`config*.json`, `tokenizer.json`, `model.safetensors`), the card
//!   `README.md`, licence and notice files, and the manifest [`MODEL_MANIFEST`] (format 3:
//!   compact JSON, keys in a fixed order, no time stamps), which lists every other file with its
//!   role, size and SHA-256.
//!
//! The reader works on the blob in memory (`&[u8]`) and returns the files as slices of it (no
//! copy, never extracting to disk). A blob is hostile input (it may come from a foreign store):
//! every header checksum is checked, sizes are bounded by the remaining bytes, names are
//! validated, an unknown entry type, a duplicate or an unsorted name, more than
//! [`MAX_MODEL_FILES`] files and a manifest that does not match the entries are refused. Nothing
//! here panics. No dependency; builds for wasm32.

use std::borrow::Cow;
use std::io::{self, Read};

use crate::value::physical::{self, PhysicalValue};
use crate::value::vector::embed::identity::{FAMILY, IDENTITY_ROLES, identity_digest};
use crate::value::vector::embed::sha256::{Sha256, hex};

/// The manifest's file name inside a model tar.
pub(crate) const MODEL_MANIFEST: &str = "graphersal-model.json";
/// The manifest format this build writes and reads.
pub(crate) const MANIFEST_FORMAT: i64 = 3;
/// The most files of one model besides its manifest.
pub(crate) const MAX_MODEL_FILES: usize = 64;
/// The longest file name.
pub(crate) const MAX_FILE_NAME_BYTES: usize = 40;
/// The largest file: what eleven octal digits of a ustar size field hold (8 GiB - 1).
pub(crate) const MAX_MODEL_FILE_BYTES: u64 = (8 << 30) - 1;
/// The largest manifest.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;
/// The ustar block size.
const BLOCK: usize = 512;
/// The JSON nesting a manifest may use.
const MANIFEST_DEPTH: usize = 6;
/// The most base models a manifest keeps.
pub(crate) const MAX_BASE_MODELS: usize = 16;

/// Whether `name` may name a file of a model (1 to [`MAX_FILE_NAME_BYTES`] bytes of ASCII
/// letters, digits, `.`, `_` and `-`, not starting with `.`, not the manifest's name).
pub(crate) fn is_model_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_BYTES
        && !name.starts_with('.')
        && name != MODEL_MANIFEST
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// One file the manifest lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestFile {
    pub(crate) name: String,
    /// `config`, `tokenizer`, `model`, `card`, `license`, `notice` (a role a newer version
    /// wrote is kept as text).
    pub(crate) role: String,
    pub(crate) size: u64,
    /// Lowercase hex.
    pub(crate) sha256: String,
}

/// A base model the manifest names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestBase {
    pub(crate) repo: String,
    pub(crate) license: Option<String>,
}

/// The manifest of a model tar (format 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelManifest {
    pub(crate) family: String,
    pub(crate) identity: String,
    pub(crate) repo: String,
    pub(crate) revision: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) license: String,
    pub(crate) license_text: String,
    pub(crate) base_models: Vec<ManifestBase>,
    pub(crate) gated: bool,
    /// Every other file of the tar, sorted by name.
    pub(crate) files: Vec<ManifestFile>,
}

impl ModelManifest {
    /// The manifest's bytes: compact JSON, keys in the order `format`, `family`, `identity`,
    /// `repo`, `revision`, `commit`, `license`, `license_text`, `base_models`, `gated`, `files`
    /// (absent optional keys left out), so the same model gives the same bytes.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let text = |s: &str| PhysicalValue::String(Cow::Owned(s.to_string()));
        let mut fields: Vec<(Cow<'static, str>, PhysicalValue<'static>)> = vec![
            ("format".into(), PhysicalValue::Int(MANIFEST_FORMAT)),
            ("family".into(), text(&self.family)),
            ("identity".into(), text(&self.identity)),
            ("repo".into(), text(&self.repo)),
        ];
        if let Some(revision) = &self.revision {
            fields.push(("revision".into(), text(revision)));
        }
        if let Some(commit) = &self.commit {
            fields.push(("commit".into(), text(commit)));
        }
        fields.push(("license".into(), text(&self.license)));
        fields.push(("license_text".into(), text(&self.license_text)));
        fields.push((
            "base_models".into(),
            PhysicalValue::Array(
                self.base_models
                    .iter()
                    .map(|base| {
                        let mut entry = vec![(Cow::Borrowed("repo"), text(&base.repo))];
                        if let Some(license) = &base.license {
                            entry.push((Cow::Borrowed("license"), text(license)));
                        }
                        PhysicalValue::Object(entry)
                    })
                    .collect(),
            ),
        ));
        fields.push(("gated".into(), PhysicalValue::Bool(self.gated)));
        fields.push((
            "files".into(),
            PhysicalValue::Array(
                self.files
                    .iter()
                    .map(|file| {
                        PhysicalValue::Object(vec![
                            (Cow::Borrowed("name"), text(&file.name)),
                            (Cow::Borrowed("role"), text(&file.role)),
                            (
                                Cow::Borrowed("size"),
                                PhysicalValue::Int(i64::try_from(file.size).unwrap_or(i64::MAX)),
                            ),
                            (Cow::Borrowed("sha256"), text(&file.sha256)),
                        ])
                    })
                    .collect(),
            ),
        ));
        let mut out = String::new();
        // Never fails: no float in a manifest.
        let _ = physical::write_json(&PhysicalValue::Object(fields), &mut out);
        out.into_bytes()
    }

    /// Reads a manifest. A format newer than [`MANIFEST_FORMAT`] is refused (the definition says
    /// whether the model is served; this reader only reads its own format and older).
    pub(crate) fn parse(bytes: &[u8]) -> Result<ModelManifest, String> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| "the manifest is not UTF-8".to_string())?;
        let value = physical::read_json(text, MANIFEST_DEPTH)
            .map_err(|err| format!("the manifest is not JSON: {err}"))?;
        let PhysicalValue::Object(fields) = value else {
            return Err("the manifest is not a JSON object".into());
        };
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let string = |key: &str| match get(key) {
            Some(PhysicalValue::String(s)) => Some(s.to_string()),
            _ => None,
        };
        let format = match get("format") {
            Some(PhysicalValue::Int(n)) => *n,
            _ => return Err("the manifest has no format".into()),
        };
        if format > MANIFEST_FORMAT {
            return Err(format!(
                "the manifest has the format {format}, written by a newer Graphersal"
            ));
        }
        let mut manifest = ModelManifest {
            family: string("family").ok_or("the manifest has no family")?,
            identity: string("identity").ok_or("the manifest has no identity")?,
            repo: string("repo").ok_or("the manifest has no repo")?,
            revision: string("revision"),
            commit: string("commit"),
            license: string("license").unwrap_or_else(|| "unknown".into()),
            license_text: string("license_text").unwrap_or_else(|| "none".into()),
            base_models: Vec::new(),
            gated: matches!(get("gated"), Some(PhysicalValue::Bool(true))),
            files: Vec::new(),
        };
        if let Some(PhysicalValue::Array(bases)) = get("base_models") {
            for base in bases.iter().take(MAX_BASE_MODELS) {
                if let PhysicalValue::Object(entry) = base {
                    let field = |key: &str| {
                        entry.iter().find_map(|(k, v)| match v {
                            PhysicalValue::String(s) if k == key => Some(s.to_string()),
                            _ => None,
                        })
                    };
                    if let Some(repo) = field("repo") {
                        manifest.base_models.push(ManifestBase {
                            repo,
                            license: field("license"),
                        });
                    }
                }
            }
        }
        let Some(PhysicalValue::Array(files)) = get("files") else {
            return Err("the manifest has no \"files\" list".into());
        };
        if files.len() > MAX_MODEL_FILES {
            return Err(format!(
                "the manifest lists {} files (at most {MAX_MODEL_FILES})",
                files.len()
            ));
        }
        for file in files {
            let PhysicalValue::Object(entry) = file else {
                return Err("a file entry is not an object".into());
            };
            let field = |key: &str| entry.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            let name = match field("name") {
                Some(PhysicalValue::String(s)) if is_model_file_name(s) => s.to_string(),
                _ => return Err("a file entry has no valid name".into()),
            };
            let role = match field("role") {
                Some(PhysicalValue::String(s)) => s.to_string(),
                _ => return Err(format!("the file {name:?} has no role")),
            };
            let size = match field("size") {
                Some(PhysicalValue::Int(n)) => u64::try_from(*n)
                    .ok()
                    .filter(|n| *n <= MAX_MODEL_FILE_BYTES)
                    .ok_or_else(|| format!("the size of {name:?} is not a valid size"))?,
                _ => return Err(format!("the file {name:?} has no size")),
            };
            let sha256 = match field("sha256") {
                Some(PhysicalValue::String(s))
                    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    s.to_ascii_lowercase()
                }
                _ => return Err(format!("the file {name:?} has no SHA-256")),
            };
            manifest.files.push(ManifestFile {
                name,
                role,
                size,
                sha256,
            });
        }
        Ok(manifest)
    }

    /// The file of `role` when exactly one has it.
    pub(crate) fn file_of(&self, role: &str) -> Option<&ManifestFile> {
        let mut matching = self.files.iter().filter(|f| f.role == role);
        match (matching.next(), matching.next()) {
            (Some(file), None) => Some(file),
            _ => None,
        }
    }

    /// The full identity digest of the three model files the manifest lists (`None`: it does
    /// not list exactly one of each role).
    pub(crate) fn digest(&self) -> Option<String> {
        let [config, tokenizer, weights] = IDENTITY_ROLES.map(|role| self.file_of(role));
        let (config, tokenizer, weights) = (config?, tokenizer?, weights?);
        Some(identity_digest([
            (config.size, &config.sha256),
            (tokenizer.size, &tokenizer.sha256),
            (weights.size, &weights.sha256),
        ]))
    }

    /// Whether this is a model of the family this version runs.
    pub(crate) fn is_known_family(&self) -> bool {
        self.family == FAMILY
    }
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// The 512-byte ustar header of the regular file `name` of `size` bytes.
pub(crate) fn header(name: &str, size: u64) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    let name = name.as_bytes();
    let n = name.len().min(100);
    block[..n].copy_from_slice(&name[..n]);
    block[100..108].copy_from_slice(b"0000644\0");
    block[108..116].copy_from_slice(b"0000000\0");
    block[116..124].copy_from_slice(b"0000000\0");
    let size_text = format!("{:011o}\0", size.min(MAX_MODEL_FILE_BYTES));
    block[124..136].copy_from_slice(size_text.as_bytes());
    block[136..148].copy_from_slice(b"00000000000\0");
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[329..337].copy_from_slice(b"0000000\0");
    block[337..345].copy_from_slice(b"0000000\0");
    let sum = checksum(&block);
    let sum_text = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(sum_text.as_bytes());
    block
}

/// The ustar checksum: the byte sum with the checksum field read as eight spaces.
fn checksum(block: &[u8; BLOCK]) -> u64 {
    block
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                32
            } else {
                *b as u64
            }
        })
        .sum()
}

/// The zero bytes that pad `size` bytes of data to a whole block.
fn padding(size: u64) -> usize {
    ((BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64) as usize
}

/// The length of the tar of files of these sizes (headers, data, padding, end).
pub(crate) fn tar_length(sizes: impl IntoIterator<Item = u64>) -> u64 {
    let mut total = 2 * BLOCK as u64;
    for size in sizes {
        total = total
            .saturating_add(BLOCK as u64)
            .saturating_add(size)
            .saturating_add(padding(size) as u64);
    }
    total
}

/// Where the bytes of one file of a tar being written come from.
pub(crate) enum TarContent {
    /// Bytes in memory.
    Bytes(Vec<u8>),
    /// A file of the local file system, read again while the tar is written (its size and
    /// SHA-256 were taken in a first pass and are checked while it streams).
    #[cfg(not(target_arch = "wasm32"))]
    Path(std::path::PathBuf),
}

/// One file of a tar being written: its name, size, SHA-256 (hex) and content.
pub(crate) struct TarFile {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) content: TarContent,
}

/// The tar of `files` as a stream: what `Store::write_blob` reads. Files are written in name
/// order; each one's bytes are hashed while they stream and compared with the SHA-256 the
/// manifest states (a file that changed since the first pass fails the stream).
pub(crate) struct TarStream {
    files: std::vec::IntoIter<TarFile>,
    /// Header, padding or end bytes still to hand out.
    pending: Vec<u8>,
    at: usize,
    data: Option<DataPart>,
    /// After the last file: the end blocks were queued.
    ended: bool,
}

struct DataPart {
    name: String,
    reader: Box<dyn Read + Send>,
    remaining: u64,
    hash: Sha256,
    expected: String,
    size: u64,
}

impl TarStream {
    /// The stream of `files` (sorted here by name bytewise).
    pub(crate) fn new(mut files: Vec<TarFile>) -> Self {
        files.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        TarStream {
            files: files.into_iter(),
            pending: Vec::new(),
            at: 0,
            data: None,
            ended: false,
        }
    }

    /// Queues the next part: the next file's header (and opens its data), or the end blocks.
    fn advance(&mut self) -> io::Result<bool> {
        if let Some(file) = self.files.next() {
            self.pending = header(&file.name, file.size).to_vec();
            self.at = 0;
            let reader: Box<dyn Read + Send> = match file.content {
                TarContent::Bytes(bytes) => Box::new(io::Cursor::new(bytes)),
                #[cfg(not(target_arch = "wasm32"))]
                TarContent::Path(path) => Box::new(std::fs::File::open(&path).map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!("cannot read '{}': {err}", path.display()),
                    )
                })?),
            };
            self.data = Some(DataPart {
                name: file.name,
                reader,
                remaining: file.size,
                hash: Sha256::new(),
                expected: file.sha256,
                size: file.size,
            });
            return Ok(true);
        }
        if self.ended {
            return Ok(false);
        }
        self.ended = true;
        self.pending = vec![0u8; 2 * BLOCK];
        self.at = 0;
        Ok(true)
    }
}

impl Read for TarStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.at < self.pending.len() {
                let n = (self.pending.len() - self.at).min(buf.len());
                buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
                self.at += n;
                return Ok(n);
            }
            if let Some(data) = &mut self.data {
                if data.remaining > 0 {
                    let want = buf
                        .len()
                        .min(usize::try_from(data.remaining).unwrap_or(usize::MAX));
                    let n = data.reader.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!(
                                "the model file {:?} became shorter while it was read",
                                data.name
                            ),
                        ));
                    }
                    data.hash.update(&buf[..n]);
                    data.remaining -= n as u64;
                    return Ok(n);
                }
                // The file's data is complete: check it, then pad.
                let Some(done) = self.data.take() else {
                    continue;
                };
                let got = hex(&done.hash.finish());
                if got != done.expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("the model file {:?} changed while it was read", done.name),
                    ));
                }
                self.pending = vec![0u8; padding(done.size)];
                self.at = 0;
                continue;
            }
            if !self.advance()? {
                return Ok(0);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// A model tar read from a blob in memory: its manifest and its files as slices of the blob.
#[derive(Debug)]
pub(crate) struct ModelTar<'a> {
    pub(crate) manifest: ModelManifest,
    files: Vec<(&'a str, &'a [u8])>,
}

impl<'a> ModelTar<'a> {
    /// The bytes of the file `name`.
    pub(crate) fn file(&self, name: &str) -> Option<&'a [u8]> {
        self.files
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, data)| *data)
    }

    /// The bytes of the file of `role` (exactly one in the manifest).
    pub(crate) fn file_of(&self, role: &str) -> Option<&'a [u8]> {
        self.manifest
            .file_of(role)
            .and_then(|file| self.file(&file.name))
    }

    /// The files in tar order (the manifest included).
    pub(crate) fn files(&self) -> impl Iterator<Item = (&'a str, &'a [u8])> + '_ {
        self.files.iter().copied()
    }
}

/// Parses an octal field (digits, then NULs or spaces).
fn octal(field: &[u8]) -> Option<u64> {
    let digits_end = field
        .iter()
        .position(|b| *b == 0 || *b == b' ')
        .unwrap_or(field.len());
    let (digits, rest) = field.split_at(digits_end);
    if digits.is_empty() || !rest.iter().all(|b| *b == 0 || *b == b' ') {
        return None;
    }
    let mut value: u64 = 0;
    for b in digits {
        if !(b'0'..=b'7').contains(b) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(value)
}

/// Reads the model tar `bytes` (see the module docs for every check). The error says what is
/// wrong; a caller treats it as content damage of the model's blob.
pub(crate) fn read_tar(bytes: &[u8]) -> Result<ModelTar<'_>, String> {
    let mut at = 0usize;
    let mut files: Vec<(&str, &[u8])> = Vec::new();
    loop {
        let Some(block) = bytes.get(at..at + BLOCK) else {
            return Err(format!(
                "the archive ends at byte {} inside a header (no end blocks)",
                bytes.len()
            ));
        };
        let block: &[u8; BLOCK] = block
            .try_into()
            .map_err(|_| "a header block is short".to_string())?;
        if block.iter().all(|b| *b == 0) {
            // The end: a second zero block, then nothing but zeros.
            let rest = &bytes[at + BLOCK..];
            if rest.len() < BLOCK || !rest.iter().all(|b| *b == 0) {
                return Err("the archive's end is not two zero blocks".into());
            }
            break;
        }
        if octal(&block[148..156]) != Some(checksum(block)) {
            return Err(format!("the header at byte {at} has a wrong checksum"));
        }
        if &block[257..263] != b"ustar\0" || &block[263..265] != b"00" {
            return Err(format!("the header at byte {at} is not a ustar header"));
        }
        if block[156] != b'0' {
            return Err(format!(
                "the entry at byte {at} is not a regular file (type {:?})",
                block[156] as char
            ));
        }
        if block[345] != 0 || block[157] != 0 {
            return Err(format!(
                "the entry at byte {at} has a prefix or a link name"
            ));
        }
        let name_end = block[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&block[..name_end])
            .map_err(|_| format!("the entry name at byte {at} is not UTF-8"))?;
        if !(is_model_file_name(name) || name == MODEL_MANIFEST) {
            return Err(format!("the entry name {name:?} is not allowed"));
        }
        if let Some((last, _)) = files.last()
            && last.as_bytes() >= name.as_bytes()
        {
            return Err(format!(
                "the entry {name:?} is a duplicate or out of order (after {last:?})"
            ));
        }
        if files.len() > MAX_MODEL_FILES {
            return Err(format!(
                "the archive holds more than {MAX_MODEL_FILES} files and a manifest"
            ));
        }
        let size = octal(&block[124..136])
            .filter(|size| *size <= MAX_MODEL_FILE_BYTES)
            .ok_or_else(|| format!("the size of {name:?} is not valid"))?;
        let start = at + BLOCK;
        let end = usize::try_from(size)
            .ok()
            .and_then(|size| start.checked_add(size))
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| format!("the file {name:?} reaches past the end of the archive"))?;
        files.push((name, &bytes[start..end]));
        at = end
            .checked_add(padding(size))
            .filter(|next| *next <= bytes.len())
            .ok_or_else(|| format!("the padding of {name:?} reaches past the end"))?;
    }
    let manifest_bytes = files
        .iter()
        .find(|(name, _)| *name == MODEL_MANIFEST)
        .map(|(_, data)| *data)
        .ok_or("the archive holds no manifest")?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("the manifest is too large".into());
    }
    let manifest = ModelManifest::parse(manifest_bytes)?;
    let others: Vec<(&str, u64)> = files
        .iter()
        .filter(|(name, _)| *name != MODEL_MANIFEST)
        .map(|(name, data)| (*name, data.len() as u64))
        .collect();
    let listed: Vec<(&str, u64)> = manifest
        .files
        .iter()
        .map(|f| (f.name.as_str(), f.size))
        .collect();
    let mut listed_sorted = listed.clone();
    listed_sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if listed_sorted != others {
        return Err("the manifest does not list exactly the files of the archive".into());
    }
    Ok(ModelTar { manifest, files })
}

/// The SHA-256 (hex) of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hex(&hash.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(files: &[(&str, &str, &[u8])]) -> ModelManifest {
        let mut listed: Vec<ManifestFile> = files
            .iter()
            .map(|(name, role, data)| ManifestFile {
                name: name.to_string(),
                role: role.to_string(),
                size: data.len() as u64,
                sha256: sha256_hex(data),
            })
            .collect();
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        ModelManifest {
            family: FAMILY.into(),
            identity: "model2vec/o/n@000000000000".into(),
            repo: "o/n".into(),
            revision: Some("main".into()),
            commit: None,
            license: "mit".into(),
            license_text: "none".into(),
            base_models: vec![ManifestBase {
                repo: "b/c".into(),
                license: Some("apache-2.0".into()),
            }],
            gated: false,
            files: listed,
        }
    }

    fn tar_of(files: &[(&str, &str, &[u8])]) -> Vec<u8> {
        let manifest = manifest(files);
        let mut entries: Vec<TarFile> = files
            .iter()
            .map(|(name, _, data)| TarFile {
                name: name.to_string(),
                size: data.len() as u64,
                sha256: sha256_hex(data),
                content: TarContent::Bytes(data.to_vec()),
            })
            .collect();
        let bytes = manifest.to_bytes();
        entries.push(TarFile {
            name: MODEL_MANIFEST.into(),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            content: TarContent::Bytes(bytes),
        });
        let mut out = Vec::new();
        TarStream::new(entries).read_to_end(&mut out).unwrap();
        out
    }

    const FILES: [(&str, &str, &[u8]); 3] = [
        ("tokenizer.json", "tokenizer", b"{\"t\":1}"),
        ("config.json", "config", b"{}"),
        ("model.safetensors", "model", &[7u8; 700]),
    ];

    #[test]
    fn a_tar_round_trips_and_is_deterministic() {
        let one = tar_of(&FILES);
        let mut reversed = FILES;
        reversed.reverse();
        assert_eq!(one, tar_of(&reversed));
        assert_eq!(
            one.len() as u64,
            tar_length(
                FILES
                    .iter()
                    .map(|f| f.2.len() as u64)
                    .chain([manifest(&FILES).to_bytes().len() as u64])
            )
        );
        let tar = read_tar(&one).unwrap();
        assert_eq!(tar.file("config.json"), Some(&b"{}"[..]));
        assert_eq!(tar.file_of("model"), Some(&[7u8; 700][..]));
        assert_eq!(tar.manifest, manifest(&FILES));
        let names: Vec<&str> = tar.files().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                "config.json",
                "graphersal-model.json",
                "model.safetensors",
                "tokenizer.json"
            ]
        );
    }

    #[test]
    fn every_flipped_byte_is_refused_or_read_without_a_panic() {
        let good = tar_of(&FILES);
        for at in 0..good.len() {
            let mut bad = good.clone();
            bad[at] ^= 0x41;
            let _ = read_tar(&bad);
        }
        for cut in 0..good.len() {
            assert!(read_tar(&good[..cut]).is_err(), "cut at {cut}");
        }
    }

    /// A raw tar of `entries` (name, type flag, data) with valid checksums and end blocks.
    fn raw_tar(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, flag, data) in entries {
            let mut block = header(name, data.len() as u64);
            block[156] = *flag;
            block[148..156].copy_from_slice(b"        ");
            let sum = format!("{:06o}\0 ", checksum(&block));
            block[148..156].copy_from_slice(sum.as_bytes());
            out.extend_from_slice(&block);
            out.extend_from_slice(data);
            out.extend(std::iter::repeat_n(0u8, padding(data.len() as u64)));
        }
        out.extend_from_slice(&[0u8; 2 * BLOCK]);
        out
    }

    #[test]
    fn hostile_tars_are_refused() {
        let manifest = manifest(&FILES[..1]).to_bytes();
        let ok = raw_tar(&[
            (FILES[0].0, b'0', FILES[0].2),
            (MODEL_MANIFEST, b'0', &manifest),
        ]);
        // The tar sorted wrongly: the manifest before the config would be unsorted.
        assert!(
            read_tar(&ok).is_err(),
            "a manifest that lists another order"
        );
        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "duplicate",
                raw_tar(&[("a.txt", b'0', b"1"), ("a.txt", b'0', b"2")]),
            ),
            (
                "unsorted",
                raw_tar(&[("b.txt", b'0', b"1"), ("a.txt", b'0', b"2")]),
            ),
            ("link", raw_tar(&[("a.txt", b'2', b"")])),
            ("directory", raw_tar(&[("a", b'5', b"")])),
            ("pax", raw_tar(&[("a.txt", b'x', b"")])),
            ("long name", raw_tar(&[(&"a".repeat(41), b'0', b"1")])),
            ("path", raw_tar(&[("a/b", b'0', b"1")])),
            ("no manifest", raw_tar(&[("a.txt", b'0', b"1")])),
            (
                "no end",
                raw_tar(&[("a.txt", b'0', b"1")])[..BLOCK * 2].to_vec(),
            ),
            ("junk after the end", {
                let mut t = raw_tar(&[("a.txt", b'0', b"1")]);
                t.push(1);
                t
            }),
        ];
        for (what, tar) in cases {
            assert!(read_tar(&tar).is_err(), "{what}");
        }
        // 66 entries: more than 64 files and a manifest.
        let names: Vec<String> = (0..66).map(|i| format!("f{i:03}")).collect();
        let entries: Vec<(&str, u8, &[u8])> = names
            .iter()
            .map(|n| (n.as_str(), b'0', &b"x"[..]))
            .collect();
        let err = read_tar(&raw_tar(&entries)).unwrap_err();
        assert!(err.contains("more than"), "{err}");
        // A size beyond the archive.
        let mut big = raw_tar(&[("a.txt", b'0', b"1")]);
        let mut block: [u8; BLOCK] = big[..BLOCK].try_into().unwrap();
        block[124..136].copy_from_slice(b"77777777777\0");
        block[148..156].copy_from_slice(b"        ");
        let sum = format!("{:06o}\0 ", checksum(&block));
        block[148..156].copy_from_slice(sum.as_bytes());
        big[..BLOCK].copy_from_slice(&block);
        assert!(read_tar(&big).unwrap_err().contains("past the end"));
    }

    #[test]
    fn every_header_field_is_fixed() {
        let block = header("config.json", 5);
        assert_eq!(&block[100..108], b"0000644\0");
        assert_eq!(&block[108..124], b"0000000\00000000\0");
        assert_eq!(&block[124..136], b"00000000005\0");
        assert_eq!(&block[136..148], b"00000000000\0");
        assert_eq!(block[156], b'0');
        assert_eq!(&block[257..265], b"ustar\000");
        assert!(block[265..329].iter().all(|b| *b == 0), "uname/gname empty");
        assert!(block[345..].iter().all(|b| *b == 0), "prefix empty");
        assert_eq!(octal(&block[148..156]), Some(checksum(&block)));
    }

    #[test]
    fn a_header_field_change_is_caught_by_the_checksum() {
        let mut bad = tar_of(&FILES);
        bad[100] = b'1';
        assert!(read_tar(&bad).unwrap_err().contains("checksum"));
    }
}
