#!/usr/bin/env bash
set -euo pipefail
artifact_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo test -p graphcoder --bin graphcoder
cargo test -p acyclic-harness-tui --features tui --test screen
# Select Cargo's exact native artifact, even when a newer Loom binary exists.
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py \
    task_volume::files:: local::tests::task_files:: local::tests::checkout:: \
    local::tests::volume_tools:: local::tests::checkout_reopen:: \
    local::tests::checkout_reconciliation:: local::tests::reconciliation_interruptions::

cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --test client_session documents::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --test context-discovery manifest::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --test default_tools
cargo build -p acyclic-harness-fuzz --bin ui_prompt
timeout 8 "$artifact_dir/debug/ui_prompt" rust/crates/harness-fuzz/corpus/ui_prompt -runs=64 -max_len=4096
RUSTFLAGS="--cfg acyclic_harness_loom --cfg acyclic_loom" cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py task_volume::files::models:: local::checkout_tools::tests::
