#!/usr/bin/env bash
# Job-local bounded logs only; this script performs no source acquisition.
set -euo pipefail
cd "$(dirname "$0")/.."
mode=${1:?}
[[ "$mode" == on || "$mode" == off ]]
exec 3>&1
mkdir -p build/ios/logs
exec >build/ios/logs/composition.log 2>&1
trap 'printf "FAIL: %s\n" "$stage" >&3' ERR
stage=composition-source-fixtures
python3 ios-obs-composition/test_source.py
stage=composition-orchestration-fixtures
python3 ios-obs-composition/test_orchestration.py
stage=composition-execution-fixtures
python3 ios-obs-composition/test_execution.py
stage=composition-positive-fixtures
python3 scripts/test-ios-positive.py
stage=composition-result-fixtures
python3 scripts/test-ios-test-results.py
stage=composition-framework-build
bash scripts/build-ios.sh
stage=composition-prepare
if [[ "$mode" == on ]]; then
  bash ios-obs-composition/install.sh
else
  [[ ! -e build/ios/obs-producer && ! -e build/ios/obs-installed ]]
  python3 ios-obs-composition/prepare.py --mode off --dest build/ios/positive-consumer
fi
stage=composition-link
# Build the actual test bundle before executing any test. No device build claim.
env -u SDKROOT xcodebuild -project build/ios/positive-consumer/MadInferSmoke.xcodeproj \
  -scheme MadInferSmoke -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build/ios/simulator-consumer ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=NO build-for-testing
stage=composition-symbols
products=build/ios/simulator-consumer/Build/Products/Debug-iphonesimulator
python3 ios-obs-composition/inspect.py --mode "$mode" --resolve-bundle "$products"
if [[ "$mode" == off ]]; then
  # The framework and app must also contain no Obs dependency. They do not
  # necessarily depend on the framework dynamically (Swift app may not use it).
  for binary in \
    build/ios/simulator-consumer/Build/Products/Debug-iphonesimulator/MadInferSmoke.app/MadInferSmoke \
    build/ios/simulator-consumer/Build/Products/Debug-iphonesimulator/MadInferSmoke.app/Frameworks/MadInfer.framework/MadInfer; do
    python3 ios-obs-composition/inspect.py --mode off --standalone-binary "$binary"
  done
fi
stage=composition-native-five
# Isolated runner preserves original runtime/create/boot/cleanup/result contract;
# exact xctestrun test-without-building prevents replacing inspected bytes.
bash ios-obs-composition/simulator.sh "$mode" >&3
printf 'PASS: composition-native-five\n' >&3
