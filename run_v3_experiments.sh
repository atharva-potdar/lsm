#!/usr/bin/env bash
set -e

for skew in 0.0 0.5 0.8 0.9 0.99; do
    echo "======================================"
    echo "Running skew $skew"
    echo "======================================"
    cargo run --release --bin benchmark -- \
      -w 100000 -n 500000 -k 100000 -s $skew \
      --memtable-capacity 524288 \
      --target-file-size 262144 \
      --base-level-size 1048576 \
      -d ./v3_sweep_data
done 2>&1 | tee v3_sweep_results.log

echo "======================================"
echo "Running Uniform Control Arm (63,490 keys)"
echo "======================================"
cargo run --release --bin benchmark -- \
  -w 100000 -n 500000 -k 100000 -s 0.0 \
  --uniform-range 63490 \
  --memtable-capacity 524288 \
  --target-file-size 262144 \
  --base-level-size 1048576 \
  -d ./v3_control_data 2>&1 | tee v3_control_results.log
