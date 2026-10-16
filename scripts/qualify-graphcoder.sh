#!/usr/bin/env bash
set -euo pipefail
artifact_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo test -p graphcoder --features acyclic-harness/grpc --bin graphcoder
cargo test -p acyclic-harness-tui --features tui --test screen
# Select Cargo's exact native artifact, even when a newer Loom binary exists.
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --lib --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py \
    contract::errors::tests:: task_volume::files:: local::tests::task_files:: local::tests::checkout:: \
    local::tests::volume_tools:: local::tests::volumes:: local::tests::checkout_reopen:: \
    local::tests::checkout_reconciliation:: local::tests::reconciliation_interruptions:: \
    local::checkout_tools::publisher::tests:: tools::git::view::tests::

cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --test client_session --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py documents:: views:: jsonl_tests::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --test context-discovery manifest::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --test default_tools
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --test git_executor facade_commits_branches_merges_and_inspects_managed_files
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --test main saved_tool_metadata::
cargo build -p acyclic-harness-fuzz --bin ui_prompt
timeout 8 "$artifact_dir/debug/ui_prompt" rust/crates/harness-fuzz/corpus/ui_prompt -runs=64 -max_len=4096
RUSTFLAGS="--cfg acyclic_harness_loom --cfg acyclic_loom" cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc --lib --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py task_volume::files::models:: local::checkout_tools::tests::
