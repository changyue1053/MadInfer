#!/usr/bin/env bash
# Requires existing exact pinned Apple tools, cached deps and optionally a disposable local MadObs checkout.
set -euo pipefail
cd "$(dirname "$0")/.."
mode=${1:?}
[[ "$mode" == on || "$mode" == off ]]
export CARGO_NET_OFFLINE=true
# Explicit rustup selection does not provision a missing pinned toolchain.
export RUSTUP_TOOLCHAIN=1.100.0
if [[ "$mode" == on ]]; then
  # The inherited installer deletes its verified *disposable* producer.
  [[ "${MADINFER_DISPOSABLE_MADOBS:-}" == "$PWD/build/ios/obs-producer" ]] || {
    printf 'FAIL: explicitly nominate the disposable local MadObs copy
' >&2
    exit 1
  }
fi
exec 3>&1
mkdir -p build/ios/logs
exec >build/ios/logs/composition.log 2>&1
trap 'printf "FAIL: %s\n" "$stage" >&3' ERR
stage=composition-source-fixtures
python3 -B ios-obs-composition/test_source.py
stage=composition-orchestration-fixtures
python3 -B ios-obs-composition/test_orchestration.py
stage=composition-execution-fixtures
python3 -B ios-obs-composition/test_execution.py
stage=composition-positive-fixtures
python3 scripts/test-ios-positive.py
stage=composition-result-fixtures
python3 scripts/test-ios-test-results.py
stage=app-demo-source-fixtures
python3 ios-app-obs-demo/test_source.py
python3 ios-app-obs-demo/test_qualification.py
stage=app-cancellation-source-fixtures
python3 ios-app-obs-lifecycle/test_overlay.py
stage=composition-framework-build
bash scripts/build-ios.sh
stage=composition-prepare
if [[ "$mode" == on ]]; then
  bash ios-app-obs-demo/install.sh
else
  [[ ! -e build/ios/obs-producer && ! -e build/ios/obs-installed ]]
  python3 ios-app-obs-demo/prepare.py --mode off --dest build/ios/positive-consumer
fi
stage=app-cancellation-overlay
python3 ios-app-obs-lifecycle/overlay.py --mode "$mode" --dest build/ios/positive-consumer
stage=app-demo-project-boundary
python3 ios-app-obs-demo/qualify.py --mode "$mode" --project-only
stage=composition-link
# Build the actual test bundle before executing any test. No device build claim.
env -u SDKROOT xcodebuild -project build/ios/positive-consumer/MadInferSmoke.xcodeproj \
  -scheme MadInferSmoke -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build/ios/simulator-consumer ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=NO build-for-testing
stage=app-cancellation-ownership
# Invokes the unchanged app-owned verifier before checking lifecycle symbols.
python3 ios-app-obs-lifecycle/ownership.py --mode "$mode"
stage=composition-native-six
# Isolated runner preserves original runtime/create/boot/cleanup/result contract;
# exact xctestrun test-without-building prevents replacing inspected bytes.
bash ios-app-obs-lifecycle/simulator.sh "$mode" >&3
printf 'PASS: composition-native-six\n' >&3
