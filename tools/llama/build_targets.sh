#!/usr/bin/env bash
# 三端 + 宿主交叉编译 llama.cpp 静态库并收集产物（madai-w17 I-C-17；I4-01 `MADINFER_LLAMA_LIB_DIR` 的目录形态）。
#
# 用法：bash tools/llama/build_targets.sh [--targets a,b,…] [--tag <llama tag>] [--out DIR] [--log-dir DIR] [--with-ffi]
#   --targets   逗号分隔的 rust 三元组；缺省四目标：aarch64-apple-darwin,aarch64-apple-ios-sim,aarch64-linux-android,aarch64-unknown-linux-ohos
#   --tag       llama.cpp tag（缺省读 third_party/PINS.toml [llama_cpp] tag）
#   --out       产物根目录（缺省 target/madinfer-llama）；每目标落 <out>/<tag>/<triple>/{5 个 .a, MANIFEST.txt}
#   --log-dir   cargo 全量日志目录（缺省 <out>/<tag>/logs）；每目标 build-<triple>.log（--with-ffi 另有 build-ffi-<triple>.log）
#   --with-ffi  每目标再 `cargo build -p madinfer-ffi --features madinfer-backend-llama/llama-sys --target <t> --release`（cdylib / staticlib 链得过），
#               libmadinfer_ffi.{a,dylib,so} 字节写进 MANIFEST（不拷贝：ffi 产物不是 MADINFER_LLAMA_LIB_DIR 的内容）
# 行为：对每个目标 `cargo build -p madinfer-backend-llama --features llama-sys --target <t> --release --offline`，从
#   target/<t>/release/build/madinfer-backend-llama-*/out/llama-build/lib/ 取最新一组 5 个 .a（llama / ggml / ggml-base / ggml-cpu /
#   madinfer_llama_bridge）拷到产物目录，写 MANIFEST.txt（tag / triple / 工具链版本 / cmake 版本 / 构建时长 / 每文件 sha256 与字节）。
#   失败目标写实：该目标的 MANIFEST 不写、目录清空，继续下一目标，末行 RESULT 点名，exit 1。
# 环境：ANDROID_NDK_HOME（缺省 ~/Library/Android/sdk/ndk/27.2.12479018）、OHOS_SDK_NATIVE（缺省 DevEco 内嵌）、
#   MADINFER_OHOS_RUSTUP_TOOLCHAIN（缺省 stable：rust-toolchain.toml 钉的 1.85 没装 ohos 目标）。
#   鸿蒙 / Android 的 rustc 链接器由本脚本以 CARGO_TARGET_<T>_LINKER 给（与 mad 仓 tools/ohos/build_entry.sh 同法），不改 .cargo/config.toml。
# 退出码：0 全部目标绿；1 有目标红（末行 RESULT 点名）；2 参数 / 工具链缺席。只用 bash 3.2 + coreutils + shasum / sha256sum。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DEFAULT_TARGETS="aarch64-apple-darwin,aarch64-apple-ios-sim,aarch64-linux-android,aarch64-unknown-linux-ohos"
TARGETS="${DEFAULT_TARGETS}"; TAG=""; OUT_ROOT="${ROOT}/target/madinfer-llama"; LOG_DIR=""; WITH_FFI=0
while [ $# -gt 0 ]; do
  case "$1" in
    --targets) TARGETS="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --out) OUT_ROOT="$2"; shift 2 ;;
    --log-dir) LOG_DIR="$2"; shift 2 ;;
    --with-ffi) WITH_FFI=1; shift ;;
    -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
    *) echo "[err] 未知参数 $1"; exit 2 ;;
  esac
done
case "${OUT_ROOT}" in /*) ;; *) OUT_ROOT="${ROOT}/${OUT_ROOT}" ;; esac
[ -n "${TAG}" ] || TAG="$(sed -nE 's/^tag[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "${ROOT}/third_party/PINS.toml" | head -1)"
[ -n "${TAG}" ] || { echo "[err] third_party/PINS.toml 读不到 [llama_cpp] tag，给 --tag"; exit 2; }
[ -n "${LOG_DIR}" ] || LOG_DIR="${OUT_ROOT}/${TAG}/logs"
case "${LOG_DIR}" in /*) ;; *) LOG_DIR="${ROOT}/${LOG_DIR}" ;; esac
mkdir -p "${LOG_DIR}"

NDK="${ANDROID_NDK_HOME:-${HOME}/Library/Android/sdk/ndk/27.2.12479018}"
NDK_BIN="${NDK}/toolchains/llvm/prebuilt/darwin-x86_64/bin"
OHOS_SDK="${OHOS_SDK_NATIVE:-/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native}"
OHOS_BIN="${OHOS_SDK}/llvm/bin"
OHOS_TC="${MADINFER_OHOS_RUSTUP_TOOLCHAIN:-stable}"
STATIC_LIBS="libmadinfer_llama_bridge.a libllama.a libggml.a libggml-cpu.a libggml-base.a"

sha256_of() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'; else sha256sum "$1" | awk '{print $1}'; fi; }
bytes_of() { stat -f%z "$1" 2>/dev/null || stat -c%s "$1"; }

# 目标 → 该目标 cargo 命令前缀的 env 赋值（数组，按元素 append 给 env）。
target_env() {
  case "$1" in
    aarch64-linux-android)
      echo "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=${NDK_BIN}/aarch64-linux-android24-clang"
      echo "CARGO_TARGET_AARCH64_LINUX_ANDROID_AR=${NDK_BIN}/llvm-ar"
      echo "ANDROID_NDK_HOME=${NDK}" ;;
    aarch64-unknown-linux-ohos)
      echo "RUSTUP_TOOLCHAIN=${OHOS_TC}"
      echo "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER=${OHOS_BIN}/aarch64-unknown-linux-ohos-clang"
      echo "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_AR=${OHOS_BIN}/llvm-ar"
      echo "OHOS_SDK_NATIVE=${OHOS_SDK}" ;;
    *) ;;
  esac
}
# 目标 → C 编译器版本一行（写进 MANIFEST）。
cc_version() {
  case "$1" in
    aarch64-apple-*) xcrun clang --version 2>/dev/null | head -1 ;;
    aarch64-linux-android) "${NDK_BIN}/clang" --version 2>/dev/null | head -1 ;;
    aarch64-unknown-linux-ohos) "${OHOS_BIN}/clang" --version 2>/dev/null | head -1 ;;
    *) cc --version 2>/dev/null | head -1 ;;
  esac
}
rustc_version() {
  case "$1" in
    aarch64-unknown-linux-ohos) rustup run "${OHOS_TC}" rustc --version 2>/dev/null ;;
    *) rustc --version ;;
  esac
}
toolchain_present() {
  case "$1" in
    aarch64-linux-android) [ -x "${NDK_BIN}/aarch64-linux-android24-clang" ] || { echo "NDK 缺席 ${NDK_BIN}"; return 1; } ;;
    aarch64-unknown-linux-ohos)
      [ -x "${OHOS_BIN}/aarch64-unknown-linux-ohos-clang" ] || { echo "DevEco 缺席 ${OHOS_BIN}"; return 1; }
      rustup run "${OHOS_TC}" rustc --print target-list 2>/dev/null | grep -qx "$1" || { echo "rustup toolchain ${OHOS_TC} 无目标 $1"; return 1; } ;;
    *) ;;
  esac
  return 0
}
# 该目标最新一组静态库所在目录（同 target 多个 build hash 时取 bridge .a 最新的那份）。
lib_dir_of() {
  ls -t "${ROOT}/target/$1/release/build/"madinfer-backend-llama-*/out/llama-build/lib/libmadinfer_llama_bridge.a 2>/dev/null | head -1 | xargs -I{} dirname {}
}
ffi_artifacts() { # <triple> → 该目标 release 下的 libmadinfer_ffi.* 清单
  ls "${ROOT}/target/$1/release/"libmadinfer_ffi.a "${ROOT}/target/$1/release/"libmadinfer_ffi.dylib "${ROOT}/target/$1/release/"libmadinfer_ffi.so 2>/dev/null || true
}

CMAKE_VER="$(cmake --version 2>/dev/null | head -1)"
N=0; OK=0; FAILED=""
echo "===== build_targets.sh · tag=${TAG} · out=${OUT_ROOT#"${ROOT}"/}/${TAG} · targets=${TARGETS} · with_ffi=${WITH_FFI} ====="
OLD_IFS="${IFS}"; IFS=','; set -- ${TARGETS}; IFS="${OLD_IFS}"
for T in "$@"; do
  N=$((N + 1))
  DEST="${OUT_ROOT}/${TAG}/${T}"
  LOG="${LOG_DIR}/build-${T}.log"
  echo "--- [${N}] ${T} ---"
  if ! reason="$(toolchain_present "${T}")"; then
    echo "[err]  ${T}: ${reason}"; FAILED="${FAILED} ${T}"; rm -rf "${DEST}"; continue
  fi
  ENVS=()
  while IFS= read -r line; do [ -n "${line}" ] && ENVS+=("${line}"); done <<EOF
$(target_env "${T}")
EOF
  t0=$(date +%s)
  echo "RUN  env ${ENVS[*]:-} cargo build -p madinfer-backend-llama --features llama-sys --target ${T} --release --offline  (log ${LOG#"${ROOT}"/})"
  if (cd "${ROOT}" && env ${ENVS[@]+"${ENVS[@]}"} cargo build -p madinfer-backend-llama --features llama-sys --target "${T}" --release --offline) > "${LOG}" 2>&1; then
    rc=0
  else
    rc=$?
  fi
  elapsed=$(( $(date +%s) - t0 ))
  if [ "${rc}" != 0 ]; then
    echo "[err]  ${T}: cargo exit=${rc} elapsed=${elapsed}s（grep -n 'error' ${LOG#"${ROOT}"/}）"
    grep -n "^error\|error:" "${LOG}" | head -5
    FAILED="${FAILED} ${T}"; rm -rf "${DEST}"; continue
  fi
  SRC="$(lib_dir_of "${T}")"
  MISSING=""
  for lib in ${STATIC_LIBS}; do [ -n "${SRC}" ] && [ -f "${SRC}/${lib}" ] || MISSING="${MISSING} ${lib}"; done
  if [ -n "${MISSING}" ]; then
    echo "[err]  ${T}: cargo 绿但静态库缺${MISSING}（${SRC:-<无 out/llama-build/lib>}）"
    FAILED="${FAILED} ${T}"; rm -rf "${DEST}"; continue
  fi
  ffi_elapsed=""; FFI_LIST=""
  if [ "${WITH_FFI}" = 1 ]; then
    FFI_LOG="${LOG_DIR}/build-ffi-${T}.log"
    t1=$(date +%s)
    echo "RUN  env ${ENVS[*]:-} cargo build -p madinfer-ffi --features madinfer-backend-llama/llama-sys --target ${T} --release --offline  (log ${FFI_LOG#"${ROOT}"/})"
    if (cd "${ROOT}" && env ${ENVS[@]+"${ENVS[@]}"} cargo build -p madinfer-ffi --features madinfer-backend-llama/llama-sys --target "${T}" --release --offline) > "${FFI_LOG}" 2>&1; then
      ffi_elapsed=$(( $(date +%s) - t1 ))
      FFI_LIST="$(ffi_artifacts "${T}")"
    else
      rc=$?
      echo "[err]  ${T}: madinfer-ffi cargo exit=${rc}（grep -n 'error' ${FFI_LOG#"${ROOT}"/}）"
      grep -n "^error\|error:\|undefined\|ld:" "${FFI_LOG}" | head -8
      FAILED="${FAILED} ${T}(ffi)"; rm -rf "${DEST}"; continue
    fi
  fi
  rm -rf "${DEST}"; mkdir -p "${DEST}"
  TOTAL=0
  {
    echo "# madinfer llama.cpp prebuilt static libs（tools/llama/build_targets.sh）"
    echo "llama_tag=${TAG}"
    echo "triple=${T}"
    echo "rustc=$(rustc_version "${T}")"
    echo "cc=$(cc_version "${T}")"
    echo "cmake=${CMAKE_VER}"
    echo "cargo_build_seconds=${elapsed}"
    [ -z "${ffi_elapsed}" ] || echo "ffi_build_seconds=${ffi_elapsed}"
    echo "built_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "# sha256  bytes  file"
  } > "${DEST}/MANIFEST.txt"
  for lib in ${STATIC_LIBS}; do
    cp "${SRC}/${lib}" "${DEST}/${lib}"
    b="$(bytes_of "${DEST}/${lib}")"; TOTAL=$((TOTAL + b))
    printf '%s  %s  %s\n' "$(sha256_of "${DEST}/${lib}")" "${b}" "${lib}" >> "${DEST}/MANIFEST.txt"
  done
  echo "static_total_bytes=${TOTAL}" >> "${DEST}/MANIFEST.txt"
  if [ -n "${FFI_LIST}" ]; then
    echo "# ffi（不拷贝，只记读数）：sha256  bytes  path" >> "${DEST}/MANIFEST.txt"
    for f in ${FFI_LIST}; do printf '%s  %s  %s\n' "$(sha256_of "${f}")" "$(bytes_of "${f}")" "${f#"${ROOT}"/}" >> "${DEST}/MANIFEST.txt"; done
  fi
  OK=$((OK + 1))
  echo "[ok]   ${T}: 5 .a 共 ${TOTAL} B，cargo ${elapsed}s${ffi_elapsed:+，ffi ${ffi_elapsed}s} → ${DEST#"${ROOT}"/}/"
done
if [ -z "${FAILED}" ]; then
  echo "RESULT build_targets.sh exit=0 targets=${N} ok=${OK} tag=${TAG} out=${OUT_ROOT#"${ROOT}"/}/${TAG}"
  exit 0
fi
echo "RESULT build_targets.sh exit=1 targets=${N} ok=${OK} failed=${FAILED# } tag=${TAG} out=${OUT_ROOT#"${ROOT}"/}/${TAG}"
exit 1
