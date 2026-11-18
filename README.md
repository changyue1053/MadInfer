# MadInfer

Independent Rust inference SDK. **Development preview, not a production release.**
No IMCore, message database, login orchestration, other Mad SDK or private sibling checkout is required.

## Backend truth

- Default build is **deterministic emulation for tests**, not AI inference. It is never a substitute for a native model benchmark.
- `madinfer-ffi/llama-sys` compiles the pinned llama.cpp CPU backend. Select `BackendKindRecord::Llama` and supply a model path; unavailable native support returns `FeatureDisabled` (4005), never echo fallback.
- C hosts can call `madinfer_core_create_with_backend(..., 0|1, model_path, ...)` and `madinfer_backend_kind`. Legacy `madinfer_core_create` remains explicitly emulation-only.
- UniFFI `probe().backend` and per-entry constraints identify the actual implementation. Native constraints are currently `None`; emulated grammar is not native constrained decoding. Text generation only; no native multimodal or tool-execution claim.
- Model weights are not shipped. Host owns model acquisition, authorization and account-scoped storage. No message-store traversal is included.

## Build and verify

Rust 1.100.0 is the tested toolchain. C/C++ compiler and CMake are required only for native backend builds.

```sh
cargo test --workspace --locked                 # emulated suite
cargo build -p madinfer-ffi --locked
bash ci/verify.sh
MADINFER_MODEL_CACHE="$PWD/target/models" bash tools/models/fetch.sh smollm2-135m-instruct-q4_k_m
export MADINFER_LLAMA_MODEL="$PWD/target/models/smollm2-135m-instruct-q4_k_m.gguf"
bash ci/native.sh                               # missing model is a failure
```

The committed lockfile resolves public registry dependencies; registry packages are not copied from the old vendor tree. llama.cpp's audited source subset and notices are retained in `third_party/`.

## Integration boundaries

Telemetry is supplied by the host port; no MadObs linkage or runtime is mandatory. A MadObs adapter is not yet implemented. Original signing/configuration, scheduler, device budgets, cancellation, context isolation, assets, telemetry and tests are retained. `drop_contexts("acct:<id>/")` invalidates matching cached contexts, but does not forcibly erase an in-flight native computation. Hosts must cancel/drain account work before switching identities.

**Known release gates:** event callbacks still execute on the control lane and must immediately enqueue elsewhere; arbitrary slow/reentrant callbacks are not yet isolated by a bounded delivery queue. C handles require exactly-once destruction with external synchronization; stale/double-destroy handles are not safe. No three-platform package/device validation has been performed in this migration. Do not advertise G3/G4/GA completion.

See [support matrix](SUPPORT_MATRIX.md), [API contract](PUBLIC_API.md), [source record](MIGRATION_SOURCE.toml), [verification](docs/verification/STAGE1.md), and [third-party notices](THIRD_PARTY.md). `docs/legacy-readme.md` and historical inline milestone notes are preserved background, not current support claims.

## License

First-party source preserves the upstream `Proprietary` declaration. Public visibility is not an open-source license. Third-party licenses remain applicable. See LICENSE and THIRD_PARTY.md.
