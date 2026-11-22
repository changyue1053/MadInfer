# iOS synthetic CPU positive qualification candidate

Base: public d081496d1f193fe658e15b3011651f8a1e07c7aa. The Linux
synthetic native result on that base does not qualify this Apple candidate.
Historical qualification covered native iOS Simulator CPU decode. No production API, engine, native bridge,
framework packaging code, original smoke consumer, or original preview workflow
is changed. No pretrained assets, upstream model/tokenizer assets, network model
fetch, GGUF binaries, artifact uploads, signing accounts or device execution.

## Required evidence

The retained local gate builds the existing CPU-only standalone XCFramework
with Xcode 26.2 / 17C52, Rust 1.100.0 and CMake 4.4.5. It accepts only a clean
exact checkout and pinned engine sources; no platform/toolchain provisioning
or workflow is added to main. See [qualification](QUALIFIED_SOURCE.md).

`prepare-ios-positive.py` creates a fresh, isolated copy of the original binary
consumer under ignored build output. It preserves the original three ABI/error
methods and appends two positive XCTest methods. Generated first-party A and B
GGUF resources are byte-for-byte hash pinned to the already qualified Linux
fixture generator (115584 bytes each). They are copied by Xcode's test bundle
resource phase, not installed in the SDK framework. No resources are downloaded
or committed. An existing output directory or changed fixture fails preparation.

Each positive case loads its bundle fixture through public
`madinfer_core_create_with_backend(..., 1, ...)`, verifies backend kind 1,
submits the same user prompt `A` through `madinfer_stream_start`, and requests
three tokens. Completion, not just lazy core creation, is required:

- A must yield exactly `AAA`; B must yield exactly `BBB`.
- Concatenated delta bytes, complete.text and complete.output must all match.
- Exactly one started and one complete, no failure, at least one delta;
  strictly increasing delta sequence from zero, no events out of order.
- Bundled source, two prompt tokens, three generated tokens, text output,
  zero fallback steps and zero authorization requests.
- Owner-thread destroy joins callbacks before inspecting/releasing host state.
  All borrowed output bytes are copied in callbacks; retained text is bounded.
- Missing resource, swapped/wrong model, emulation, timeout, bad output or stats
  is a hard failure, never a skip or fallback.

The production C ABI exposes text and token counts, not token IDs or engine
build number. Exact `AAA`/`BBB` plus counts is the available public ABI assertion;
no private symbol or testing API is added. Engine identity is established by
source checksum and existing native build path. CPU-only is structural:
`csrc/CMakeLists.txt` forces GGML_METAL OFF and
`madinfer_llama_bridge.c:43` sets n_gpu_layers=0. This is not a GPU test.

The result gate demands all five exact test identifiers passed, zero skipped,
zero failed; the legacy three-only result cannot pass. The old parser defaults
and old 17 parser tests remain intact. Device consumer compilation is unsigned
and is not physical-device execution. Simulator output is not an iOS hardware,
performance, model quality, language understanding or production-readiness claim.

The original exact hosted run passed five required tests. Results from this
integrated source tree still require their own execution; portable parser and
generator tests do not prove a native Apple result.
