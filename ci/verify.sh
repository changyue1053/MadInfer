#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build -p madinfer-ffi --locked
cc -std=c11 -Wall -Wextra -Werror -Icrates/madinfer-ffi/include samples/c/probe.c -Ltarget/debug -lmadinfer_ffi -Wl,-rpath,"$PWD/target/debug" -o target/c-probe
target/c-probe
cargo metadata --locked --format-version 1 > target/dependency-inventory.json
python3 ci/public_audit.py
