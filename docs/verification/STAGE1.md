# Stage 1: standalone source preview and native host evidence

Date: 2026-11-18. Source IMInfer main `2d420a75e6771afb1959ef599062aea620d1902a`; chosen independently, not asserted equal to historical mad-kernel import. Toolchain Linux x86_64, Rust 1.100.0, GCC 14.2.0, CMake 4.4.5.

## Executed checks

- Original selected source default workspace suite: **312 passed**, zero failed/ignored. No model inference in this baseline run.
- Renamed/patched default suite: **315 passed**, zero failed/ignored. Added explicit C backend-kind selection/query, unavailable-native fail-closed, and timed-out-verifier/destroy lifetime regression.
- `bash ci/verify.sh`: pass (fmt, default workspace tests, clippy `-D warnings`, FFI build, compiled and executed C consumer, source hygiene scan, dependency inventory). Cargo 1.100 emits informational manifest warnings about inherited unused workspace dependencies; Rust/clippy checks pass, including a separate native-feature all-targets clippy run.
- Actual llama.cpp native build: pass, no stub engine. Model bytes 105454432 and SHA256 `2e8040ceae7815abe0dcb3540b9995eaa1fa0d2ca9e797d0a635ae4433c68c2d` verified.
- Real-model workspace suite: **331 passed**, zero failed/ignored. Includes core-to-FFI generation, backend load/stream/repeatability/cancel/resource tests, native C backend classification as configured. Backend native integration suite repeated separately: **9 passed**.
- Missing-model `ci/native.sh`: nonzero failure as required. All three native model test entry helpers fail when model absent. No skip-as-green.
- Exact 136 first-party source Git blobs and 373 llama.cpp vendor Git blobs match selected baseline and official pinned commit. 85 resolved packages have license metadata; applicable license texts/notices retained. Lightweight secret-pattern scan found zero candidates; no model weights or build artifacts tracked. Not a comprehensive security audit.

## Failures investigated, not concealed

1. Renaming a literal xxhash golden input initially changed its expected bucket. Restored original test input (`iminfer`), preserving the original algorithm/vector.
2. Original model downloader used BSD `stat -f`, which printed filesystem information on Linux. Replaced with portable `wc -c`; actual checksum and byte count verified.
3. Independent review found detached verifier callbacks could outlive C host context after destroy. All seven HostBridge callback entry paths now hold a close/drain ticket. A blocking-verifier regression confirms destroy waits until callback returns. Destroy can wait indefinitely for an uncooperative host; no bounded-shutdown claim.
4. Native output on Linux did **not** match inherited macOS ARM golden. The old ARM vector remains untouched apart from namespace comments and is NOT validated here. Added `llama_linux_x86_64.txt` scoped to exact engine/model/toolchain/CPU configuration, then reran without update mode; token/text equality and load/context/thread repeatability pass. Removed auto-write-golden test bypass. This is a platform regression vector, NOT proof of cross-platform equality or model quality.
5. Rust 1.100 clippy flagged inherited style patterns. Applied semantics-preserving while-let, derived Default and checked_div cleanups, plus a test-only mutex lint allowance for global diagnostics serialization.

## Not complete / release blockers

- Bounded callback delivery queue remains pending; control-lane callbacks must enqueue immediately. Slow arbitrary callbacks can block progress.
- C handles remain externally synchronized, exactly-once raw handles. Generation-safe stale/double-destroy support not implemented.
- MadObs adapter not implemented; host telemetry is optional and does not link MadObs.
- Android/iOS/OHOS packages, native device tests, foreground/background resource controls and multi-SDK link matrix NOT run.
- Independent security/model-distribution/legal audit and binary-release SBOM/signatures NOT complete. This source-stage publication is not GA or G3/G4 completion.

## Post-publication CPU-budget regression

The first public CI run (44307614828) passed emulated checks and all nine native
backend tests, but a native FFI determinism test timed out in its unchanged
five-second event wait. Locally, the published binary under Linux two-CPU
affinity reproduced failures in both real-model FFI tests in three consecutive
full FFI runs (46/48 passed; 28.57, 28.58 and 31.91 seconds). An isolated serial
native run also failed, so test concurrency alone was not the fix.

The scheduler's four-thread default was forwarded unbounded to GGML even when
only two CPUs were available. Native context creation now caps the hint to
`std::thread::available_parallelism()` (one thread on query failure). This is a
per-context resource bound, not a process-wide concurrency limiter. Affinity or
quota changes after context creation are not dynamically re-applied.

With only that resource-bound change, the same two-CPU parallel full FFI suite
passed 48/48 three times (2.03, 2.11 and 2.03 seconds). Neither the five-second
test wait nor the ten-second request deadline nor golden assertions changed.
Final local verification: default **315 passed**; entire native workspace under
two-CPU affinity **332 passed**, zero failures/ignored; native-feature clippy
with `-D warnings` passed. One new test exhaustively checks all u8 thread hints
against representative capacities including zero and usize::MAX. These local
results do not by themselves establish the follow-up remote CI outcome.
