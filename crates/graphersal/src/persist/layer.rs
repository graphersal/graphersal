//! The layer file of a layer snapshot (format spec 7.5, segment kind 128): the identity of the
//! base the layer was built over ([`BaseRef`]) and the layer's changes to that base
//! ([`BasePatch`] entries, by base element id). The layer's own elements are ordinary vertex
//! and edge segments beside it; the schema, the catalog, the lineage id, the position and the
//! auto-id sequences are in the manifest as in every snapshot.
//!
//! The kind is critical (128..=255): a reader that does not know it never takes the layer's
//! elements for a whole graph (a Store opens read-only for it, format spec 16 rule 3). This
//! version knows it: a snapshot with the row loads only into a layer over the base it names
//! (`snapshot::load_snapshot_with`), and a layer loads only a snapshot with the row.
//!
//! The base's identity is written twice: in the file's header (the record) and in the
//! segment-table row's `first id` / `last id` (the lineage id as canonical UUID text,
//! `<commit seq>@<commit time>`), so a loader refuses a layer over another base from the
//! manifest alone, before it reads a single segment; the two must agree.

use compact_str::CompactString;

use super::codec::{
    self, DecodeError, DecodeResult, InlineKeys, LAYER_FORMAT_VERSION, Reader, put_opt_str,
    put_properties, put_str, put_u8, put_u32, put_u64, put_uvar,
};
use super::error::PersistError;
use super::manifest::SegmentEntry;
use crate::graph::definition::DefinitionKind;
use crate::graph::{BasePatch, BaseRef};

/// Segment kind byte of the layer file (format spec 7.5: critical).
pub(crate) const KIND_LAYER: u8 = 128;

/// The layer file's name in a snapshot (its segment-table row's path).
pub(crate) const LAYER_FILE: &str = "layer.bin";

/// Magic of a layer file.
const LAYER_MAGIC: &[u8; 8] = b"GRSLLAYR";

/// Bytes before the first entry: magic, version, base lineage id, commit seq, commit time,
/// entry count.
const HEADER_BYTES: usize = 8 + 4 + 16 + 8 + 8 + 8;

/// Entry types (format spec 7.5, kind 128).
const REMOVED_EDGE: u8 = 1;
const REMOVED_VERTEX: u8 = 2;
const VERTEX_IMAGE: u8 = 3;
const EDGE_IMAGE: u8 = 4;
const REMOVED_DEFINITION: u8 = 5;

/// Where a decode failure of the layer file is reported.
pub(crate) const LAYER_LOCATION: &str = "layer file";

/// The encoded layer file of a layer over `base` with `patch`.
pub(crate) fn encode(base: &BaseRef, patch: &[BasePatch]) -> Result<Vec<u8>, PersistError> {
    let mut out = Vec::with_capacity(HEADER_BYTES + patch.len() * 16);
    out.extend_from_slice(LAYER_MAGIC);
    put_u32(&mut out, LAYER_FORMAT_VERSION);
    out.extend_from_slice(&base.graph_id().to_be_bytes());
    put_u64(&mut out, base.commit_seq());
    out.extend_from_slice(&base.committed_at().to_le_bytes());
    put_u64(&mut out, patch.len() as u64);
    let fail = |id: &str, reason: String| {
        PersistError::encode(
            "the layer file",
            format!("the image of base element '{id}': {reason}"),
        )
    };
    for entry in patch {
        match entry {
            BasePatch::RemovedEdge { id } => {
                put_u8(&mut out, REMOVED_EDGE);
                put_str(&mut out, id);
            }
            BasePatch::RemovedVertex { id } => {
                put_u8(&mut out, REMOVED_VERTEX);
                put_str(&mut out, id);
            }
            BasePatch::VertexImage {
                id,
                labels,
                properties,
            } => {
                put_u8(&mut out, VERTEX_IMAGE);
                put_str(&mut out, id);
                put_uvar(&mut out, labels.len() as u64);
                for label in labels {
                    put_str(&mut out, label);
                }
                put_properties(&mut out, Some(properties), &mut InlineKeys)
                    .map_err(|reason| fail(id, reason))?;
            }
            BasePatch::EdgeImage {
                id,
                label,
                properties,
            } => {
                put_u8(&mut out, EDGE_IMAGE);
                put_str(&mut out, id);
                put_opt_str(&mut out, label.as_deref());
                match properties {
                    None => put_u8(&mut out, 0),
                    Some(properties) => {
                        put_u8(&mut out, 1);
                        put_properties(&mut out, Some(properties), &mut InlineKeys)
                            .map_err(|reason| fail(id, reason))?;
                    }
                }
            }
            BasePatch::RemovedDefinition { kind, name } => {
                put_u8(&mut out, REMOVED_DEFINITION);
                put_u8(&mut out, kind.code());
                put_str(&mut out, name);
            }
        }
    }
    Ok(out)
}

/// The segment-table row of a layer file `file` over `base` with `entries` patch entries.
pub(crate) fn row(base: &BaseRef, entries: usize, file: &[u8]) -> SegmentEntry {
    SegmentEntry {
        kind: KIND_LAYER,
        path: LAYER_FILE.into(),
        first_id: crate::value::uuid::format_canonical(base.graph_id()),
        last_id: format!("{}@{}", base.commit_seq(), base.committed_at()).into(),
        count: entries as u64,
        byte_len: file.len() as u64,
        crc: codec::crc(file),
    }
}

/// The base's identity a layer row names (`None`: the row's ids are not a base reference).
pub(crate) fn row_base_ref(row: &SegmentEntry) -> Option<BaseRef> {
    let graph_id = crate::value::uuid::parse_canonical(&row.first_id)?;
    let (seq, time) = row.last_id.split_once('@')?;
    Some(BaseRef::new(
        graph_id,
        seq.parse().ok()?,
        time.parse().ok()?,
    ))
}

/// The base's identity named by the layer row of `segments`: `Ok(None)` when there is no
/// layer row; `Err` (damage of the manifest) when there are several or the row's ids do not
/// name a base.
pub(crate) fn layer_row(segments: &[SegmentEntry]) -> Result<Option<BaseRef>, PersistError> {
    let mut rows = segments.iter().filter(|s| s.kind == KIND_LAYER);
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    if rows.next().is_some() {
        return Err(PersistError::corrupt(
            "manifest",
            0,
            "the segment table lists more than one layer file",
        ));
    }
    row_base_ref(row).map(Some).ok_or_else(|| {
        PersistError::corrupt(
            "manifest",
            0,
            format!(
                "the layer file's row does not name a base (first id {:?}, last id {:?})",
                row.first_id, row.last_id
            ),
        )
    })
}

/// A decoded layer file.
#[derive(Debug)]
pub(crate) struct LayerFile {
    pub(crate) base: BaseRef,
    pub(crate) patch: Vec<BasePatch>,
}

/// Decodes a layer file (hostile input: every length and tag checked); `max_depth` bounds the
/// nesting of property values.
pub(crate) fn decode(bytes: &[u8], max_depth: usize) -> DecodeResult<LayerFile> {
    let mut r = Reader::new(bytes);
    if r.bytes(LAYER_MAGIC.len())? != LAYER_MAGIC {
        return Err(DecodeError::new(0, "not a layer file (bad magic)"));
    }
    let version = r.u32()?;
    // Written only in the layer version (format spec 7.5, 15).
    if version > LAYER_FORMAT_VERSION {
        return Err(DecodeError::newer(8, "layer file", version));
    }
    if version != LAYER_FORMAT_VERSION {
        return Err(DecodeError::new(
            8,
            format!("layer file format version {version}"),
        ));
    }
    let mut r = r.with_version(version);
    let base = BaseRef::new(r.u128_be()?, r.u64()?, r.i64()?);
    let count_at = r.offset();
    let count = r.u64()?;
    // Each entry takes at least two bytes (its type and an id length).
    if count > (r.remaining() / 2) as u64 {
        return Err(DecodeError::new(
            count_at,
            "entry count exceeds the file size",
        ));
    }
    let mut patch = Vec::with_capacity(count as usize);
    let keys = InlineKeys;
    for _ in 0..count {
        let at = r.offset();
        let entry = match r.u8()? {
            REMOVED_EDGE => BasePatch::removed_edge(r.str()?),
            REMOVED_VERTEX => BasePatch::removed_vertex(r.str()?),
            VERTEX_IMAGE => {
                let id = CompactString::from(r.str()?);
                let label_count = r.count(1)?;
                let mut labels = Vec::with_capacity(label_count);
                for _ in 0..label_count {
                    labels.push(CompactString::from(r.str()?));
                }
                let properties = codec::read_properties(&mut r, &keys, max_depth)?;
                BasePatch::vertex_image(id, labels, properties)
            }
            EDGE_IMAGE => {
                let id = CompactString::from(r.str()?);
                let label = r.opt_str()?.map(CompactString::from);
                let properties = match r.u8()? {
                    0 => None,
                    1 => Some(codec::read_properties(&mut r, &keys, max_depth)?),
                    other => {
                        return Err(DecodeError::new(
                            r.offset() - 1,
                            format!("edge image map marker {other}"),
                        ));
                    }
                };
                BasePatch::edge_image(id, label, properties)
            }
            REMOVED_DEFINITION => {
                let kind = DefinitionKind::from_code(r.u8()?);
                BasePatch::removed_definition(kind, r.str()?)
            }
            other => {
                return Err(DecodeError::new(at, format!("layer entry type {other}")));
            }
        };
        patch.push(entry);
    }
    if !r.is_empty() {
        return Err(DecodeError::new(
            r.offset(),
            "trailing bytes in the layer file",
        ));
    }
    Ok(LayerFile { base, patch })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ElementProperty, PropertyMap};

    fn sample() -> (BaseRef, Vec<BasePatch>) {
        let mut map = PropertyMap::default();
        map.insert("name", ElementProperty::from("ann"));
        map.insert("age", ElementProperty::from(31));
        let patch = vec![
            BasePatch::removed_edge("e1"),
            BasePatch::removed_vertex("v1"),
            BasePatch::vertex_image("v2", vec!["person".into(), "admin".into()], map.clone()),
            BasePatch::edge_image("e2", None, None),
            BasePatch::edge_image("e3", Some("met".into()), Some(PropertyMap::default())),
            BasePatch::edge_image("e4", Some("knows".into()), Some(map)),
            BasePatch::removed_definition(DefinitionKind::QUERY, "q1"),
        ];
        (BaseRef::new(0xabcdef, 42, 1_700_000_000_000_000), patch)
    }

    #[test]
    fn a_layer_file_round_trips_and_its_row_names_the_base() {
        let (base, patch) = sample();
        let bytes = encode(&base, &patch).unwrap();
        let file = decode(&bytes, 64).unwrap();
        assert_eq!(file.base, base);
        assert_eq!(file.patch, patch);
        let row = row(&base, patch.len(), &bytes);
        assert_eq!(row.kind, KIND_LAYER);
        // In the critical range (a reader that does not know it treats it as critical), and
        // known to this version.
        assert!(row.kind >= 128);
        assert!(!super::super::manifest::is_critical_segment_kind(row.kind));
        assert_eq!(layer_row(&[row.clone()]).unwrap(), Some(base));
        assert!(layer_row(&[row.clone(), row]).is_err());
    }

    #[test]
    fn a_damaged_or_newer_layer_file_is_refused() {
        let (base, patch) = sample();
        let bytes = encode(&base, &patch).unwrap();
        for cut in [0, 7, HEADER_BYTES - 1, bytes.len() - 1] {
            assert!(decode(&bytes[..cut], 64).is_err(), "cut at {cut}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode(&trailing, 64).is_err());
        let mut newer = bytes.clone();
        newer[8..12].copy_from_slice(&(LAYER_FORMAT_VERSION + 1).to_le_bytes());
        let err = decode(&newer, 64).unwrap_err();
        assert!(err.at(LAYER_LOCATION, 0).to_string().contains("newer"));
        let mut bad_type = bytes;
        bad_type[HEADER_BYTES] = 99;
        assert!(
            decode(&bad_type, 64)
                .unwrap_err()
                .reason
                .contains("entry type 99")
        );
    }

    /// A packed layer snapshot of a small layer (a removed base vertex, an image, an added
    /// vertex) and its base.
    fn layer_snapshot() -> (std::sync::Arc<crate::graph::TraversalGraph>, Vec<u8>) {
        use crate::graph::{GraphStorage, LayeredGraph, TraversalGraph};
        let base = std::sync::Arc::new(TraversalGraph::tinkerpop_modern());
        let mut layer = LayeredGraph::over(std::sync::Arc::clone(&base)).unwrap();
        let mark = layer.begin_unit().unwrap();
        let one = layer.get_vertex_by_id("1").unwrap().unwrap();
        layer.drop_vertex(one).unwrap();
        let two = layer.get_vertex_by_id("2").unwrap().unwrap();
        layer
            .set_vertex_property(two, "age".into(), ElementProperty::from(28))
            .unwrap();
        layer
            .insert_vertex(
                Some("new".into()),
                vec!["person".into()],
                PropertyMap::default(),
            )
            .unwrap();
        layer.commit_unit(mark).unwrap();
        let mut bytes = Vec::new();
        crate::persist::write_snapshot(&layer, &mut bytes, &Default::default()).unwrap();
        (base, bytes)
    }

    /// `packed` with the kind of its layer row replaced by `kind` (the manifest re-encoded,
    /// every section copied): what a reader that does not know kind 128 sees, given as a
    /// critical kind this version does not know either.
    fn with_layer_kind(packed: &[u8], kind: u8) -> Vec<u8> {
        use super::super::snapshot::{
            PACKED_MAGIC, PackedReader, SECTION_MANIFEST, SECTION_SCHEMA, extra_file_prefix,
            section_kind,
        };
        let mut reader = PackedReader::open(packed).unwrap();
        let mut files = Vec::new();
        for _ in 0..reader.manifest.segments.len() {
            let mut bytes = Vec::new();
            reader.copy_segment(&mut bytes).unwrap();
            files.push(bytes);
        }
        let mut manifest = reader.manifest.clone();
        let schema = reader.schema.clone();
        reader.finish().unwrap();
        for row in &mut manifest.segments {
            if row.kind == KIND_LAYER {
                row.kind = kind;
            }
        }
        let mut out = PACKED_MAGIC.to_vec();
        out.extend_from_slice(&codec::FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(2 + manifest.segments.len() as u32).to_le_bytes());
        let mut section = |kind: u8, bytes: &[u8]| {
            out.push(kind);
            out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(bytes);
        };
        section(SECTION_MANIFEST, &manifest.encode());
        section(SECTION_SCHEMA, &schema);
        for (row, file) in manifest.segments.iter().zip(&files) {
            if row.kind == super::super::segment::KIND_VERTEX
                || row.kind == super::super::segment::KIND_EDGE
            {
                section(section_kind(row.kind), file);
            } else {
                let mut named = extra_file_prefix(&row.path);
                named.extend_from_slice(file);
                section(section_kind(row.kind), &named);
            }
        }
        let trailer = codec::crc(&out);
        out.extend_from_slice(&trailer.to_le_bytes());
        out
    }

    /// The layer row is a critical kind: a reader that does not know it (simulated with a
    /// critical kind this version does not know, the path such a reader takes) opens a Store
    /// of it read-only and never writes around it (format spec 7.5, 16 rule 3); this version
    /// knows kind 128 and refuses to load the layer as a whole graph.
    #[test]
    fn a_reader_that_does_not_know_the_layer_kind_never_writes_the_layer_as_a_graph() {
        use crate::persist::{PersistError, ReadOptions, Store, StoreOptions, read_snapshot};
        let (base, packed) = layer_snapshot();
        let manifest = super::super::snapshot::PackedReader::open(packed.as_slice())
            .unwrap()
            .manifest;
        let row = manifest
            .segments
            .iter()
            .find(|s| s.kind == KIND_LAYER)
            .unwrap();
        // How a reader that does not know the kind judges it (format spec 7.5).
        assert!(row.kind >= 128 && row.kind != super::super::segment::KIND_VERTEX);
        assert_eq!(
            row_base_ref(row),
            Some(BaseRef::of(&base)),
            "the row names the base"
        );
        // This version: refused as a whole graph.
        assert!(matches!(
            read_snapshot(packed.as_slice(), &ReadOptions::new()),
            Err(PersistError::SnapshotKindMismatch { .. })
        ));
        // A whole-graph Store of it: refused as a layer (S3.3: a layer store from a `.gsnap` is
        // not supported yet), never damage, nothing left behind.
        let target = crate::persist::MemDir::new();
        assert!(matches!(
            Store::create_from_packed(&target, packed.as_slice(), StoreOptions::new()),
            Err(PersistError::Store {
                cause: crate::persist::StoreFailure::LayerStore,
                ..
            })
        ));
        assert!(target.file_names().is_empty());
        // A reader for which the row is an unknown critical kind: read-only, nothing written.
        let unknown = with_layer_kind(&packed, 200);
        let store = Store::create_from_packed(
            crate::persist::MemDir::new(),
            unknown.as_slice(),
            StoreOptions::new(),
        )
        .unwrap();
        let reason = store.read_only_reason().unwrap().unwrap();
        assert!(reason.contains("critical segment kind 200"), "{reason}");
        assert!(matches!(
            store.checkpoint(None),
            Err(PersistError::ReadOnly { .. })
        ));
    }

    /// The version gate of releases up to 0.6 (they read file format versions 1 to 4: the
    /// packed header, every manifest of a store's header walk, `check_version`) refuses a layer
    /// snapshot as written by a newer version, in its packed form and as a snapshot directory;
    /// a whole-graph snapshot keeps version 4 and passes that gate.
    #[test]
    fn releases_up_to_0_6_refuse_a_layer_snapshot_as_newer() {
        use super::super::manifest::{FIXED_BYTES, Manifest};
        use super::super::snapshot::PackedReader;
        /// The gate of a 0.6 reader: versions 1..=4, a newer one refused, never damage.
        fn old_gate(what: &str, version: u32) -> Result<(), PersistError> {
            let old_max = 4;
            if version > old_max {
                return Err(PersistError::UnsupportedVersion {
                    location: what.into(),
                    found: version,
                    supported: old_max,
                });
            }
            Ok(())
        }
        let version_at =
            |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let (base, layer) = layer_snapshot();
        // The packed form: the header's version word.
        assert_eq!(version_at(&layer, 8), LAYER_FORMAT_VERSION);
        assert!(matches!(
            old_gate("packed snapshot", version_at(&layer, 8)),
            Err(PersistError::UnsupportedVersion { found: 5, .. })
        ));
        // The snapshot directory form: its manifest (what a store's header walk reads first).
        let reader = PackedReader::open(layer.as_slice()).unwrap();
        let manifest = reader.manifest_bytes.clone();
        assert_eq!(version_at(&manifest, 8), LAYER_FORMAT_VERSION);
        assert_eq!(version_at(&manifest, manifest.len() - FIXED_BYTES + 8), 5);
        assert!(old_gate("manifest", version_at(&manifest, 8)).is_err());
        let mut decoded = Manifest::decode(&manifest).unwrap();
        assert_eq!(decoded.encode(), manifest, "re-encoded in the same version");
        // The element segments stay version 4 (nothing in them changed).
        let mut segment = Vec::new();
        let mut reader = reader;
        reader.copy_segment(&mut segment).unwrap();
        assert_eq!(version_at(&segment, 8), codec::FORMAT_VERSION);
        // A whole graph: version 4 everywhere, through the old gate.
        let mut whole = Vec::new();
        base.write_snapshot(&mut whole).unwrap();
        assert_eq!(version_at(&whole, 8), codec::FORMAT_VERSION);
        assert!(old_gate("packed snapshot", version_at(&whole, 8)).is_ok());
        // This build: the layer version without a layer file is a newer version's snapshot.
        let plain = with_layer_kind(&layer, 200);
        let mut forged = plain.clone();
        forged[8..12].copy_from_slice(&LAYER_FORMAT_VERSION.to_le_bytes());
        let trailer_at = forged.len() - 4;
        let trailer = codec::crc(&forged[..trailer_at]);
        forged[trailer_at..].copy_from_slice(&trailer.to_le_bytes());
        assert!(matches!(
            crate::persist::read_snapshot(forged.as_slice(), &Default::default()),
            Err(PersistError::UnsupportedVersion { found: 5, .. })
        ));
    }
}
