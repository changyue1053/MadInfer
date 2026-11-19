#!/usr/bin/env bash
# Isolated exact-checkout CPU qualification. Never calls ci/native.sh/model fetch.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MADINFER_EXPECTED_SHA:?full reviewed candidate SHA required}"
[[ "$MADINFER_EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]]
test "$(git rev-parse HEAD)" = "$MADINFER_EXPECTED_SHA"
test -z "$(git status --porcelain)"
test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
# No foreign prebuilt library or target/compiler injection into this qualification.
for variable in MADINFER_LLAMA_LIB_DIR CARGO_BUILD_TARGET RUSTFLAGS CARGO_ENCODED_RUSTFLAGS; do
  test -z "${!variable:-}" || { echo "FAIL: unsupported build override" >&2; exit 1; }
done
export CARGO_TARGET_DIR="$PWD/target/synthetic-native"
export MADINFER_SYNTHETIC_DIR="$CARGO_TARGET_DIR/fixtures"
export MADINFER_LLAMA_CMAKE_JOBS=2
mkdir -p "$MADINFER_SYNTHETIC_DIR"
python3 -m unittest discover -s tests/synthetic -v
python3 scripts/generate-synthetic-gguf.py --output-dir "$MADINFER_SYNTHETIC_DIR"
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
path = pathlib.Path(os.environ['MADINFER_SYNTHETIC_DIR'])
expected = {'A': '23696b2e27a65419bc4b81ddb49014063c09af369ea542188bb98b70c15ff9c2',
            'B': '1cfe72cce2f88fdae105088ad24574ff47421e1b8e9bdbcd569d7e8347cb3703'}
for variant, digest in expected.items():
    assert hashlib.sha256((path / f'synthetic-{variant}.gguf').read_bytes()).hexdigest() == digest
assert not subprocess.check_output(['git', 'ls-files', '*.gguf']), 'no committed GGUF allowed'
a = (path / 'synthetic-A.gguf').read_bytes()
(path / 'bad-magic.gguf').write_bytes(b'BAD!' + a[4:])
(path / 'truncated.gguf').write_bytes(a[:24])
assert not (path / 'must-not-exist.gguf').exists()
print('QUALIFICATION_HEAD=' + subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip())
PY
cargo test -p madinfer-backend-llama --locked --features llama-sys --test synthetic_native -- --list > "$CARGO_TARGET_DIR/tests.list"
python3 - <<'PY'
import os, pathlib
actual = {line.removesuffix(': test') for line in
          (pathlib.Path(os.environ['CARGO_TARGET_DIR']) / 'tests.list').read_text().splitlines()
          if line.endswith(': test')}
expected = {'synthetic_a_real_decode', 'synthetic_b_real_decode',
            'synthetic_context_and_incremental_decode', 'synthetic_missing_and_corrupt_fail',
            'engine::thread_budget_tests::thread_hint_never_exceeds_available_cpus'}
assert actual == expected, (actual, expected)
PY
cargo test -p madinfer-backend-llama --locked --features llama-sys --test synthetic_native -- --test-threads=1
printf 'PASS: exact-SHA synthetic native CPU qualification (not inference quality)\n'
