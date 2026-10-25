//! The sidecar form of the workload statistics (format spec 4.4): every tier of every series
//! (minutes, hours, days) and the job block, written whole by a Store through
//! `StoreDir::write_atomic`, so a restart loses none of them (buckets that aged meanwhile drop
//! out at the next sample).
//!
//! ```text
//! magic "GRWLSTAT" | version u32 = 2 | series count u32
//! per series: byte length u32 | kind u8 | name (u32 length + UTF-8) | channel u8
//!             | tier count u8 | per tier: tier u8 (0 minute, 1 hour, 2 day) | bucket count u16
//!             | per bucket: index i64 (bucket start / width) | count, failed, items, total, max u64
//! job count u32 | per job: name | started_at i64 | micros u64 | error flag u8 [+ text]
//! section count u32 | per section: tag u8 | byte length u32 | bytes          (version 2)
//!     tag 1, the system jobs' settings: count u32 | per job: name | schedule flag u8 [+ text]
//!            | enabled u8 (0 unset, 1 off, 2 on)
//! CRC-32C u32 of everything before it
//! ```
//!
//! Version 1 (no sections) is still read; a version 2 file may leave the sections out. A section of a tag this version does not know is
//! skipped by its byte length.
//!
//! All integers little-endian. Reading is best effort and never fails a Store: a wrong magic,
//! checksum or length, or a newer version, yields `None` (the statistics start anew); a series
//! of a kind or channel this version does not know is skipped by its byte length.

use compact_str::CompactString;

use super::{
    Channel, Counters, Inner, JobRunRecord, Series, SeriesKey, SeriesKind, TIERS, WorkloadLevel,
    WorkloadStats,
};

const MAGIC: &[u8; 8] = b"GRWLSTAT";
const VERSION: u32 = 2;
const SECTION_JOB_OVERRIDES: u8 = 1;
const KIND_JOB: u8 = 1;
const KIND_LOCK: u8 = 2;
const KIND_QUERY: u8 = 3;
const KIND_PROPERTY: u8 = 4;

fn put_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// A bounds-checked reader over the sidecar bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| {
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            u64::from_le_bytes(a)
        })
    }
    fn i64(&mut self) -> Option<i64> {
        self.u64().map(|v| v as i64)
    }
    fn str(&mut self) -> Option<&'a str> {
        let len = self.u32()? as usize;
        std::str::from_utf8(self.take(len)?).ok()
    }
}

impl WorkloadStats {
    /// The sidecar bytes of these statistics (module docs).
    pub(crate) fn to_sidecar(&self) -> Vec<u8> {
        let inner = self.inner.lock();
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        let mut keys: Vec<&SeriesKey> = inner.series.keys().collect();
        keys.sort();
        out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for key in keys {
            let Some(series) = inner.series.get(key) else {
                continue;
            };
            let mut body = Vec::new();
            let (kind, name) = match &key.kind {
                SeriesKind::Job(name) => (KIND_JOB, name),
                SeriesKind::Lock(name) => (KIND_LOCK, name),
                SeriesKind::Query(name) => (KIND_QUERY, name),
                SeriesKind::Property(property) => (KIND_PROPERTY, &property.key),
            };
            body.push(kind);
            put_str(&mut body, name);
            body.push(key.channel.code());
            // A property series names its element kind, label and access after the channel.
            if let SeriesKind::Property(property) = &key.kind {
                body.push(match property.element {
                    None => 0,
                    Some(crate::graph::ElementKind::Vertex) => 1,
                    Some(crate::graph::ElementKind::Edge) => 2,
                });
                match &property.label {
                    Some(label) => {
                        body.push(1);
                        put_str(&mut body, label);
                    }
                    None => body.push(0),
                }
                body.push(property.access.code());
            }
            body.push(TIERS.len() as u8);
            for (tier_index, tier) in series.tiers.iter().enumerate() {
                body.push(tier_index as u8);
                let count = tier.buckets.len().min(u16::MAX as usize);
                body.extend_from_slice(&(count as u16).to_le_bytes());
                for (index, c) in tier.buckets.iter().take(count) {
                    body.extend_from_slice(&index.to_le_bytes());
                    for value in [c.count, c.failed, c.items, c.total_micros, c.max_micros] {
                        body.extend_from_slice(&value.to_le_bytes());
                    }
                }
            }
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&body);
        }
        let mut jobs: Vec<(&CompactString, &JobRunRecord)> = inner.jobs.iter().collect();
        jobs.sort_by(|a, b| a.0.cmp(b.0));
        out.extend_from_slice(&(jobs.len() as u32).to_le_bytes());
        for (name, run) in jobs {
            put_str(&mut out, name);
            out.extend_from_slice(&run.started_at.to_le_bytes());
            out.extend_from_slice(&run.micros.to_le_bytes());
            match &run.error {
                Some(error) => {
                    out.push(1);
                    put_str(&mut out, error);
                }
                None => out.push(0),
            }
        }
        let mut overrides: Vec<_> = inner.overrides.iter().collect();
        overrides.sort_by(|a, b| a.0.cmp(b.0));
        let mut section = Vec::new();
        section.extend_from_slice(&(overrides.len() as u32).to_le_bytes());
        for (name, setting) in overrides {
            put_str(&mut section, name);
            match &setting.schedule {
                Some(schedule) => {
                    section.push(1);
                    put_str(&mut section, schedule);
                }
                None => section.push(0),
            }
            section.push(match setting.enabled {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            });
        }
        out.extend_from_slice(&1u32.to_le_bytes());
        out.push(SECTION_JOB_OVERRIDES);
        out.extend_from_slice(&(section.len() as u32).to_le_bytes());
        out.extend_from_slice(&section);
        let crc = crc32c::crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Statistics at `level` read from sidecar `bytes`, or `None` when the bytes are not a
    /// sidecar this version reads (module docs).
    pub(crate) fn from_sidecar(bytes: &[u8], level: WorkloadLevel) -> Option<WorkloadStats> {
        let (body, crc) = bytes.split_at_checked(bytes.len().checked_sub(4)?)?;
        if crc32c::crc32c(body).to_le_bytes() != crc {
            return None;
        }
        let mut r = Reader { bytes: body, at: 0 };
        if r.take(8)? != MAGIC {
            return None;
        }
        let version = r.u32()?;
        if !(1..=VERSION).contains(&version) {
            return None;
        }
        let mut inner = Inner::default();
        for _ in 0..r.u32()? {
            let len = r.u32()? as usize;
            let mut s = Reader {
                bytes: r.take(len)?,
                at: 0,
            };
            let kind = s.u8()?;
            let name = CompactString::from(s.str()?);
            let channel = s.u8()?;
            let property = if kind == KIND_PROPERTY {
                let element = match s.u8()? {
                    0 => None,
                    1 => Some(crate::graph::ElementKind::Vertex),
                    2 => Some(crate::graph::ElementKind::Edge),
                    _ => continue,
                };
                let label = match s.u8()? {
                    0 => None,
                    _ => Some(CompactString::from(s.str()?)),
                };
                let Some(access) = super::AccessKind::from_code(s.u8()?) else {
                    continue;
                };
                Some(super::PropertyUse {
                    element,
                    label,
                    key: name.clone(),
                    access,
                })
            } else {
                None
            };
            let (Some(kind), Some(channel)) = (
                match kind {
                    KIND_JOB => Some(SeriesKind::Job(name)),
                    KIND_LOCK => Some(SeriesKind::Lock(name)),
                    KIND_QUERY => Some(SeriesKind::Query(name)),
                    KIND_PROPERTY => property.map(SeriesKind::Property),
                    _ => None,
                },
                Channel::from_code(channel),
            ) else {
                // A kind or channel of a newer version: skipped (its bytes are consumed).
                continue;
            };
            let mut series = Series::default();
            for _ in 0..s.u8()? {
                let tier_index = s.u8()? as usize;
                let mut buckets = std::collections::VecDeque::new();
                for _ in 0..s.u16()? {
                    let index = s.i64()?;
                    let counters = Counters {
                        count: s.u64()?,
                        failed: s.u64()?,
                        items: s.u64()?,
                        total_micros: s.u64()?,
                        max_micros: s.u64()?,
                    };
                    buckets.push_back((index, counters));
                }
                // A tier a newer version adds is skipped (its buckets are consumed).
                let (Some(tier), Some((_, keep))) =
                    (series.tiers.get_mut(tier_index), TIERS.get(tier_index))
                else {
                    continue;
                };
                if buckets.len() > *keep {
                    let drop = buckets.len() - keep;
                    buckets.drain(..drop);
                }
                tier.buckets = buckets;
            }
            inner.series.insert(SeriesKey::new(kind, channel), series);
        }
        for _ in 0..r.u32()? {
            let name = CompactString::from(r.str()?);
            let started_at = r.i64()?;
            let micros = r.u64()?;
            let error = match r.u8()? {
                0 => None,
                _ => Some(r.str()?.to_string()),
            };
            inner.jobs.insert(
                name,
                JobRunRecord {
                    started_at,
                    micros,
                    error,
                },
            );
        }
        // The sections (version 2) may be left out altogether: none.
        if version >= 2 && r.at < body.len() {
            for _ in 0..r.u32()? {
                let tag = r.u8()?;
                let len = r.u32()? as usize;
                let mut s = Reader {
                    bytes: r.take(len)?,
                    at: 0,
                };
                if tag != SECTION_JOB_OVERRIDES {
                    // A section of a newer version: skipped.
                    continue;
                }
                for _ in 0..s.u32()? {
                    let name = CompactString::from(s.str()?);
                    let schedule = match s.u8()? {
                        0 => None,
                        _ => Some(s.str()?.to_string()),
                    };
                    let enabled = match s.u8()? {
                        0 => None,
                        1 => Some(false),
                        _ => Some(true),
                    };
                    inner
                        .overrides
                        .insert(name, super::JobOverride { schedule, enabled });
                }
                if s.at != len {
                    return None;
                }
            }
        }
        if r.at != body.len() {
            return None;
        }
        Some(WorkloadStats {
            level: parking_lot::RwLock::new(level),
            inner: parking_lot::Mutex::new(inner),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DAY, JobOverride, Sample};
    use super::*;

    #[test]
    fn every_tier_and_the_jobs_round_trip() {
        let stats = WorkloadStats::new(WorkloadLevel::Persisted);
        let t0 = 1_800_000_000_000_000i64;
        let lock = SeriesKey::new(SeriesKind::Lock("query".into()), Channel::Mcp);
        stats.record(lock.clone(), t0, Sample::new(4, 10));
        stats.record(lock.clone(), t0 + DAY, Sample::new(1, 30).failed());
        stats.record_job_run(
            "stats_flush",
            JobRunRecord {
                started_at: t0,
                micros: 7,
                error: Some("disk full".into()),
            },
        );
        let bytes = stats.to_sidecar();
        let back = WorkloadStats::from_sidecar(&bytes, WorkloadLevel::Persisted).unwrap();
        let (a, b) = (stats.report(t0 + DAY), back.report(t0 + DAY));
        assert_eq!(a.jobs, b.jobs);
        assert_eq!(a.series.len(), b.series.len());
        for (x, y) in a.series.iter().zip(&b.series) {
            assert_eq!(x.key, y.key);
            assert_eq!(x.last_ninety_days, y.last_ninety_days);
        }
        // Every tier survives.
        for (x, y) in a.series.iter().zip(&b.series) {
            assert_eq!(x.last_hour, y.last_hour);
            assert_eq!(x.last_two_days, y.last_two_days);
        }
        let lock_back = b.series.iter().find(|s| s.key == lock).unwrap();
        assert_eq!(lock_back.last_hour.count, 1);
    }

    #[test]
    fn damaged_or_foreign_bytes_start_anew() {
        let stats = WorkloadStats::new(WorkloadLevel::Persisted);
        stats.record(
            SeriesKey::new(SeriesKind::Job("j".into()), Channel::Job),
            0,
            Sample::new(1, 1),
        );
        let bytes = stats.to_sidecar();
        for cut in 0..bytes.len() {
            assert!(WorkloadStats::from_sidecar(&bytes[..cut], WorkloadLevel::Memory).is_none());
        }
        for i in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[i] ^= 0x10;
            assert!(
                WorkloadStats::from_sidecar(&flipped, WorkloadLevel::Memory).is_none(),
                "byte {i}"
            );
        }
        assert!(WorkloadStats::from_sidecar(b"GRAPH", WorkloadLevel::Memory).is_none());
    }

    #[test]
    fn a_series_of_an_unknown_kind_is_skipped() {
        // Hand-built: one series of kind 99, then one job.
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        let mut body = vec![99u8];
        put_str(&mut body, "future");
        body.push(0);
        body.push(0);
        body.extend_from_slice(b"extra bytes of a newer layout");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&0u32.to_le_bytes());
        let crc = crc32c::crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        let back = WorkloadStats::from_sidecar(&out, WorkloadLevel::Memory).unwrap();
        assert!(back.report(0).series.is_empty());
    }

    fn job_overrides_hand_built(version: u32, sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        if version >= 2 {
            out.extend_from_slice(&(sections.len() as u32).to_le_bytes());
            for (tag, bytes) in sections {
                out.push(*tag);
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
            }
        }
        let crc = crc32c::crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    #[test]
    fn the_system_jobs_settings_round_trip_and_survive_clear() {
        let stats = WorkloadStats::new(WorkloadLevel::Persisted);
        stats.set_job_override("compact", JobOverride::new().with_enabled(true));
        stats.set_job_override(
            "stats_flush",
            JobOverride::new()
                .with_schedule("*/5 * * * *")
                .with_enabled(false),
        );
        stats.clear(None);
        let back =
            WorkloadStats::from_sidecar(&stats.to_sidecar(), WorkloadLevel::Persisted).unwrap();
        assert_eq!(back.job_overrides(), stats.job_overrides());
        assert_eq!(
            back.job_override("stats_flush"),
            Some(JobOverride {
                schedule: Some("*/5 * * * *".into()),
                enabled: Some(false)
            })
        );
        // An empty setting removes it.
        back.set_job_override("compact", JobOverride::new());
        assert_eq!(back.job_override("compact"), None);
    }

    #[test]
    fn version_one_is_read_and_an_unknown_section_is_skipped() {
        let v1 = job_overrides_hand_built(1, &[]);
        let back = WorkloadStats::from_sidecar(&v1, WorkloadLevel::Memory).unwrap();
        assert!(back.job_overrides().is_empty());
        let mut known = Vec::new();
        known.extend_from_slice(&1u32.to_le_bytes());
        put_str(&mut known, "compact");
        known.push(0);
        known.push(2);
        let v2 = job_overrides_hand_built(
            2,
            &[
                (77, b"a newer section".to_vec()),
                (SECTION_JOB_OVERRIDES, known),
            ],
        );
        let back = WorkloadStats::from_sidecar(&v2, WorkloadLevel::Memory).unwrap();
        assert_eq!(
            back.job_override("compact"),
            Some(JobOverride::new().with_enabled(true))
        );
        // A newer version is not read.
        assert!(
            WorkloadStats::from_sidecar(&job_overrides_hand_built(3, &[]), WorkloadLevel::Memory)
                .is_none()
        );
    }
}
