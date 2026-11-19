#!/usr/bin/env bash
# Packaging only: no emulator, APK, fixture, inference or uploaded artifact.
set -euo pipefail
stage=preflight
relocated=
finish() {
  result=$?
  if [[ -n $relocated ]]; then rm -rf "$relocated"; fi
  if (( result != 0 )); then printf 'FAIL: android-native stage=%s\n' "$stage" >&3; fi
}
trap finish EXIT
# Dedicated bounded-status descriptor; detailed output remains job-local.
: >&3
cd "$(dirname "$0")/.."
: "${MADINFER_EXPECTED_SHA:?exact reviewed SHA required}"
[[ $MADINFER_EXPECTED_SHA =~ ^[0-9a-f]{40}$ ]]
[[ $(git rev-parse HEAD) == "$MADINFER_EXPECTED_SHA" && -z $(git status --porcelain) ]]
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]
for name in MADINFER_LLAMA_LIB_DIR MADINFER_LLAMA_ARM_ARCH CARGO_BUILD_TARGET RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CC CXX CFLAGS CXXFLAGS LDFLAGS CMAKE_TOOLCHAIN_FILE CMAKE_GENERATOR CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS; do
  [[ -z ${!name:-} ]] || { echo 'FAIL: unsupported build override' >&2; exit 1; }
done
stage=sdk-prerequisites
: "${ANDROID_HOME:?preinstalled SDK required}"
[[ -s "$ANDROID_HOME/licenses/android-sdk-license" ]]
[[ -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" ]]
# Do not accept a new license. Closed stdin makes any interactive request fail.
# Preserve existing runner license bytes even if installation fails.
stage=sdk-install
licenses_before=$(find "$ANDROID_HOME/licenses" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum)
install_status=0
"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --sdk_root="$ANDROID_HOME" \
  'ndk;28.2.13676358' 'cmake;3.22.1' </dev/null || install_status=$?
licenses_after=$(find "$ANDROID_HOME/licenses" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum)
[[ $licenses_before == "$licenses_after" && $install_status == 0 ]]
stage=toolchain-identity
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/28.2.13676358"
grep -Eq '^Pkg.Revision *= *28\.2\.13676358$' "$ANDROID_NDK_HOME/source.properties"
export CMAKE="$ANDROID_HOME/cmake/3.22.1/bin/cmake"
"$CMAKE" --version | sed -n '1p' | python3 scripts/check-android-native-source.py --cmake-version
tools="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
cc="$tools/aarch64-linux-android24-clang"
[[ -x $cc && -x "$tools/llvm-nm" && -x "$tools/llvm-readelf" ]]
export RUSTUP_TOOLCHAIN=1.100.0
[[ $(rustc --version) == 'rustc 1.100.0 '* ]]
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$cc"
export CC_aarch64_linux_android="$cc"
export CXX_aarch64_linux_android="$tools/aarch64-linux-android24-clang++"
export AR_aarch64_linux_android="$tools/llvm-ar"
export MADINFER_LLAMA_ARM_ARCH=armv8-a
export MADINFER_LLAMA_CMAKE_JOBS=2
export CARGO_TARGET_DIR="$PWD/target/android-native-preview"
[[ ! -e $CARGO_TARGET_DIR && ! -e build/android-native-preview ]]
stage=source-and-inspector-tests
python3 scripts/test-android-preview.py
python3 scripts/check-android-native-source.py
# True Android target; host build dependencies are not Android runtime validation.
stage=native-cross-build
export CMAKE_EXPORT_COMPILE_COMMANDS=ON
cargo rustc -p madinfer-ffi --lib --crate-type staticlib --locked --offline --release \
  --target aarch64-linux-android --no-default-features --features standalone,llama-sys
stage=actual-compile-proof
python3 scripts/check-android-native-source.py "$CARGO_TARGET_DIR" "$ANDROID_NDK_HOME"
stage=package-relocation
package="$PWD/build/android-native-preview/sdk"
mkdir -p "$package/include" "$package/lib/arm64-v8a" "$package/notices"
cp crates/madinfer-ffi/include/madinfer.h "$package/include/"
cp "$CARGO_TARGET_DIR/aarch64-linux-android/release/libmadinfer_ffi.a" "$package/lib/arm64-v8a/"
cp LICENSE THIRD_PARTY.md third_party/PINS.toml "$package/notices/"
cp third_party/llama.cpp/LICENSE "$package/notices/llama-LICENSE"
cp -R third_party/licenses "$package/notices/"
relocated=$(mktemp -d)
mv "$package" "$relocated/sdk"
cp android/NativePreview/consumer.c "$relocated/consumer.c"
# Independent C compilation sees only the relocated public include directory.
stage=consumer-compile
(cd "$relocated" && "$cc" -std=c17 -Wall -Wextra -Werror -fPIC -I sdk/include -c consumer.c -o consumer.o)
# Archive under test, not another .so/rlib, supplies the native implementation.
stage=consumer-link
(cd "$relocated" && "$cc" -shared consumer.o sdk/lib/arm64-v8a/libmadinfer_ffi.a \
  -Wl,-z,defs,-z,max-page-size=16384,-z,common-page-size=16384 \
  -lc++_static -lc++abi -ldl -lm -llog -latomic -o consumer.so)
stage=elf-and-symbol-inspection
python3 scripts/check-android-preview.py "$relocated/sdk" "$relocated/consumer.so" "$tools"
printf 'PASS: Android native packaging exact SHA %s (no execution)\n' "$MADINFER_EXPECTED_SHA"
