#!/bin/bash
set -e

rm -f final_results.log final_detailed.log

# Fixed Geometry: 512KB MemTable, 256KB SST, 4MB L1 target
GEOMETRY="--memtable-capacity 524288 --target-file-size 262144 --base-level-size 4194304"
WORKLOAD="-w 150000 -n 500000 -k 150000"

for SEED in 42 43 44 45 46; do
    echo "======================================" | tee -a final_results.log
    echo "          SEED $SEED                  " | tee -a final_results.log
    echo "======================================" | tee -a final_results.log

    # Arm 1: Skew Sweep
    for SKEW in 0.0 0.5 0.8 0.9 0.99; do
        echo "--> Skew $SKEW" | tee -a final_results.log
        cargo run --release --bin benchmark -- $WORKLOAD -s $SKEW --seed $SEED $GEOMETRY -d ./final_data 2>&1 | tee -a final_detailed.log | grep -E "WA|Total Compaction|Entries|Level|Distinct" | tee -a final_results.log
    done

    # Arm 2: Working-Set Control Arm (63,490 keys)
    echo "--> Uniform Control Arm (63,490 keys)" | tee -a final_results.log
    cargo run --release --bin benchmark -- $WORKLOAD -s 0.0 --uniform-range 63490 --seed $SEED $GEOMETRY -d ./final_data 2>&1 | tee -a final_detailed.log | grep -E "WA|Total Compaction|Entries|Level|Distinct" | tee -a final_results.log

    # Arm 2b: Working-Set Control Arm (77,042 keys for exact K=150k Zipf 0.99 match)
    echo "--> Uniform Control Arm (77,042 keys)" | tee -a final_results.log
    cargo run --release --bin benchmark -- $WORKLOAD -s 0.0 --uniform-range 77042 --seed $SEED $GEOMETRY -d ./final_data 2>&1 | tee -a final_detailed.log | grep -E "WA|Total Compaction|Entries|Level|Distinct" | tee -a final_results.log

    # Arm 3: No-Overwrite Control (Insert-Only)
    echo "--> Insert-Only Control" | tee -a final_results.log
    cargo run --release --bin benchmark -- $WORKLOAD -s 0.0 --insert-only --seed $SEED $GEOMETRY -d ./final_data 2>&1 | tee -a final_detailed.log | grep -E "WA|Total Compaction|Entries|Level|Distinct" | tee -a final_results.log
done

echo "======================================" | tee -a final_results.log
echo "ALL SEEDS AND ARMS COMPLETED" | tee -a final_results.log
echo "======================================" | tee -a final_results.log
