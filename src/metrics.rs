use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct EngineMetrics {
    pub bytes_written_user: AtomicU64,
    pub bytes_written_wal: AtomicU64,
    pub bytes_written_flush: AtomicU64,
    pub bytes_written_compaction: AtomicU64,
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
    pub fn write_amplification(&self) -> f64 {
        let user = self.user_bytes() as f64;
        if user == 0.0 {
            0.0
        } else {
            self.total_disk_bytes() as f64 / user
        }
    }

    pub fn reset(&self) {
        self.bytes_written_user.store(0, Ordering::Relaxed);
        self.bytes_written_wal.store(0, Ordering::Relaxed);
        self.bytes_written_flush.store(0, Ordering::Relaxed);
        self.bytes_written_compaction.store(0, Ordering::Relaxed);
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
        metrics.bytes_written_compaction.store(1850, Ordering::Relaxed);

        assert_eq!(metrics.user_bytes(), 1000);
        assert_eq!(metrics.total_disk_bytes(), 1050 + 1100 + 1850); // 4000
        assert!((metrics.write_amplification() - 4.0).abs() < f64::EPSILON);
    }
}
