#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MADINFER_LLAMA_MODEL:?Real model required; native tests cannot skip green}"
test -f "$MADINFER_LLAMA_MODEL"
cargo test --workspace --locked --features madinfer-ffi/llama-sys
