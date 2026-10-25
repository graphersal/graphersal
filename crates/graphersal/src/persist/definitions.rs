//! The catalog of definitions in the binary format (format spec 6.3 and 16): one definition is
//! `kind u8 | flags u8 | name string | length uvar | payload`, the payload of a known kind is a
//! property map with inline keys, the payload of a kind this version does not know is kept as
//! its bytes (written back unchanged, format spec 16 rule 2). The manifest's definitions table
//! and the WAL's op 10 `SetDefinition` share this encoding.

use super::codec::{
    DecodeError, DecodeResult, FORMAT_VERSION, InlineKeys, Reader, put_properties, put_str, put_u8,
    put_uvar, read_properties,
};
use crate::graph::compression::CompressionDefinition;
use crate::graph::definition::{
    Definition, DefinitionBody, DefinitionFlags, DefinitionKind, QueryDefinition,
    RESERVED_PAYLOAD_KEYS,
};
use crate::graph::property_index::PropertyIndexDefinition;
use crate::value::PropertyMap;

/// One definition as stored: kind, flags, name and the payload bytes. The manifest keeps the
/// table in this form (decoding the payloads needs the load's value depth bound).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DefinitionRecord {
    pub(crate) kind: u8,
    pub(crate) flags: u8,
    pub(crate) name: String,
    pub(crate) payload: Vec<u8>,
}

impl DefinitionRecord {
    /// The record of `definition`: a known kind's payload encoded as its property map, an
    /// opaque payload copied byte for byte.
    pub(crate) fn encode(definition: &Definition) -> Result<Self, String> {
        let known = match definition.body() {
            DefinitionBody::Query(query) => Some(query.to_payload()),
            DefinitionBody::Compression(rule) => Some(rule.to_payload()),
            DefinitionBody::PropertyIndex(index) => Some(index.to_payload()),
            DefinitionBody::Job(job) => Some(job.to_payload()),
            DefinitionBody::Opaque(_) => None,
        };
        let payload = match (known, definition.body()) {
            (Some(map), _) => encode_payload(&map).map_err(|reason| {
                format!(
                    "the {} '{}': {reason}",
                    definition.kind(),
                    definition.name()
                )
            })?,
            (None, DefinitionBody::Opaque(bytes)) => bytes.clone(),
            (
                None,
                DefinitionBody::Query(_)
                | DefinitionBody::Compression(_)
                | DefinitionBody::PropertyIndex(_)
                | DefinitionBody::Job(_),
            ) => Vec::new(),
        };
        Ok(Self {
            kind: definition.kind().code(),
            flags: definition.flags().bits(),
            name: definition.name().to_string(),
            payload,
        })
    }

    /// The definition: a known kind's payload decoded, an unknown kind kept opaque. A payload
    /// that is not a valid property map (or has a reserved key) is invalid: `Err` with the
    /// reason and the offset within the payload. A valid map whose fields this version cannot
    /// read as its kind (a newer Graphersal wrote them) is never an error: the definition is
    /// kept opaque, its bytes written back unchanged (format spec 16 rule 2); an unknown value
    /// of a known key it does understand (a compression codec, an index type) is decoded into a
    /// definition that is kept and not served.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn decode(&self, max_depth: usize) -> DecodeResult<Definition> {
        self.decode_in(max_depth, FORMAT_VERSION)
    }

    /// [`DefinitionRecord::decode`] of a record read from a file of format `version` (a
    /// temporal value in the payload of a version 1 file is invalid).
    pub(crate) fn decode_in(&self, max_depth: usize, version: u32) -> DecodeResult<Definition> {
        let kind = DefinitionKind::from_code(self.kind);
        let flags = DefinitionFlags::from_bits(self.flags);
        if !kind.is_known() {
            return Ok(Definition::opaque(
                kind,
                flags,
                self.name.clone(),
                self.payload.clone(),
            ));
        }
        let map = decode_payload_in(&self.payload, max_depth, version)?;
        let decoded = if kind == DefinitionKind::COMPRESSION {
            CompressionDefinition::from_payload(&map)
                .map(|rule| Definition::compression(self.name.clone(), rule))
        } else if kind == DefinitionKind::JOB {
            crate::graph::jobs::JobDefinition::from_payload(&map)
                .map(|job| Definition::job(self.name.clone(), job))
        } else if kind == DefinitionKind::PROPERTY_INDEX {
            PropertyIndexDefinition::from_payload(&map)
                .map(|index| Definition::property_index(self.name.clone(), index))
        } else {
            QueryDefinition::from_payload(&map)
                .map(|query| Definition::query(self.name.clone(), query))
        };
        match decoded {
            Ok(definition) => Ok(definition.with_flags(flags)),
            Err(err) => {
                tracing::warn!(
                    "the {kind} '{}' is kept opaque: this version cannot read its payload ({err})",
                    self.name
                );
                Ok(Definition::opaque(
                    kind,
                    flags,
                    self.name.clone(),
                    self.payload.clone(),
                ))
            }
        }
    }

    /// Writes the record: `kind u8 | flags u8 | name string | length uvar | payload`.
    pub(crate) fn put(&self, out: &mut Vec<u8>) {
        put_u8(out, self.kind);
        put_u8(out, self.flags);
        put_str(out, &self.name);
        put_uvar(out, self.payload.len() as u64);
        out.extend_from_slice(&self.payload);
    }

    /// Reads one record; the payload length is checked against the remaining bytes.
    pub(crate) fn read(r: &mut Reader<'_>) -> DecodeResult<Self> {
        let kind = r.u8()?;
        let flags = r.u8()?;
        let name = r.str()?.to_string();
        let at = r.offset();
        let length = r.uvar()?;
        let length = usize::try_from(length)
            .ok()
            .filter(|length| *length <= r.remaining())
            .ok_or_else(|| {
                DecodeError::new(
                    at,
                    format!("a definition payload of {length} bytes runs past the end"),
                )
            })?;
        let payload = r.bytes(length)?.to_vec();
        Ok(Self {
            kind,
            flags,
            name,
            payload,
        })
    }
}

/// Writes the definitions table (format spec 6.3): `uvar count | definition*`, in the order
/// given (the catalog's: kind, then name bytewise).
pub(crate) fn put_table(out: &mut Vec<u8>, records: &[DefinitionRecord]) {
    put_uvar(out, records.len() as u64);
    for record in records {
        record.put(out);
    }
}

/// Reads the definitions table: the count is checked against the remaining bytes (a record
/// takes at least 4), the rows must be strictly increasing by kind, then name bytewise (so a
/// repeated kind and name is invalid). Offsets are within `r`.
pub(crate) fn read_table(r: &mut Reader<'_>) -> DecodeResult<Vec<DefinitionRecord>> {
    let count = r.count(4)?;
    let mut records: Vec<DefinitionRecord> = Vec::with_capacity(count);
    for _ in 0..count {
        let at = r.offset();
        let record = DefinitionRecord::read(r)?;
        if let Some(previous) = records.last()
            && (previous.kind, previous.name.as_bytes()) >= (record.kind, record.name.as_bytes())
        {
            return Err(DecodeError::new(
                at,
                "definitions out of order or repeated (kind, then name)",
            ));
        }
        records.push(record);
    }
    Ok(records)
}

/// Encodes a payload property map (format spec 6.3: `uvar count | (key | value)*` with inline
/// keys and the codec's tagged values). A reserved key ([`RESERVED_PAYLOAD_KEYS`]) or a value
/// that is not storable is `Err` with the reason.
pub(crate) fn encode_payload(map: &PropertyMap) -> Result<Vec<u8>, String> {
    if let Some(key) = map
        .keys()
        .find(|key| RESERVED_PAYLOAD_KEYS.contains(&key.as_str()))
    {
        return Err(format!("the payload key '{key}' is reserved"));
    }
    let mut out = Vec::new();
    put_properties(&mut out, Some(map), &mut InlineKeys)?;
    Ok(out)
}

/// Decodes a payload written by [`encode_payload`]: every count and length is checked against
/// the remaining bytes before allocating, values nest at most `max_depth` levels; trailing
/// bytes, a repeated key and a reserved key are errors (offsets within the payload).
pub(crate) fn decode_payload(payload: &[u8], max_depth: usize) -> DecodeResult<PropertyMap> {
    decode_payload_in(payload, max_depth, FORMAT_VERSION)
}

/// [`decode_payload`] of a payload read from a file of format `version`.
fn decode_payload_in(payload: &[u8], max_depth: usize, version: u32) -> DecodeResult<PropertyMap> {
    let mut r = Reader::new(payload).with_version(version);
    let map = read_properties(&mut r, &InlineKeys, max_depth)?;
    if !r.is_empty() {
        return Err(r.error("trailing bytes after the payload map"));
    }
    if let Some(key) = map
        .keys()
        .find(|key| RESERVED_PAYLOAD_KEYS.contains(&key.as_str()))
    {
        return Err(DecodeError::new(
            0,
            format!("the payload key '{key}' is reserved"),
        ));
    }
    Ok(map)
}

/// The records of a catalog, in its order (kind, then name).
pub(crate) fn records_of<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
) -> Result<Vec<DefinitionRecord>, String> {
    definitions
        .into_iter()
        .map(DefinitionRecord::encode)
        .collect()
}

/// The first critical definition of a kind this version does not know, described for a
/// read-only reason (format spec 16 rule 3).
pub(crate) fn critical_unknown<'a>(
    definitions: impl IntoIterator<Item = &'a Definition>,
) -> Option<String> {
    definitions
        .into_iter()
        .find(|d| d.flags().is_critical() && d.is_opaque())
        .map(|d| {
            let what = if d.kind().is_known() {
                "whose payload this version cannot read (a newer Graphersal wrote it)"
            } else {
                "which this version does not know"
            };
            format!(
                "it holds the critical definition '{}' of {}, {what} (a writer that does not \
                 maintain it would corrupt it)",
                d.name(),
                d.kind()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::definition::QueryParam;
    use crate::graph::schema::PropertySchema;

    fn query() -> Definition {
        Definition::query(
            "adults",
            QueryDefinition::new("fn adults(min) { g.v() }")
                .with_folder("people")
                .with_param(
                    "min",
                    QueryParam::new(PropertySchema::integer()).with_default(18),
                ),
        )
    }

    #[test]
    fn records_round_trip_and_keep_unknown_payloads() {
        let opaque = Definition::opaque(
            DefinitionKind::from_code(77),
            DefinitionFlags::from_bits(0b1000_0000),
            "x",
            vec![1, 2, 3, 255],
        );
        let records = records_of([&query(), &opaque]).unwrap();
        let mut out = Vec::new();
        put_table(&mut out, &records);
        let mut r = Reader::new(&out);
        let back = read_table(&mut r).unwrap();
        assert!(r.is_empty());
        assert_eq!(back, records);
        assert_eq!(back[0].decode(128).unwrap(), query());
        assert_eq!(back[1].decode(128).unwrap(), opaque);
    }

    #[test]
    fn out_of_order_reserved_keys_and_bad_lengths_are_invalid() {
        let records = records_of([&query(), &query()]).unwrap();
        let mut out = Vec::new();
        put_table(&mut out, &records);
        assert!(read_table(&mut Reader::new(&out)).is_err());

        let mut payload = Vec::new();
        let mut map = crate::value::PropertyMap::default();
        map.insert("source", "fn x() { 1 }".into());
        map.insert("kind", "query".into());
        put_properties(&mut payload, Some(&map), &mut InlineKeys).unwrap();
        let record = DefinitionRecord {
            kind: 1,
            flags: 0,
            name: "x".into(),
            payload,
        };
        assert!(record.decode(128).is_err());

        let mut out = Vec::new();
        put_uvar(&mut out, 1);
        out.extend_from_slice(&[1, 0, 1, b'x']);
        put_uvar(&mut out, 1 << 40);
        assert!(read_table(&mut Reader::new(&out)).is_err());
    }

    /// A commit with every shape of op 10: define, change, drop, an unknown kind.
    fn commit() -> crate::graph::ChangeSet {
        use crate::graph::change::{CommitMetadata, CommitStamp, Mutation};
        let unknown = Definition::opaque(
            DefinitionKind::from_code(200),
            DefinitionFlags::NONE,
            "u",
            vec![0, 1, 2],
        );
        let mutation = |before: Option<&Definition>, after: Option<&Definition>| {
            let named = before.or(after).unwrap();
            Mutation::SetDefinition {
                kind: named.kind(),
                name: named.name().to_string(),
                before: before.cloned().map(Box::new),
                after: after.cloned().map(Box::new),
            }
        };
        let moved = {
            let mut moved = query();
            moved.as_query_mut().unwrap().set_folder(Some("x".into()));
            moved
        };
        crate::graph::ChangeSet::new(
            CommitStamp {
                commit_seq: 3,
                committed_at: 7,
                node_sequence: 1,
                edge_sequence: 2,
            },
            CommitMetadata::new(),
            vec![
                mutation(None, Some(&query())),
                mutation(Some(&query()), Some(&moved)),
                mutation(Some(&moved), None),
                mutation(None, Some(&unknown)),
            ],
        )
    }

    #[test]
    fn op_10_round_trips_and_checks_its_images() {
        use super::super::codec::Codec;
        use super::super::wal::{decode_commit, encode_commit};
        let changes = commit();
        let payload = encode_commit(&changes, Codec::None).unwrap();
        assert_eq!(decode_commit(&payload, 128).unwrap(), changes);
        // An image of another name than its change is invalid.
        let mut renamed = payload.clone();
        let at = renamed
            .windows(8)
            .rposition(|w| w == b"\x00\x06adults")
            .unwrap();
        renamed[at + 2] = b'b';
        assert!(decode_commit(&renamed, 128).is_err());
    }

    /// A tiny deterministic generator (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize % n.max(1)
        }
    }

    fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
        let mut out = base.to_vec();
        for _ in 0..1 + rng.below(4) {
            if out.is_empty() {
                break;
            }
            let at = rng.below(out.len());
            match rng.below(5) {
                0 => out.truncate(at),
                1 => out[at] ^= 1 << rng.below(8),
                2 => out[at] = [0x00, 0xFF, 0x7F, 0x80, 0x01][rng.below(5)],
                3 => {
                    let end = (at + rng.below(16)).min(out.len());
                    out.drain(at..end);
                }
                _ => {
                    let end = (at + 8).min(out.len());
                    for byte in &mut out[at..end] {
                        *byte = 0xFF;
                    }
                }
            }
        }
        out
    }

    /// Hostile input (CLAUDE.md rule 1): mutated definitions tables and op 10 commit payloads
    /// (no CRC in the way) are refused or decoded, never a panic or an unchecked allocation.
    #[test]
    fn mutated_definitions_never_crash_the_decoders() {
        use super::super::codec::Codec;
        use super::super::wal::{decode_commit, encode_commit};
        let opaque = Definition::opaque(
            DefinitionKind::from_code(77),
            DefinitionFlags::NONE,
            "x",
            vec![1, 2, 3],
        );
        let mut table = Vec::new();
        put_table(&mut table, &records_of([&query(), &opaque]).unwrap());
        let payload = encode_commit(&commit(), Codec::None).unwrap();
        let mut rng = Rng(0x5EED_0010);
        for _ in 0..4000 {
            let input = mutate(&mut rng, &table);
            if let Ok(records) = read_table(&mut Reader::new(&input)) {
                for record in records {
                    let _ = record.decode(16);
                }
            }
            let input = mutate(&mut rng, &payload);
            let _ = decode_commit(&input, 16);
        }
    }
}
