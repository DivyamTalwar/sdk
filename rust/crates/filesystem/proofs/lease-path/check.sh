#!/usr/bin/env bash
set -euo pipefail

cargo +nightly-2026-07-01 kani \
  --manifest-path "$(dirname "${BASH_SOURCE[0]}")/Cargo.toml" --lib \
  --jobs --output-format terse --solver kissat -Z unstable-options \
  --cbmc-args --object-bits 10 --symex-cache-dereferences --slice-formula
