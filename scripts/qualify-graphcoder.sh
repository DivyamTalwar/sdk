#!/usr/bin/env bash
set -euo pipefail
artifact_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo test -p graphcoder --bin graphcoder
cargo test -p acyclic-harness-tui --features tui --test screen
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib task_volume::files::
# Reuse that exact native test binary for the LocalHarness binding cases.
test_binary=$(find "$artifact_dir/debug/deps" -maxdepth 1 -type f -name 'acyclic_harness-*' -executable -printf '%T@ %p\n' | sort -nr | sed -n '1s/^[^ ]* //p')
timeout 9 "$test_binary" local::tests::task_files::
cargo build -p acyclic-harness-local-fuzz --bin ui_prompt
timeout 8 "$artifact_dir/debug/ui_prompt" rust/crates/harness/fuzz/corpus/ui_prompt -runs=64 -max_len=4096
RUSTFLAGS="--cfg acyclic_harness_loom --cfg acyclic_loom" cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib task_volume::files::models::
