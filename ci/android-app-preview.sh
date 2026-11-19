#!/usr/bin/env bash
# Called only by opt-in app workflow while the verified relocated SDK is alive.
set -euo pipefail
[[ $# == 2 && ${MADINFER_ANDROID_APP_PREVIEW:-} == 1 ]]
package=$1; tools=$2
[[ $package = /* && -f $package/lib/arm64-v8a/libmadinfer_ffi.a ]]
[[ $(git rev-parse --abbrev-ref HEAD) == HEAD || -n ${GITHUB_SHA:-} ]]
licenses_before=$(find "$ANDROID_HOME/licenses" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum)
status=0
"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --sdk_root="$ANDROID_HOME" \
  'platforms;android-35' 'build-tools;35.0.0' </dev/null || status=$?
licenses_after=$(find "$ANDROID_HOME/licenses" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum)
[[ $licenses_before == "$licenses_after" && $status == 0 ]]
platform="$ANDROID_HOME/platforms/android-35"
bt="$ANDROID_HOME/build-tools/35.0.0"
grep -Eq '^Pkg.Revision *= *2$' "$platform/source.properties"
grep -Eq '^AndroidVersion.ApiLevel *= *35$' "$platform/source.properties"
grep -Eq '^Pkg.Revision *= *35\.0\.0$' "$bt/source.properties"
[[ -f $platform/android.jar && -x $bt/aapt2 && -x $bt/zipalign ]]
out="$PWD/build/android-app-preview"
[[ ! -e $out ]]; mkdir -p "$out"
cc="$tools/aarch64-linux-android24-clang"
# The exact previously verified relocated package is the only SDK include/archive.
"$cc" -std=c17 -Wall -Wextra -Werror -fPIC -I "$package/include" -c android/AppPreview/activity.c -o "$out/activity.o"
"$cc" -std=c17 -Wall -Wextra -Werror -fPIC -I "$package/include" -c android/NativePreview/consumer.c -o "$out/consumer.o"
"$cc" -shared "$out/activity.o" "$out/consumer.o" "$package/lib/arm64-v8a/libmadinfer_ffi.a" \
  -Wl,-z,defs,-z,max-page-size=16384,-z,common-page-size=16384 \
  -lc++_static -lc++abi -ldl -lm -llog -latomic -o "$out/libmadinfer_preview.so"
"$tools/llvm-nm" --dynamic --defined-only "$out/libmadinfer_preview.so" | grep -E ' T ANativeActivity_onCreate$' >/dev/null
python3 scripts/check-android-preview.py "$package" "$out/libmadinfer_preview.so" "$tools"
"$bt/aapt2" link --manifest android/AppPreview/AndroidManifest.xml -I "$platform/android.jar" -o "$out/unaligned.apk"
python3 - "$out" <<'PY'
import pathlib,sys,zipfile
p=pathlib.Path(sys.argv[1])
with zipfile.ZipFile(p/'unaligned.apk','a') as z:
    z.write(p/'libmadinfer_preview.so','lib/arm64-v8a/libmadinfer_preview.so',compress_type=zipfile.ZIP_STORED)
PY
"$bt/zipalign" -P 16 4 "$out/unaligned.apk" "$out/preview-unsigned.apk"
"$bt/zipalign" -c -P 16 4 "$out/preview-unsigned.apk"
python3 scripts/check-android-apk.py "$out/preview-unsigned.apk" "$out/libmadinfer_preview.so"
