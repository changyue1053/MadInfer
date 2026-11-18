//! 请求管线的数据类型与纯函数：业务可见的 [`GenRequest`] / [`Event`] / [`EventSink`]，以及"目标 → 后端 / 槽 / 模型键 / 来源"的映射。
//! 无线程、无 IO；编排在 [`crate::core`]。
//!
//! 本批口径（I2b，随 I3 / 资产接入替换）：
//! - 目标 → 槽：`Bundled` 走 `SlotClass::Cpu`，`System` 走 `External(0)`，`Cloud` 走 `External(1)`；I3 起由 `BackendCaps` 声明槽。
//! - 目标 → 权重 id：`Bundled` 的 `ModelKey::weights` 是由包引用派生的**占位** id（内容寻址库接入后换 manifest 的 base_weights blob id）；
//!   `System` / `Cloud` 无本地权重，用全零 id。
//! - `Cloud { endpoint }` 的后端 id = endpoint 字符串（C8 `CloudBackend` 以端点 id 注册）。

use madinfer_backend::{Constraint, GenerateOptions, Turn};
use madinfer_core_types::SlotClass;
use madinfer_core_types::{
    AssetId, BackendId, Budget, BusinessId, CapabilityId, ContextKey, InferError, PackageRef,
    PrivacyTag, RequestId, Source, TraceCtx,
};
use madinfer_router::{RouteReason, Target};
use madinfer_runtime::ModelKey;

use crate::framer::Delta;
pub use crate::structured::OutputSpec;

/// 一次生成请求（request.v1 op = stream 的 Rust 形式）。业务代码中不出现模型名、后端名、引擎参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenRequest {
    /// 能力。
    pub capability: CapabilityId,
    /// 业务（配额与观测归因）。
    pub business: BusinessId,
    /// 隐私标签（必填，缺省不允许）。
    pub privacy: PrivacyTag,
    /// 预算。
    pub budget: Budget,
    /// 输入（载荷）。
    pub turn: Turn,
    /// 采样参数（只能收窄算法包声明区间；本批不校验）。
    pub options: GenerateOptions,
    /// 输出形态（request.v1 `output`；madai-w08 G4）：`Structured` 在受理时编成 GBNF 填进后端约束，终态前校验。
    pub output: OutputSpec,
    /// 后端约束（载荷）。调用方直接给 GBNF 时用；`output` 为 `Structured` 时由核按 schema 编译覆盖。
    pub constraint: Option<Constraint>,
    /// 生成上下文键（KV / 适配器复用；本批只透传，不接 ContextPool）。
    pub context: Option<ContextKey>,
    /// 追踪上下文。
    pub trace: Option<TraceCtx>,
}

/// 输出（event.v1 output；Embedding / Labels 随 C12 后续落地）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// 文本（载荷）。
    Text(String),
    /// 结构化（madai-w08 G4）：**已过 schema 校验**的 JSON 文本（紧凑序列化；载荷）。只出现在 Complete；
    /// Failed 的 partial 仍是 Text（未校验的已生成文本）。
    Structured(String),
}

/// 生成统计（event.v1 complete.stats）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenStats {
    /// 首 token 毫秒（从 worker 开始执行起算）。
    pub ttft_ms: u32,
    /// 解码毫秒（首 token 到结束）。
    pub decode_ms: u32,
    /// prompt token。
    pub prompt_tokens: u32,
    /// 生成 token。
    pub gen_tokens: u32,
    /// 吞吐 × 100。
    pub tps_x100: u32,
    /// 峰值内存 MB。
    pub peak_memory_mb: u32,
    /// 实现来源。
    pub source: Option<Source>,
    /// 回退步数。
    pub fallback_steps: u8,
    /// 前缀缓存命中（C5 前缀缓存未接，恒 false）。
    pub prefix_hit: bool,
}

/// 业务事件（event.v1 镜像）。顺序保证：Started 恰一次 → Delta* → Complete / Failed 恰一次，之后无事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// 开始（延迟到首个 Delta / Complete 前一刻发出）。
    Started {
        /// 请求。
        request: RequestId,
        /// 实现来源。
        source: Source,
        /// 算法包（Bundled 时有）。
        package: Option<PackageRef>,
        /// 路由原因。
        route_reason: RouteReason,
        /// 排队毫秒。
        queued_ms: u32,
    },
    /// 一帧增量。
    Delta(Delta),
    /// 完成。
    Complete {
        /// 请求。
        request: RequestId,
        /// 输出。
        output: Output,
        /// 统计。
        stats: GenStats,
    },
    /// 失败（含取消：3008）。
    Failed {
        /// 请求。
        request: RequestId,
        /// 错误。
        error: InferError,
        /// 已生成部分。
        partial: Option<Output>,
    },
}

impl Event {
    /// 所属请求。
    pub fn request(&self) -> RequestId {
        match self {
            Self::Started { request, .. }
            | Self::Complete { request, .. }
            | Self::Failed { request, .. } => *request,
            Self::Delta(delta) => delta.request,
        }
    }

    /// 是否终态事件。
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. })
    }
}

/// 事件接收方。本批在 control 线程被调用（I3 移到 delivery 线程）；实现必须只入队立即返回。终态投递后被 Drop。
pub trait EventSink: Send {
    /// 一条事件。
    fn on_event(&mut self, event: Event);
}

/// 目标对应的后端 id。
pub fn backend_of(target: &Target) -> BackendId {
    match target {
        Target::System { backend } | Target::Bundled { backend, .. } => backend.clone(),
        Target::Cloud { endpoint, .. } => BackendId(endpoint.clone()),
    }
}

/// 目标对应的算法包。
pub fn package_of(target: &Target) -> Option<PackageRef> {
    match target {
        Target::Bundled { package, .. } => Some(package.clone()),
        Target::System { .. } | Target::Cloud { .. } => None,
    }
}

/// 目标对应的实现来源。
pub const fn source_of(target: &Target) -> Source {
    match target {
        Target::System { .. } => Source::SystemModel,
        Target::Bundled { .. } => Source::Bundled,
        Target::Cloud { .. } => Source::Cloud,
    }
}

/// 目标对应的执行槽（本批固定表，见模块文档）。
pub const fn slot_of(target: &Target) -> SlotClass {
    match target {
        Target::Bundled { .. } => SlotClass::Cpu,
        Target::System { .. } => SlotClass::External(0),
        Target::Cloud { .. } => SlotClass::External(1),
    }
}

/// 目标对应的模型键（权重 id 见模块文档的占位口径）。
pub fn model_key_of(target: &Target) -> ModelKey {
    let weights = match target {
        Target::Bundled { package, .. } => placeholder_asset_id(&package.to_wire()),
        Target::System { .. } | Target::Cloud { .. } => placeholder_asset_id(""),
    };
    ModelKey {
        backend: backend_of(target),
        weights,
        is_backbone: true,
    }
}

/// 由字符串派生占位 AssetId（FNV-1a 64 展开到 32 字节；空串 → 全零）。**不是内容 hash**，只作注册表键。
pub fn placeholder_asset_id(seed: &str) -> AssetId {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(7 + 64);
    hex.push_str("sha256:");
    if seed.is_empty() {
        hex.push_str(&"0".repeat(64));
    } else {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for round in 0..4u8 {
            hash ^= u64::from(round);
            for b in seed.bytes() {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            for byte in hash.to_be_bytes() {
                hex.push(HEX[(byte >> 4) as usize] as char);
                hex.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    // 形式固定为 sha256:<64 hex>，parse 不可能失败；失败时退化为全零 id 而不 panic。
    AssetId::parse(&hex).unwrap_or_else(|| {
        AssetId::parse(&format!("sha256:{}", "0".repeat(64))).unwrap_or_else(|| unreachable!())
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use madinfer_router::EndpointKind;

    #[test]
    fn target_projections_are_total_and_placeholder_ids_are_stable() {
        let bundled = Target::Bundled {
            backend: BackendId("llama.cpp".into()),
            package: PackageRef::parse("catchup-summary@1.4.2").expect("pkg"),
        };
        let system = Target::System {
            backend: BackendId("ios.foundation_models".into()),
        };
        let cloud = Target::Cloud {
            endpoint: "cloud.enterprise".into(),
            kind: EndpointKind::Enterprise,
        };
        assert_eq!(backend_of(&cloud).0, "cloud.enterprise");
        assert_eq!(
            package_of(&bundled).map(|p| p.to_wire()),
            Some("catchup-summary@1.4.2".into())
        );
        assert_eq!(package_of(&system), None);
        assert_eq!(source_of(&system), Source::SystemModel);
        assert_eq!(slot_of(&bundled), SlotClass::Cpu);
        assert_eq!(slot_of(&system), SlotClass::External(0));
        assert_eq!(slot_of(&cloud), SlotClass::External(1));
        let k1 = model_key_of(&bundled);
        let k2 = model_key_of(&bundled);
        assert_eq!(k1, k2, "同包同键");
        assert_ne!(
            k1.weights,
            model_key_of(&Target::Bundled {
                backend: BackendId("llama.cpp".into()),
                package: PackageRef::parse("catchup-summary@1.5.0").expect("pkg"),
            })
            .weights,
            "不同版本不同键"
        );
        assert_eq!(
            model_key_of(&system).weights.to_wire(),
            format!("sha256:{}", "0".repeat(64))
        );
        assert!(placeholder_asset_id("x").to_wire().starts_with("sha256:"));
    }
}
