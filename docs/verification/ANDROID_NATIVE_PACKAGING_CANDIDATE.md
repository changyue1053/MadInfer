# Android ARM64 native SDK packaging candidate (not runtime)

Current configured-vendor graph fix base: public d94b037ba6b007249894b7b2bcea9b528a821c13.
Original packaging base: public 56db9979cfe3f8710a265b808178ebee3dc8506e. Its successful iOS
synthetic execution is not evidence of Android compilation or execution.
This candidate has not been cross-compiled locally. No Rust/CMake/Android NDK
is installed in the current workspace. Actual qualification requires independent
review and an explicitly authorized exact-SHA hosted run.

## Scope and toolchain

Only scripts, an independent C consumer, documentation and a branch-scoped
workflow are added. Production Rust/backend/C bridge/framework and pinned
vendored sources are unchanged. The regular emulated source checks still run;
only the pretrained-download job excludes this new branch/head. No model
weights or fixtures are generated/downloaded, and no compiled files, APK,
emulator, JNI integration, Android inference or model quality are claimed.

Official NDK r28c is SDK package `ndk;28.2.13676358`. Use the already installed
runner SDK command-line tools, requiring an existing nonempty SDK license file.
Install only that NDK and SDK CMake `3.22.1`, with closed stdin. The version
line accepts only exact 3.22.1, optionally followed by a Kitware `-g` revision
of 7–40 lowercase hexadecimal digits; LF/CRLF are allowed. Other versions or
suffixes fail. This does not change the pinned SDK package. Never run
`--licenses`, pipe `yes`, or accept terms. Hash all license files before/after
and fail on change. Missing packages/toolchain/licenses fail closed. Existing
accepted runner licensing is a prerequisite, not an instruction to grant it.

References:
- https://github.com/android/ndk/releases/tag/r28c
- https://developer.android.com/ndk/guides/other_build_systems
- https://developer.android.com/ndk/guides/cmake
- https://developer.android.com/guide/practices/page-sizes

Rust 1.100.0 targets `aarch64-linux-android`, API24, using NDK's explicit
`aarch64-linux-android24-clang`, not the Linux host compiler. CMake resolves the
existing backend build.rs Android branch to arm64-v8a / android-24 /
c++_static / armv8-a. CPU-only backend flags and the exact 373-file engine
source checksum are verified. Post-build inspection requires exactly one
engine CMake cache, pinned NDK C/C++ compiler paths and actual exported compile commands,
API24, ARM baseline flags and disabled native-machine specialization. A schema
or toolchain mismatch fails, rather than assuming the correct target.

## Package and independent consumer

Build only the actual FFI staticlib with `--no-default-features --features
standalone,llama-sys`, preserving existing release settings and vendored
llama.cpp b10883. Copy the public madinfer.h, static archive and license/notices
into a fresh SDK layout. Move the entire SDK into a temporary directory outside
the checkout. The independent C17 consumer compiles using only that installed
header, with a static assertion of ABI header version 2. It links that archive
with NDK libc++_static/libc++abi and Android system dependencies, requiring no
unresolved strong references (`-z defs`). The consumer is never executed.

Inspect actual archive symbols for public C ABI and the native engine
(llama_decode, model loader, CPU backend initializer and bridge build-number
symbol). Inspect the independent consumer for exported ABI/probe symbols,
ELF64 little-endian AArch64 ET_DYN, bounded program headers, non-W+X PT_LOADs
with valid offset/VA congruence and >=16384-byte power-of-two alignment, and
only allowlisted Android system DT_NEEDED entries. Reject RPATH/RUNPATH and
TEXTREL. Static archives themselves have no ELF PT_LOAD alignment; the linked
consumer provides the 16KiB link-layout evidence. This does not prove APK zip
alignment, Android loader behavior, 16KiB hardware compatibility or runtime ABI
return values. ABI 2 here is a header/link check, not a runtime handshake.

Artifacts/logs remain job-local, with no upload or release step. The temporary
relocated package is removed after inspection; this is qualification preview,
not a distributed SDK release.

## Local evidence

Thirty-seven synthetic inspector tests pass, covering malformed ELF, wrong architecture,
wrong ELF class/type, truncated headers, 4KiB/misaligned/W+X loads, missing loads,
and wrong cross-toolchain settings. The 373-file engine checksum passes.
Shell syntax, diff whitespace and public source-hygiene checks pass. Missing
or wrong candidate SHA rejects before installation. No Android/native compile,
ELF artifact inspection, consumer link, APK or device execution has occurred.


## Compile-proof correction, not a successful hosted rerun

Public run 44498092437 on exact 7ac1de039101f89546d245a57762b86b9594b91b
reports failure only in the aggregate cross-build/relocation step. The public
bounded log contains the generic failure label, not the hidden package log or
an exact underlying error. Therefore we cannot claim its actual failure occurred
at the compiler-target checker. Independently, that checker has a source-proven
defect: CMake v3.22.1's C and CXX compiler templates do not serialize
CMAKE_*_COMPILER_TARGET. Requiring that field is not valid target evidence:

- https://github.com/Kitware/CMake/blob/v3.22.1/Modules/CMakeCCompiler.cmake.in
- https://github.com/Kitware/CMake/blob/v3.22.1/Modules/CMakeCXXCompiler.cmake.in
- https://cmake.org/cmake/help/v3.22/envvar/CMAKE_EXPORT_COMPILE_COMMANDS.html

The packaging invocation now explicitly exports CMAKE_EXPORT_COMPILE_COMMANDS=ON
without modifying the production build scripts. After the actual Cargo native
build succeeds, the checker requires the database from that sole engine build.
Every entry must use the exact pinned NDK clang/clang++ driver for its declared
C/C++ source language, one exact aarch64 Android API24 target, pinned sysroot,
source within the real bridge/vendored graph, and an output in the actual build.
All five native targets and both languages must be present. CPU commands must
use exactly armv8-a; hidden response files, conflicting target/sysroot flags,
language overrides, host CPU specialization and foreign outputs fail closed.
The final archive-symbol, relocated consumer and strict ELF gates are unchanged.

The six additional compile-command tests use explicitly synthetic databases,
covering arguments and shell-command representations plus missing targets,
wrong API/architecture/compiler/sysroot/language, hidden/conflicting flags and
foreign sources/objects. They are parser tests, not genuine compilation evidence.

Fixed failure stage labels now distinguish preflight, SDK prerequisites/install,
toolchain identity, source/inspector checks, native cross build, actual compile
proof, package relocation, consumer compile/link, and ELF/symbol inspection.
Only this fixed stage label escapes the local detailed log. No raw compiler,
path, source, token or environment diagnostics are published. These changes still
require exact-SHA reviewed hosted execution before any Android packaging pass.


## Legacy NDK cache correction and bounded rejection categories

Hosted public 3ba1a4963b3049b5c89184d54abcd198720a6235, run 44507962666,
reached `actual-compile-proof` after native cross-build. This does not establish
that the independent relocated consumer linked; that later gate was not reached.
The exact rejection reason remains unavailable from the aggregate fixed label.

The official r28c legacy toolchain defines CMAKE_C_COMPILER and CMAKE_CXX_COMPILER
with normal `set`, not `CACHE` (lines 612–613). Compiler variables therefore need
not exist in CMakeCache.txt. Their absence is now allowed in cache inspection;
if present, compiler paths are still checked. This is not optional compiler
proof: every actual compile-command entry must still use the exact pinned
NDK driver for its C/C++ source language. All other cache options, source graph,
Android target/API/sysroot checks, strict linked ELF/16K gates and license/tooling
requirements remain unchanged. No local CMake executable is available to run a
minimal toy toolchain; this finding is based on official source, not a claimed
local CMake reproduction.

Official source:
https://android.googlesource.com/platform/ndk/+/refs/tags/ndk-r28c/build/cmake/android-legacy.toolchain.cmake
Decoded source SHA256: a225790c91d565ee556731748d3ce1f54f9162432820067a05d2d86259c2a9a7.
Lines 332–354 construct the ARM64/API target, 405 sets the NDK sysroot,
426–427 set compiler-target normal variables, and 419–424 select clang/clang++.
These declarations support the expected emitted options but are not a substitute
for examining this run's real compile database.

Every known cache/command/database rejection now has a fixed allowlisted enum
on descriptor 3. Unknown errors become only `unexpected`; exception text,
commands, file paths and JSON values cannot enter this diagnostic channel.
Additional tests prove missing compiler cache entries do not remove mandatory
command proof, wrong present cache values still fail, every fixed reason is
bounded, unknown errors cannot leak data, and success emits no failure.
No hosted rerun or consumer link has been performed for this correction.


## Authoritative Cargo OUT_DIR discovery

Public run 44513694172 at exact 85a36f039572e36bbedeee3db5d380072a2fd8c1
reported `proof=cache-count` after Cargo succeeded. The assumed intermediate
`target/<triple>/release/build/<package-hash>/out` glob is removed completely.
No recursive search or fallback layout is substituted.

The pinned Cargo invocation now writes `--message-format=json` stdout only to
a job-local file. `cargo metadata --locked --offline --no-deps --format-version 1`
supplies the exact backend package ID after verification of the canonical local
workspace manifest, package name, workspace membership, local source and
workspace/target directories. Exactly one matching `build-script-executed`
event with an absolute canonical out_dir is mandatory, along with exactly one
successful final `build-finished` event. Out_dir must resolve inside the fresh
dedicated build root. Only its exact `llama-build/CMakeCache.txt` and
`llama-build/compile_commands.json` are read, rejecting missing files and
symlink escapes. Both CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR are explicitly
set to that same dedicated directory, avoiding inherited intermediate-location
configuration. Production build.rs is unchanged. The final staticlib stays at
the documented target/profile artifact location; all archive/link/ELF gates
still apply and will reject absent/wrong artifacts.

Cargo's documented JSON protocol explicitly supplies build-script out_dir and
opaque package IDs, while build.build-dir controls intermediate output separate
from final artifacts:
- https://doc.rust-lang.org/cargo/reference/external-tools.html
- https://doc.rust-lang.org/cargo/reference/config.html#buildbuild-dir

The versioned 1.100.0 documentation URL was not accessible and no local Cargo
binary exists. Therefore the exact 1.100 default directory layout has NOT been
reproduced or asserted. This change intentionally consumes authoritative emitted
paths rather than depending on a version's internal directory naming. The
runner remains pinned to Rust 1.100.0, and missing/changed JSON schema fails.

Six additional synthetic parser tests cover two illustrative intermediate
layouts, missing/duplicate/wrong package events, absolute/canonical containment,
symlink escape, wrong metadata source/membership/manifest, missing/failed finish
and invalid schemas. These are not captured genuine Cargo streams and do not
prove a successful cross-build/link of this revision. Metadata/stream payloads
are never printed; errors remain fixed allowlisted diagnostic labels.


## Narrow CMake cache syntax correction

Exact public 4ebbfb0880cb64ce9c9577b3c303c901bf8eba6a, hosted run
44514780911, passed native cross-build and authoritative Cargo OUT_DIR discovery,
then reported `cache-option-ANDROID_ABI`. This revision changes only the cache
parser and regression documentation/tests, not build tooling or compiler flags.

The previous multiline regexp excluded colons/newlines inconsistently and could
consume a preceding blank line and // documentation as part of an entry's key.
The replacement parses one split line at a time with fullmatch, ignoring blank,
# and // documentation lines. Typed entries (including UNINITIALIZED used for
command-line ABI/platform values) preserve exact keys and values; CRLF is handled
by splitlines. Values may contain equals signs. Duplicate keys, even with an
identical value, and malformed noncomment lines reject with fixed categories.
All expected Android ABI/platform/toolchain/CPU values are unchanged. Whitespace
or case drift in an expected value is still a failure, never silently trimmed.

Five new explicitly synthetic CMake-cache regressions cover representative
comments/blank lines and UNINITIALIZED entries, CRLF and equals-containing
unrelated values, duplicate ambiguity, unchanged exact-value validation and
malformed records. Thirty total local tests pass. No hosted rerun, relocated
consumer link or Android runtime claim is made for this revision. All existing
final ELF16K/symbol, source/command-proof and license gates remain unchanged.


## Configured-only vendor graph versus compiled graph

Public d94b037ba6b007249894b7b2bcea9b528a821c13, run 44516421362,
reached the compile database and reported `command-graph-target`. The pinned
engine's CMakeLists.txt:227 unconditionally configures vendor/. Its hash target
has exactly hash.cpp, xxhash/xxhash.c, sha1/sha1.c and sha256/sha256.c. SHA1 is
explicitly LANGUAGE CXX; the other configured vendor libraries are INTERFACE.
CMake's compile database may include configured-but-unbuilt targets.

This checker now permits only those four exact canonical source paths paired
with exactly vendor-hash as auxiliary. SHA1 alone must use the exact clang++
driver and a single `-x c++`; no other language override is allowed. Auxiliary
entries still require the same exact NDK compiler, API24 target, sysroot and
path constraints. They do not contribute to the five required main targets or
C/C++ language coverage. Unknown targets or vendor source substitutions fail.
The native production configuration remains unchanged; no vendor target is added
to its build/link dependencies.

For the required main graphs, every declared object must now exist and have an
ELF64 little-endian AArch64 ET_REL header after the successful native build.
Auxiliary objects need not exist. The gate additionally rejects creation of the
known lib/libvendor-hash.a and rejects its four known member names in the final
FFI static archive. This supplements, not replaces, the unchanged native symbol
and relocated final ARM64/16KiB ELF/link checks. No actual vendor-link absence
or final consumer success is claimed until this revision executes on the host.

Seven explicitly synthetic regressions cover exact auxiliary entries, retained
main graph coverage, source/target pairing, SHA1-only override, auxiliary
compiler/API/sysroot rejection, required-object existence/header checks and
vendor-member rejection. Synthetic object headers are parser fixtures only;
they are never presented as compiled evidence. Total 37 local tests pass.
