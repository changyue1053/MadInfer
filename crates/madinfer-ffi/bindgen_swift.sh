#!/usr/bin/env bash
# I-S-01：madinfer-ffi 的 Swift 绑定生成命令链（生成物不入库，见本目录 .gitignore）。
# 用法：bash crates/madinfer-ffi/bindgen_swift.sh   （在 MadInfer 仓根执行；全程 --offline）
#
# 为什么要仓外 bindgen 宿主：uniffi_bindgen 0.29.4 不发独立可执行档（crates.io 包无 [[bin]]），
# 官方形态 = 自建 bin 调 uniffi::uniffi_bindgen_main()（cli feature）。本机 ~/.cargo/bin 的
# uniffi-bindgen 是 0.31.1——scaffolding 与 bindgen 版本必须一致（uniffi contract version 校验），
# 不能拿它给 0.29.4 的库生成。宿主放仓外（缺省 /tmp/madinfer-bindgen-host）：cli feature 会把
# clap + uniffi_bindgen 闭包（askama / goblin …）拉进解析图，进本仓 workspace 会撑大 deny 白名单
# ——imcore 同款取舍（bindgen 活在 Gobley/gradle，不在 rust workspace）。
# mad 正式接线（I3）不用本脚本：KMP 绑定由 Gobley（gobley-uniffi-bindgen 0.3.7，钉 uniffi_bindgen
# =0.29.4）在 mad gradle 侧生成；本脚本只服务 spike 验证与非 mad 宿主的 Swift 直连形态。
#
# 两 profile 同一份绑定（madai-w11 I-C-10；MADINFER_BINDGEN_PROFILE 分叉退役）：0.9.0 的调试面只在 debug_assertions 下存在，
# 绑定面随宿主 cdylib 的 profile 分叉，madai-w10 总控只能以 MADINFER_BINDGEN_PROFILE 选边。0.10.0 起调试面符号两 profile 都导出
# （release 函数体回 4005 FeatureDisabled），绑定面不再随 profile 变——本脚本每次都从 debug 与 release 两枚宿主 cdylib 各生成
# 一份，`diff -r` 为空才把 release 那份落到 bindings/；不为空即退出 1（那是符号面又分叉了的信号，不是该被放宽的信号）。
# 仍设 MADINFER_BINDGEN_PROFILE 只打一行"已退役、忽略"，产物不变。
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
scratch="${MADINFER_BINDGEN_SCRATCH:-/tmp/madinfer-bindgen-host}"
lang=swift
out_dir="crates/madinfer-ffi/bindings/$lang"

if [[ -n "${MADINFER_BINDGEN_PROFILE:-}" ]]; then
  echo "[bindgen] MADINFER_BINDGEN_PROFILE=$MADINFER_BINDGEN_PROFILE 已退役（madai-w11 I-C-10）：两 profile 生成同一份绑定，忽略该变量" >&2
fi

# ① 仓外一次性构建 0.29.4 bindgen 宿主（~/.cargo/registry 缓存离线可得，实测 27 s）
if [[ ! -x "$scratch/target/release/madinfer-bindgen-host" ]]; then
  mkdir -p "$scratch/src"
  cat > "$scratch/Cargo.toml" <<'EOF'
# I-S-01 仓外 bindgen 宿主（不属任何 workspace）：版本与 MadInfer scaffolding 同锁 0.29.4。
[package]
name = "madinfer-bindgen-host"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
uniffi = { version = "=0.29.4", features = ["cli"] }

[workspace]
EOF
  cat > "$scratch/src/main.rs" <<'EOF'
fn main() {
    uniffi::uniffi_bindgen_main()
}
EOF
  (cd "$scratch" && cargo build --release --offline)
fi

# ② 两枚 host cdylib（内嵌 uniffi 元数据；--library 模式的输入）
cd "$root"
cargo build -p madinfer-ffi --offline
cargo build -p madinfer-ffi --offline --release

# ③ 各生成一份 Swift 三件（madinfer_ffi.swift / madinfer_ffiFFI.h / madinfer_ffiFFI.modulemap）到 scratch，diff 为空才落 bindings/
compare="$scratch/compare-$lang"
rm -rf "$compare"
for profile in debug release; do
  "$scratch/target/release/madinfer-bindgen-host" generate \
    --library "target/$profile/libmadinfer_ffi.dylib" \
    --language "$lang" \
    --out-dir "$compare/$profile"
done
if ! diff -r "$compare/debug" "$compare/release"; then
  echo "[bindgen] debug / release 宿主 cdylib 生成的 $lang 绑定不同：符号面随 profile 分叉了（见上方 diff）" >&2
  exit 1
fi
echo "[bindgen] $lang 绑定 debug / release 两 profile diff -r 为空：$(ls "$compare/release" | tr '\n' ' ')"
rm -rf "$out_dir"
mkdir -p "$(dirname "$out_dir")"
cp -R "$compare/release" "$out_dir"
ls -la "$out_dir/"
