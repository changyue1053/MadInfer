# Optional installed iOS composition: consumer-source checkpoint

This test-only consumer source retains the already qualified checkpoint. No release claim.
It composes the existing **dynamic MadInfer XCFramework** with an optional
**static MadObs installed archive**. It is not a dual-static framework proof.

## Frozen inputs for subsequent orchestration

- MadInfer `56db9979cfe3f8710a265b808178ebee3dc8506e`, tree
  `26f296efd7234902e239a8dd8cc4f77fe87a1f25`.
- MadObs `4af77ffa464a48307e5ff2d95f10b835919d205b`, tree
  `3a03801beb18c9601630cc4421b95c2743018dfb`.
- Reference only: MadDB `28d28fe98f4573d6d9e99b7cd344b7aec25d4a3a`, tree
  `9557064204ba3aa1a33da3a1c97e554d55e3fffc`.

No production code, ABI, backend, default dependency, original framework build,
original XCTest or workflow is modified. No binary/fixture is committed.

## Two isolated consumer modes

`prepare.py --mode off --dest <fresh-consumer>` calls the unchanged positive
consumer generator and does not accept, inspect, include or link MadObs.
It preserves the same five tests and deterministic A/B resources.

`prepare.py --mode on --dest <fresh-consumer> --installed <installed-prefix>
--removed-producer <absent-source-path>` requires a separate installed archive
and public headers, rejects symlinks, moves that prefix into the consumer,
and adds only a test C++ adapter to the XCTest target. The source producer must
already be absent. The app/framework targets and original five test assertions
are unchanged. Original delta callbacks still validate AAA/BBB output only.

**This preparer does not prove install provenance or target architecture.**
An archive magic check and an absent path are necessary hygiene, not such proof.
A separate reviewed orchestration phase MUST establish exact MadObs commit/tree,
run its real `cmake --install` for ARM64 Simulator with Xcode 26.2/17C52 and SDK
26.2, runtime ON/shared OFF/JNI OFF/BoringSSL OFF with no provider override,
explicit SDK zlib, then remove its disposable producer and build locations.
It must verify the relocated archive/symbols, link and run the independent
consumer, and establish no MadObs undefined references in standalone-off.
The retained install helper uses an already present, pinned disposable checkout; it acquires nothing.
Both lanes must retain exact five passed, zero failed/skipped checks from
`scripts/check-ios-positive-results.py`; no fake runtime substitutes qualify.

## Observation and lifecycle

The original callbacks update their existing lock-protected finite status
counts. They do not invoke the adapter or MadObs. After `madinfer_core_destroy`
has joined callbacks and all original correctness assertions pass, the owner
thread passes only started/completed/failed counts to `after_join`. It rejects
anything except 1/1/0 before creating a runtime, then emits exactly two finite
events (STARTED, COMPLETED). This is a **post-join status snapshot**, not a live
telemetry stream or measurement of callback timing. It does not claim to observe
failed-inference telemetry; failed native tests remain failures.

The adapter API cannot accept deltas, output text, model names, paths as event
attributes, keys, timestamps or opaque `telemetry.v1` bytes. The WAL path is a
local runtime storage argument only. Zero attributes/timestamps and a fixed
test-local schema are sanitized. This identifier is not a global registration.

Actual `madobs_emit_generic_event` admission must succeed twice. After stop,
exact enqueued=drained=2, queue_dropped=wal_dropped=0 are required. The local
sink validates bounded private batches, discards bytes without retaining or
logging them, completes each callback once, and requires 1..16 send attempts
and no validation failures. It never contacts an endpoint. This does not assert
that the underlying runtime contains no networking code, that its internal WAL
or batch framing contains no timestamps, or that null-provider WAL is encrypted.

Shutdown: inference destroy/join -> owner emits -> MadObs stop/join -> read stats
-> MadObs destroy -> free callback policy/context -> remove only test-owned WAL
directory. On failed stop/destroy the adapter deliberately retains context and
the test fails; no unsafe cleanup/freed callback state. No global shutdown or
bounded cancellation promise for arbitrarily stuck runtimes is made.

Unlike MadDB's synchronous operation bridge, there is no observation call from
an inference worker, hence no new queue, cross-thread ownership or backpressure
on that worker. No DB hidden Swift/C symbols or implementation are reused.

## Portable verification (not iOS/runtime evidence)

- `python3 ios-obs-composition/test_source.py`: five structural tests, including
  original positive check preservation, five-test preservation, off isolation,
  producer/symlink/archive negatives and zero-delta-forwarding contract.
- Compile `Adapter.cpp` plus `AdapterTest.cpp` using C++17 and public installed
  MadObs headers (`include`, with bundled imobs types). The latter implements
  a clearly labeled fake runtime: exact events, invalid status snapshots,
  admission rejection, unsafe batches, missing drains and dropped stats fail.
  This tests adapter behavior only, not native MadObs or inference.
- Original portable positive/generator/result tests must continue passing.

The exact historically qualified hosted composition is listed in
[qualification](../docs/verification/QUALIFIED_SOURCE.md). No model-quality claim.

ON destinations must contain only ASCII letters, digits, slash, underscore,
dot and hyphen. Preparation rejects whitespace and build/shell metacharacters
before creating output or moving the package. This deliberately avoids an
unverified Xcode OTHER_LDFLAGS multi-layer quoting contract. OFF is unchanged.

### Reviewed execution-reference hardening

Credential checks now suppress all config output and permit only the absent-key
exit status 1; found/error are failures.
The test binary resolver accepts exactly the top-level XCTest bundle or its
app/PlugIns equivalent, rejects ambiguity and symlinks, and never recursively
picks an arbitrary bundle. Archive qualification now inspects every unique
linkable .o member for ARM64 and Simulator platform; only exact Darwin symbol
index metadata names are exempt.

The isolated simulator launcher preserves the original runtime, device type,
boot, ARM64 check, cleanup and five-result gate, but invokes test-without-building
with the unique generated .xctestrun rather than rebuilding. The narrow parser
accepts the documented v1 single target or v2 single enabled configuration with
one target. It rejects selection overrides, destination artifacts, UI tests,
foreign host/bundle paths, missing/ambiguous manifests and bundle-pointer mismatch.
It re-inspects the referenced binary and hashes the manifest plus all test/host
bundle files before execution, then requires identical paths/bytes and rechecks
symbols after execution. No artifact hashes or model bytes are printed.

Format background: Apple's WWDC 2016 Advanced Testing and Continuous Integration
session documents `test-without-building -xctestrun`; Xcode's bundled
`xcodebuild.xctestrun(5)` documents version 2 and default version 1 when metadata
is absent. Local test plists are explicitly synthetic structural examples, not
captured Xcode 26.2 output. Unsupported actual generated shapes fail with the
fixed composition-execution-reference label and require separate review, not
fallback or relaxation.

### Execution and prerequisite diagnostics

Execution failures now emit only fixed categories through the inherited FD3:
manifest set/format, exact host/bundle identity, platform, symbol linkage,
closure snapshot, actual xcodebuild nonzero versus launch failure, or post-test
change. Raw exception text, commands, paths and tool output are never relayed.
The only path acceptance addition is Apple's documented own-app host shape:
`__TESTROOT__/Debug-iphonesimulator/MadInferSmoke.app`, in addition to the existing
exact executable path. Both map to the same required existing executable and
contained test bundle, with the same pre/post hashes. The app-hosted example in
`xcodebuild.xctestrun(5)` pairs an .app host with
`__TESTHOST__/PlugIns/MyAppTests.xctest`; the regression is a synthetic equivalent,
not a claim to have captured the actual failing runner's manifest. Unrelated
app names, absolute paths and traversal remain rejected.
