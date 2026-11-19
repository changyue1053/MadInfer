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
python3 ios-obs-composition/source_diagnostics.py >&3
stage=composition-orchestration-fixtures
python3 ios-obs-composition/source_diagnostics.py orchestration >&3
python3 ios-obs-composition/source_diagnostics.py diagnostics >&3
stage=composition-execution-fixtures
python3 ios-obs-composition/source_diagnostics.py execution >&3
stage=composition-positive-fixtures
python3 scripts/test-ios-positive.py
stage=composition-result-fixtures
python3 scripts/test-ios-test-results.py
stage=app-demo-source-fixtures
python3 ios-app-obs-demo/test_source.py
python3 ios-app-obs-demo/test_qualification.py
stage=composition-framework-build
bash scripts/build-ios.sh
stage=composition-prepare
if [[ "$mode" == on ]]; then
  bash ios-app-obs-demo/install.sh
else
  [[ ! -e build/ios/obs-producer && ! -e build/ios/obs-installed ]]
  python3 ios-app-obs-demo/prepare.py --mode off --dest build/ios/positive-consumer
fi
stage=app-demo-project-boundary
python3 ios-app-obs-demo/qualify.py --mode "$mode" --project-only
stage=composition-link
# Build the actual test bundle before executing any test. No device build claim.
env -u SDKROOT xcodebuild -project build/ios/positive-consumer/MadInferSmoke.xcodeproj \
  -scheme MadInferSmoke -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build/ios/simulator-consumer ARCHS=arm64 ONLY_ACTIVE_ARCH=YES \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=NO build-for-testing
stage=app-demo-ownership
python3 ios-app-obs-demo/qualify.py --mode "$mode" --inspect-only
stage=composition-native-five
# Isolated runner preserves original runtime/create/boot/cleanup/result contract;
# exact xctestrun test-without-building prevents replacing inspected bytes.
bash ios-app-obs-demo/simulator.sh "$mode" >&3
printf 'PASS: composition-native-five\n' >&3
