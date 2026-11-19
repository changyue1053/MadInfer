#!/usr/bin/env bash
# Same exact requirements; separate fixed diagnostics, never raw tool output.
set -euo pipefail
cd "$(dirname "$0")/.."
exec 3>&1
mkdir -p build/ios/logs
exec >build/ios/logs/composition-prerequisites.log 2>&1
stage=runner-architecture
trap 'printf "FAIL: prerequisite-%s\n" "$stage" >&3' ERR
test "$(uname -m)" = arm64
printf 'PASS: prerequisite-runner-architecture\n' >&3
stage=xcode
test "$(xcodebuild -version)" = $'Xcode 26.2\nBuild version 17C52'
printf 'PASS: prerequisite-xcode\n' >&3
stage=sdk-device
test "$(xcrun --sdk iphoneos --show-sdk-version)" = 26.2
printf 'PASS: prerequisite-sdk-device\n' >&3
stage=sdk-simulator
test "$(xcrun --sdk iphonesimulator --show-sdk-version)" = 26.2
printf 'PASS: prerequisite-sdk-simulator\n' >&3
stage=cmake
test "$(cmake --version | head -n 1)" = 'cmake version 4.4.5'
printf 'PASS: prerequisite-cmake\n' >&3
stage=rust-toolchain
rustup toolchain install 1.100.0 --profile minimal --no-self-update
printf 'PASS: prerequisite-rust-toolchain\n' >&3
stage=rust-targets
rustup target add --toolchain 1.100.0 aarch64-apple-ios aarch64-apple-ios-sim
printf 'PASS: prerequisite-rust-targets\n' >&3
stage=locked-fetch
CARGO_NET_OFFLINE=false cargo fetch --locked --target aarch64-apple-ios --target aarch64-apple-ios-sim
printf 'PASS: prerequisite-locked-fetch\n' >&3
