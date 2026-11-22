#!/usr/bin/env bash
# Isolated exact-checkout CPU qualification. Never calls ci/native.sh/model fetch.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MADINFER_EXPECTED_SHA:?full reviewed candidate SHA required}"
[[ "$MADINFER_EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]]
test "$(git rev-parse HEAD)" = "$MADINFER_EXPECTED_SHA"
test -z "$(git status --porcelain)"
test "$(uname -s)" = Darwin
test "$(uname -m)" = arm64
# No foreign prebuilt library or target/compiler injection into this qualification.
for variable in MADINFER_LLAMA_LIB_DIR CARGO_BUILD_TARGET RUSTFLAGS CARGO_ENCODED_RUSTFLAGS; do
  test -z "${!variable:-}" || { echo "FAIL: unsupported build override" >&2; exit 1; }
done
python3 - <<'PY'
import hashlib, os, pathlib, subprocess
root = pathlib.Path('third_party/llama.cpp')
lines = b''.join(hashlib.sha256(p.read_bytes()).hexdigest().encode() + b'  ./' +
                 p.relative_to(root).as_posix().encode() + b'\n'
                 for p in sorted((p for p in root.rglob('*') if p.is_file()),
                                 key=lambda p: p.relative_to(root).as_posix().encode()))
assert hashlib.sha256(lines).hexdigest() == '6b48c4a979aa11de3018a13c176c4c648521d534afefa7c552148e89ec4a68b2', 'vendored engine drift'
pins = pathlib.Path('third_party/PINS.toml').read_text()
assert 'commit = "91f6a6cf361385700bbe15981f0f39909df77498"' in pins
assert 'tag = "b10883"' in pins
assert not subprocess.check_output(['git', 'ls-files', '*.gguf']), 'no committed GGUF allowed'
print('QUALIFICATION_HEAD=' + subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip())
PY
