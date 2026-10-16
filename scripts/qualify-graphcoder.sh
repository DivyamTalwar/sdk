#!/usr/bin/env bash
set -euo pipefail
artifact_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo test -p graphcoder --features acyclic-harness/grpc,acyclic-harness/search-http --bin graphcoder
cargo test -p acyclic-harness-tui --features tui --test screen
# Retain Cargo's exact native artifact across the separate instrumented build.
native_artifact=$(mktemp "$artifact_dir/graphcoder-native.XXXXXX.jsonl")
trap 'rm -f "$native_artifact"' EXIT
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --lib --no-run --message-format=json > "$native_artifact"
python3 scripts/qualify-graphcoder-artifact.py \
    contract::errors::tests:: task_volume::files:: task_volume::segment_tests:: executor::tests::invocation_bounds:: local::tests::task_files:: local::tests::documents:: \
    local::checkout_tools::publisher::tests:: tools::git::view::tests:: < "$native_artifact"

cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --test client_session --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py documents:: views:: jsonl_tests::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --test context-discovery manifest::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --test default_tools
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --test git_executor facade_commits_branches_merges_and_inspects_managed_files
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --test main saved_tool_metadata::
cargo build -p acyclic-harness-fuzz --bin ui_prompt
timeout 8 "$artifact_dir/debug/ui_prompt" rust/crates/harness-fuzz/corpus/ui_prompt -runs=64 -max_len=4096
RUSTFLAGS="--cfg acyclic_harness_loom --cfg acyclic_loom" cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support,grpc,search-http --lib --no-run --message-format=json |
python3 scripts/qualify-graphcoder-artifact.py task_volume::files::models:: local::checkout_tools::tests::

# Dependency integration gates use the same already-built native binary.
python3 scripts/qualify-graphcoder-artifact.py \
    local::tests::checkout:: local::tests::volume_tools:: local::tests::volumes:: \
    local::tests::checkout_reopen:: local::tests::checkout_reconciliation:: \
    local::tests::reconciliation_interruptions:: < "$native_artifact"
