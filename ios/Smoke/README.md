# MadInfer independent iOS binary consumer

The app and hosted XCTest target consume only `../../build/ios/MadInfer.xcframework`.
The app embeds the dynamic framework; both targets link it. `import MadInfer` and
`@import MadInfer` use the framework's public C module and its bundled `madinfer.h`.
No Rust source, source checkout header search path, producer target, package, or
external library is attached to this consumer. The Objective-C helper is test-host
code, exposed to Swift by a local bridging header, not a substitute implementation.

Minimum iOS 16, arm64 device/simulator, Swift 5. Regenerate the project from the
repository root with `python3 ios/Smoke/generate_project.py`.

## Three XCTest contracts

1. ABI handshake returns exactly 2.
2. `madinfer_core_create_with_backend(..., 1, missingPath, ...)` returns null and
   exactly 4003. Feature-disabled 4005 is a failure, never accepted as a skip.
3. An existing small **non-model text file** permits lazy core creation; querying
   `madinfer_backend_kind` returns 1. Starting a stream reaches exactly one
   asset-corrupt failure (3005), with no deltas, successful completion, or cloud
   authorization. No model weights are downloaded, created, or used.

All required host callbacks and a nonempty release trusted-public-key table are
provided. Signature verification always rejects, never trusts all. No signed
configuration is applied. Host KV storage is in-memory and lock-protected. Native
checks run on a background queue without UI callback dependencies. Callbacks only
record scalar results or provide bounded host services; they never destroy the
core. Each successfully created raw handle is destroyed exactly once by its owner,
after which callback state and temporary files can be released. There are no stale
raw-handle probes, borrowed event pointers retained, or cloud/telemetry uploads.

## Commands from the repository root

After separately building the final XCFramework, unsigned device compile/link:

```sh
xcodebuild -project ios/Smoke/MadInferSmoke.xcodeproj \
  -scheme MadInferSmoke -configuration Release \
  -destination 'generic/platform=iOS' \
  -derivedDataPath build/ios/consumer-device \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO build
```

On an authorized Apple Silicon simulator, using its explicit UDID:

```sh
xcodebuild -project ios/Smoke/MadInferSmoke.xcodeproj \
  -scheme MadInferSmoke -configuration Debug \
  -destination "platform=iOS Simulator,id=$MADINFER_SIMULATOR_UDID" \
  -derivedDataPath build/ios/consumer-simulator \
  -resultBundlePath build/ios/consumer-tests.xcresult \
  -parallel-testing-enabled NO ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=NO test
```

Use a fresh result-bundle path for repeat runs. Simulator signing is local ad-hoc
signing, without credentials. The generic device build is not a device installation.

## Evidence limits

Project references and source contracts were checked statically on Linux. Apple
SDK compilation, simulator installation and XCTest execution have **not** been
performed here. Even a future green run proves only binary integration, native
backend selection and model-load rejection. It does **not** prove successful
inference, model quality, performance, minimum-iOS execution, or physical-device
behavior. The 10-second callback wait and 30-second XCTest wait are failure guards,
not a guarantee that a faulty native shutdown cannot block.
