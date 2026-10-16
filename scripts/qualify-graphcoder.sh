#!/usr/bin/env bash
set -euo pipefail
artifact_dir=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo test -p graphcoder --bin graphcoder
cargo test -p acyclic-harness-tui --features tui --test screen
# Select Cargo's exact native artifact, even when a newer Loom binary exists.
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib --no-run --message-format=json |
python3 -c '
import json, subprocess, sys
executable = None
for line in sys.stdin:
    message = json.loads(line)
    if message.get("reason") == "compiler-message":
        rendered = message["message"].get("rendered")
        if rendered:
            print(rendered, end="", file=sys.stderr)
    if (message.get("reason") == "compiler-artifact"
            and message.get("profile", {}).get("test")
            and "lib" in message["target"]["kind"]
            and message.get("executable")):
        executable = message["executable"]
if executable is None:
    sys.exit("Cargo did not produce the native Harness test executable")
for selected in ["task_volume::files::", "local::tests::task_files::"]:
    subprocess.run([executable, selected], check=True, timeout=9)
'
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --test client_session documents::
cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --test context-discovery manifest::
cargo build -p acyclic-harness-local-fuzz --bin ui_prompt
timeout 8 "$artifact_dir/debug/ui_prompt" rust/crates/harness/fuzz/corpus/ui_prompt -runs=64 -max_len=4096
RUSTFLAGS="--cfg acyclic_harness_loom --cfg acyclic_loom" cargo test -p acyclic-harness --features filesystem-local,native-execution,terminal,tui,test-support --lib task_volume::files::models::
