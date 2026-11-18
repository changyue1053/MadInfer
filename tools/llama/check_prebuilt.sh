#!/usr/bin/env bash
# MADINFER_LLAMA_LIB_DIR 缓存路径用例（madai-w17 I-C-17；I4-01 "预编静态库经 MADINFER_LLAMA_LIB_DIR 接入"的验收）。
#
# 用法：bash tools/llama/check_prebuilt.sh [--triple <t>] [--dir <lib dir>] [--tag <llama tag>]
#   --triple  目标三元组（缺省 aarch64-apple-darwin，宿主：不需要交叉链接器）
#   --dir     预编静态库目录（缺省 target/madinfer-llama/<tag>/<triple>，即 build_targets.sh 产物；先跑它）
# 两条断言（两条都过才 exit 0）：
#   ① 正例：CMAKE=/usr/bin/false + MADINFER_LLAMA_LIB_DIR=<dir> 下 `cargo build -p madinfer-backend-llama --features llama-sys
#      --target <t> --release --offline` 绿——目录在场 build.rs 一次都不调 cmake（调了就是 /usr/bin/false 退 1，构建红）；
#   ② 反例：把目录拷到临时处删掉 libggml-cpu.a，同样命令必须红且 stderr 点名 "缺静态库 libggml-cpu.a"（不静默回退 cmake）。
# 退出码：0 两条断言绿；1 任一红（末行 RESULT 点名）；2 目录缺席。日志落 target/madinfer-llama/<tag>/logs/check_prebuilt-*.log。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TRIPLE="aarch64-apple-darwin"; DIR=""; TAG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --triple) TRIPLE="$2"; shift 2 ;;
    --dir) DIR="$2"; shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "[err] 未知参数 $1"; exit 2 ;;
  esac
done
[ -n "${TAG}" ] || TAG="$(sed -nE 's/^tag[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "${ROOT}/third_party/PINS.toml" | head -1)"
[ -n "${DIR}" ] || DIR="${ROOT}/target/madinfer-llama/${TAG}/${TRIPLE}"
case "${DIR}" in /*) ;; *) DIR="${ROOT}/${DIR}" ;; esac
[ -f "${DIR}/libllama.a" ] || { echo "[err] ${DIR#"${ROOT}"/} 无 libllama.a：先跑 bash tools/llama/build_targets.sh --targets ${TRIPLE}"; exit 2; }
LOGS="${ROOT}/target/madinfer-llama/${TAG}/logs"; mkdir -p "${LOGS}"

BAD=0
say() { echo "$*"; }
build() { # build <log> <lib dir>
  (cd "${ROOT}" && env CMAKE=/usr/bin/false MADINFER_LLAMA_LIB_DIR="$2" \
     cargo build -p madinfer-backend-llama --features llama-sys --target "${TRIPLE}" --release --offline) > "$1" 2>&1
}

# ① 正例
POS_LOG="${LOGS}/check_prebuilt-positive-${TRIPLE}.log"
if build "${POS_LOG}" "${DIR}" && grep -q "用预编静态库" "${POS_LOG}"; then
  say "[ok]   正例：MADINFER_LLAMA_LIB_DIR=${DIR#"${ROOT}"/} 且 CMAKE=/usr/bin/false 构建绿（cmake 未被调用）"
else
  say "[err]  正例红：$(grep -n 'error\|panicked\|失败' "${POS_LOG}" | head -3)（${POS_LOG#"${ROOT}"/}）"; BAD=$((BAD + 1))
fi

# ② 反例：缺一个 .a 必须点名失败
TMP="$(mktemp -d "${TMPDIR:-/tmp}/madinfer-prebuilt-XXXXXX")"
trap 'rm -rf "${TMP}"' EXIT
cp "${DIR}"/*.a "${TMP}/" && rm -f "${TMP}/libggml-cpu.a"
NEG_LOG="${LOGS}/check_prebuilt-negative-${TRIPLE}.log"
if build "${NEG_LOG}" "${TMP}"; then
  say "[err]  反例假绿：缺 libggml-cpu.a 仍构建成功（${NEG_LOG#"${ROOT}"/}）"; BAD=$((BAD + 1))
elif grep -q "缺静态库 libggml-cpu.a" "${NEG_LOG}"; then
  say "[ok]   反例：缺 libggml-cpu.a → 构建红并点名（不回退 cmake）"
else
  say "[err]  反例红但未点名缺哪一个：$(grep -n 'panicked' "${NEG_LOG}" | head -2)（${NEG_LOG#"${ROOT}"/}）"; BAD=$((BAD + 1))
fi

if [ "${BAD}" = 0 ]; then echo "RESULT check_prebuilt.sh exit=0 triple=${TRIPLE} dir=${DIR#"${ROOT}"/} asserts=2 bad=0"; exit 0; fi
echo "RESULT check_prebuilt.sh exit=1 triple=${TRIPLE} dir=${DIR#"${ROOT}"/} asserts=2 bad=${BAD}"
exit 1
