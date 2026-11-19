# Android ARM64 native SDK packaging candidate (not runtime)

Base: public 56db9979cfe3f8710a265b808178ebee3dc8506e. Its successful iOS
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
engine CMake cache, pinned NDK C/C++ compiler paths and compiler-target metadata,
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

Eight synthetic inspector tests pass, covering malformed ELF, wrong architecture,
wrong ELF class/type, truncated headers, 4KiB/misaligned/W+X loads, missing loads,
and wrong cross-toolchain settings. The 373-file engine checksum passes.
Shell syntax, diff whitespace and public source-hygiene checks pass. Missing
or wrong candidate SHA rejects before installation. No Android/native compile,
ELF artifact inspection, consumer link, APK or device execution has occurred.
