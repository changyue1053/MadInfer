# Installed iOS native lifecycle qualification

Optional standalone test checkpoint, based on public
`56db9979cfe3f8710a265b808178ebee3dc8506e`. It uses the same installed dynamic
XCFramework, generated synthetic GGUF, trusted-fixture fail-closed host verifier,
and existing five named XCTest checks. No product SDK/API change, inference
emulation, arbitrary model, model download, credentials or transport.

The additional test constructs an explicit native backend (kind 1) and keeps
two synthetic INTERNAL requests deterministically in host authorization. The
logical endpoint ID is not a URL; the pinned core does not register a cloud
backend. The host records a bounded consent event without giving a reply. An
owner thread cancels the first request, demands one 3008/cancelled terminal
for exactly that handle and no Started/Delta/Complete/partial/retry metadata,
then repeats cancellation and tries an impossible admitted handle. It checks
the second distinct request reaches its own authorization callback before the
owner destroys the core outside all callbacks. The same exact terminal must
be delivered before destroy returns; counters and queued signals confirm no
duplicate/late events through the join. Strong callback contexts remain alive
through cleanup. Bounded semaphore waits only, no scheduling sleeps.

The gate requires exactly six named passes, no skips or failures. The original
five-case gate, simulator settings, package builder and app are untouched. The
separate branch-only workflow is `checkpoint/madinfer-ios-native-lifecycle`.
Generic source checks remain enabled, but the existing model-download native
job is excluded only for this exact branch, on push and PR. Toolchain/SDK/runtime
pins are identical to the original positive checkpoint and fail closed if the
stock runner lacks a pin. Only Simulator runs tests; device link is unsigned
build-only. Portable tests verify generator prefix/resource equality, exact
case counts, and routing and test-source boundaries; none replaces Apple
runtime evidence or proves arbitrary callback/transport behavior.
