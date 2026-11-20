#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MADINFER_LLAMA_MODEL:?Real model required; native tests cannot skip green}"
test -f "$MADINFER_LLAMA_MODEL"
# Workspace tests also include the independent first-party synthetic suite.
# Always prepare its inputs in this invocation's private temporary directory;
# use neither the pretrained model nor stale caller-provided synthetic assets.
MADINFER_SYNTHETIC_DIR=$(mktemp -d)
export MADINFER_SYNTHETIC_DIR
trap 'rm -rf -- "$MADINFER_SYNTHETIC_DIR"' EXIT
python3 scripts/generate-synthetic-gguf.py --output-dir "$MADINFER_SYNTHETIC_DIR" >/dev/null
python3 - <<'PYFIXTURES'
import hashlib, os
from pathlib import Path

path = Path(os.environ["MADINFER_SYNTHETIC_DIR"])
expected = {
    "A": "23696b2e27a65419bc4b81ddb49014063c09af369ea542188bb98b70c15ff9c2",
    "B": "1cfe72cce2f88fdae105088ad24574ff47421e1b8e9bdbcd569d7e8347cb3703",
}
for variant, digest in expected.items():
    data = (path / f"synthetic-{variant}.gguf").read_bytes()
    if hashlib.sha256(data).hexdigest() != digest:
        raise SystemExit("FAIL: generic native synthetic fixture digest")
a = (path / "synthetic-A.gguf").read_bytes()
(path / "bad-magic.gguf").write_bytes(b"BAD!" + a[4:])
(path / "truncated.gguf").write_bytes(a[:24])
if (path / "must-not-exist.gguf").exists():
    raise SystemExit("FAIL: generic native missing fixture unexpectedly present")
PYFIXTURES
# Real-model functional/golden tests share a finite CPU budget. Run test cases
# serially: separate contexts each have a per-context thread cap, not a global
# inference budget. Request deadlines and assertions remain unchanged. This
# gate does not establish concurrent-context latency or throughput guarantees.
cargo test --workspace --locked --features madinfer-ffi/llama-sys -- --test-threads=1
