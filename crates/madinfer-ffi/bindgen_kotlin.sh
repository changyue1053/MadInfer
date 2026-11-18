#!/usr/bin/env bash
# I-CD-01：madinfer-ffi 的 Kotlin 绑定生成命令链（生成物不入库，见本目录 .gitignore）。
# 用法：bash crates/madinfer-ffi/bindgen_kotlin.sh   （在 MadInfer 仓根执行；全程 --offline）
#
# 形态同 bindgen_swift.sh（仓外 0.29.4 bindgen 宿主的论证见那里，不重复）。本脚本产出的是
# **单文件 jvm 形态** Kotlin 绑定（uniffi-bindgen 自带的 kotlin 语言后端），服务两类场景：
#   ① 非 Gobley 宿主的直连消费与 spike 验证；② 符号面 / 契约面的快速人工核对。
# mad 正式接线不用本脚本：KMP 多源集绑定（commonMain / jvmMain 两半）由 Gobley
# （gobley-uniffi-bindgen 0.3.7，钉 uniffi_bindgen =0.29.4）在 mad gradle 侧从同一 cdylib
# 元数据生成进 build/generated/uniffi/（capability-infer-ffi 的 uniffi { generateFromLibrary() }），
# 与 imcore im-core-ffi 同一驱动方式，生成物同样不入库。
#
# 两 profile 同一份绑定（madai-w11 I-C-10；MADINFER_BINDGEN_PROFILE 分叉退役）：见 bindgen_swift.sh 同段——每次从 debug 与
# release 两枚宿主 cdylib 各生成一份，`diff -r` 为空才把 release 那份落到 bindings/，不为空退出 1。仍设该变量只打一行"已退役、忽略"。
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
scratch="${MADINFER_BINDGEN_SCRATCH:-/tmp/madinfer-bindgen-host}"
lang=kotlin
out_dir="crates/madinfer-ffi/bindings/$lang"

if [[ -n "${MADINFER_BINDGEN_PROFILE:-}" ]]; then
  echo "[bindgen] MADINFER_BINDGEN_PROFILE=$MADINFER_BINDGEN_PROFILE 已退役（madai-w11 I-C-10）：两 profile 生成同一份绑定，忽略该变量" >&2
fi

# ① 仓外一次性构建 0.29.4 bindgen 宿主（与 bindgen_swift.sh 共用同一 scratch）
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

# ③ 各生成一份 Kotlin（madinfer_ffi.kt，包名 uniffi.madinfer_ffi；JNA 直连形态）到 scratch，diff 为空才落 bindings/
# --no-format（madai-w09 I-C-08）：uniffi-bindgen 缺省生成后调 `ktlint -F` 整理源码，机器无 ktlint 时打
# "Warning: Unable to auto-format … using ktlint"（退出码仍 0）——门禁读输出会误判红。本脚本的产物是契约核对物
# 不是交付源码，关掉格式化：产物字节与 ktlint 缺席时的缺省路径逐字节相同（ktlint 找不到即不改文件），有 ktlint
# 的机器上也不再被格式化——两台机器产同一份文件才能 diff。
compare="$scratch/compare-$lang"
rm -rf "$compare"
for profile in debug release; do
  "$scratch/target/release/madinfer-bindgen-host" generate \
    --library "target/$profile/libmadinfer_ffi.dylib" \
    --language "$lang" \
    --no-format \
    --out-dir "$compare/$profile"
done
if ! diff -r "$compare/debug" "$compare/release"; then
  echo "[bindgen] debug / release 宿主 cdylib 生成的 $lang 绑定不同：符号面随 profile 分叉了（见上方 diff）" >&2
  exit 1
fi
echo "[bindgen] $lang 绑定 debug / release 两 profile diff -r 为空：$(find "$compare/release" -type f | sed "s#$compare/release/##" | tr '\n' ' ')"
rm -rf "$out_dir"
mkdir -p "$(dirname "$out_dir")"
cp -R "$compare/release" "$out_dir"
ls -la "$out_dir/uniffi/madinfer_ffi/"
