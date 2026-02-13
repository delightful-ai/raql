#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export RAQL_CONFORMANCE=1
if [[ -z "${RAQL_CONFORMANCE_CACHE_ROOT:-}" ]]; then
  export RAQL_CONFORMANCE_CACHE_ROOT="${TMPDIR:-/tmp}/raql-conformance-cache"
fi
mkdir -p "$RAQL_CONFORMANCE_CACHE_ROOT"

cargo test \
  --release \
  -p raql-host-ra \
  conformance_corpus_runtime_gate \
  -- \
  --ignored \
  --nocapture \
  --test-threads=1
