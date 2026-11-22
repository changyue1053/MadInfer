#!/usr/bin/env bash
# Called only by optional-on. Source acquisition belongs to a pinned checkout step.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
producer="$root/build/ios/obs-producer"
work="$root/build/ios/obs-native"
stage="$root/build/ios/obs-installed"
[[ -d "$producer/.git" && ! -L "$producer" && ! -e "$work" && ! -e "$stage" ]]
[[ $(git -C "$producer" rev-parse HEAD) == 4af77ffa464a48307e5ff2d95f10b835919d205b ]]
[[ $(git -C "$producer" rev-parse HEAD^{tree}) == 3a03801beb18c9601630cc4421b95c2743018dfb ]]
[[ -z $(git -C "$producer" status --porcelain) ]]
origin=$(git -C "$producer" remote get-url origin)
[[ "$origin" == https://github.com/changyue1053/MadObs || "$origin" == https://github.com/changyue1053/MadObs.git ]]
# No persisted checkout credentials. Reject local HTTP authorization overrides.
header_status=0
git -C "$producer" config --local --name-only --get-regexp 'http\..*extraheader' >/dev/null 2>&1 || header_status=$?
[[ $header_status == 1 ]] # Only documented absent-key status is permitted.
[[ $(uname -m) == arm64 ]]
[[ $(xcodebuild -version) == $'Xcode 26.2\nBuild version 17C52' ]]
[[ $(cmake --version | head -n 1) == 'cmake version 4.4.5' ]]
sdkroot=$(xcrun --sdk iphonesimulator --show-sdk-path)
[[ $(xcrun --sdk iphonesimulator --show-sdk-version) == 26.2 ]]
[[ -f "$sdkroot/usr/include/zlib.h" && -f "$sdkroot/usr/lib/libz.tbd" ]]
# Simulator-only optional proof. MadInfer's unchanged builder still makes both slices.
cmake -S "$producer" -B "$work" -G Xcode \
  -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT="$sdkroot" \
  -DCMAKE_OSX_ARCHITECTURES=arm64 -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 \
  -DCMAKE_XCODE_ATTRIBUTE_CODE_SIGNING_ALLOWED=NO \
  -DCMAKE_XCODE_ATTRIBUTE_CODE_SIGNING_REQUIRED=NO \
  -DCMAKE_INSTALL_PREFIX="$stage" -DBUILD_TESTING=OFF \
  -DMADOBS_BUILD_RUNTIME=ON -DMADOBS_BUILD_SHARED=OFF -DMADOBS_BUILD_JNI=OFF \
  -DMADOBS_PACKAGE_BORINGSSL=OFF -DIMOBS_SAN= \
  -DIMOBS_BORINGSSL_ROOT= -DIMOBS_BORINGSSL_INCLUDE= -DIMOBS_BORINGSSL_CRYPTO_LIB= \
  -DZLIB_INCLUDE_DIR="$sdkroot/usr/include" -DZLIB_LIBRARY="$sdkroot/usr/lib/libz.tbd"
cmake --build "$work" --config Release --target imobs_core --parallel 3
cmake --install "$work" --config Release
python3 ios-obs-composition/artifact_inspector.py --mode on --archive "$stage/lib/libmadobs_runtime.a"
# Relocate only the archive and public headers, not CMake build metadata.
python3 - <<'PYRELOCATE'
from pathlib import Path
import shutil
source=Path('build/ios/obs-installed')
dest=Path('build/ios/obs-relocated')
assert not dest.exists() and not dest.is_symlink()
files=('lib/libmadobs_runtime.a','include/madobs/runtime.h',
       'include/madobs/observer.h','include/madobs/privacy.h','include/imobs/imobs_c.h')
assert not any(p.is_symlink() for p in source.rglob('*'))
for name in files:
    src=source/name
    assert src.is_file()
    target=dest/name; target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(src,target)
PYRELOCATE
# Only fixed job-owned paths beneath this exact checkout; no user installs.
[[ "$producer" == "$root/build/ios/obs-producer" && "$work" == "$root/build/ios/obs-native" ]]
[[ ! -L "$work" && ! -L "$stage" ]]
rm -rf -- "$producer" "$work" "$stage"
[[ ! -e "$producer" && ! -e "$work" && ! -e "$stage" ]]
python3 ios-obs-composition/prepare.py --mode on --dest build/ios/positive-consumer \
  --installed "$root/build/ios/obs-relocated" --removed-producer "$producer"
[[ ! -e "$root/build/ios/obs-relocated" ]]
