# LSM-RS

> An LSM tree in Rust. Lock-free SkipList with MVCC, Kirsch-Mitzenmacher bloom filter, LEB128 encoding, two-level SSTables, K-way merge compaction, and durable WAL.

---

## 1. Architecture Overview

* **Memory Tier:** Lock-free, concurrent `SkipList` with Epoch-based memory reclamation (`crossbeam-epoch`).
* **Durability Tier:** Sync-safe Write-Ahead Log (WAL) with binary record framing for deterministic crash recovery.
* **Disk Tier:** Fixed-size prefix-compressed data blocks (LEB128 delta encoding) with restart arrays and $O(\log N + K)$ binary-searchable block iterators.
* **Index & Filter Tier:** Two-level SSTable index blocks with embedded XXH3 Kirsch-Mitzenmacher Bloom filters.
* **Compaction Engine:** Asynchronous Leveled Compaction orchestrator executing Min-Heap $K$-way merges across levels with safe tombstone incineration.

---

## 2. Concurrency & Isolation Model

* **Lock-Free Read Path:** Foreground queries traverse lock-free memory structures and immutable on-disk versions without acquiring global read/write locks.
* **Decoupled Background Orchestration:** Background compaction runs on dedicated threads referencing engine state via `Weak<DB>` pointers, preventing reference cycles and eliminating deadlock risks on shutdown.
* **Lock-Free Snapshot Isolation & GC:** Reads pin point-in-time `Arc<Version>` snapshots. When compactions commit new `VersionEdit` deltas, obsolete SSTables are unlinked only when `Weak<Version>::strong_count() == 0`, ensuring active readers are never interrupted by background deletions.

---

## 3. Data Lifecycle (The Path of a Key)

* **Write (`put` / `delete`):**
  1. Record appended to sequential `wal.log` with inverted LSN framing.
  2. Inverted LSN internal key (`[user_key] + [inverted_lsn] + [op_type]`) inserted into active `MemTable` via lock-free CAS.
  3. Automatic or explicit flush rotates active `MemTable` into immutable queue, flushes to Level 0 SSTable, syncs `manifest.log`, and signals compaction.

* **Read (`get`):**
  1. Probe active `MemTable` (returns latest version or explicit tombstone).
  2. Probe queued immutable `MemTables` (newest to oldest).
  3. Scan Level 0 SSTables in reverse chronological order via Bloom filter $\rightarrow$ two-level index $\rightarrow$ data block.
  4. Binary search across non-overlapping Level 1+ SSTables via file boundary metadata.

---

## 4. Academic & Research Roadmap

`LSM-RS` is architected as an empirical research platform to evaluate storage engine trade-offs under rigorous workloads:

* **H1 (Variance Decomposition):** Fractional factorial sweeps (compaction policy $\times$ size ratio $\times$ filter bits $\times$ cache policy) to decompose variance in write, read, and space amplification.
* **H2 (Filter Allocation & Topology Interaction):** Map where Monkey-style dynamic per-level bit allocations break down under leveled vs. tiered/lazy-leveling compaction.
* **H3 (Delete Persistence Pareto Frontier):** Characterize the write-amplification cost required to enforce strict tombstone lifespan bounds (GDPR/compliance triggers vs. Lethe-style age policies).
* **H4 (Compaction Churn & Caching):** Evaluate scan-resistant cache eviction algorithms (SIEVE, S3-FIFO vs. 2Q/LRU) against LSM compaction invalidation bursts with pinned vs. unpinned index/filter blocks.
* **H5 (Analytical Model Breakdown):** Measure divergence between classical write-amplification closed-form models and real empirical measurements under heavy Zipfian skew and upper-level overwrite collapse.
* **H6 (Benchmarking Methodology Sensitivity):** Map how component rankings shift depending on steady-state saturation, memtable-to-dataset ratios, and evaluation harness designs.
* **H7 (Range-Filter Crossover Map):** Identify the operational boundary where range filters (prefix Bloom / trie-based filters) yield net performance gains over point Bloom filters.

---

## 5. Building & Testing

```bash
# Run unit and integration tests
cargo test

# Run strict clippy analysis
cargo clippy
```
