//! The sidecar form of the workload statistics (format spec "Workload statistics sidecar"): the
//! day tier of every series and the job block, written whole by a Store through
//! `StoreDir::write_atomic`.
//!
//! ```text
//! magic "GRWLSTAT" | version u32 = 1 | series count u32
//! per series: byte length u32 | kind u8 | name (u32 length + UTF-8) | channel u8
//!             | bucket count u16 | per bucket: day i64 | count, failed, items, total, max u64
//! job count u32 | per job: name | started_at i64 | micros u64 | error flag u8 [+ text]
//! CRC-32C u32 of everything before it
//! ```
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
const VERSION: u32 = 1;
const KIND_JOB: u8 = 1;
const KIND_WRITE_LOCK: u8 = 2;
/// The tier the sidecar holds (days).
const DAY_TIER: usize = 2;

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
                SeriesKind::WriteLock(name) => (KIND_WRITE_LOCK, name),
            };
            body.push(kind);
            put_str(&mut body, name);
            body.push(key.channel.code());
            let buckets = &series.tiers[DAY_TIER].buckets;
            let count = buckets.len().min(u16::MAX as usize);
            body.extend_from_slice(&(count as u16).to_le_bytes());
            for (day, c) in buckets.iter().take(count) {
                body.extend_from_slice(&day.to_le_bytes());
                for value in [c.count, c.failed, c.items, c.total_micros, c.max_micros] {
                    body.extend_from_slice(&value.to_le_bytes());
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
        if r.take(8)? != MAGIC || r.u32()? != VERSION {
            return None;
        }
        let mut inner = Inner::default();
        let (_, keep) = TIERS[DAY_TIER];
        for _ in 0..r.u32()? {
            let len = r.u32()? as usize;
            let mut s = Reader {
                bytes: r.take(len)?,
                at: 0,
            };
            let kind = s.u8()?;
            let name = CompactString::from(s.str()?);
            let channel = s.u8()?;
            let (Some(kind), Some(channel)) = (
                match kind {
                    KIND_JOB => Some(SeriesKind::Job(name)),
                    KIND_WRITE_LOCK => Some(SeriesKind::WriteLock(name)),
                    _ => None,
                },
                Channel::from_code(channel),
            ) else {
                // A kind or channel of a newer version: skipped (its bytes are consumed).
                continue;
            };
            let mut series = Series::default();
            for _ in 0..s.u16()? {
                let day = s.i64()?;
                let counters = Counters {
                    count: s.u64()?,
                    failed: s.u64()?,
                    items: s.u64()?,
                    total_micros: s.u64()?,
                    max_micros: s.u64()?,
                };
                series.tiers[DAY_TIER].buckets.push_back((day, counters));
            }
            if series.tiers[DAY_TIER].buckets.len() > keep {
                let drop = series.tiers[DAY_TIER].buckets.len() - keep;
                series.tiers[DAY_TIER].buckets.drain(..drop);
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
    use super::super::{DAY, Sample};
    use super::*;

    #[test]
    fn the_day_tier_and_the_jobs_round_trip() {
        let stats = WorkloadStats::new(WorkloadLevel::Persisted);
        let t0 = 1_800_000_000_000_000i64;
        let lock = SeriesKey::new(SeriesKind::WriteLock("query".into()), Channel::Mcp);
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
        // Only the day tier is kept: the minute and hour tiers start empty.
        assert_eq!(b.series[0].last_hour.count, 0);
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
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(b"extra bytes of a newer layout");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&0u32.to_le_bytes());
        let crc = crc32c::crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        let back = WorkloadStats::from_sidecar(&out, WorkloadLevel::Memory).unwrap();
        assert!(back.report(0).series.is_empty());
    }
}
