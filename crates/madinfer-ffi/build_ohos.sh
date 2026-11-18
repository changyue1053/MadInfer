#!/usr/bin/env bash
# I-C-06：madinfer-ffi 的鸿蒙 staticlib 产物流（产物不入库，见本目录 .gitignore dist/）。
# 用法：bash crates/madinfer-ffi/build_ohos.sh   （在 MadInfer 仓根执行；全程 --offline --locked）
#
# 形态同 bindgen_*.sh：一条命令出产物 + 自检留痕。产出：
#   crates/madinfer-ffi/dist/ohos/libmadinfer_ffi.a      release staticlib（aarch64-unknown-linux-ohos；mad 侧 K/N sharedLib 静态收纳、
#                                                      或过渡期 -DMADINFER_LINK_TARGET=<本文件> 直接链进 libmadinfer_napi.so）
#   crates/madinfer-ffi/dist/ohos/include/madinfer.h      头文件拷贝（事实源 crates/madinfer-ffi/include/madinfer.h；mad 侧镜像由总控 sync 门比对）
#   crates/madinfer-ffi/dist/ohos/include/madinfer_debug.h 调试面头文件拷贝（两符号两 profile 都导出，release 函数体回 4005）
#   crates/madinfer-ffi/dist/ohos/SYMBOLS.txt            llvm-nm 的 madinfer_* 导出清单（release；恰 14 个才算过——十符号 + 调试面两符号；madai-w14 I-C-12 前 11）
#   crates/madinfer-ffi/dist/ohos/SYMBOLS.debug.txt      同上、debug staticlib 的清单；两份 diff 为空才算过（madai-w11 I-C-10：单核形态下
#                                                      KMP 绑定自 debug 宿主 cdylib 生成、release .so 缺符号即 JNA 整库装载失败，符号面须两 profile 同数）
# 工具链口径（沿 I-H-01 spike / mad shell/lib/release_ohos.sh ①）：仓钉 1.85 未装 ohos 目标，本脚本用 rustup stable
# （RUSTUP_TOOLCHAIN 覆盖 rust-toolchain.toml；可用 MADINFER_OHOS_TOOLCHAIN 改），链接器 / cc / ar 取 DevEco SDK 自带的
# aarch64-unknown-linux-ohos-clang 包装器与 llvm-ar（OHOS_NATIVE 指向 SDK native 目录，缺省 DevEco 安装位）。
# 产物流不产 cdylib 供鸿蒙消费（lane-rules 2：mad 进程内单核二进制；cargo 顺带出的 .so 留在 target/ 不拷）。
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
native="${OHOS_NATIVE:-/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native}"
bin="$native/llvm/bin"
target=aarch64-unknown-linux-ohos
toolchain="${MADINFER_OHOS_TOOLCHAIN:-stable}"
dist="$root/crates/madinfer-ffi/dist/ohos"
expected_symbols=14

if [[ ! -x "$bin/aarch64-unknown-linux-ohos-clang" ]]; then
  echo "[ohos] 缺 DevEco SDK clang 包装器：$bin/aarch64-unknown-linux-ohos-clang（设 OHOS_NATIVE 指向 SDK native 目录）" >&2
  exit 3
fi
if ! rustup "+$toolchain" target list --installed 2>/dev/null | grep -qx "$target"; then
  echo "[ohos] rustup $toolchain 未装目标 $target（rustup +$toolchain target add $target）" >&2
  exit 3
fi

export RUSTUP_TOOLCHAIN="$toolchain"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$bin/aarch64-unknown-linux-ohos-clang"
export CC_aarch64_unknown_linux_ohos="$bin/aarch64-unknown-linux-ohos-clang"
export CXX_aarch64_unknown_linux_ohos="$bin/aarch64-unknown-linux-ohos-clang++"
export AR_aarch64_unknown_linux_ohos="$bin/llvm-ar"

cd "$root"
echo "===== [1/4] cargo build -p madinfer-ffi --release --target $target --offline --locked（toolchain=$(rustc --version)） ====="
cargo build -p madinfer-ffi --release --target "$target" --offline --locked
echo "===== [2/4] cargo build -p madinfer-ffi --target $target --offline --locked（debug profile，只为符号面比对） ====="
cargo build -p madinfer-ffi --target "$target" --offline --locked

out="$root/target/$target/release"
mkdir -p "$dist/include"
cp "$out/libmadinfer_ffi.a" "$dist/libmadinfer_ffi.a"
cp "$root/crates/madinfer-ffi/include/madinfer.h" "$dist/include/madinfer.h"
cp "$root/crates/madinfer-ffi/include/madinfer_debug.h" "$dist/include/madinfer_debug.h"

# --no-llvm-bc：workspace release profile（lto = fat）让 cargo 给每个成员 .o 内嵌 .llvmbc（rustc LLVM 22 产），SDK 的
# llvm-nm（LLVM 15）默认会去读那段 bitcode 并报 "Unknown attribute kind" 退出 1——符号表本身是普通 ELF，关掉 bitcode 读取即可。
# 链接不受影响（lld 不带 -flto 时忽略 .llvmbc）。
list_symbols() {
  "$bin/llvm-nm" --no-llvm-bc --defined-only "$1" 2>/dev/null \
    | awk '$2 == "T" && $3 ~ /^madinfer_/ { print $3 }' | sort -u
}

echo "===== [3/4] llvm-nm：madinfer_* 导出（release / debug 各恰 $expected_symbols 个且同名） ====="
list_symbols "$dist/libmadinfer_ffi.a" > "$dist/SYMBOLS.txt"
list_symbols "$root/target/$target/debug/libmadinfer_ffi.a" > "$dist/SYMBOLS.debug.txt"
count="$(wc -l < "$dist/SYMBOLS.txt" | tr -d ' ')"
count_debug="$(wc -l < "$dist/SYMBOLS.debug.txt" | tr -d ' ')"
sed 's/^/    T /' "$dist/SYMBOLS.txt"
if [[ "$count" != "$expected_symbols" ]]; then
  echo "[ohos] release madinfer_* 导出 = $count，期望 $expected_symbols" >&2
  exit 1
fi
if [[ "$count_debug" != "$expected_symbols" ]]; then
  echo "[ohos] debug madinfer_* 导出 = $count_debug，期望 $expected_symbols" >&2
  exit 1
fi
if ! diff -q "$dist/SYMBOLS.txt" "$dist/SYMBOLS.debug.txt" >/dev/null; then
  echo "[ohos] debug / release 的 madinfer_* 导出清单不同：" >&2
  diff "$dist/SYMBOLS.txt" "$dist/SYMBOLS.debug.txt" >&2 || true
  exit 1
fi
form="$(strings "$dist/libmadinfer_ffi.a" | grep -m1 -o 'form=napi-rust abi=[0-9]*' || true)"
[[ -n "$form" ]] || { echo "[ohos] 产物内缺留痕标签 form=napi-rust" >&2; exit 1; }

echo "===== [4/4] 产物 ====="
size="$(stat -f%z "$dist/libmadinfer_ffi.a" 2>/dev/null || stat -c%s "$dist/libmadinfer_ffi.a")"
echo "[ohos] staticlib=${dist#"$root"/}/libmadinfer_ffi.a bytes=$size symbols=$count symbols_debug=$count_debug $form header=${dist#"$root"/}/include/madinfer.h"
echo "[ohos] 交接：mad shell/shellkit-infer-ohos/native/CMakeLists.txt 以 -DMADINFER_LINK_TARGET=<上述 .a> -DMADINFER_NAPI_FORM=napi-rust 换链接对象（C 桩 madinfer_stub.c 退役）"
