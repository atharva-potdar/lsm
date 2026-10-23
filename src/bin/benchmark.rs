use clap::Parser;
use lsm::db::{DB, FilterPolicy};
use rand::RngExt;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(
    name = "benchmark",
    about = "LSM-RS Telemetry & Zipfian Workload Benchmark"
)]
struct Args {
    /// Total number of write operations to execute
    #[arg(short = 'n', long, default_value_t = 1_000_000)]
    num_keys: usize,

    /// Zipfian skew parameter theta (0.0 = Uniform, 0.99 = Heavy Zipfian)
    #[arg(short = 's', long, default_value_t = 0.0)]
    skew: f64,

    /// Size of each value in bytes
    #[arg(short = 'v', long, default_value_t = 1024)]
    value_size: usize,

    /// Memtable capacity in bytes
    #[arg(short = 'm', long, default_value_t = 4 * 1024 * 1024)]
    memtable_capacity: usize,

    /// Directory for LSM data files
    #[arg(short = 'd', long, default_value = "./benchmark_data")]
    work_dir: PathBuf,

    /// Number of ghost read misses to execute
    #[arg(short = 'r', long, default_value_t = 0)]
    read_misses: usize,

    /// Filter policy: "static" (10 bits/key) or "monkey" (L0=20, L1=15, L2=10, L3+=5)
    #[arg(short = 'p', long, default_value = "static")]
    filter_policy: String,
}

/// Classic YCSB / Gray Zipfian distribution generator.
pub struct ZipfGenerator {
    n: u64,
    theta: f64,
    alpha: f64,
    zetan: f64,
    eta: f64,
}

impl ZipfGenerator {
    pub fn new(n: u64, theta: f64) -> Self {
        assert!(n > 0);
        let alpha = 1.0 / (1.0 - theta);
        let zetan = Self::zeta(n, theta);
        let zeta2 = Self::zeta(2, theta);
        let eta = (1.0 - (2.0 / n as f64).powf(1.0 - theta)) / (1.0 - zeta2 / zetan);
        Self {
            n,
            theta,
            alpha,
            zetan,
            eta,
        }
    }

    fn zeta(n: u64, theta: f64) -> f64 {
        let mut sum = 0.0;
        let limit = n.min(10_000);
        for i in 1..=limit {
            sum += 1.0 / (i as f64).powf(theta);
        }
        if n > 10_000 {
            sum += ((n as f64).powf(1.0 - theta) - (10_000.0_f64).powf(1.0 - theta))
                / (1.0 - theta);
        }
        sum
    }

    pub fn next(&self, rng: &mut impl rand::Rng) -> u64 {
        let u: f64 = rng.random::<f64>();
        let uz = u * self.zetan;
        if uz < 1.0 {
            0
        } else if uz < 1.0 + 0.5_f64.powf(self.theta) {
            1
        } else {
            let val = (self.n as f64 * (self.eta * u - self.eta + 1.0).powf(self.alpha)) as u64;
            val.min(self.n - 1)
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("================================================================================");
    println!("                    LSM-RS TELEMETRY & WORKLOAD BENCHMARK                       ");
    println!("================================================================================");
    println!("Configuration:");
    println!("  Operations:          {:>12}", format_number(args.num_keys));
    println!("  Key Space:           {:>12} keys", format_number(args.num_keys));
    println!("  Value Size:          {:>12} bytes", format_number(args.value_size));
    println!(
        "  Distribution:        {:>12} (skew θ = {:.2})",
        if args.skew == 0.0 { "Uniform" } else { "Zipfian" },
        args.skew
    );
    println!(
        "  Filter Policy:       {:>12}",
        if args.filter_policy.to_lowercase() == "monkey" {
            "Monkey (L0=20, L1=15, L2=10, L3+=5)"
        } else {
            "Static (10 bits/key)"
        }
    );
    println!("  MemTable Capacity:   {:>12.2} MB", args.memtable_capacity as f64 / 1_048_576.0);
    println!("  Working Directory:   {:>12?}", args.work_dir);
    println!("--------------------------------------------------------------------------------");

    // Clean up previous runs
    let _ = std::fs::remove_dir_all(&args.work_dir);
    std::fs::create_dir_all(&args.work_dir)?;

    let policy = match args.filter_policy.to_lowercase().as_str() {
        "monkey" => FilterPolicy::Monkey,
        _ => FilterPolicy::Static(10),
    };

    let db = DB::open_with_policy(&args.work_dir, args.memtable_capacity, policy)?;
    let metrics = db.metrics();

    let zipf = if args.skew > 0.0 {
        Some(ZipfGenerator::new(args.num_keys as u64, args.skew))
    } else {
        None
    };

    let mut rng = rand::rng();
    let value = vec![b'x'; args.value_size];

    println!("Executing workload...");
    let start_time = Instant::now();
    let report_interval = (args.num_keys / 10).max(1);

    for i in 1..=args.num_keys {
        let key_idx = if let Some(ref z) = zipf {
            z.next(&mut rng)
        } else {
            rng.random_range(0..args.num_keys as u64)
        };

        let key = format!("user_{:010}", key_idx * 2);
        db.put(key.as_bytes(), &value)?;

        if i % report_interval == 0 || i == args.num_keys {
            let pct = (i as f64 / args.num_keys as f64) * 100.0;
            let elapsed = start_time.elapsed().as_secs_f64();
            let ops_sec = i as f64 / elapsed;
            print!("\rProgress: [{:>6.1}%]  {:>10}/{:<10}  ({:>8.0} ops/sec)", pct, i, args.num_keys, ops_sec);
            std::io::Write::flush(&mut std::io::stdout())?;
        }
    }
    println!();

    let write_elapsed = start_time.elapsed().as_secs_f64();
    let write_ops_sec = args.num_keys as f64 / write_elapsed;
    let write_mb_sec = (metrics.user_bytes() as f64 / 1_048_576.0) / write_elapsed;

    println!("\nWorkload completed in {:.2}s ({:.0} ops/sec, {:.2} MB/s user throughput).",
             write_elapsed, write_ops_sec, write_mb_sec);

    println!("Flushing remaining active memtable and awaiting background compactions...");
    let settle_start = Instant::now();
    db.force_flush()?;
    db.wait_for_compaction();
    let settle_elapsed = settle_start.elapsed().as_secs_f64();
    println!("Tree settled in {:.2}s.", settle_elapsed);

    let version = db.current_version();
    println!("LSM-Tree Physical Hierarchy:");
    for (lvl, files) in version.files.iter().enumerate() {
        if !files.is_empty() {
            let size: u64 = files.iter().map(|f| f.file_size).sum();
            println!("  Level {}: {:>3} files, {:>8.2} MB", lvl, files.len(), size as f64 / 1_048_576.0);
        }
    }

    let total_elapsed = start_time.elapsed().as_secs_f64();

    let user_bytes = metrics.user_bytes();
    let wal_bytes = metrics.bytes_written_wal.load(std::sync::atomic::Ordering::Relaxed);
    let flush_bytes = metrics.bytes_written_flush.load(std::sync::atomic::Ordering::Relaxed);
    let comp_bytes = metrics.bytes_written_compaction.load(std::sync::atomic::Ordering::Relaxed);
    let total_disk = metrics.total_disk_bytes();
    let wa = metrics.write_amplification();

    println!("--------------------------------------------------------------------------------");
    println!("Telemetry & Write Amplification:");
    println!("  User Bytes Written:       {:>10.2} MB  ({} bytes)", user_bytes as f64 / 1_048_576.0, format_number(user_bytes as usize));
    println!("  WAL Bytes Written:        {:>10.2} MB  ({} bytes)", wal_bytes as f64 / 1_048_576.0, format_number(wal_bytes as usize));
    println!("  Flush Bytes Written:      {:>10.2} MB  ({} bytes)", flush_bytes as f64 / 1_048_576.0, format_number(flush_bytes as usize));
    println!("  Compaction Bytes Written: {:>10.2} MB  ({} bytes)", comp_bytes as f64 / 1_048_576.0, format_number(comp_bytes as usize));
    println!("  Total Disk Bytes Written: {:>10.2} MB  ({} bytes)", total_disk as f64 / 1_048_576.0, format_number(total_disk as usize));
    println!("--------------------------------------------------------------------------------");
    println!("  Final Write Amplification (WA): {:.4}", wa);
    println!("  Total Elapsed Time:             {:.2}s", total_elapsed);
    println!("================================================================================");

    if args.read_misses > 0 {
        println!("\n================================================================================");
        println!("                    PHASE 2: GHOST READ-MISS BENCHMARK                          ");
        println!("================================================================================");
        println!("Executing {} ghost read-miss queries...", format_number(args.read_misses));

        metrics.bytes_read_disk.store(0, std::sync::atomic::Ordering::Relaxed);
        let read_start = Instant::now();
        let read_interval = (args.read_misses / 10).max(1);

        for i in 1..=args.read_misses {
            let ghost_idx = rng.random_range(0..args.num_keys as u64);
            let ghost_key = format!("user_{:010}", ghost_idx * 2 + 1);
            let res = db.get(ghost_key.as_bytes())?;
            debug_assert!(res.is_none());

            if i % read_interval == 0 || i == args.read_misses {
                let pct = (i as f64 / args.read_misses as f64) * 100.0;
                let elapsed = read_start.elapsed().as_secs_f64();
                let ops_sec = i as f64 / elapsed;
                print!(
                    "\rRead Progress: [{:>6.1}%]  {:>10}/{:<10}  ({:>8.0} ops/sec)",
                    pct, i, args.read_misses, ops_sec
                );
                std::io::Write::flush(&mut std::io::stdout())?;
            }
        }
        println!();

        let read_elapsed = read_start.elapsed().as_secs_f64();
        let read_ops_sec = args.read_misses as f64 / read_elapsed;
        let bytes_read = metrics.bytes_read_disk.load(std::sync::atomic::Ordering::Relaxed);
        let mb_read = bytes_read as f64 / 1_048_576.0;
        let avg_bytes = bytes_read as f64 / args.read_misses as f64;

        println!("--------------------------------------------------------------------------------");
        println!("Ghost Read-Miss Results:");
        println!("  Total Queries:            {:>12}", format_number(args.read_misses));
        println!("  Elapsed Time:             {:>12.2}s", read_elapsed);
        println!("  Read Throughput:          {:>12.0} ops/sec", read_ops_sec);
        println!("  Disk Bytes Read:          {:>12.2} MB  ({} bytes)", mb_read, format_number(bytes_read as usize));
        println!("  Avg Disk Read per Query:  {:>12.2} bytes/op", avg_bytes);
        println!("================================================================================");
    }

    // Clean up
    let _ = std::fs::remove_dir_all(&args.work_dir);

    Ok(())
}

fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let mut count = 0;
    for c in s.chars().rev() {
        if count > 0 && count % 3 == 0 {
            result.push(',');
        }
        result.push(c);
        count += 1;
    }
    result.chars().rev().collect()
}
