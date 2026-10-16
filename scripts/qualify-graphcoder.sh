#!/usr/bin/env bash
set -euo pipefail
cargo test -p graphcoder --bin graphcoder
cargo test -p acyclic-harness-tui --features tui --test screen
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib task_volume::files::
# Reuse that exact native test binary for the LocalHarness binding cases.
test_binary=$(find /work/target/debug/deps -maxdepth 1 -type f -name 'acyclic_harness-*' -executable -printf '%T@ %p\n' | sort -nr | head -n 1 | cut -d ' ' -f 2-)
timeout 9 "$test_binary" local::tests::task_files::
cargo build -p acyclic-harness-local-fuzz --bin ui_prompt
timeout 8 /work/target/debug/ui_prompt rust/crates/harness/fuzz/corpus/ui_prompt -runs=64 -max_len=4096
