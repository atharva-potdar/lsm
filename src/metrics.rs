use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct EngineMetrics {
    pub bytes_written_user: AtomicU64,
    pub bytes_written_wal: AtomicU64,
    pub bytes_written_flush: AtomicU64,
    pub bytes_written_compaction: AtomicU64,
    pub bytes_read_disk: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,

    // Per-level compaction byte counters (Levels 0 through 6)
    pub compaction_bytes_by_level: [AtomicU64; 7],

    // Evidence of Overwrite Absorption
    pub compaction_entries_in: AtomicU64,
    pub compaction_entries_out: AtomicU64,
}

impl EngineMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn total_disk_bytes(&self) -> u64 {
        self.bytes_written_wal.load(Ordering::Relaxed)
            + self.bytes_written_flush.load(Ordering::Relaxed)
            + self.bytes_written_compaction.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn user_bytes(&self) -> u64 {
        self.bytes_written_user.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn disk_read_bytes(&self) -> u64 {
        self.bytes_read_disk.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn cache_hit_ratio(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed) as f64;
        let misses = self.cache_misses.load(Ordering::Relaxed) as f64;
        let total = hits + misses;
        if total == 0.0 { 0.0 } else { hits / total }
    }

    /// Standard LSM Write Amplification (excluding WAL): (Flush + Compaction) / User
    #[must_use]
    pub fn write_amplification(&self) -> f64 {
        let user = self.user_bytes().max(1) as f64;
        let disk = (self.bytes_written_flush.load(Ordering::Relaxed)
            + self.bytes_written_compaction.load(Ordering::Relaxed)) as f64;
        disk / user
    }

    /// Total Write Amplification including WAL: (WAL + Flush + Compaction) / User
    #[must_use]
    pub fn write_amplification_with_wal(&self) -> f64 {
        let user = self.user_bytes().max(1) as f64;
        self.total_disk_bytes() as f64 / user
    }

    pub fn reset(&self) {
        self.bytes_written_user.store(0, Ordering::Relaxed);
        self.bytes_written_wal.store(0, Ordering::Relaxed);
        self.bytes_written_flush.store(0, Ordering::Relaxed);
        self.bytes_written_compaction.store(0, Ordering::Relaxed);
        self.bytes_read_disk.store(0, Ordering::Relaxed);
        self.cache_hits.store(0, Ordering::Relaxed);
        self.cache_misses.store(0, Ordering::Relaxed);
        self.compaction_entries_in.store(0, Ordering::Relaxed);
        self.compaction_entries_out.store(0, Ordering::Relaxed);
        for level in &self.compaction_bytes_by_level {
            level.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_metrics_write_amplification() {
        let metrics = EngineMetrics::new();
        assert_eq!(metrics.write_amplification(), 0.0);

        metrics.bytes_written_user.store(1000, Ordering::Relaxed);
        metrics.bytes_written_wal.store(1050, Ordering::Relaxed);
        metrics.bytes_written_flush.store(1100, Ordering::Relaxed);
        metrics
            .bytes_written_compaction
            .store(1850, Ordering::Relaxed);

        assert_eq!(metrics.user_bytes(), 1000);
        assert_eq!(metrics.total_disk_bytes(), 1050 + 1100 + 1850); // 4000
        // Without WAL: (1100 + 1850) / 1000 = 2.95
        assert!((metrics.write_amplification() - 2.95).abs() < f64::EPSILON);
        // With WAL: 4000 / 1000 = 4.0
        assert!((metrics.write_amplification_with_wal() - 4.0).abs() < f64::EPSILON);
    }
}
