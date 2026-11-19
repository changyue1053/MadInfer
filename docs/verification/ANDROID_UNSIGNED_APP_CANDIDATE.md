# Standalone unsigned NativeActivity packaging candidate

Base: exact public 0493c2a5be55a50613387f26b5c0c4d0c49b094b, whose hosted
run 44521252156 verified the real ARM64 native archive, relocated independent C
consumer and ELF 16KiB load layout. That pass does not validate this app candidate.

## Least-moving-parts design

Use the Android framework's android.app.NativeActivity with hasCode=false, no
Java/Kotlin/Gradle/AGP/Prefab or custom JNI API. The app's standard
ANativeActivity_onCreate symbol is implemented in an independent C file against
the installed public SDK header and the NDK activity header. It only references
ABI version, creates no workers/models/resources and promises no lifecycle or
inference behavior. The existing independent link-probe is included so the real
native engine remains part of the linked archive consumer. No production source,
backend configuration, model or external application asset is changed.

Official support:
- https://developer.android.com/ndk/reference/group/native-activity
- https://developer.android.com/tools/aapt2
- https://developer.android.com/tools/zipalign

The new checkpoint/madinfer-android-app-preview branch workflow opts into the
app step only after all existing native-package gates pass, while the relocated
SDK is still alive. The old workflow's default remains packaging-only. Generic
source checks stay enabled, with only this branch/head excluded from the
pretrained model-download job. Neither workflow uploads binaries or logs.

## Exact tools and acceptance

Keep NDK r28c, Rust 1.100.0, SDK CMake 3.22.1 and Android ARM64/API24 native
compile options unchanged. Require official SDK packages platforms;android-35
revision 2 and build-tools;35.0.0. Their platform/build-tool package identity is
checked from source.properties. The Google SDK repository catalog lists these
packages under the existing android-sdk-license; installation uses closed stdin,
no license acceptance, and verifies license-file hashes are unchanged. Missing
or mismatched tools/packages fail. Catalog consulted:
https://dl.google.com/android/repository/repository2-1.xml

AAPT2 links a binary AndroidManifest.xml using the platform android.jar. The
manifest declares minimum SDK24, target SDK35, NativeActivity, the exact
madinfer_preview native library name, hasCode=false, extractNativeLibs=false,
no permissions, no backup and no external files. Only the native shared object
is appended uncompressed at lib/arm64-v8a/libmadinfer_preview.so. No DEX is needed
for the framework-provided Activity. The standard native entrypoint must be an
exported defined symbol. The actual app library must independently pass all
existing strict native archive/symbol/dependency/ELF checks.

Official zipalign -P 16 then zipalign -c -P 16 verifies the unsigned APK. A second
inspector independently checks exact entry allowlist, duplicate/CRC rejection,
stored unencrypted ARM64 library, central/local header consistency, actual local
file data offset divisible by 16384, exact byte identity to the linked app
library, and ELF64 ARM64 load alignment. ELF and ZIP alignment are distinct
requirements; neither substitutes for device behavior.

## Boundary and local evidence

This is an **unsigned APK packaging fixture**, not an installable/signed release,
Android runtime test, UI demo, JNI/Kotlin API, real-device/16KiB-device assurance
or inference-quality result. No key is created or accessed. No signer, emulator,
ADB, device discovery, cloud account, release or upload step exists. The APK and
logs stay job-local. Host execution awaits independent review and explicit
publication authorization.

Local Python tests: prior native inspectors 37/37 and synthetic APK inspector
4/4 pass. Synthetic ZIP/ELF fixtures exercise alignment, ABI, compression, byte
identity, duplicates and unexpected entries; they are not real compiled APKs.
Shell syntax, diff whitespace and public source-hygiene pass. No local Android
SDK/Rust compiler exists, so Activity compilation, AAPT2, zipalign and real APK
construction have not run for this candidate. This limitation must remain
explicit until the exact reviewed hosted revision passes.
