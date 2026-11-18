//! madinfer-ffi 构建脚本（madai-w08 I-C-07 G7）：Android 目标的 16 KB 页对齐链接参数。
//!
//! Android 15 起支持 16 KB 页设备，so 的 PT_LOAD 段对齐须 ≥ 0x4000（mad 侧 `run_android.sh` 以 `llvm-readelf` 硬判）。此前靠 mad
//! gradle 的 RUSTFLAGS 中间态补（I-D-05 报告 §31），本脚本落地后可删。
//!
//! 作用范围：`cargo:rustc-cdylib-link-arg` 只作用于**本 crate 自己产的 cdylib**（Cargo 语义：依赖的 build script 不能给上游最终产物
//! 加链接参数）。所以它覆盖的是 madinfer-ffi 独立形态（非 mad 宿主 dlopen 的 `libmadinfer_ffi.so`）；mad 形态下最终 so 是
//! im-core-ffi 的 `libim_core_uniffi.so`，对齐参数由 imcore 自己的 build.rs / RUSTFLAGS 负责（I-CS-01 契约 §"同一二进制"）。
//! 独立形态由 feature `standalone`（缺省开）标识；作依赖时宿主 `default-features = false` 关掉，本脚本不再产任何链接参数。

use std::env;

fn main() {
    // 只在目标三元组变化时重跑（build.rs 本体变了 cargo 自会重跑）。
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STANDALONE");
    let standalone = env::var_os("CARGO_FEATURE_STANDALONE").is_some();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if standalone && target_os == "android" {
        // 16 KB 页对齐：ELF PT_LOAD 段 p_align = 0x4000。common-page-size 一并给到 16 KB，避免 lld 在 4 KB 设备上多留空洞。
        println!("cargo:rustc-cdylib-link-arg=-Wl,-z,max-page-size=16384");
        println!("cargo:rustc-cdylib-link-arg=-Wl,-z,common-page-size=16384");
    }
}
