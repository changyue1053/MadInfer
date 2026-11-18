//! 验签端口。真实实现（ed25519 + JCS RFC 8785 规范化）随 madinfer-assets 的 `verify.rs` 落地（manifest 与 config 共用同一套密钥与规范化）；
//! I2 只定义 trait 与开发 / 测试用的 [`TrustAllVerifier`]。生产构建禁止装配 TrustAllVerifier（madinfer-ffi 装配期断言）。
//!
//! madai-w11 I-C-10：`verify` 的结论从 bool 变为三态 [`VerifyOutcome`]——验签算力在宿主（madinfer-ffi `BridgeVerifier` 经宿主回调），
//! 宿主可能卡死，验签器要能说"没等到"（`TimedOut`）而不是把它算成"验假"；管线把 `TimedOut` 落在同一拒因
//! `RejectReason::SignatureInvalid { timed_out: true }`（拒因表不扩），只在 detail 留痕 `verify=timeout`。

use crate::doc::Signature;

/// 一次验签的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// 验真。
    Verified,
    /// 验假（算法 / 密钥 / 签名 / 载荷任一不符，或验签实现异常）。
    Rejected,
    /// 期限内没等到验签实现的回执（madinfer-ffi：宿主 `verify_ed25519` 超 `VERIFY_DEADLINE_MS`）。
    TimedOut,
}

/// 验签：`payload` 是去掉 `signature` 字段后的文档字节（由调用方按 JCS 规范化后传入）。
pub trait SignatureVerifier: Send + Sync {
    /// 三态结论；管线只把 [`VerifyOutcome::Verified`] 视为通过。
    fn verify(&self, payload: &[u8], signature: &Signature) -> VerifyOutcome;

    /// 已知的 key_id（配置里的 keys 轮换需在此登记）。
    fn knows_key(&self, key_id: &str) -> bool;
}

/// 全信（开发 / 测试）。
#[derive(Debug, Default, Clone, Copy)]
pub struct TrustAllVerifier;

impl SignatureVerifier for TrustAllVerifier {
    fn verify(&self, _payload: &[u8], signature: &Signature) -> VerifyOutcome {
        if signature.alg == "ed25519" && !signature.sig.is_empty() {
            VerifyOutcome::Verified
        } else {
            VerifyOutcome::Rejected
        }
    }

    fn knows_key(&self, _key_id: &str) -> bool {
        true
    }
}
