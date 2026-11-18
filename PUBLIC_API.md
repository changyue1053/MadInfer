# API v0 migration contract

Canonical C header: `crates/madinfer-ffi/include/madinfer.h`; UniFFI namespace `madinfer_ffi`.
C ABI version 2 preserves original layouts under the new symbol prefix. Added functions are additive: `madinfer_core_create_with_backend` (0 emulated, 1 native; other values 4001), `madinfer_backend_kind` (actual selection). No native fallback. Existing C create intentionally remains emulated-only.

Caller inputs are borrowed for the duration specified in the header; asynchronous data are copied. Caller owns callback context and must retain it until destroy/drain completes. Outputs use header capacity protocols. Model paths are local host-authorized resources, not downloaded by create. Native model load occurs at generation, so successful create/probe is not proof a model loaded.

The inherited raw-pointer handle is not a generation-checked registry. A handle may be destroyed once, not concurrently with other operations; null destroy is safe but repeated stale destroy is undefined. This is an explicit release blocker, not an idempotent-destroy guarantee. Synchronous APIs must not be called from control-thread callbacks. Event/telemetry callbacks must immediately enqueue; delivery isolation is pending.

Cancellation is cooperative. It is not a hard deadline interrupting arbitrary native code. Account cache invalidation must be paired with host cancellation/drain. Privacy authorization remains host-injected; no cloud transport or plaintext extraction is added.

All host callbacks (clock/KV/telemetry/authorization/verifier/thread hooks) acquire a lifetime ticket. Destroy closes the gate and drains outstanding tickets before returning. A verifier that timed out operationally may still be running and therefore delays destroy; an indefinitely blocked host callback means destroy may wait indefinitely. No hard shutdown deadline is claimed. Rust catch_unwind only catches unwinding Rust panics, not memory faults, aborts, or C++/native crashes.

The inherited default `debug_script_backend` feature is debug-build-only behavior; release entry points reject debug operations with 4005. Production consumers should explicitly disable default features and select `standalone` plus `llama-sys` as appropriate. Emulation remains a test facility, not a production inference capability.
