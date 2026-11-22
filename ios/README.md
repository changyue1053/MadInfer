# iOS native backend integration/error-path preview

This source recipe builds a **real vendored llama.cpp CPU backend** into a dynamic
`MadInfer.xcframework`, exposing the existing C ABI v2. It is not a Swift SDK API,
model distribution or successful inference demo. No model weights are downloaded,
generated or bundled. First-party **Proprietary** licensing is unchanged; this
source candidate does not grant binary distribution rights.

## Scope and dependencies

- iOS **16.0** minimum, arm64 device + Apple-silicon arm64 Simulator only.
- Rust **1.100.0**, unchanged Cargo.lock, `standalone,llama-sys` with default
  features disabled (no debug script backend). Existing runtime can explicitly
  select emulation through the old API; this test deliberately selects native.
- Existing pinned llama.cpp `b10883` / `91f6a6cf361385700bbe15981f0f39909df77498`
  CPU subtree; no new upstream implementation, registry dependencies or model assets.
- Xcode **26.2 / 17C52**, SDK/runtime **26.2**, preinstalled CMake **4.4.5**.
- CPU-only, `GGML_NATIVE=OFF`, no Metal/Accelerate/BLAS/OpenMP. Uses system libc++.
  Apple architecture baseline is not a performance or cross-platform golden claim.

Xcode framework targets perform actual linking, generated plist processing and
resource copying. A linker export allowlist exposes only the 12 existing public
C ABI entry points. Rust, ggml, llama and UniFFI internals are not public exports.
The framework includes original first-party and third-party notices, not source
redistribution or model licenses. Consumer uses only the final binary/header.

## Build gates

On an already prepared Apple-silicon host with the pinned tools and target
Cargo cache, run `bash scripts/build-ios.sh`. That script refuses a prebuilt
engine override or existing archives, compiles both targets offline, and
checks the packaged framework/platform/arm64/header/symbols/notices.

Build `ios/Smoke/MadInferSmoke.xcodeproj` for generic iOS with
`CODE_SIGNING_ALLOWED=NO` for an unsigned device **link** only.
`bash scripts/test-ios-simulator.sh` runs the original installed ABI/missing/
corrupt fixture in a disposable iOS 26.2 Simulator. See
[qualification](../docs/verification/QUALIFIED_SOURCE.md) for the synthetic,
standalone lifecycle and optional app-owned modes. No device execution,
credentials, provisioning, upload or dependency acquisition by these scripts.

## Lifecycle and evidence limits

The C API still requires externally synchronized, exactly-once raw-handle destroy;
it does **not** inherit MadDB's stale-token safety. Callbacks must return promptly,
copy borrowed buffers before returning and never destroy synchronously from within
a callback. Hosts own their callback context until destroy finishes. This preview
is not a production wrapper or an aggregate memory/concurrency budget.

The native backend lazily loads model tensors. Selecting/querying backend `1`
proves configuration, not successful model loading. Missing model rejection
(`4003`, not feature-disabled `4005`) and any corrupt-file error test do not prove
prefill/decode, model quality, peak memory, performance or real inference. No
production signature is accepted by the test host; verification rejects input.

The original three error tests alone are not a decode result; the separate
positive suite and lifecycle material retain the historical exact checkpoint
evidence described in the qualification guide. No new Apple run on this
integrated tree is represented.

References: [Apple XCFramework guidance](https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle),
[official runner inventory](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md),
[backend contract](../README.md), [original license](../LICENSE).

### Exact XCTest evidence and output boundary

The simulator gate requires both `xcodebuild` success and Xcode 26.2
`xcresulttool get test-results` summary + full named test tree: exactly three
passes, zero failures/skips, and the three `MadInferSmokeTests` methods in the
consumer source, each explicitly passed. Missing, duplicate, extra, unexpected,
or incomplete case results fail closed. The optional summary `testStatus` may
be absent; if present it must be `Passed`. Counts alone never establish a pass.
Synthetic parser regressions do not execute iOS or prove native inference.

Preview CI keeps raw tool output and xcresult bundles only in the ephemeral
runner workspace, with no artifact upload. Console output from source/build/
installer stages is fixed stage labels; the simulator parser emits only fixed
allowlisted method labels/counts or fixed errors. No raw JSON, compiler output,
environment values, backend diagnostics or storage paths are published by these
steps. Local developers may inspect local logs without distributing them.
