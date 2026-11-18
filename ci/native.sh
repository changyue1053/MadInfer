#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MADINFER_LLAMA_MODEL:?Real model required; native tests cannot skip green}"
test -f "$MADINFER_LLAMA_MODEL"
# Real-model functional/golden tests share a finite CPU budget. Run test cases
# serially: separate contexts each have a per-context thread cap, not a global
# inference budget. Request deadlines and assertions remain unchanged. This
# gate does not establish concurrent-context latency or throughput guarantees.
cargo test --workspace --locked --features madinfer-ffi/llama-sys -- --test-threads=1
