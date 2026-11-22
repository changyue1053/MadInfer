# Qualified Linux and iOS source

The standalone checkpoints below are retained as source, deterministic fixture
generators, and explicit local verification helpers. No hardware or production
release qualification is implied, and the checkpoint results are for their exact
public commits, not automatically for another source tree.

| Exact prior checkpoint | Observed scope |
| --- | --- |
| [Linux native d081496](https://github.com/changyue1053/MadInfer/actions/runs/44493346592) | Five required real CPU native tests; deterministic first-party A/B fixtures and context/invalid-input checks. |
| [iOS synthetic 56db997](https://github.com/changyue1053/MadInfer/actions/runs/44494776064) | On an ARM64 Simulator, original three plus two real CPU A/B checks. Unsigned generic iOS device link only. |
| [iOS lifecycle e621a78](https://github.com/changyue1053/MadInfer/actions/runs/44735061480) | Six installed native Simulator tests including distinct explicit cancellation and joined pending-authorization teardown, with no cloud grant/transport. |
| [iOS optional 047d7c1](https://github.com/changyue1053/MadInfer/actions/runs/44588960851) | Separate installed MadInfer dynamic XCFramework and optional MadObs static runtime; OFF/ON five each. |
| [iOS app-owned a6b0e57](https://github.com/changyue1053/MadInfer/actions/runs/44602259653) | App-owned equivalent, OFF/ON five each. |
| [iOS app-owned cancellation 09001d2](https://github.com/changyue1053/MadInfer/actions/runs/44814470054) | OFF/ON six each, including cancellation; ON app drains two fixed local post-join statuses, no network export, concurrent teardown or payload. |

## Explicit local use

- Linux: with existing Rust 1.100.0, CMake, C/C++ tools and cached locked
  dependencies, run `MADINFER_EXPECTED_SHA=$(git rev-parse HEAD) bash
  ci/synthetic-native.sh` from a clean checkout. No pretrained model, no fixture
  downloads and no committed GGUF. A missing prerequisite fails rather than
  substitutes an engine. Rust/Cargo dependency builds are locked and offline.
- Apple: use a clean Apple-silicon host with already installed Xcode
  26.2/17C52 (both SDKs and Simulator runtime 26.2), Rust 1.100.0 with both ARM64
  iOS targets, CMake 4.4.5, and the exact locked dependencies already cached.
  [iOS build instructions](../../ios/README.md) describe the baseline. For
  positive standalone use `python3 -B scripts/prepare-ios-positive.py` then
  `bash scripts/test-ios-positive-simulator.sh` after the framework build. For
  standalone lifecycle substitute the two lifecycle scripts.
- App-owned OFF: on the same prepared Apple host and fresh output paths, run
  `bash ios-app-obs-lifecycle/run.sh off`; it uses no MadObs source or archive.
  ON requires a **disposable**, clean, separately prepared local checkout
  of MadObs `4af77ffa464a48307e5ff2d95f10b835919d205b` at
  `$PWD/build/ios/obs-producer`, tree
  `3a03801beb18c9601630cc4421b95c2743018dfb` with the expected official origin and
  no checkout-auth override. Run
  `MADINFER_DISPOSABLE_MADOBS="$PWD/build/ios/obs-producer" bash
  ios-app-obs-lifecycle/run.sh on`. The installer intentionally removes that
  disposable source and its private build/install directories before independent
  consumption. Never place a working checkout there. Both modes use the original
  explicit test receipt and selected binary closure validation with no device
  runtime, accounts, telemetry export or uploads.

No workflow changes accompany this source. The existing generic main native CI
still acquires the previously configured pretrained test model; this source adds
only hash-checked local synthetic fixtures to that existing workspace test run.
Default consumers acquire no new dependency. Source license and vendored
notice files remain unchanged.
