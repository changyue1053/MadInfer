//! madinfer-ffi —— FFI 门（设计文档 C17）：UniFFI 出口、panic `catch_unwind` 收口、`infer_abi_version()`。
//!
//! ## I-CD-01 真实面（madai-w06；首笔提交 = 面冻结，契约 = w06 首日 E12）
//!
//! I-S-01 的 spike 桩面（`probe_stub` / `stream_echo` / `EchoSink`）自本批退役，换成对着
//! [`madinfer_api::Core`]（真核 actor + worker 池）的正式导出面：
//!
//! | 导出 | 契约 |
//! |---|---|
//! | [`infer_abi_version`] | 握手常量（COMPAT.toml `[madinfer].abi_version` 同源）= 2（madai-w08 追加面升版） |
//! | [`InferCore::create`] | 建站：注入 [`HostBridge`] 宿主端口 + [`CoreOptions`]，注册 backend-llama 后端——`options.backend` 选仿真核（缺省）或真实 llama.cpp（madai-w17 I-C-16：本 crate feature `llama-sys` 开且 `model_path` 指向在盘文件才建得出；feature 关 → 4005、路径缺席 → 4003；C 旧 create 为仿真；create_with_backend 可显式选择真实后端） |
//! | [`InferCore::stream`] | 请求泵：`RequestRecord` + [`EventSink`] → 句柄 `u64`；事件恰一次序 Started → Delta* → Complete/Failed（K1，泳道规则 9） |
//! | [`InferCore::cancel`] | 取消：终态恰一次 Failed(Cancelled, 3008)，幂等 |
//! | [`InferCore::probe`] | 能力 + 档位快照（[`ProbeRecord`]，由 madinfer-api 能力索引实答；`backend` 字段回本站后端形态 [`BackendKindRecord`]，madai-w17 I-C-16） |
//! | [`InferCore::signal`] | 资源信号入口（madai-w07 F1；[`OsSignalRecord`] 七变体，只入队） |
//! | [`InferCore::apply_config`] | 签名 config.v1 文档进核，同步等应用结论（madai-w08 I-C-07 G2；核已停 4004） |
//! | [`InferCore::drop_contexts`] | 按前缀丢弃生成上下文（madai-w14 I-C-12；账户关闭 `acct:<id>/`，control 线程执行、同步回条数；核已停 4004；C 面第 10 符号 `madinfer_drop_contexts`） |
//! | [`InferCore::stats`] | 运行期台账快照 [`CoreStatsRecord`]（madai-w15 I-C-13；上下文池命中 / 丢弃 / 条目 / KV、本站验签超时、在飞 Job、运行时长；control 线程一次取齐、同步回；核已停 4004；**只在 uniffi 面**，C 面不加符号） |
//! | [`test_panic`] | 注入核内 panic 验证收口（对齐 im-core-cabi `im_core_test_panic` 用途） |
//! | [`debug`] 模块（导出无条件、函数体门控） | `InferCore::create_debug`（建站选项 `debug_script_backend`：testkit 脚本后端，prompt 即产出）+ `InferCore::test_stop_control`（4004 真核验）+ `InferCore::debug_set_token_delay_ms`（脚本后端逐词元节拍，madai-w12 I-C-11）+ `InferCore::debug_set_verify_deadline_ms`（本站验签 deadline 注入，`VerifyTimeout` 可复现，madai-w16 I-C-14）；后两者只在 uniffi 面，C 面不加符号；release 构建符号在、函数体恒 4005 `FeatureDisabled`，两 profile 生成同一份绑定（madai-w10 I-C-09 / madai-w11 I-C-10） |
//!
//! ## 纪律
//!
//! - 全仓唯一产 cdylib / staticlib 的 crate；namespace = `madinfer_ffi`，符号族
//!   `uniffi_madinfer_ffi_*` / `ffi_madinfer_ffi_*`，与 imcore 的 `uniffi_im_core_uniffi_*` 族无交集
//!   （mad 白名单四前缀已备；一进程单核二进制的正式收编归 im-core-ffi `infer_door`，lane-rules 2）。
//! - 边界数据全类型化 record（lane-rules 5）：观测以 [`TelemetryBatchRecord`]（类型化头 + 云端载荷
//!   不透明字节）交付，授权以 [`ConsentRecord`] / [`ConsentDecisionRecord`] 往返；模型生成文本
//!   （Delta / Complete / partial）是载荷字符串。
//! - panic 不越界：每个 FFI 入口整体 `catch_unwind`（profile `panic = "unwind"` 配对），出错回 4xxx
//!   （[`InferFfiError`]，与核内 1xxx～3xxx 不相交，单测守）；宿主回调（[`EventSink`]）panic 在
//!   投递点收口吞掉并计数，不得拖死 control 线程。
//! - 回调只投递（imcore K12 同律）：[`EventSink`] / [`HostBridge`] 实现体只准入队立即返回；
//!   [`HostBridge::authorize`] 允许阻塞等用户，因此核侧在专用线程调它（control 线程不等弹窗）。
//! - 本文件无 `unsafe`（workspace lints `unsafe_code = "deny"`）：uniffi 生成的 scaffolding
//!   经 proc-macro 展开，lint 按 span 归属不射生成代码（imcore 同一说明）。全仓唯一手写 `unsafe`
//!   区是 [`cabi`]（C 边界，模块级放开 + 逐处 SAFETY 注释）。
//!
//! ## 第二面：手写 C ABI（madai-w07 I-C-06 F2；[`cabi`] + `include/madinfer.h`）
//!
//! 鸿蒙 NAPI 兜底场景的八符号 `madinfer_*` over 同一个 [`InferCore`]（不另建核）：宿主 vtable 包成
//! [`HostBridge`]、sink vtable 包成 [`EventSink`]，K1 / 取消 / panic 收口 / 授权专用线程 / AllowRemember
//! 记忆全是本文件这一份实现。头文件是事实源（mad 侧镜像），字段与本文件 record 一一对应。

#![deny(missing_docs)]

pub mod cabi;
pub mod debug;

use std::collections::{BTreeSet, HashMap};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use madinfer_api::{
    capability_index::PackageReadiness, ApplyReport, CapState, CapabilityIndex, Core, CoreHandle,
    Event, GenRequest, IndexInputs, Output, OutputSpec,
};
use madinfer_backend::{GenerateOptions, Message, Role, Turn};
use madinfer_backend_llama::{LlamaBackend, LlamaOptions};
use madinfer_config::{
    ApplyOutcome, ApplyStage, RejectReason, Signature, SignatureVerifier, VerifyOutcome,
};
use madinfer_core_types::{
    Budget, BusinessId, CapabilityId, ContextKey, InferError, Os, PackageRef, PrivacyTag, QoS,
    RequestId, SlotClass, Source, Tier,
};
use madinfer_host::{
    AppState, Authorizer, Clock, ConsentContext, ConsentDecision, MemoryPressure, NetState,
    OsSignal, TelemetrySink, ThermalState, ThreadHooks, WorkerClass,
};
use madinfer_observe::BatchMeta;
use madinfer_router::{
    Candidate, Conditions, EndpointKind, MappingEntry, MappingTable, RouteReason, Target,
};
use madinfer_sched::SlotTable;

uniffi::setup_scaffolding!("madinfer_ffi");

/// ABI 版本（COMPAT.toml `[madinfer].abi_version` 同源常量；宿主 init 前握手，失配即拒绝加载）。
pub const INFER_ABI_VERSION: u32 = 2;

/// 仿真核统一算法包引用（资产层未接：全部能力共用一个包键；I-C-04 后随 manifest 实答）。
pub const SIM_PACKAGE_REF: &str = "sim-core@0.5.0";

/// ABI 版本握手（im-core-ffi `abi_version()` 的同形态对应）。
#[uniffi::export]
pub fn infer_abi_version() -> u32 {
    INFER_ABI_VERSION
}

// ---------------------------------------------------------------------------
// 错误（4xxx：FFI 调用层专属空间，madinfer-core-types error.rs 头注预留）
// ---------------------------------------------------------------------------

/// FFI 调用层错误（4xxx；与核内 1xxx～3xxx 不相交，单测逐码核对）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Error)]
pub enum InferFfiError {
    /// 4001 请求形式错误（非法能力名 / 业务 id 为 0 等）：Started 前拒绝。
    InvalidArgument,
    /// 4002 核内 panic 已在 FFI 边界收口：进程存活，仅本次调用失败。
    Panicked,
    /// 4003 建站失败（control 线程 / worker 池起不来，或生产构建缺验签器）。
    CoreStartFailed,
    /// 4004 核已停（control 线程退出后仍被调用：`apply_config` 等需回执的入口才会遇到）。
    CoreStopped,
    /// 4005 本构建未编入该功能（madai-w11 I-C-10；形态同 imcore `attach_tcp_link` 的 `FeatureDisabled`）：调试面
    /// [`InferCore::create_debug`] / [`InferCore::test_stop_control`] 在 release 构建里符号在、函数体回本码——
    /// 宿主据此判"链到的是生产产物"，两 profile 的绑定同一份。
    FeatureDisabled,
}

impl InferFfiError {
    /// 全部变体（枚举闭合与错误码空间测试用）。
    pub const ALL: [Self; 5] = [
        Self::InvalidArgument,
        Self::Panicked,
        Self::CoreStartFailed,
        Self::CoreStopped,
        Self::FeatureDisabled,
    ];

    /// 稳定错误码（4xxx；只加不改）。
    pub const fn code(&self) -> u16 {
        match self {
            Self::InvalidArgument => 4001,
            Self::Panicked => 4002,
            Self::CoreStartFailed => 4003,
            Self::CoreStopped => 4004,
            Self::FeatureDisabled => 4005,
        }
    }

    /// 协议字符串（全拼，naming §3.6）。
    pub const fn wire(&self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::Panicked => "panicked",
            Self::CoreStartFailed => "core_start_failed",
            Self::CoreStopped => "core_stopped",
            Self::FeatureDisabled => "feature_disabled",
        }
    }
}

/// 验签 deadline 毫秒（madai-w11 I-C-10；`madinfer.h` §5 "验签回投" 条 / `MADINFER_VERIFY_DEADLINE_MS` 同值，单测核对）：
/// control 线程把 `HostBridge::verify_ed25519` 投到专用瞬时线程 `madinfer-ffi-verify` 后等回执的上限，超时按
/// `SignatureInvalid` + detail `verify=timeout` 拒本次 apply，宿主验签卡死不再拖死核站。
/// 取值论证：宿主验签承诺 ≤ 50 ms、鸿蒙 TSF 回投最坏读数 202 ms（I-H-05），2 000 ms 给主循环忙时 10 倍余量。
pub const VERIFY_DEADLINE_MS: u64 = 2000;

impl std::fmt::Display for InferFfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "infer ffi error {} ({})", self.code(), self.wire())
    }
}

impl std::error::Error for InferFfiError {}

// ---------------------------------------------------------------------------
// 过界 record（lane-rules 5：契约数据全类型化；生成文本是载荷字符串）
// ---------------------------------------------------------------------------

/// 受信公钥（madai-w07 首日 F1）：配置 / 算法包签名的 `key_id` ↔ ed25519 公钥（32 字节，hex）。
/// 非空即建站注入 [`BridgeVerifier`]（验签算力在宿主：`HostBridge::verify_ed25519`）；为空沿旧口径
/// （调试构建 TrustAllVerifier，生产构建拒绝建站 4003）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TrustedKeyRecord {
    /// 签名文档里的 key_id。
    pub key_id: String,
    /// 公钥 hex（64 字符）。
    pub public_key_hex: String,
}

/// 配置应用管线阶段（madai-w08 G2；镜 `madinfer_config::ApplyStage`）：拒绝时给出"走到哪一步被拒"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ApplyStageRecord {
    /// 字节到达（验签未过即停在此）。
    Received,
    /// 验签通过（解析失败停在此）。
    Verified,
    /// 解析通过（版本 / min_sdk / 分桶 / 语义失败停在此）。
    Parsed,
    /// 校验通过。
    Validated,
    /// 已切换为活动配置。
    Applied,
    /// 已拒绝。
    Rejected,
}

/// 配置拒绝原因（madai-w08 G2；镜 `madinfer_config::RejectReason`，注册制、无自由文本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ApplyRejectReasonRecord {
    /// 签名 key_id 不在受信公钥表。
    UnknownKey,
    /// 签名不符。
    SignatureInvalid,
    /// JSON / schema 解析失败。
    SchemaInvalid,
    /// 版本不大于当前活动版本。
    VersionNotNewer,
    /// min_sdk 不满足。
    MinSdkUnsatisfied,
    /// 设备桶不在灰度区间。
    OutOfRollout,
    /// 语义校验失败（未知能力 / 后端 / 阈值越界等）。
    SemanticInvalid,
}

/// `ApplyOutcomeRecord.detail` 的类型化种类（madai-w15 I-C-13；uniffi-only 只加，C 面 `madinfer_apply_outcome` 结构不动、
/// `detail` 串不变）：`detail` 自由文本按拒因各有固定形态，本枚举把"它是什么"说成类型——尤其把验签**超时**
/// （`VerifyTimeout`，`detail = "verify=timeout"`）与验签器**回假**（`VerifyRejected`，`detail` 空）在 KMP 面分开：两者
/// `reject_reason` 同为 `SignatureInvalid`、`stage` 同为 `Received`（ABI 2 口径；ABI 3 候选 `MADINFER_APPLY_STAGE_VERIFY` 仍不实施），
/// I-D-13 实测 Android 软件验签 0.6～2.3 s 撞 deadline 时消费方此前只能靶 `detail` 串。每个 `RejectReason` 恰映射一值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ApplyDetailKindRecord {
    /// 无诊断：应用成功（`detail` 空串）。
    None,
    /// 验签超时——control 线程等满 [`VERIFY_DEADLINE_MS`] 未得宿主回执（`detail = "verify=timeout"`）。
    VerifyTimeout,
    /// 验签器回假（算法 / 签名 / 载荷不符；`detail` 空串）。
    VerifyRejected,
    /// 签名 `key_id` 不在受信公钥表（`detail = key_id`）。
    UnknownKey,
    /// JSON / schema 解析失败（`detail` = 解析错误描述）。
    Parse,
    /// 版本不大于当前活动版本（`detail = "received=<n> active=<m>"`）。
    VersionNotNewer,
    /// min_sdk 不满足（`detail` = semver 要求）。
    MinSdkUnsatisfied,
    /// 设备桶不在灰度区间（`detail = "bucket=<n>"`）。
    OutOfRollout,
    /// 语义校验失败（`detail` = 语义错误 Debug 描述）。
    Semantic,
}

/// `InferCore.apply_config` 回执（madai-w08 G2）：已应用时 `applied = true`、`active_version` 为新版本；
/// 被拒时 `reject_reason` 有值、`active_version` 仍是旧版本（原子：拒绝不动活动配置）。
/// `detail` 是诊断用自由文本（key_id / 版本对 / 语义错误描述），宿主只记日志不解读；`detail_kind`（madai-w15 I-C-13 只加）
/// 是它的类型化种类，宿主分支只准靶它、不准解 `detail` 串。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ApplyOutcomeRecord {
    /// 是否已切换为活动配置。
    pub applied: bool,
    /// 调用后的活动配置版本。
    pub active_version: u64,
    /// 管线停在的阶段（应用成功为 `Applied`）。
    pub stage: ApplyStageRecord,
    /// 拒绝原因（应用成功为 None）。
    pub reject_reason: Option<ApplyRejectReasonRecord>,
    /// 诊断文本（应用成功为空串）。
    pub detail: String,
    /// `detail` 的类型化种类（应用成功为 `None`；madai-w15 I-C-13 只加，生成绑定的构造签名随之多一个参数）。
    pub detail_kind: ApplyDetailKindRecord,
}

/// `InferCore.stats` 回执（madai-w15 I-C-13；uniffi-only 可观测面，C 面无对应结构）：核站运行期台账快照，control 线程在两条
/// 命令之间一次取齐（`context_pool_*` / `inflight_jobs` / `uptime_ms` 彼此一致），`verify_timeouts` 是本站验签器的计数。
/// 用途：运行时契约 T6（关户后推理上下文不跨账户）的核侧读数——A 关户 `drop_contexts` 后 B 同键请求，`context_pool_hits`
/// 不增、`context_pool_dropped` ≥ 1；`verify_timeouts` 把"验签超时"与"验签假"在 KMP 面分开（两者 `reject_reason` 同为
/// `SignatureInvalid`）。只读，不改任何状态。
///
/// madai-w16 I-C-14 起池读数是**后端实际驻留**：worker 按键驻留上下文、命中即复用，`context_pool_kv_mb` = 后端申报的每上下文 KV
/// （仿真核 8 MB）× 条目；预算 `thresholds.kv_budget_mb[tier]` 未配置（0）时任何键都不进池、不驻留——要在宿主面读到
/// `context_pool_hits` > 0 须先 `apply_config` 带该档预算的配置。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CoreStatsRecord {
    /// 本站后端形态（madai-w17 I-C-16 只加；与 `probe().backend` 同值——建站时定、不变）。
    pub backend_kind: BackendKindRecord,
    /// 上下文池命中（同键复用——worker 复用驻留句柄、不再 open_context）累计。
    pub context_pool_hits: u64,
    /// 上下文池按前缀丢弃（`drop_contexts`）累计，含在飞条目。
    pub context_pool_dropped: u64,
    /// 池内上下文数（量规；= worker 实际驻留的后端句柄数）。
    pub context_pool_entries: u32,
    /// 池内 KV 已用 MB（量规；后端申报的每上下文 KV × 条目，含在飞延迟结清的记账）。
    pub context_pool_kv_mb: u32,
    /// 池 KV 预算上限 MB（量规；config `thresholds.kv_budget_mb[tier]`，未配置 0）。
    pub context_pool_kv_budget_mb: u32,
    /// 本站 `apply_config` 验签超时累计（control 线程等满 [`VERIFY_DEADLINE_MS`]；进程级计数见 [`verify_timeout_count`]）。
    pub verify_timeouts: u64,
    /// 在飞 Job 数（量规：已提交调度、尚未终态，含排队与执行中）。
    pub inflight_jobs: u32,
    /// 自建站起的毫秒数（量规；宿主 `now_ms`）。
    pub uptime_ms: u64,
}

/// 热状态（madinfer-host `ThermalState` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ThermalRecord {
    /// 正常。
    Nominal,
    /// 偏热。
    Fair,
    /// 严重。
    Serious,
    /// 临界。
    Critical,
}

/// 内存压力（madinfer-host `MemoryPressure` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MemoryPressureRecord {
    /// 正常。
    Normal,
    /// 警告。
    Warning,
    /// 临界。
    Critical,
}

/// 网络状态（madinfer-host `NetState` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NetStateRecord {
    /// 离线。
    Offline,
    /// 计量网络。
    Metered,
    /// 非计量网络。
    Unmetered,
}

/// 前后台（madinfer-host `AppState` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AppStateRecord {
    /// 前台。
    Foreground,
    /// 后台。
    Background,
    /// 后台任务窗口。
    BackgroundTask,
    /// 即将终止。
    Terminating,
}

/// 资源信号（madai-w07 首日 F1 补入口；madinfer-host `OsSignal` 镜像——KMP `InferResourceSignalsPort` 早有、ffi 面此前缺）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum OsSignalRecord {
    /// 热状态。
    Thermal {
        /// 状态。
        state: ThermalRecord,
    },
    /// 电量。
    Battery {
        /// 0～100；None = 未知（madai-w08 G7：宿主拿不到电量读数时仍可报 charging / low_power_mode）。
        /// None **原样进核**（madai-w09 I-C-08：madinfer-host `OsSignal::Battery.level_pct` 已是 `Option<u8>`，此前以 100
        /// 透传的中间态删除）：未知不参与电量门控，核不以满电 / 空电代入。0～100 之外的读数由壳映射为 None 再过界
        /// （本入口无错误回执；C 面同一取值回 4001）。
        level_pct: Option<u8>,
        /// 充电中。
        charging: bool,
        /// 低电量模式。
        low_power_mode: bool,
    },
    /// 内存压力。
    Memory {
        /// 等级。
        level: MemoryPressureRecord,
    },
    /// 前后台。
    App {
        /// 状态。
        state: AppStateRecord,
        /// 后台预算毫秒（有则给）。
        budget_ms: Option<u32>,
    },
    /// 网络。
    Network {
        /// 状态。
        state: NetStateRecord,
    },
    /// 空闲。
    Idle {
        /// 是否空闲。
        idle: bool,
    },
    /// 主链路忙。
    MainLinkBusy {
        /// 是否忙。
        busy: bool,
    },
}

/// 机型档位（core-types `Tier` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TierRecord {
    /// 低端。
    Low,
    /// 中端。
    Mid,
    /// 旗舰。
    Flagship,
    /// 画像未完成。
    Unknown,
}

/// 操作系统（core-types `Os` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OsRecord {
    /// iOS / iPadOS。
    Ios,
    /// Android。
    Android,
    /// HarmonyOS。
    Ohos,
    /// 桌面 JVM（预览与测试）。
    Jvm,
}

/// 隐私标签（core-types `PrivacyTag` 镜像；请求必填）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PrivacyRecord {
    /// 可出端。
    Public,
    /// 企业内。
    Internal,
    /// E2EE 明文：只准端侧。
    E2eePlaintext,
    /// 受监管：只准端侧。
    Regulated,
}

/// QoS 三档（core-types `QoS` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum QosRecord {
    /// 交互。
    Interactive,
    /// 异步。
    Async,
    /// 后台批量。
    Background,
}

/// 消息角色（backend `Role` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RoleRecord {
    /// system。
    System,
    /// user。
    User,
    /// assistant。
    Assistant,
    /// tool。
    Tool,
}

/// 实现来源（core-types `Source` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SourceRecord {
    /// 系统模型。
    SystemModel,
    /// 内联引擎 + 算法包。
    Bundled,
    /// 云端。
    Cloud,
}

/// 路由原因（router `RouteReason` 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RouteReasonRecord {
    /// 隐私门强制端侧。
    PrivacyForcedLocal,
    /// 静态档位。
    StaticTier,
    /// 可用性。
    Availability,
    /// 开关。
    Switch,
    /// 缺省。
    Default,
    /// 回退。
    Fallback,
}

/// 一条消息（content 是业务已脱敏的载荷文本）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MessageRecord {
    /// 角色。
    pub role: RoleRecord,
    /// 内容（载荷）。
    pub content: String,
}

/// 采样参数（None = 用核缺省；定点形式与 request.v1 一致）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OptionsRecord {
    /// temperature × 100（0～200）。
    pub temperature_x100: Option<u16>,
    /// top_p 万分比。
    pub top_p_bp: Option<u16>,
    /// 生成上限。
    pub max_tokens: Option<u32>,
    /// 停止串（≤ 8）。
    pub stop: Vec<String>,
    /// 随机种子（仿真核：同 seed 同输入 → 同输出）。
    pub seed: Option<u64>,
}

/// 输出形态（request.v1 `output`；madai-w08 G4 / I-E-04 §7-1）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum OutputSpecRecord {
    /// 自由文本。
    Text,
    /// 结构化：核按 schema（JSON Schema draft 2020-12 子集）编 GBNF 约束后端，终态前校验全文；
    /// 编不过 / 校验不过 → `on_failed`(2003)。`schema_json` 是载荷（核不解读语义、不回传、不落日志）。
    Structured {
        /// schema JSON 文本。
        schema_json: String,
    },
}

/// 输出（event.v1 `output`；Complete 携带）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum OutputRecord {
    /// 文本（载荷）。
    Text {
        /// 完整文本。
        text: String,
    },
    /// 结构化：**已过 schema 校验**的 JSON（紧凑序列化；载荷）。
    Structured {
        /// JSON 文本。
        json: String,
    },
}

/// 一次流式生成请求（request.v1 op = stream 的过界形式）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RequestRecord {
    /// 能力名（注册制词表，`[a-z0-9._-]{1,64}`）。
    pub capability: String,
    /// 业务 id（1～65535）。
    pub business: u16,
    /// 隐私标签（必填）。
    pub privacy: PrivacyRecord,
    /// QoS。
    pub qos: QosRecord,
    /// 截止毫秒。
    pub deadline_ms: u32,
    /// 消息历史（载荷）。
    pub messages: Vec<MessageRecord>,
    /// 采样参数。
    pub options: OptionsRecord,
    /// 输出形态（madai-w08 G4）。
    pub output: OutputSpecRecord,
    /// 生成上下文键（request.v1 `context`：1～128 字节的业务稳定串，如 `conv:<id>`；核内作 KV / 适配器复用键，
    /// 本批只透传保存不接 ContextPool）。形式非法 → 4001。
    pub context: Option<String>,
}

/// 建站参数（init.v1 的过界子集；本批只携仿真核所需字段）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CoreOptions {
    /// 观测实例标识。
    pub instance_name: String,
    /// 档位。
    pub tier: TierRecord,
    /// 操作系统。
    pub os: OsRecord,
    /// 合帧窗口毫秒。
    pub frame_ms: u32,
    /// 观测采样万分比。
    pub sampling_bp: u16,
    /// 注册能力词表（每个能力路由到 backend-llama 仿真核）。
    pub capabilities: Vec<String>,
    /// 授权链路演示位：设置后每个能力的回退链头部插入企业云候选（Internal 隐私触发升云授权，
    /// [`HostBridge::authorize`] 被调）。云后端（C8）未接：Allow 后该目标以后端缺失回退到仿真核
    /// （fallback_steps = 1）；Deny 恰一次 Failed(3007)。E2EE / Regulated 隐私门照常剔除它。
    pub consent_endpoint: Option<String>,
    /// 受信公钥表（madai-w07 F1）：非空即注入 [`BridgeVerifier`]，生产构建自此可建站；为空沿旧口径。
    pub trusted_keys: Vec<TrustedKeyRecord>,
    /// 后端形态（madai-w17 I-C-16 只加）：`Emulated` = 确定性仿真核（此前唯一形态，行为不变）；`Llama` = 真实 llama.cpp——
    /// 只在本 crate 编入 feature `llama-sys` 时建得出（未编入 → 4005 `FeatureDisabled`，不静默回仿真核），且要求 `model_path`
    /// 指向在盘文件（缺席 / 不是文件 → 4003 `CoreStartFailed`）。C 面 `madinfer_core_create` 固定 `Emulated`。
    pub backend: BackendKindRecord,
    /// GGUF 权重文件路径（madai-w17 I-C-16 只加；只对 `backend = Llama` 生效，`Emulated` 忽略）。debug / 内测形态壳直给；
    /// 生产形态由 KMP 资产链给已验签、已安装文件的路径——同一字段。随每次模型加载经 `LoadOpts::weights_path` 进后端。
    pub model_path: Option<String>,
}

/// 后端形态（madai-w17 I-C-16；`CoreOptions.backend` 选、`ProbeRecord.backend` / `CoreStatsRecord.backend_kind` 回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BackendKindRecord {
    /// backend-llama 确定性仿真核（缺省）。
    Emulated,
    /// 真实 llama.cpp（本 crate feature `llama-sys`）。
    Llama,
}

impl BackendKindRecord {
    /// 观测 / 走查用字面（I-D-16 读 `backend=llama|emulated`）。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Emulated => "emulated",
            Self::Llama => "llama",
        }
    }
}

/// Started（K1：恰一次，先于任何 Delta / Complete）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StartedRecord {
    /// 请求句柄。
    pub request: u64,
    /// 实现来源。
    pub source: SourceRecord,
    /// 算法包（Bundled 时有）。
    pub package: Option<String>,
    /// 路由原因。
    pub route_reason: RouteReasonRecord,
    /// 排队毫秒。
    pub queued_ms: u32,
}

/// 一帧增量（text 是生成载荷）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeltaRecord {
    /// 请求句柄。
    pub request: u64,
    /// 帧序号（自 0 递增）。
    pub seq: u32,
    /// 文本（载荷）。
    pub text: String,
    /// token 数。
    pub tokens: u32,
    /// 结构化输出的这一帧 JSON Patch（RFC 6902 数组文本；event.v1 E6）：`Structured` 请求每帧携带（可为 `[]`），
    /// 逐帧独立可应用、按 seq 顺序应用到 `{}` 恒等于该帧时刻已完整的前缀文档，应用到最后一帧 == Complete 的 json；
    /// `Text` 请求为 None。op 子集：add / replace / remove，数组只尾追加（`/…/-`）。
    pub structured_patch: Option<String>,
}

/// 生成统计（event.v1 complete.stats 镜像）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StatsRecord {
    /// 首 token 毫秒。
    pub ttft_ms: u32,
    /// 解码毫秒。
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
    pub source: Option<SourceRecord>,
    /// 回退步数。
    pub fallback_steps: u8,
    /// 前缀缓存命中。
    pub prefix_hit: bool,
}

/// 终态 Complete（恰一次）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CompleteRecord {
    /// 请求句柄。
    pub request: u64,
    /// 完整文本（载荷；兼容字段——Structured 时与 `output.json` 同文）。
    pub text: String,
    /// 输出形态（madai-w08 G4）。
    pub output: OutputRecord,
    /// 统计。
    pub stats: StatsRecord,
}

/// 终态 Failed（恰一次；取消 = code 3008）。code / wire 与 core-types error.rs 同表。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FailedRecord {
    /// 请求句柄。
    pub request: u64,
    /// 稳定错误码（1xxx～3xxx）。
    pub code: u16,
    /// 协议字符串。
    pub wire: String,
    /// 建议等待毫秒（仅 Retryable）。
    pub retry_after_ms: Option<u32>,
    /// 已生成部分（载荷；已出 Delta 后失败时有）。
    pub partial_text: Option<String>,
}

/// 观测批次（lane-rules 5：类型化头 + 云端载荷不透明字节——telemetry.v1 JSON 只在核内对云端
/// 序列化，KMP 不解读 [`Self::payload`]，原样转交上报通道）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TelemetryBatchRecord {
    /// telemetry.v1 schema 版本。
    pub schema_version: u32,
    /// 批内事件数。
    pub event_count: u32,
    /// 已序列化批次（不透明字节，KMP 侧禁解析）。
    pub payload: Vec<u8>,
}

/// 授权上下文（升云前的一次性确认）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConsentRecord {
    /// 能力名。
    pub capability: String,
    /// 隐私标签。
    pub privacy: PrivacyRecord,
    /// 云端端点 id。
    pub endpoint_id: String,
}

/// 授权结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ConsentDecisionRecord {
    /// 本次允许。
    Allow,
    /// 允许并记住。
    AllowRemember,
    /// 拒绝。
    Deny,
}

/// 能力状态（capabilities.v1 entries[].state 的扁平镜像；仿真核无下载态字节 / 进度，暂不携带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CapStateRecord {
    /// 可用。
    Available,
    /// 需下载。
    NeedsDownload,
    /// 下载中。
    Downloading,
    /// 此设备不支持。
    Unsupported,
    /// 被开关关闭。
    Disabled,
}

/// 延迟档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LatencyRecord {
    /// < 300 ms。
    Fast,
    /// < 2 s。
    Normal,
    /// 其余。
    Slow,
}

/// 后端约束支持度（`BackendCaps.constraint` 镜像；madai-w08 G4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ConstraintSupportRecord {
    /// 不支持：Structured 请求到该后端以 Unsupported 失败 / 回退。
    None,
    /// 仿真：按语法生成合法文档（不是真采样；仿真核中间态）。
    Emulated,
    /// 原生语法约束采样。
    Native,
}

/// 一条能力状态。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CapabilityEntryRecord {
    /// 能力名。
    pub capability: String,
    /// 状态。
    pub state: CapStateRecord,
    /// 实现来源（Available 时）。
    pub source: Option<SourceRecord>,
    /// 延迟档。
    pub latency: LatencyRecord,
    /// 结构化输出支持度（Available 时取路由到的后端能力；否则 None）。
    pub constraint: ConstraintSupportRecord,
}

/// 探测快照（能力 + 档位；由 madinfer-api 能力索引在建站输入上实答）。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProbeRecord {
    /// ABI 版本。
    pub abi_version: u32,
    /// 核版本串。
    pub core_version: String,
    /// 档位。
    pub tier: TierRecord,
    /// 快照单调版本。
    pub snapshot_version: u64,
    /// 按能力名排序的条目。
    pub entries: Vec<CapabilityEntryRecord>,
    /// 本站后端形态（madai-w17 I-C-16 只加；建站时定）。C 面 `madinfer_probe` 结构不携此字段（恒 Emulated）。
    pub backend: BackendKindRecord,
}

// ---------------------------------------------------------------------------
// 回调接口（宿主实现，核回调；只入队纪律见模块文档）
// ---------------------------------------------------------------------------

/// 流式事件回执（K1 时序：`on_started` 恰一次 → `on_delta`* → `on_complete` / `on_failed` 恰一次）。
///
/// 实现体只准把事件塞进宿主侧队列后立即返回；允许在回调里反调本绑定的 `cancel`（只入队，
/// 不会死锁——Rust 单测锁死该性质）。
#[uniffi::export(callback_interface)]
pub trait EventSink: Send + Sync {
    /// 开始（延迟到首个 delta / complete 前一刻发出）。
    fn on_started(&self, event: StartedRecord);
    /// 一帧增量。
    fn on_delta(&self, event: DeltaRecord);
    /// 正常完结（恰一次；失败路径不发）。
    fn on_complete(&self, event: CompleteRecord);
    /// 失败完结（恰一次；含取消 3008）。
    fn on_failed(&self, event: FailedRecord);
}

/// 宿主端口桥（madinfer-host 四端口的 UniFFI 过界形态；宿主实现、核消费）。
///
/// - [`Self::now_ms`]：单调毫秒（宿主保证不回退）；核内一切时间经它。
/// - [`Self::kv_get`] / [`Self::kv_put`]：小键值（install 标记、授权记忆键 [`KV_CONSENT_PREFIX`]；值 ≤ 4 KB，
///   可阻塞 ≤ 50 ms）。中间态：本批核内配置持久化未接 KvStore，核只写 install 标记与授权记忆两类键。
/// - [`Self::telemetry_send`]：观测批次（类型化 record，lane-rules 5）。
/// - [`Self::authorize`]：升云授权；允许阻塞等用户——核在专用线程调它，control 线程不等。回 `AllowRemember`
///   时核侧落记忆键（madai-w07 F9），此后同 `<capability>.<endpoint>` 的请求不再调本方法；宿主零记忆逻辑。
#[uniffi::export(callback_interface)]
pub trait HostBridge: Send + Sync {
    /// 单调毫秒。
    fn now_ms(&self) -> u64;
    /// 读键值。
    fn kv_get(&self, key: String) -> Option<Vec<u8>>;
    /// 写键值（覆盖）。
    fn kv_put(&self, key: String, value: Vec<u8>);
    /// 交付一批观测。
    fn telemetry_send(&self, batch: TelemetryBatchRecord);
    /// 升云授权确认。
    fn authorize(&self, context: ConsentRecord) -> ConsentDecisionRecord;
    /// ed25519 验签（madai-w07 首日 F1）：验签算力在宿主（jvm `java.security` / iOS CryptoKit / 鸿蒙 cryptoFramework），
    /// 核只持受信公钥表；实现方对任何异常返回 false，禁抛。`signature` 64 字节、`public_key` 32 字节原始编码。
    /// 线程（madai-w10 I-C-09）：在核的 control 线程上被调、宿主须本地同步实现（≤ 50 ms）；调用它的 [`InferCore::apply_config`]
    /// 正阻塞等回执——若实现经主线程 / 消息队列回投再算，则 `apply_config` 不得在该线程上调（自锁 → 计假 → 合法文档被拒）。
    fn verify_ed25519(&self, message: Vec<u8>, signature: Vec<u8>, public_key: Vec<u8>) -> bool;
    /// 线程钩子（madai-w08 G7 ↔ madinfer-host `ThreadHooks`）：核的每条工作线程启动时**在该线程上**调一次，
    /// 宿主据 `class` 给当前线程设优先级 / QoS / 名字（iOS `qos_class`、Android `setThreadPriority`、鸿蒙 QoS）。
    /// 只准同步设置立即返回；不设置就空实现。核侧留痕 `ffi=thread_started class=<class>`。
    /// 档位映射口径见 [`WorkerClassRecord`]（`Background` → utility 档，禁 background 档；madinfer.h §5b 同文）。
    /// 授权专用线程 `madinfer-ffi-consent`（madai-w10 I-C-09）：每次升云授权起一条，线程体第一行以 `Control` 调本钩子、
    /// 随后才调 [`Self::authorize`]——宿主"是否核线程"守卫与 QoS 设置因此覆盖到授权回调体（I-S-05 U-5）。
    fn on_worker_started(&self, class: WorkerClassRecord);
}

/// 工作线程类别（madinfer-host `WorkerClass` 镜像；madai-w08 G7）。
///
/// 类别按**槽位**分派、不按请求 QoS：`Interactive` = Cpu / Gpu / Npu 槽（内联引擎）worker，`Background` = External 槽
/// （系统模型 / 云端适配器）worker；三档 QoS 的请求都可能落在任一类 worker 上。Async 与 Background（批量）两档的区分在
/// 调度（门控准入 / 配额 / 抢占），不在线程优先级——madai-w09 I-C-08 总控裁决（I-S-04 反馈 U-3 销项）：枚举不拆、不升 ABI，
/// 拆池等真 llama 后端出现两档争用读数后再议。宿主映射口径三端同（madinfer.h §5b 同文）：`Background` worker → **utility 档，
/// 禁 background 档**；其余三类沿 iOS 先例映射用户发起档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WorkerClassRecord {
    /// control 线程（调度 actor）；授权专用线程 `madinfer-ffi-consent` 也以本类别上报（madai-w10 I-C-09：它替 control
    /// 线程承担"等宿主授权"的那一段，每次授权一条、authorize 前调）。
    Control,
    /// Cpu / Gpu / Npu 槽（内联引擎）worker；承接三档 QoS 的本地推理。
    Interactive,
    /// External 槽（系统模型 / 云端适配器）worker。宿主映射 **utility 档，禁 background 档**（iOS `QOS_CLASS_UTILITY`
    /// 而非 `QOS_CLASS_BACKGROUND`、鸿蒙 `QOS_UTILITY` 而非 `QOS_BACKGROUND`、Android 不用 `THREAD_PRIORITY_BACKGROUND`）：
    /// background 档在热 / 低电下会被系统压到零配额，把同一 worker 上的 Async 请求随批量一起饿死。
    Background,
    /// 投递线程。**预留**：本批合帧与业务回调仍在 control 线程执行（madinfer-api core.rs 头注），投递线程尚未拆出，
    /// 宿主今天不会收到本类回调（I-D-07 §7-5 对账：jvm / AVD 只见 Control / Interactive / Background 是口径而非缺漏）。
    Delivery,
}

const fn worker_class_record_of(class: WorkerClass) -> WorkerClassRecord {
    match class {
        WorkerClass::Control => WorkerClassRecord::Control,
        WorkerClass::Interactive => WorkerClassRecord::Interactive,
        WorkerClass::Background => WorkerClassRecord::Background,
        WorkerClass::Delivery => WorkerClassRecord::Delivery,
    }
}

/// 线程钩子桥：宿主回调 panic 收口计数（工作线程不得因宿主异常死亡）。
struct BridgeThreadHooks(Arc<Bridge>);

impl ThreadHooks for BridgeThreadHooks {
    fn on_worker_started(&self, class: WorkerClass) {
        let record = worker_class_record_of(class);
        if catch_unwind(AssertUnwindSafe(|| self.0 .0.on_worker_started(record))).is_err() {
            SINK_PANICS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// install 标记键（建站写入，演示 KvStore 通路；正式消费归配置持久化批次）。
pub const KV_INSTALL_MARKER_KEY: &str = "madinfer.install_marker";

/// 授权记忆键前缀（madai-w07 F9）：`madinfer.consent.<capability>.<endpoint>`，值 [`KV_CONSENT_REMEMBERED`]。
/// 记忆位归核侧：宿主回 `AllowRemember` 即 `kv_put`，此后同键请求不再问宿主——uniffi 与 C 两面同一实现处
/// （[`BridgeAuthorizer`]，专用线程上读写，control 线程不碰 kv）。
pub const KV_CONSENT_PREFIX: &str = "madinfer.consent.";

/// 授权记忆键的值（单字节 1）；读到首字节为 1 即视为已记忆。
pub const KV_CONSENT_REMEMBERED: u8 = 1;

/// 授权记忆键（F9）。
pub fn consent_memory_key(capability: &str, endpoint_id: &str) -> String {
    format!("{KV_CONSENT_PREFIX}{capability}.{endpoint_id}")
}

// ---------------------------------------------------------------------------
// 宿主端口适配（HostBridge → madinfer-host trait 族）
// ---------------------------------------------------------------------------

struct Bridge(Box<dyn HostBridge>);

struct BridgeClock(Arc<Bridge>);

impl Clock for BridgeClock {
    fn now_ms(&self) -> u64 {
        self.0 .0.now_ms()
    }
}

struct BridgeTelemetry(Arc<Bridge>);

impl TelemetrySink for BridgeTelemetry {
    fn emit(&self, batch_json: &str) {
        // 过界前把 JSON 文本收进类型化 record：头字段核内解析（同进程同 crate 族的 telemetry.v1），
        // payload 原字节转交（KMP 不解读，lane-rules 5）。
        let (schema_version, event_count) =
            match serde_json::from_str::<serde_json::Value>(batch_json) {
                Ok(doc) => (
                    u32::try_from(doc["schema"].as_u64().unwrap_or(0)).unwrap_or(0),
                    u32::try_from(doc["events"].as_array().map_or(0, Vec::len)).unwrap_or(u32::MAX),
                ),
                Err(_) => (0, 0),
            };
        self.0 .0.telemetry_send(TelemetryBatchRecord {
            schema_version,
            event_count,
            payload: batch_json.as_bytes().to_vec(),
        });
    }
}

/// 授权桥（uniffi 与 C 两面共用）：专用线程上先查记忆位、再问宿主、AllowRemember 落记忆（madai-w07 F9）。
struct BridgeAuthorizer(Arc<Bridge>);

impl BridgeAuthorizer {
    /// 记忆位命中判定（kv 值首字节 = 1）。
    fn remembered(bridge: &Bridge, key: &str) -> bool {
        // 宿主 kv_get panic 收口为"未记忆"（保守：多问一次用户，不放行）。
        catch_unwind(AssertUnwindSafe(|| bridge.0.kv_get(key.to_owned())))
            .ok()
            .flatten()
            .is_some_and(|value| value.first() == Some(&KV_CONSENT_REMEMBERED))
    }
}

impl Authorizer for BridgeAuthorizer {
    fn request(&self, context: ConsentContext, reply: Box<dyn FnOnce(ConsentDecision) + Send>) {
        // HostBridge::authorize 允许阻塞等用户：起专用线程调它，control 线程不等（回执经 reply
        // 闭包投回核邮箱）。kv_get / kv_put 可阻塞 ≤ 50 ms，同样只在专用线程读写。
        // 线程起不来时按拒绝回执（保守失败，不吊死请求）。
        let bridge = Arc::clone(&self.0);
        let record = ConsentRecord {
            capability: context.capability.as_str().to_owned(),
            privacy: privacy_record_of(context.privacy),
            endpoint_id: context.endpoint_id,
        };
        let spawned = std::thread::Builder::new()
            .name("madinfer-ffi-consent".into())
            .spawn(move || {
                // madai-w10 I-C-09 J5-a（I-S-05 U-5）：专用线程第一行走线程钩子（类别复用 Control——它替 control 线程
                // 承担"等宿主"的那一段，不是推理 worker），壳据此设 QoS、"是否核线程"守卫覆盖到 authorize 回调体；
                // 钩子里宿主 panic 由 BridgeThreadHooks 收口计数，不影响本次授权。
                BridgeThreadHooks(Arc::clone(&bridge)).on_worker_started(WorkerClass::Control);
                let key = consent_memory_key(&record.capability, &record.endpoint_id);
                // F9 记忆位：命中即放行、不再打扰宿主（决议按 Allow 回执，观测记 Allow）。
                if Self::remembered(&bridge, &key) {
                    reply(ConsentDecision::Allow);
                    return;
                }
                let decision = match catch_unwind(AssertUnwindSafe(|| bridge.0.authorize(record))) {
                    Ok(ConsentDecisionRecord::Allow) => ConsentDecision::Allow,
                    Ok(ConsentDecisionRecord::AllowRemember) => {
                        // 记忆归核侧（宿主零逻辑）：kv_put 失败 / panic 只影响下次是否再问，不影响本次放行。
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            bridge.0.kv_put(key, vec![KV_CONSENT_REMEMBERED]);
                        }));
                        ConsentDecision::AllowRemember
                    }
                    // 宿主回调 panic 收口为拒绝（保守失败）。
                    Ok(ConsentDecisionRecord::Deny) | Err(_) => ConsentDecision::Deny,
                };
                reply(decision);
            });
        if spawned.is_err() {
            // reply 已被移动进闭包；spawn 失败时闭包随 Err 返回被丢弃，请求会停在 AwaitingConsent。
            // Builder::spawn 只在系统线程资源枯竭时失败——此时进程已不可用，接受该边界。
        }
    }
}

// ---------------------------------------------------------------------------
// 事件适配（madinfer_api::Event → record 回调；panic 在投递点收口）
// ---------------------------------------------------------------------------

/// 验签瞬时线程名（每次验签一条；`madinfer.h` §5 同文）。
pub const VERIFY_THREAD_NAME: &str = "madinfer-ffi-verify";

/// 验签代数票发号器：每次 `verify` 取一张，回执携票；票已过期（control 线程已按超时放弃）的回执哑火计数。
static VERIFY_TICKETS: AtomicU64 = AtomicU64::new(0);
/// 验签超时累计（control 线程等满 [`VERIFY_DEADLINE_MS`] 未得回执）。
static VERIFY_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
/// 迟到回执累计（宿主 `verify_ed25519` 在超时之后才返回；核只计数、不重投、不再触碰宿主）。
static VERIFY_LATE_REPLIES: AtomicU64 = AtomicU64::new(0);

/// 验签超时累计次数（进程级；`madinfer.h` §5 "验签回投" 条的留痕面，三端用例据此断言 deadline 路径）。
#[uniffi::export]
pub fn verify_timeout_count() -> u64 {
    VERIFY_TIMEOUTS.load(Ordering::Relaxed)
}

/// 迟到验签回执累计次数（进程级；票已过期的回执哑火计数）。
#[uniffi::export]
pub fn verify_late_reply_count() -> u64 {
    VERIFY_LATE_REPLIES.load(Ordering::Relaxed)
}

/// 验签回执（瞬时线程 → control 线程）：代数票 + 结论。
struct VerifyReply {
    ticket: u64,
    verified: bool,
}

/// 配置 / 算法包验签器（madai-w07 首日 F1）：`SignatureVerifier` 的宿主回调实装——按 `key_id` 查受信公钥，
/// base64 解签名，经 [`HostBridge::verify_ed25519`] 验；宿主回调 panic 一律计验假（不越界）。
///
/// deadline（madai-w11 I-C-10，I-S-05 U-3 / w10 R1）：`verify` 在 control 线程上被 `ConfigStore` 调用，但宿主回调不在
/// control 线程执行——投到专用瞬时线程 [`VERIFY_THREAD_NAME`]（线程体第一行走 `on_worker_started(Control)`，与授权线程同口径），
/// control 线程 `recv_timeout(VERIFY_DEADLINE_MS)` 等回执：超时 → [`VerifyOutcome::TimedOut`]（管线落 `signature_invalid` +
/// detail `verify=timeout`）并计 [`verify_timeout_count`]；此后宿主回调才返回的回执按代数票哑火，计 [`verify_late_reply_count`]。
/// 宿主验签卡死只卡住那条瞬时线程，核站其余入口不受影响。
///
/// 超时计数两份（madai-w15 I-C-13）：进程级 [`VERIFY_TIMEOUTS`]（[`verify_timeout_count`]，旧口径不变）+ 本站 `timeouts`
/// （与 [`InferCore`] 共享，`InferCore::stats().verify_timeouts`），同一处 `fetch_add`、不会分叉。
///
/// deadline 取值（madai-w16 I-C-14）：`deadline_ms` 与 [`InferCore`] 共享，建站 = [`VERIFY_DEADLINE_MS`]；只有调试面
/// `InferCore::debug_set_verify_deadline_ms`（debug 构建）会改它——"撞 deadline"由靶慢机器变成可注入复现；release 构建恒为常量。
pub struct BridgeVerifier {
    bridge: Arc<Bridge>,
    keys: HashMap<String, Vec<u8>>,
    timeouts: Arc<AtomicU64>,
    deadline_ms: Arc<AtomicU64>,
}

impl BridgeVerifier {
    fn new(
        bridge: Arc<Bridge>,
        trusted: &[TrustedKeyRecord],
        timeouts: Arc<AtomicU64>,
        deadline_ms: Arc<AtomicU64>,
    ) -> Result<Self, InferFfiError> {
        let mut keys = HashMap::with_capacity(trusted.len());
        for key in trusted {
            let raw = hex_decode(&key.public_key_hex).ok_or(InferFfiError::InvalidArgument)?;
            if raw.len() != 32 || key.key_id.is_empty() {
                return Err(InferFfiError::InvalidArgument);
            }
            keys.insert(key.key_id.clone(), raw);
        }
        Ok(Self {
            bridge,
            keys,
            timeouts,
            deadline_ms,
        })
    }
}

impl SignatureVerifier for BridgeVerifier {
    fn verify(&self, payload: &[u8], signature: &Signature) -> VerifyOutcome {
        if signature.alg != "ed25519" {
            return VerifyOutcome::Rejected;
        }
        let Some(key) = self.keys.get(&signature.key_id) else {
            return VerifyOutcome::Rejected;
        };
        let Some(sig) = base64_decode(&signature.sig) else {
            return VerifyOutcome::Rejected;
        };
        if sig.len() != 64 {
            return VerifyOutcome::Rejected;
        }
        let bridge = Arc::clone(&self.bridge);
        let (payload, key) = (payload.to_vec(), key.clone());
        let ticket = VERIFY_TICKETS.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = std::sync::mpsc::channel::<VerifyReply>();
        let spawned = std::thread::Builder::new()
            .name(VERIFY_THREAD_NAME.into())
            .spawn(move || {
                // 与授权线程同口径：第一行走线程钩子（类别 Control——它替 control 线程承担"等宿主"的那一段），
                // 壳据此设 QoS / 核线程守卫；钩子里宿主 panic 由 BridgeThreadHooks 收口计数。
                BridgeThreadHooks(Arc::clone(&bridge)).on_worker_started(WorkerClass::Control);
                let verified = catch_unwind(AssertUnwindSafe(|| {
                    bridge.0.verify_ed25519(payload, sig, key)
                }))
                .unwrap_or(false);
                // 接收端已按超时离场 → 票过期，哑火计数；核不重投、不再触碰宿主。
                if tx.send(VerifyReply { ticket, verified }).is_err() {
                    VERIFY_LATE_REPLIES.fetch_add(1, Ordering::Relaxed);
                }
            });
        if spawned.is_err() {
            // 线程起不来（系统线程资源枯竭）：保守按验假，不吊死 control 线程。
            return VerifyOutcome::Rejected;
        }
        let deadline_ms = self.deadline_ms.load(Ordering::Relaxed);
        match rx.recv_timeout(std::time::Duration::from_millis(deadline_ms)) {
            Ok(VerifyReply {
                ticket: got,
                verified,
            }) if got == ticket => {
                if verified {
                    VerifyOutcome::Verified
                } else {
                    VerifyOutcome::Rejected
                }
            }
            // 通道按次新建，票不匹配在形态上不可达；按代数票纪律仍作陈旧回执处置（哑火计数、按验假）。
            Ok(_) => {
                VERIFY_LATE_REPLIES.fetch_add(1, Ordering::Relaxed);
                VerifyOutcome::Rejected
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                VERIFY_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                self.timeouts.fetch_add(1, Ordering::Relaxed);
                VerifyOutcome::TimedOut
            }
            // 发送端没回执就消失（线程体在 send 前终止）：形态上不可达（宿主 panic 已 catch_unwind）；保守按验假。
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => VerifyOutcome::Rejected,
        }
    }

    fn knows_key(&self, key_id: &str) -> bool {
        self.keys.contains_key(key_id)
    }
}

/// hex → 字节；长度奇数或非 hex 字符返回 None。
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    bytes
        .chunks(2)
        .map(|pair| Some((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

/// 标准字母表 base64（RFC 4648 §4，容忍缺省填充）→ 字节；非法字符返回 None。
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let trimmed = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in trimmed.bytes() {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

fn os_signal_of(record: OsSignalRecord) -> OsSignal {
    match record {
        OsSignalRecord::Thermal { state } => OsSignal::Thermal(match state {
            ThermalRecord::Nominal => ThermalState::Nominal,
            ThermalRecord::Fair => ThermalState::Fair,
            ThermalRecord::Serious => ThermalState::Serious,
            ThermalRecord::Critical => ThermalState::Critical,
        }),
        OsSignalRecord::Battery {
            level_pct,
            charging,
            low_power_mode,
        } => OsSignal::Battery {
            // None 原样进核：未知不参与电量门控（madai-w09 I-C-08 删除 100 透传）。
            level_pct,
            charging,
            low_power_mode,
        },
        OsSignalRecord::Memory { level } => OsSignal::Memory(match level {
            MemoryPressureRecord::Normal => MemoryPressure::Normal,
            MemoryPressureRecord::Warning => MemoryPressure::Warning,
            MemoryPressureRecord::Critical => MemoryPressure::Critical,
        }),
        OsSignalRecord::App { state, budget_ms } => OsSignal::App {
            state: match state {
                AppStateRecord::Foreground => AppState::Foreground,
                AppStateRecord::Background => AppState::Background,
                AppStateRecord::BackgroundTask => AppState::BackgroundTask,
                AppStateRecord::Terminating => AppState::Terminating,
            },
            budget_ms,
        },
        OsSignalRecord::Network { state } => OsSignal::Network(match state {
            NetStateRecord::Offline => NetState::Offline,
            NetStateRecord::Metered => NetState::Metered,
            NetStateRecord::Unmetered => NetState::Unmetered,
        }),
        OsSignalRecord::Idle { idle } => OsSignal::Idle(idle),
        OsSignalRecord::MainLinkBusy { busy } => OsSignal::MainLinkBusy(busy),
    }
}

static SINK_PANICS: AtomicU64 = AtomicU64::new(0);

/// 宿主回调 panic 被投递点收口的累计次数。
#[uniffi::export]
pub fn sink_panic_count() -> u64 {
    SINK_PANICS.load(Ordering::Relaxed)
}

struct SinkAdapter {
    sink: Box<dyn EventSink>,
    /// 请求是否结构化：决定 `DeltaRecord.structured_patch` 是 Some（每帧携带）还是 None。
    structured: bool,
}

impl SinkAdapter {
    fn deliver(&self, run: impl FnOnce(&dyn EventSink)) {
        // 宿主回调体 panic（Kotlin 异常经 uniffi 转为 Rust panic）不得拖死 control 线程：
        // 投递点收口吞掉并计数（AssertUnwindSafe 理由：sink 是宿主对象，核侧无跨 unwind 状态）。
        if catch_unwind(AssertUnwindSafe(|| run(self.sink.as_ref()))).is_err() {
            SINK_PANICS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl madinfer_api::EventSink for SinkAdapter {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Started {
                request,
                source,
                package,
                route_reason,
                queued_ms,
            } => self.deliver(|s| {
                s.on_started(StartedRecord {
                    request: request.0,
                    source: source_record_of(source),
                    package: package.map(|p| p.to_wire()),
                    route_reason: route_reason_record_of(route_reason),
                    queued_ms,
                });
            }),
            Event::Delta(delta) => {
                let structured_patch = self.structured.then(|| patch_text_of(&delta.patch));
                self.deliver(|s| {
                    s.on_delta(DeltaRecord {
                        request: delta.request.0,
                        seq: delta.seq,
                        text: delta.text,
                        tokens: delta.tokens,
                        structured_patch,
                    });
                })
            }
            Event::Complete {
                request,
                output,
                stats,
            } => self.deliver(|s| {
                s.on_complete(CompleteRecord {
                    request: request.0,
                    text: text_of(output.clone()),
                    output: output_record_of(output),
                    stats: StatsRecord {
                        ttft_ms: stats.ttft_ms,
                        decode_ms: stats.decode_ms,
                        prompt_tokens: stats.prompt_tokens,
                        gen_tokens: stats.gen_tokens,
                        tps_x100: stats.tps_x100,
                        peak_memory_mb: stats.peak_memory_mb,
                        source: stats.source.map(source_record_of),
                        fallback_steps: stats.fallback_steps,
                        prefix_hit: stats.prefix_hit,
                    },
                });
            }),
            Event::Failed {
                request,
                error,
                partial,
            } => self.deliver(|s| {
                s.on_failed(FailedRecord {
                    request: request.0,
                    code: error.code(),
                    wire: error.kind_wire().to_owned(),
                    retry_after_ms: match error {
                        InferError::Retryable { retry_after_ms, .. } => Some(retry_after_ms),
                        _ => None,
                    },
                    partial_text: partial.map(text_of),
                });
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// 建站（InferCore over madinfer_api::Core）
// ---------------------------------------------------------------------------

/// 核站（uniffi Object）：[`Self::create`] 建站起 control 线程 + worker 池；Kotlin 侧句柄
/// `destroy()` / GC 触发 Drop → `CoreHandle` 的 Drop 发 Shutdown 并 join（关停段）。
#[derive(uniffi::Object)]
pub struct InferCore {
    handle: CoreHandle,
    probe: ProbeRecord,
    /// 本站验签超时累计（与 [`BridgeVerifier`] 共享；[`Self::stats`] 的 `verify_timeouts`）。无受信公钥的站恒 0。
    verify_timeouts: Arc<AtomicU64>,
    /// 本站验签 deadline 毫秒（与 [`BridgeVerifier`] 共享；madai-w16 I-C-14 `debug_set_verify_deadline_ms` 的落点）。
    /// 无受信公钥的站没有验签器 → `None`（调试入口回 4001）；release 下函数体门控在读它之前就回 4005，字段只写不读。
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    verify_deadline_ms: Option<Arc<AtomicU64>>,
    /// 调试建站留下的脚本后端句柄（madai-w12 I-C-11：`debug_set_token_delay_ms` 的落点）；生产站 / release 构建恒空
    /// （release 下函数体门控在读它之前就回 4005，字段只写不读——零大小空壳，放行 dead_code）。
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    debug_script: debug::DebugScript,
}

#[uniffi::export]
impl InferCore {
    /// 建站：注入宿主端口 + 选项，注册 backend-llama 确定性仿真核。
    ///
    /// 验签（madai-w07 F1）：`options.trusted_keys` 非空即注入 [`BridgeVerifier`]（验签算力在宿主回调），生产构建可建站；
    /// 为空沿旧口径——调试构建 `TrustAllVerifier`（泳道规则 14 点名中间态）、生产构建装配期拒绝 4003。
    #[uniffi::constructor]
    pub fn create(
        host: Box<dyn HostBridge>,
        options: CoreOptions,
    ) -> Result<Arc<Self>, InferFfiError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| Self::build(host, options)));
        match outcome {
            Ok(result) => result.map(Arc::new),
            Err(_) => Err(InferFfiError::Panicked),
        }
    }

    /// 资源信号入口（madai-w07 F1）：热 / 电量 / 内存 / 前后台 / 网络 / 空闲 / 主链路忙进核调度门控；
    /// 邮箱满则丢并计数（capability.rules 10），不阻塞调用方。
    pub fn signal(&self, signal: OsSignalRecord) {
        self.handle.signal(os_signal_of(signal));
    }

    /// 探测：能力 + 档位快照（建站输入上由能力索引实答；下载态随资产层接入变为动态）。
    pub fn probe(&self) -> ProbeRecord {
        self.probe.clone()
    }

    /// 发起流式生成；返回请求句柄（事件与 [`Self::cancel`] 都以它为锚）。
    ///
    /// 时序契约（K1）：`on_started` 恰一次 → `on_delta`* → `on_complete` / `on_failed` 恰一次，
    /// 之后无事件；形式非法（4001）在 Started 前拒绝，宿主不会收到任何回执。
    pub fn stream(
        &self,
        request: RequestRecord,
        sink: Box<dyn EventSink>,
    ) -> Result<u64, InferFfiError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let gen_request = gen_request_of(&request)?;
            let structured = matches!(gen_request.output, OutputSpec::Structured { .. });
            Ok(self
                .handle
                .stream(gen_request, Box::new(SinkAdapter { sink, structured }))
                .0)
        }));
        match outcome {
            Ok(result) => result,
            Err(_) => Err(InferFfiError::Panicked),
        }
    }

    /// 取消：未完成请求恰一次 `on_failed`(3008)；已终态 / 未知句柄幂等 no-op（泳道规则 9）。
    pub fn cancel(&self, handle: u64) {
        if catch_unwind(AssertUnwindSafe(|| self.handle.cancel(RequestId(handle)))).is_err() {
            // cancel 无返回通道：panic 收口后静默（核侧邮箱纪律使该路径实际不可达）。
        }
    }

    /// 送一份已签名 config.v1 文档进核并同步拿回应用结论（madai-w08 G2；"核发起验签"端到端）。
    ///
    /// 管线全部在 control 线程上由 `ConfigStore` 走完：验签（`trusted_keys` 非空时经 [`BridgeVerifier`] →
    /// 宿主 `verify_ed25519`；为空时调试构建 TrustAll、生产构建建站已被 4003 拒）→ 解析 → 版本递增 /
    /// min_sdk / 设备分桶 / 语义 → 原子切换。被拒不动活动配置；结论同时进 telemetry
    /// （`config_apply` / `config_rejected`）。核已停返回 4004；核内 panic 收口为 4002。
    ///
    /// 线程契约（madai-w10 I-C-09，I-H-05 反馈 ①；madai-w11 I-C-10 加 deadline；`madinfer.h` §5 / §6 同文）：本方法阻塞
    /// 调用线程等核回执；核的 control 线程把 [`HostBridge::verify_ed25519`] 投到专用瞬时线程 `madinfer-ffi-verify`
    /// （线程体第一行以 `WorkerClassRecord::Control` 调 `on_worker_started`）并最多等 [`VERIFY_DEADLINE_MS`]（2 000 ms）：
    /// 期限内回执 → 真进解析 / 假 `SignatureInvalid`；超时 → `SignatureInvalid` + `detail = "verify=timeout"`
    /// （stage `Received`），宿主验签卡死不再拖死核站，迟到回执按代数票哑火计数（[`verify_timeout_count`] /
    /// [`verify_late_reply_count`]）。每次 apply 至多一次 `verify_ed25519`（文档解析通过且 `key_id` 受信才调；未知 key /
    /// 非 JSON 不进宿主），并发 apply 在核内按入邮箱序串行、按同序回执。**禁在验签回投所依赖的线程上同步调本方法**：
    /// 宿主验签若经主线程 / 消息队列回投（iOS 主队列、Android 主 Looper、鸿蒙 TSF → ArkTS），主线程直调 = 自己等自己 →
    /// 撞 deadline → 合法文档被 `SignatureInvalid`（verify=timeout）拒。在与验签回投无依赖的线程（配置线程 / 工作线程）调用。
    pub fn apply_config(&self, document: Vec<u8>) -> Result<ApplyOutcomeRecord, InferFfiError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.handle.apply_config(document)));
        match outcome {
            Ok(Some(report)) => Ok(apply_outcome_record_of(&report)),
            Ok(None) => Err(InferFfiError::CoreStopped),
            Err(_) => Err(InferFfiError::Panicked),
        }
    }

    /// 按前缀丢弃生成上下文并同步拿回条数（madai-w14 I-C-12；设计稿 R-8 / T6：账户关闭时宿主租约以 `acct:<id>/` 调本方法，
    /// 换账户不得复用前一账户的 KV / 亲和）。
    ///
    /// 经既有邮箱口径在 control 线程执行 `ContextPool::drop_prefix`：命中的键立即不可复用（之后同键预留必是新建、`hits` 不增），
    /// 在飞任务持有的上下文延迟到任务终态才结清 KV。空前缀 / 无匹配回 0；核已停返回 4004；核内 panic 收口为 4002。
    /// 本方法阻塞调用线程等核回执（等的是核自己的 control 线程，不等宿主），**不得在任何 host / sink 回调里调**。
    pub fn drop_contexts(&self, prefix: String) -> Result<u32, InferFfiError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.handle.drop_contexts(prefix)));
        match outcome {
            Ok(Some(dropped)) => Ok(dropped),
            Ok(None) => Err(InferFfiError::CoreStopped),
            Err(_) => Err(InferFfiError::Panicked),
        }
    }

    /// 运行期台账快照（madai-w15 I-C-13；只加、uniffi-only，C 面不加符号）：经既有邮箱口径在 control 线程一次取齐
    /// [`CoreStatsRecord`]——上下文池命中 / 丢弃 / 条目 / KV 已用与预算、在飞 Job 数、运行时长，加本站验签超时累计。
    /// 只读、不改任何状态；核已停返回 4004；核内 panic 收口为 4002。本方法阻塞调用线程等核回执（等的是核自己的 control 线程，
    /// 不等宿主），**不得在任何 host / sink 回调里调**。
    pub fn stats(&self) -> Result<CoreStatsRecord, InferFfiError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.handle.stats()));
        match outcome {
            Ok(Some(stats)) => Ok(CoreStatsRecord {
                backend_kind: self.probe.backend,
                context_pool_hits: stats.contexts.hits,
                context_pool_dropped: stats.contexts.dropped,
                context_pool_entries: stats.context_entries,
                context_pool_kv_mb: stats.context_kv_mb,
                context_pool_kv_budget_mb: stats.context_kv_budget_mb,
                verify_timeouts: self.verify_timeouts.load(Ordering::Relaxed),
                inflight_jobs: stats.inflight_jobs,
                uptime_ms: stats.uptime_ms,
            }),
            Ok(None) => Err(InferFfiError::CoreStopped),
            Err(_) => Err(InferFfiError::Panicked),
        }
    }
}

impl InferCore {
    /// 生产装配：唯一后端 = backend-llama，形态由 `options.backend` 定（madai-w17 I-C-16）。`Emulated` → 仿真核（行为同前）；
    /// `Llama` → 三态：feature `llama-sys` 未编入 → 4005；`model_path` 缺席或不是文件 → 4003；否则真实引擎，权重路径随每次
    /// 加载经 `CoreBuilder::weights_path` → `LoadOpts::weights_path` 进后端（构造期不持路径）。
    fn build(host: Box<dyn HostBridge>, options: CoreOptions) -> Result<Self, InferFfiError> {
        match options.backend {
            BackendKindRecord::Emulated => Self::build_with(
                host,
                options,
                Arc::new(LlamaBackend::new(LlamaOptions::default())),
            ),
            BackendKindRecord::Llama => Self::build_llama(host, options),
        }
    }

    /// `backend = Llama` 且本 crate 编入 feature `llama-sys`：先校路径（不建站、不碰宿主），再装配真实引擎后端。
    #[cfg(feature = "llama-sys")]
    fn build_llama(host: Box<dyn HostBridge>, options: CoreOptions) -> Result<Self, InferFfiError> {
        let path = options
            .model_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_file())
            .ok_or(InferFfiError::CoreStartFailed)?;
        Self::build_with_weights(
            host,
            options,
            Arc::new(LlamaBackend::native_from_assets(LlamaOptions::default())),
            Some(path),
        )
    }

    /// `backend = Llama` 但本构建未编入 feature `llama-sys`：4005（本构建无真实引擎；不建站、不碰宿主、不回退仿真核）。
    #[cfg(not(feature = "llama-sys"))]
    fn build_llama(
        _host: Box<dyn HostBridge>,
        _options: CoreOptions,
    ) -> Result<Self, InferFfiError> {
        Err(InferFfiError::FeatureDisabled)
    }

    /// 装配主体（madai-w10 I-C-09 J5-a 抽出）：`backend` 是站内唯一后端——每个能力的 Bundled 候选、能力索引的可用后端表、
    /// probe 的约束支持度全部取自它；生产路径 [`Self::build`] 传仿真核，调试路径 [`debug`] 传 testkit 脚本后端。
    fn build_with(
        host: Box<dyn HostBridge>,
        options: CoreOptions,
        backend: Arc<dyn madinfer_backend::Backend>,
    ) -> Result<Self, InferFfiError> {
        Self::build_with_weights(host, options, backend, None)
    }

    /// 装配主体带权重路径：`weights_path` 进 `CoreBuilder::weights_path`，随每张工单进 `LoadOpts`（仿真核 / 脚本后端不读它）。
    fn build_with_weights(
        host: Box<dyn HostBridge>,
        options: CoreOptions,
        backend: Arc<dyn madinfer_backend::Backend>,
        weights_path: Option<std::path::PathBuf>,
    ) -> Result<Self, InferFfiError> {
        let backend_id = madinfer_backend::Backend::id(&backend).clone();
        let bridge = Arc::new(Bridge(host));
        let package = PackageRef::parse(SIM_PACKAGE_REF).ok_or(InferFfiError::CoreStartFailed)?;
        let mut capabilities = Vec::with_capacity(options.capabilities.len());
        for name in &options.capabilities {
            capabilities.push(CapabilityId::new(name).ok_or(InferFfiError::InvalidArgument)?);
        }
        if capabilities.is_empty() {
            return Err(InferFfiError::InvalidArgument);
        }
        // 受信公钥表先于任何宿主副作用校验（非 hex / 长度不对 → 4001 时宿主 kv 未被写入；I-C-06 C 面测试锁此序）。
        let verify_timeouts = Arc::new(AtomicU64::new(0));
        let mut verify_deadline_ms = None;
        let verifier = if options.trusted_keys.is_empty() {
            None
        } else {
            let deadline_ms = Arc::new(AtomicU64::new(VERIFY_DEADLINE_MS));
            verify_deadline_ms = Some(Arc::clone(&deadline_ms));
            Some(BridgeVerifier::new(
                Arc::clone(&bridge),
                &options.trusted_keys,
                Arc::clone(&verify_timeouts),
                deadline_ms,
            )?)
        };
        let mapping = MappingTable {
            entries: capabilities
                .iter()
                .map(|capability| {
                    let mut candidates = Vec::with_capacity(2);
                    if let Some(endpoint) = &options.consent_endpoint {
                        candidates.push(Candidate {
                            priority: 0,
                            target: Target::Cloud {
                                endpoint: endpoint.clone(),
                                kind: EndpointKind::Enterprise,
                            },
                            // 云后端未注册：requires_available = false 让候选留在链里，
                            // 授权通过后以后端缺失回退（演示位口径见 CoreOptions 文档）。
                            when: Conditions {
                                os: None,
                                tier_min: None,
                                requires_available: false,
                            },
                        });
                    }
                    candidates.push(Candidate {
                        priority: 1,
                        target: Target::Bundled {
                            backend: backend_id.clone(),
                            package: package.clone(),
                        },
                        when: Conditions {
                            os: None,
                            tier_min: None,
                            requires_available: true,
                        },
                    });
                    MappingEntry {
                        capability: capability.clone(),
                        candidates,
                    }
                })
                .collect(),
        };
        let tier = tier_of(options.tier);
        let os = os_of(options.os);
        // install 标记：KvStore 通路演示（读到即保留；缺席时写入建站时刻）。正式消费归配置持久化。
        if bridge.0.kv_get(KV_INSTALL_MARKER_KEY.to_owned()).is_none() {
            let stamp = bridge.0.now_ms().to_be_bytes().to_vec();
            bridge.0.kv_put(KV_INSTALL_MARKER_KEY.to_owned(), stamp);
        }
        // probe 用能力索引在同一份建站输入上实答（Core 内部同款输入：mapping + 后端可用 + 已装包）。
        let mut index = CapabilityIndex::new();
        let snapshot = index.update(&IndexInputs {
            mapping: mapping.clone(),
            tier,
            os: Some(os),
            available_backends: BTreeSet::from([backend_id]),
            quarantined: BTreeSet::new(),
            packages: vec![(package.clone(), PackageReadiness::Installed)]
                .into_iter()
                .collect(),
            disabled: BTreeSet::new(),
            all_off: false,
            cloud_reachable: false,
        });
        // 每个能力都路由到同一后端：Available 条目的约束支持度即它的 caps.constraint（仿真核 Emulated）。
        let llama_constraint =
            constraint_record_of(madinfer_backend::Backend::probe(&backend).constraint);
        let probe = ProbeRecord {
            abi_version: INFER_ABI_VERSION,
            core_version: env!("CARGO_PKG_VERSION").to_owned(),
            tier: options.tier,
            snapshot_version: snapshot.as_ref().map_or(0, |s| s.version),
            entries: snapshot.map_or_else(Vec::new, |s| {
                s.entries
                    .iter()
                    .map(|entry| CapabilityEntryRecord {
                        capability: entry.id.as_str().to_owned(),
                        state: cap_state_record_of(entry.state),
                        source: entry.source.map(source_record_of),
                        latency: match entry.latency {
                            madinfer_api::LatencyTier::Fast => LatencyRecord::Fast,
                            madinfer_api::LatencyTier::Normal => LatencyRecord::Normal,
                            madinfer_api::LatencyTier::Slow => LatencyRecord::Slow,
                        },
                        constraint: if entry.state == CapState::Available {
                            llama_constraint
                        } else {
                            ConstraintSupportRecord::None
                        },
                    })
                    .collect()
            }),
            backend: options.backend,
        };
        let mut builder = Core::builder()
            .clock(Arc::new(BridgeClock(Arc::clone(&bridge))))
            .telemetry_sink(Arc::new(BridgeTelemetry(Arc::clone(&bridge))))
            .authorizer(Arc::new(BridgeAuthorizer(Arc::clone(&bridge))))
            .thread_hooks(Arc::new(BridgeThreadHooks(Arc::clone(&bridge))))
            .backend(backend)
            .mapping(mapping)
            .installed_package(package)
            .tier(tier)
            .os(os)
            // 槽表带满三类：Bundled 走 Cpu，System / Cloud 目标各占一路 External——授权演示位的
            // 云目标 Job 需要 External(1) 槽才可进入执行（进而以后端缺失回退），缺槽会吊死请求。
            .slots(SlotTable::new([
                (SlotClass::Cpu, 1),
                (SlotClass::External(0), 1),
                (SlotClass::External(1), 1),
            ]))
            .sampling_bp(options.sampling_bp.min(10_000))
            .telemetry_meta(BatchMeta {
                instance: options.instance_name.clone(),
                sdk: env!("CARGO_PKG_VERSION").to_owned(),
                tier,
                os,
                os_version: String::new(),
            });
        if options.frame_ms > 0 {
            builder = builder.frame_ms(u64::from(options.frame_ms));
        }
        if let Some(path) = weights_path {
            builder = builder.weights_path(path);
        }
        if let Some(verifier) = verifier {
            builder = builder.verifier(Box::new(verifier));
        }
        let handle = builder
            .spawn()
            .map_err(|_| InferFfiError::CoreStartFailed)?;
        Ok(Self {
            handle,
            probe,
            verify_timeouts,
            verify_deadline_ms,
            debug_script: debug::DebugScript::default(),
        })
    }
}

/// 注入核内 panic 验证 `catch_unwind` 收口（对齐 im-core-cabi `im_core_test_panic` 用途）：
/// 恒返回 `Err(Panicked)` 且进程存活；生产宿主不调它。
#[uniffi::export]
pub fn test_panic() -> Result<(), InferFfiError> {
    match catch_unwind(|| panic!("madinfer-ffi 注入 panic（收口用例）")) {
        Ok(()) => Ok(()),
        Err(_) => Err(InferFfiError::Panicked),
    }
}

// ---------------------------------------------------------------------------
// record ↔ 核类型映射（纯函数）
// ---------------------------------------------------------------------------

fn apply_stage_record_of(stage: ApplyStage) -> ApplyStageRecord {
    match stage {
        ApplyStage::Received => ApplyStageRecord::Received,
        ApplyStage::Verified => ApplyStageRecord::Verified,
        ApplyStage::Parsed => ApplyStageRecord::Parsed,
        ApplyStage::Validated => ApplyStageRecord::Validated,
        ApplyStage::Applied => ApplyStageRecord::Applied,
        ApplyStage::Rejected => ApplyStageRecord::Rejected,
    }
}

/// 验签超时的诊断文本（`ApplyOutcomeRecord.detail`；`madinfer.h` §5 同文，`key=value` 留痕形态）。
pub const VERIFY_TIMEOUT_DETAIL: &str = "verify=timeout";

/// 拒绝原因 → (注册制枚举, 诊断文本, 诊断种类)。诊断文本只含 key_id / 版本对 / 桶号 / 语义错误 Debug 描述 / 验签超时留痕；
/// 种类与文本形态一一对应（madai-w15 I-C-13）。
fn apply_reject_record_of(
    reason: &RejectReason,
) -> (ApplyRejectReasonRecord, String, ApplyDetailKindRecord) {
    match reason {
        RejectReason::UnknownKey(key_id) => (
            ApplyRejectReasonRecord::UnknownKey,
            key_id.clone(),
            ApplyDetailKindRecord::UnknownKey,
        ),
        RejectReason::SignatureInvalid { timed_out: true } => (
            ApplyRejectReasonRecord::SignatureInvalid,
            VERIFY_TIMEOUT_DETAIL.to_owned(),
            ApplyDetailKindRecord::VerifyTimeout,
        ),
        RejectReason::SignatureInvalid { timed_out: false } => (
            ApplyRejectReasonRecord::SignatureInvalid,
            String::new(),
            ApplyDetailKindRecord::VerifyRejected,
        ),
        RejectReason::SchemaInvalid(detail) => (
            ApplyRejectReasonRecord::SchemaInvalid,
            detail.clone(),
            ApplyDetailKindRecord::Parse,
        ),
        RejectReason::VersionNotNewer { received, active } => (
            ApplyRejectReasonRecord::VersionNotNewer,
            format!("received={received} active={active}"),
            ApplyDetailKindRecord::VersionNotNewer,
        ),
        RejectReason::MinSdkUnsatisfied(req) => (
            ApplyRejectReasonRecord::MinSdkUnsatisfied,
            req.clone(),
            ApplyDetailKindRecord::MinSdkUnsatisfied,
        ),
        RejectReason::OutOfRollout { bucket } => (
            ApplyRejectReasonRecord::OutOfRollout,
            format!("bucket={bucket}"),
            ApplyDetailKindRecord::OutOfRollout,
        ),
        RejectReason::Semantic(err) => (
            ApplyRejectReasonRecord::SemanticInvalid,
            format!("{err:?}"),
            ApplyDetailKindRecord::Semantic,
        ),
    }
}

fn apply_outcome_record_of(report: &ApplyReport) -> ApplyOutcomeRecord {
    match &report.outcome {
        ApplyOutcome::Applied { version } => ApplyOutcomeRecord {
            applied: true,
            active_version: *version,
            stage: ApplyStageRecord::Applied,
            reject_reason: None,
            detail: String::new(),
            detail_kind: ApplyDetailKindRecord::None,
        },
        ApplyOutcome::Rejected { reason, stage } => {
            let (reject_reason, detail, detail_kind) = apply_reject_record_of(reason);
            ApplyOutcomeRecord {
                applied: false,
                active_version: report.active_version,
                stage: apply_stage_record_of(*stage),
                reject_reason: Some(reject_reason),
                detail,
                detail_kind,
            }
        }
    }
}

fn gen_request_of(record: &RequestRecord) -> Result<GenRequest, InferFfiError> {
    let capability = CapabilityId::new(&record.capability).ok_or(InferFfiError::InvalidArgument)?;
    let business = BusinessId::new(record.business).ok_or(InferFfiError::InvalidArgument)?;
    if record.options.stop.len() > 8 {
        return Err(InferFfiError::InvalidArgument);
    }
    // context：1～128 字节的稳定串（request.v1）；超长 / 空串形式非法。
    let context = match &record.context {
        Some(text) => Some(ContextKey::new(text).ok_or(InferFfiError::InvalidArgument)?),
        None => None,
    };
    let defaults = GenerateOptions::default();
    Ok(GenRequest {
        capability,
        business,
        privacy: privacy_of(record.privacy),
        budget: Budget::new(record.deadline_ms, qos_of(record.qos)),
        turn: Turn {
            messages: record
                .messages
                .iter()
                .map(|m| Message {
                    role: role_of(m.role),
                    content: m.content.clone(),
                })
                .collect(),
        },
        options: GenerateOptions {
            temperature_x100: record
                .options
                .temperature_x100
                .unwrap_or(defaults.temperature_x100),
            top_p_bp: record.options.top_p_bp.unwrap_or(defaults.top_p_bp),
            max_tokens: record.options.max_tokens.unwrap_or(defaults.max_tokens),
            stop: record.options.stop.clone(),
            seed: record.options.seed,
        },
        output: match &record.output {
            OutputSpecRecord::Text => OutputSpec::Text,
            OutputSpecRecord::Structured { schema_json } => OutputSpec::Structured {
                schema_json: schema_json.clone(),
            },
        },
        constraint: None,
        context,
        trace: None,
    })
}

fn text_of(output: Output) -> String {
    match output {
        Output::Text(text) | Output::Structured(text) => text,
    }
}

fn output_record_of(output: Output) -> OutputRecord {
    match output {
        Output::Text(text) => OutputRecord::Text { text },
        Output::Structured(json) => OutputRecord::Structured { json },
    }
}

/// 把一帧的 op 列表（每条已是 JSON 对象文本）拼成 RFC 6902 数组文本。
fn patch_text_of(ops: &[String]) -> String {
    let mut text = String::with_capacity(ops.iter().map(|o| o.len() + 1).sum::<usize>() + 2);
    text.push('[');
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            text.push(',');
        }
        text.push_str(op);
    }
    text.push(']');
    text
}

const fn constraint_record_of(
    support: madinfer_backend::ConstraintSupport,
) -> ConstraintSupportRecord {
    match support {
        madinfer_backend::ConstraintSupport::None => ConstraintSupportRecord::None,
        madinfer_backend::ConstraintSupport::Emulated => ConstraintSupportRecord::Emulated,
        madinfer_backend::ConstraintSupport::Native => ConstraintSupportRecord::Native,
    }
}

const fn tier_of(record: TierRecord) -> Tier {
    match record {
        TierRecord::Low => Tier::Low,
        TierRecord::Mid => Tier::Mid,
        TierRecord::Flagship => Tier::Flagship,
        TierRecord::Unknown => Tier::Unknown,
    }
}

const fn os_of(record: OsRecord) -> Os {
    match record {
        OsRecord::Ios => Os::Ios,
        OsRecord::Android => Os::Android,
        OsRecord::Ohos => Os::Ohos,
        OsRecord::Jvm => Os::Jvm,
    }
}

const fn privacy_of(record: PrivacyRecord) -> PrivacyTag {
    match record {
        PrivacyRecord::Public => PrivacyTag::Public,
        PrivacyRecord::Internal => PrivacyTag::Internal,
        PrivacyRecord::E2eePlaintext => PrivacyTag::E2eePlaintext,
        PrivacyRecord::Regulated => PrivacyTag::Regulated,
    }
}

const fn privacy_record_of(tag: PrivacyTag) -> PrivacyRecord {
    match tag {
        PrivacyTag::Public => PrivacyRecord::Public,
        PrivacyTag::Internal => PrivacyRecord::Internal,
        PrivacyTag::E2eePlaintext => PrivacyRecord::E2eePlaintext,
        PrivacyTag::Regulated => PrivacyRecord::Regulated,
    }
}

const fn qos_of(record: QosRecord) -> QoS {
    match record {
        QosRecord::Interactive => QoS::Interactive,
        QosRecord::Async => QoS::Async,
        QosRecord::Background => QoS::Background,
    }
}

const fn role_of(record: RoleRecord) -> Role {
    match record {
        RoleRecord::System => Role::System,
        RoleRecord::User => Role::User,
        RoleRecord::Assistant => Role::Assistant,
        RoleRecord::Tool => Role::Tool,
    }
}

const fn source_record_of(source: Source) -> SourceRecord {
    match source {
        Source::SystemModel => SourceRecord::SystemModel,
        Source::Bundled => SourceRecord::Bundled,
        Source::Cloud => SourceRecord::Cloud,
    }
}

const fn route_reason_record_of(reason: RouteReason) -> RouteReasonRecord {
    match reason {
        RouteReason::PrivacyForcedLocal => RouteReasonRecord::PrivacyForcedLocal,
        RouteReason::StaticTier => RouteReasonRecord::StaticTier,
        RouteReason::Availability => RouteReasonRecord::Availability,
        RouteReason::Switch => RouteReasonRecord::Switch,
        RouteReason::Default => RouteReasonRecord::Default,
        RouteReason::Fallback => RouteReasonRecord::Fallback,
    }
}

const fn cap_state_record_of(state: CapState) -> CapStateRecord {
    match state {
        CapState::Available => CapStateRecord::Available,
        CapState::NeedsDownload { .. } => CapStateRecord::NeedsDownload,
        CapState::Downloading { .. } => CapStateRecord::Downloading,
        CapState::Unsupported => CapStateRecord::Unsupported,
        CapState::Disabled => CapStateRecord::Disabled,
    }
}

#[cfg(test)]
mod tests;
