//! madinfer-core-types —— 端智能 SDK 全部 crate 共享的底座类型（《端智能通用 SDK 架构设计（Rust 核心）》§4.1 C1）。
//!
//! 只定义标识、枚举、错误、预算、追踪上下文；不含行为，不含业务语义。
//! 三条不变量：
//! 1. 本 crate 的破坏性变更等于整个 SDK 主版本升级；
//! 2. [`PrivacyTag`] 的判别值不得改动（进入观测与云端协议），比较用 `Ord` 而不是匹配；
//! 3. 错误码稳定映射 [`InferError::code`]（附录 B），只加不改。
//!
//! 与 KMP 侧 `capability-infer-api` 的镜像关系靠 `schemas/*.v1.json` 与本文件的 `wire()` 字符串对齐；
//! 两侧枚举顺序一致、加变体只准追加在尾部。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod budget;
mod error;
mod ids;
mod slot;
mod trace;

pub use budget::{Budget, QoS};
pub use error::{DegradeKind, FatalKind, InferError, RetryKind};
pub use ids::{
    AssetId, BackendId, BusinessId, CapabilityId, ContextKey, Os, PackageRef, PrivacyTag,
    RequestId, Source, Tier,
};
pub use slot::SlotClass;
pub use trace::TraceCtx;

/// 协议文档顶层 `schema` 字段的当前值；所有 v1 文档共用。
pub const SCHEMA_VERSION: u32 = 1;

/// C ABI 版本（`infer_abi_version()` 返回值）；宿主与核不一致时宿主拒绝加载。
pub const ABI_VERSION: u32 = 1;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn privacy_tags_are_ordered_and_wire_stable() {
        assert!(PrivacyTag::Public < PrivacyTag::Internal);
        assert!(PrivacyTag::Internal < PrivacyTag::E2eePlaintext);
        assert!(PrivacyTag::E2eePlaintext < PrivacyTag::Regulated);
        assert_eq!(PrivacyTag::E2eePlaintext.wire(), "e2ee_plaintext");
        assert_eq!(
            PrivacyTag::Regulated as u8,
            3,
            "判别值进入观测与云端协议，不得改动"
        );
        assert!(PrivacyTag::E2eePlaintext.local_only());
        assert!(!PrivacyTag::Internal.local_only());
    }

    #[test]
    fn error_codes_follow_the_three_bands() {
        assert_eq!(
            InferError::Retryable {
                kind: RetryKind::Busy,
                retry_after_ms: 200
            }
            .code(),
            1001
        );
        assert_eq!(
            InferError::Degradable {
                kind: DegradeKind::Deadline
            }
            .code(),
            2001
        );
        assert_eq!(
            InferError::Fatal {
                kind: FatalKind::CapabilityDisabled,
                detail: 0
            }
            .code(),
            3002
        );
        assert_eq!(
            InferError::Fatal {
                kind: FatalKind::Internal,
                detail: 7
            }
            .code(),
            3907
        );
        assert_eq!(
            InferError::Fatal {
                kind: FatalKind::Internal,
                detail: 250
            }
            .code(),
            3999,
            "detail 截到 0～99"
        );
        for kind in RetryKind::ALL {
            assert!((1000..2000).contains(&kind.code()), "{kind:?}");
        }
        for kind in DegradeKind::ALL {
            assert!((2000..3000).contains(&kind.code()), "{kind:?}");
        }
        for kind in FatalKind::ALL {
            assert!((3000..4000).contains(&kind.code()), "{kind:?}");
        }
    }

    #[test]
    fn budget_deadline_is_clamped_to_protocol_range() {
        assert_eq!(
            Budget::new(10, QoS::Interactive).deadline_ms,
            Budget::MIN_DEADLINE_MS
        );
        assert_eq!(
            Budget::new(10_000_000, QoS::Background).deadline_ms,
            Budget::MAX_DEADLINE_MS
        );
        assert_eq!(Budget::interactive().qos, QoS::Interactive);
        assert_eq!(QoS::Background.wire(), "background");
    }

    #[test]
    fn identifiers_validate_their_wire_form() {
        assert!(CapabilityId::new("summarize").is_some());
        assert!(CapabilityId::new("").is_none());
        assert!(BusinessId::new(0).is_none());
        assert!(BusinessId::new(65_535).is_some());
        assert!(AssetId::parse("sha256:9f3c").is_none(), "hex 长度必须为 64");
        let hex = "9f".repeat(32);
        assert!(AssetId::parse(&format!("sha256:{hex}")).is_some());
        let package = PackageRef::parse("catchup-summary@1.4.2").expect("well-formed");
        assert_eq!(package.name(), "catchup-summary");
        assert_eq!(package.version(), "1.4.2");
        assert!(PackageRef::parse("no-version").is_none());
        assert!(ContextKey::new(&"x".repeat(129)).is_none());
    }

    #[test]
    fn trace_ctx_parses_w3c_hex() {
        let trace = TraceCtx::parse("0af7651916cd43dd8448eb211c80319c", "b7ad6b7169203331", true)
            .expect("valid");
        assert_eq!(
            trace.traceparent(),
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        );
        assert!(TraceCtx::parse("short", "b7ad6b7169203331", true).is_none());
    }
}
