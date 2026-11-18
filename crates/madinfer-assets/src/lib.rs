//! madinfer-assets —— L5 资产（设计文档 C10）。
//!
//! - [`install`]：安装 FSM（11 条迁移）：Discovered → ManifestVerified → Downloading → Verifying → Installed → Active；
//!   验签失败 / hash 不符 / 下载永久失败 → Quarantined。验签先于一切：manifest 验签失败不进入下载；blob 到齐先比 sha256 再入库。
//! - [`store`]：内容寻址布局（`blobs/sha256/xx/…`、`packages/<name>/<ver>/manifest.json`）、安装计划（去重共享 blob）、
//!   [`store::PackageRegistry`] 的 ACTIVE / PREVIOUS 指针（激活 = 指针切换，回滚 = 指针切回）、GC 计划（引用为零且超保留期）。
//! - [`verify`]：sha256 纯 Rust 手写实现（FIPS 180-4，官方向量锁行为）+ blob 内容寻址校验入口 [`verify_blob`]
//!   （madai-w05 I-C-04；ed25519 + JCS 验签本体仍以 [`install::ManifestCheck`] 注入，实装归后续批次）。
//! - [`crypto`]：加密 blob 的解密契约——[`EncSpec`]（manifest.v1 `blobs[].enc`）+ [`BlobDecryptor`] 端口 +
//!   [`decrypt_and_verify`] 复核入口；真实 AES-256-GCM 实装归 I4（中间态，生产装配期断言拒绝替身）。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod crypto;
pub mod install;
pub mod store;
pub mod verify;

pub use crypto::{decrypt_and_verify, BlobDecryptor, DecryptError, EncAlg, EncSpec, GCM_NONCE_LEN};
pub use install::{
    build_install_fsm, InstallEffect, InstallEvent, InstallEventKind, InstallState, Installer,
    ManifestCheck, INSTALL_FSM_TABLE_SIZE,
};
pub use store::{
    BlobKind, BlobRef, GcPolicy, InstallPlan, InstalledPackage, PackageRegistry, StoreView,
};
pub use verify::{sha256, sha256_hex, verify_blob, BlobMismatch, Sha256};
