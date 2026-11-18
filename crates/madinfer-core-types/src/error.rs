//! 错误三分类与稳定错误码（设计文档 §7、附录 B）。1xxx 可重试、2xxx 需降级、3xxx 不可重试；只加不改。
//! 4xxx 是 FFI 调用层错误（参数 / 句柄 / ABI），定义在 madinfer-ffi，不在此处。

use core::fmt;

/// 可重试类原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetryKind {
    /// 调度队列满 / 并发上限。
    Busy,
    /// 热状态触发降档。
    ThermalThrottled,
    /// 内存压力驱逐。
    MemoryPressure,
    /// 后端瞬时错误。
    BackendTransient,
    /// 业务配额耗尽。
    QuotaExceeded,
    /// 网络（云端 / 下载）。
    Network,
}

impl RetryKind {
    /// 全部变体（测试与观测枚举用）。
    pub const ALL: [Self; 6] = [
        Self::Busy,
        Self::ThermalThrottled,
        Self::MemoryPressure,
        Self::BackendTransient,
        Self::QuotaExceeded,
        Self::Network,
    ];

    /// 稳定错误码。
    pub const fn code(self) -> u16 {
        match self {
            Self::Busy => 1001,
            Self::ThermalThrottled => 1002,
            Self::MemoryPressure => 1003,
            Self::BackendTransient => 1004,
            Self::QuotaExceeded => 1005,
            Self::Network => 1006,
        }
    }

    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::ThermalThrottled => "thermal_throttled",
            Self::MemoryPressure => "memory_pressure",
            Self::BackendTransient => "backend_transient",
            Self::QuotaExceeded => "quota_exceeded",
            Self::Network => "network",
        }
    }
}

/// 需降级类原因（结果不可信或不完整）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DegradeKind {
    /// 截止时间到，已生成部分随 partial 返回。
    Deadline,
    /// 置信度低于阈值。
    LowConfidence,
    /// 结构化输出未过 schema 校验（重试一次后仍失败）。
    SchemaInvalid,
    /// 后端 / 云端策略拒绝。
    Refused,
    /// 回退链耗尽。
    ChainExhausted,
}

impl DegradeKind {
    /// 全部变体。
    pub const ALL: [Self; 5] = [
        Self::Deadline,
        Self::LowConfidence,
        Self::SchemaInvalid,
        Self::Refused,
        Self::ChainExhausted,
    ];

    /// 稳定错误码。
    pub const fn code(self) -> u16 {
        match self {
            Self::Deadline => 2001,
            Self::LowConfidence => 2002,
            Self::SchemaInvalid => 2003,
            Self::Refused => 2004,
            Self::ChainExhausted => 2005,
        }
    }

    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::LowConfidence => "low_confidence",
            Self::SchemaInvalid => "schema_invalid",
            Self::Refused => "refused",
            Self::ChainExhausted => "chain_exhausted",
        }
    }
}

/// 不可重试类原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FatalKind {
    /// 机型 / OS 不满足。
    UnsupportedDevice,
    /// 能力被开关关闭或核门未实装。
    CapabilityDisabled,
    /// 能力不在注册词表。
    CapabilityUnknown,
    /// 算法包缺失且不可下载。
    AssetMissing,
    /// 算法包校验失败（已隔离）。
    AssetCorrupt,
    /// 隐私门拒绝（e2ee / regulated 无端侧候选）。
    PrivacyDenied,
    /// 用户拒绝升云授权。
    ConsentDenied,
    /// 调用方取消。
    Cancelled,
    /// 请求形式错误（越出 allow_override、未知业务等）。
    InvalidRequest,
    /// 重复初始化。
    AlreadyInitialized,
    /// 内部错误；实际码 = 3900 + detail（0～99）。
    Internal,
}

impl FatalKind {
    /// 全部变体。
    pub const ALL: [Self; 11] = [
        Self::UnsupportedDevice,
        Self::CapabilityDisabled,
        Self::CapabilityUnknown,
        Self::AssetMissing,
        Self::AssetCorrupt,
        Self::PrivacyDenied,
        Self::ConsentDenied,
        Self::Cancelled,
        Self::InvalidRequest,
        Self::AlreadyInitialized,
        Self::Internal,
    ];

    /// 稳定错误码（Internal 返回基码 3900）。
    pub const fn code(self) -> u16 {
        match self {
            Self::UnsupportedDevice => 3001,
            Self::CapabilityDisabled => 3002,
            Self::CapabilityUnknown => 3003,
            Self::AssetMissing => 3004,
            Self::AssetCorrupt => 3005,
            Self::PrivacyDenied => 3006,
            Self::ConsentDenied => 3007,
            Self::Cancelled => 3008,
            Self::InvalidRequest => 3009,
            Self::AlreadyInitialized => 3010,
            Self::Internal => 3900,
        }
    }

    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::UnsupportedDevice => "unsupported_device",
            Self::CapabilityDisabled => "capability_disabled",
            Self::CapabilityUnknown => "capability_unknown",
            Self::AssetMissing => "asset_missing",
            Self::AssetCorrupt => "asset_corrupt",
            Self::PrivacyDenied => "privacy_denied",
            Self::ConsentDenied => "consent_denied",
            Self::Cancelled => "cancelled",
            Self::InvalidRequest => "invalid_request",
            Self::AlreadyInitialized => "already_initialized",
            Self::Internal => "internal",
        }
    }
}

/// SDK 错误：业务只对三个分支写代码。partial 输出不在此处（它是 event.v1 `failed.partial` 的载荷，属 madinfer-api 的 Output）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InferError {
    /// 资源暂时不可用：SDK 已按 `retry_after_ms` 原地重排一次，再到业务手里说明仍失败。
    Retryable {
        /// 原因。
        kind: RetryKind,
        /// 建议等待毫秒数。
        retry_after_ms: u32,
    },
    /// 本次结果不可信或不完整。
    Degradable {
        /// 原因。
        kind: DegradeKind,
    },
    /// 此设备 / 此配置下不可能成功。
    Fatal {
        /// 原因。
        kind: FatalKind,
        /// `Internal` 的细分码 0～99；其余变体忽略。
        detail: u8,
    },
}

impl InferError {
    /// 稳定错误码（附录 B）。
    pub const fn code(&self) -> u16 {
        match *self {
            Self::Retryable { kind, .. } => kind.code(),
            Self::Degradable { kind } => kind.code(),
            Self::Fatal {
                kind: FatalKind::Internal,
                detail,
            } => 3900 + if detail > 99 { 99 } else { detail as u16 },
            Self::Fatal { kind, .. } => kind.code(),
        }
    }

    /// 协议 `kind` 字符串。
    pub const fn kind_wire(&self) -> &'static str {
        match *self {
            Self::Retryable { kind, .. } => kind.wire(),
            Self::Degradable { kind } => kind.wire(),
            Self::Fatal { kind, .. } => kind.wire(),
        }
    }

    /// 是否可重试。
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }
}

impl fmt::Display for InferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "infer error {} ({})", self.code(), self.kind_wire())
    }
}

impl std::error::Error for InferError {}
