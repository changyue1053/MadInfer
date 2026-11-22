#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || { echo 'Requires Apple silicon macOS' >&2; exit 1; }
export DEVELOPER_DIR=/Applications/Xcode_26.2.app/Contents/Developer
[[ $(xcodebuild -version) == $'Xcode 26.2\nBuild version 17C52' ]]
export RUSTUP_TOOLCHAIN=1.100.0
[[ $(cmake --version | head -n 1) == "cmake version 4.4.5" ]]
[[ $(rustc --version) == 'rustc 1.100.0 '* ]]
export IPHONEOS_DEPLOYMENT_TARGET=16.0
# Reject prebuilt backend injection: this gate compiles the audited public subtree.
[[ -z ${MADINFER_LLAMA_LIB_DIR:-} ]] || { echo "Prebuilt engine override not allowed" >&2; exit 1; }
export CMAKE=cmake
export CARGO_TARGET_DIR="$PWD/target/ios"
out="$PWD/build/ios"
# Deliberately fail on an existing package rather than merging stale slices.
[[ ! -e "$out/MadInfer.xcframework" ]]
# Resources are inputs to Xcode's Copy Bundle Resources phase, not post-archive edits.
mkdir -p "$out/Notices"
cp LICENSE THIRD_PARTY.md "$out/Notices/"
cp third_party/llama.cpp/LICENSE "$out/Notices/llama-LICENSE"
cp third_party/PINS.toml "$out/Notices/"
cp -R third_party/licenses "$out/Notices/"
for variant in device simulator; do
  if [[ $variant == device ]]; then
    target=aarch64-apple-ios; sdk=iphoneos; destination='generic/platform=iOS'
  else
    target=aarch64-apple-ios-sim; sdk=iphonesimulator; destination='generic/platform=iOS Simulator'
  fi
  export SDKROOT
  SDKROOT=$(xcrun --sdk "$sdk" --show-sdk-path)
  [[ $(xcrun --sdk "$sdk" --show-sdk-version) == 26.2 ]]
  # Only this invocation overrides the manifest crate types; Linux/Android unchanged.
  cargo rustc -p madinfer-ffi --lib --crate-type staticlib --locked --offline --release \
    --target "$target" --no-default-features --features standalone,llama-sys
  archive="$out/$variant.xcarchive"
  [[ ! -e "$archive" ]] || { echo 'Use a fresh archive output directory' >&2; exit 1; }
  # Xcode owns framework linking, generated/processed Info.plist, module interfaces,
  # standard iOS bundle layout and resource copying. Never rewrite its archive.
  env -u SDKROOT xcodebuild archive -project ios/Framework/MadInfer.xcodeproj \
    -scheme MadInfer -configuration Release -destination "$destination" \
    -archivePath "$archive" -derivedDataPath "$out/derived-$variant" \
    MADINFER_RUST_STATIC_LIBRARY="$CARGO_TARGET_DIR/$target/release/libmadinfer_ffi.a" \
    ARCHS=arm64 ONLY_ACTIVE_ARCH=NO CODE_SIGNING_ALLOWED=NO
done
xcodebuild -create-xcframework \
  -archive "$out/device.xcarchive" -framework MadInfer.framework \
  -archive "$out/simulator.xcarchive" -framework MadInfer.framework \
  -output "$out/MadInfer.xcframework"
python3 scripts/inspect-ios-package.py "$out/MadInfer.xcframework"
