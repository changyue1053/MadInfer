//! 后端错误与固定映射表（设计文档 §7.3）。枚举无自由文本：细节走观测的注册制字段，不走字符串。

use core::fmt;

use madinfer_core_types::{DegradeKind, FatalKind, InferError, RetryKind};

/// `Busy` 的建议重试间隔。
pub const BUSY_RETRY_AFTER_MS: u32 = 200;
/// `OutOfMemory` 的建议重试间隔。
pub const OOM_RETRY_AFTER_MS: u32 = 1_000;
/// `Transient` 的建议重试间隔。
pub const TRANSIENT_RETRY_AFTER_MS: u32 = 500;
/// `Network` 的建议重试间隔。
pub const NETWORK_RETRY_AFTER_MS: u32 = 1_000;

/// 后端错误（闭合枚举）。变体覆盖 §7.3 映射表的每一行；`Cancelled` 对应表中 `Aborted(_)`——
/// 取消原因由 [`crate::CancelToken::reason`] 在 worker 侧读取并分流（Deadline → 2001、Preempt / Pressure → 重排、其余 → 3008），
/// 错误本身不携原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackendError {
    /// 并发上限 / 引擎忙。
    Busy,
    /// 瞬时错误（引擎内部可重试）。
    Transient,
    /// 网络（仅云端后端）。
    Network,
    /// 内存不足。
    OutOfMemory,
    /// 引擎侧超时。
    Timeout,
    /// 此设备 / 此格式不支持。
    Unsupported,
    /// 模型权重缺失（包未安装或 blob 不在库）。
    ModelMissing,
    /// 模型权重损坏（格式 / 校验失败）。
    ModelCorrupt,
    /// 引擎 / 系统模型策略拒答。
    Refused,
    /// 引擎崩溃（已被后端捕获；不 panic）。
    Crashed,
    /// 被 [`crate::CancelToken`] 中止。
    Cancelled,
    /// 输入形式错误（空消息、越界采样参数、约束语法无法编译）。
    InvalidInput,
}

impl BackendError {
    /// 全部变体（契约测试与观测枚举用）。
    pub const ALL: [Self; 12] = [
        Self::Busy,
        Self::Transient,
        Self::Network,
        Self::OutOfMemory,
        Self::Timeout,
        Self::Unsupported,
        Self::ModelMissing,
        Self::ModelCorrupt,
        Self::Refused,
        Self::Crashed,
        Self::Cancelled,
        Self::InvalidInput,
    ];

    /// 观测用原因码（注册制字符串）。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Transient => "transient",
            Self::Network => "network",
            Self::OutOfMemory => "out_of_memory",
            Self::Timeout => "timeout",
            Self::Unsupported => "unsupported",
            Self::ModelMissing => "model_missing",
            Self::ModelCorrupt => "model_corrupt",
            Self::Refused => "refused",
            Self::Crashed => "crashed",
            Self::Cancelled => "cancelled",
            Self::InvalidInput => "invalid_input",
        }
    }

    /// 是否计入后端崩溃计数（连续 3 次进入 quarantine，§7.3 `Crash(_)` 行副作用）。
    pub const fn counts_as_crash(self) -> bool {
        matches!(self, Self::Crashed)
    }
}

/// §7.3 映射表：
///
/// | BackendError | InferError |
/// |---|---|
/// | Busy | Retryable(Busy, 200 ms) |
/// | OutOfMemory | Retryable(MemoryPressure, 1 s)（副作用：运行时 Warning 级逐出，由 owner 做） |
/// | Unsupported | Fatal(UnsupportedDevice)（副作用：能力索引重算） |
/// | Refused | Degradable(Refused) |
/// | Cancelled | Fatal(Cancelled)（`Aborted(Caller / Shutdown)` 行；Deadline / Preempt 分流在 worker） |
/// | Timeout | Degradable(Deadline) |
/// | ModelCorrupt | Fatal(AssetCorrupt)（`AssetInvalid` 行；副作用：资产标坏、触发重下） |
/// | ModelMissing | Fatal(AssetMissing)（表外补充：按三分类，缺包不可重试、可推进降级链） |
/// | Transient | Retryable(BackendTransient, 500 ms) |
/// | Network | Retryable(Network, 1 s) |
/// | Crashed | Fatal(Internal, detail 0)（副作用：Crash 计数 → quarantine） |
/// | InvalidInput | Fatal(InvalidRequest) |
impl From<BackendError> for InferError {
    fn from(error: BackendError) -> Self {
        match error {
            BackendError::Busy => Self::Retryable {
                kind: RetryKind::Busy,
                retry_after_ms: BUSY_RETRY_AFTER_MS,
            },
            BackendError::Transient => Self::Retryable {
                kind: RetryKind::BackendTransient,
                retry_after_ms: TRANSIENT_RETRY_AFTER_MS,
            },
            BackendError::Network => Self::Retryable {
                kind: RetryKind::Network,
                retry_after_ms: NETWORK_RETRY_AFTER_MS,
            },
            BackendError::OutOfMemory => Self::Retryable {
                kind: RetryKind::MemoryPressure,
                retry_after_ms: OOM_RETRY_AFTER_MS,
            },
            BackendError::Timeout => Self::Degradable {
                kind: DegradeKind::Deadline,
            },
            BackendError::Refused => Self::Degradable {
                kind: DegradeKind::Refused,
            },
            BackendError::Unsupported => Self::Fatal {
                kind: FatalKind::UnsupportedDevice,
                detail: 0,
            },
            BackendError::ModelMissing => Self::Fatal {
                kind: FatalKind::AssetMissing,
                detail: 0,
            },
            BackendError::ModelCorrupt => Self::Fatal {
                kind: FatalKind::AssetCorrupt,
                detail: 0,
            },
            BackendError::Crashed => Self::Fatal {
                kind: FatalKind::Internal,
                detail: 0,
            },
            BackendError::Cancelled => Self::Fatal {
                kind: FatalKind::Cancelled,
                detail: 0,
            },
            BackendError::InvalidInput => Self::Fatal {
                kind: FatalKind::InvalidRequest,
                detail: 0,
            },
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "backend error {}", self.wire())
    }
}

impl std::error::Error for BackendError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_maps_into_a_stable_code_band() {
        let expected: [(BackendError, u16); 12] = [
            (BackendError::Busy, 1001),
            (BackendError::Transient, 1004),
            (BackendError::Network, 1006),
            (BackendError::OutOfMemory, 1003),
            (BackendError::Timeout, 2001),
            (BackendError::Unsupported, 3001),
            (BackendError::ModelMissing, 3004),
            (BackendError::ModelCorrupt, 3005),
            (BackendError::Refused, 2004),
            (BackendError::Crashed, 3900),
            (BackendError::Cancelled, 3008),
            (BackendError::InvalidInput, 3009),
        ];
        for (error, code) in expected {
            assert_eq!(InferError::from(error).code(), code, "{error:?}");
        }
        assert_eq!(BackendError::ALL.len(), expected.len());
        let retry_after: Vec<u32> = BackendError::ALL
            .iter()
            .filter_map(|e| match InferError::from(*e) {
                InferError::Retryable { retry_after_ms, .. } => Some(retry_after_ms),
                _ => None,
            })
            .collect();
        assert_eq!(
            retry_after,
            vec![200, 500, 1_000, 1_000],
            "四个可重试项的建议间隔"
        );
        assert!(BackendError::Crashed.counts_as_crash());
        assert!(!BackendError::Busy.counts_as_crash());
    }
}
