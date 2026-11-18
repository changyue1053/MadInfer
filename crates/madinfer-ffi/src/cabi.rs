//! 手写 C ABI 出口（设计文档 C17 第二面；madai-w07 I-C-06 F2）——`include/madinfer.h` 即契约，本模块是它的 Rust 实装。
//!
//! 十符号（`#[no_mangle] extern "C"`，`madinfer_` 前缀，mad 导出白名单第五族；W07 八符号 + W08 `madinfer_apply_config` +
//! W14 `madinfer_drop_contexts`）over **同一个** [`InferCore`]：
//! 不另建核，复用 uniffi 面的 [`InferCore::create`] / [`InferCore::stream`] / [`InferCore::cancel`] /
//! [`InferCore::probe`] / [`InferCore::signal`]——宿主 vtable 包成 [`HostBridge`] 实现、sink vtable 包成
//! [`EventSink`] 实现，K1 序 / 取消恰一次 / panic 收口 / 授权专用线程都是 lib.rs 那一份逻辑。
//!
//! | 符号 | ↔ uniffi 面 | 备注 |
//! |---|---|---|
//! | [`madinfer_abi_version`] | `infer_abi_version()` | = 2 |
//! | [`madinfer_core_create`] | `InferCore::create` | vtable 按值拷走；任一函数指针 NULL → 4001 |
//! | [`madinfer_core_destroy`] | 句柄 Drop（Shutdown + join） | 授权中请求 on_failed(3008) 收口后返回；NULL 幂等 |
//! | [`madinfer_probe`] | `InferCore::probe` | 容量协议：宿主给 entries 容量、核回填 entries_len / entries_total |
//! | [`madinfer_stream_start`] | `InferCore::stream` | > 0 句柄；0 + 4xxx = Started 前拒绝 |
//! | [`madinfer_stream_cancel`] | `InferCore::cancel` | 幂等 |
//! | [`madinfer_host_reply`] | `HostBridge::authorize` 的返回值 | 请求 / 回投对的回投半边，恰一次配对 |
//! | [`madinfer_signal`] | `InferCore::signal` | 只入队不阻塞 |
//! | [`madinfer_apply_config`] | `InferCore::apply_config` | "等核回执"入口（madai-w08 I-C-07 第 9 符号）；禁在回调里调 |
//! | [`madinfer_drop_contexts`] | `InferCore::drop_contexts` | 按前缀丢弃生成上下文、同步回条数（madai-w14 I-C-12 第 10 符号；只加符号、ABI 仍 2）；禁在回调里调 |
//!
//! ## authorize 两面等价关系（I-H-02 U-6）
//!
//! uniffi 面 `HostBridge::authorize(context) -> decision` 是同步返回值，核在专用线程 `madinfer-ffi-consent` 上调它；
//! C 面把它拆成 `host.authorize(user_data, host_request, context)`（只入队立即返回）+ [`madinfer_host_reply`]。
//! 本模块的 [`HostBridge`] 实装在**同一条专用线程**上：分配 host_request → 登记回投口 → 调宿主回调 → 阻塞等回投
//! （`mpsc` 通道，不轮询）→ 把决议作为 `authorize` 的返回值交回。control 线程始终不等。destroy 时：先关核（在飞请求
//! 含授权中的以 Failed(3008) 收口），再等所有在途宿主回调（包括超时验签）返回，最后丢弃未回投的回投口——等待线程以 Deny 收口、
//! 回执落在已关停的邮箱上被丢弃。AllowRemember 记忆位（F9）在 lib.rs Bridge 层，两面同一实现。
//!
//! ## 纪律
//!
//! - **全仓唯一 C 边界**：workspace lints `unsafe_code = "deny"`，本模块以 `#![allow(unsafe_code)]` 模块级放开、
//!   `#![deny(unsafe_op_in_unsafe_fn)]` 逼每处 `unsafe {}` 显式并带 SAFETY 注释——与 madinfer-host `MonotonicClock`
//!   的 `Instant::now` 豁免同一纪律（一个豁免点、一处说明）。lib.rs 与其余 crate 零 `unsafe`。
//! - 全部入口 `catch_unwind` 收口：panic → 4002（[`InferFfiError::Panicked`]），进程存活。
//! - 字符串 UTF-8 + 长度（[`RawStr`]），不依赖 NUL；非 UTF-8 → 4001。借用语义：入参只在调用期间有效，回调实参只在
//!   回调期间有效。
//! - `kv_get` 以 [`MADINFER_KV_VALUE_MAX`]（4 KB）缓冲调用（头文件 §4 容量协议）。
//! - 布局：全部过界结构体 `#[repr(C)]`，字段与头文件同名同序同宽；enum 以 `u8` 承载、取值 = uniffi 枚举声明序
//!   （单测逐表核对 `[i] as u8 == i` 与 `size_of` / `offset_of!` 对 clang `sizeof` / `offsetof` 读数）。
//! - 留痕形态 [`MADINFER_CABI_FORM`] = `napi-rust`（C 桩退役；鸿蒙 hilog `form=` 对齐）。
//!
//! # Safety（模块级说明）
//!
//! - 裸指针解引用只发生在判空之后；长度与有效期语义由头文件声明为宿主契约（§2 借用语义、§4 user_data 有效期）。
//! - `user_data` 是宿主承诺任意线程可用的不透明指针（§5 回调任意线程），故 [`HostFns`] / [`SinkFns`] `unsafe impl Send + Sync`。
//! - 回调期间核侧不持任何锁（回调里反调 `madinfer_stream_cancel` / `madinfer_host_reply` 合法，头文件 §5）。

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, PoisonError, RwLock};

use crate::{
    AppStateRecord, ApplyOutcomeRecord, CapabilityEntryRecord, CompleteRecord,
    ConsentDecisionRecord, ConsentRecord, CoreOptions, DeltaRecord, EventSink, FailedRecord,
    HostBridge, InferCore, InferFfiError, MemoryPressureRecord, MessageRecord, NetStateRecord,
    OptionsRecord, OsRecord, OsSignalRecord, OutputRecord, OutputSpecRecord, PrivacyRecord,
    QosRecord, RequestRecord, RoleRecord, StartedRecord, TelemetryBatchRecord, ThermalRecord,
    TierRecord, TrustedKeyRecord, INFER_ABI_VERSION,
};

// ---------------------------------------------------------------------------
// §1 版本、错误码、常量（madinfer.h §1 / §2 同源）
// ---------------------------------------------------------------------------

/// 成功（`MADINFER_OK`）。
pub const MADINFER_OK: i32 = 0;
/// 留痕形态串（头文件 `MADINFER_CABI_FORM` 同源）：C 桩退役后宿主 hilog `form=` 取本值；单测核对两处一致。
pub const MADINFER_CABI_FORM: &str = "napi-rust";
/// 产物内留痕（`strings libmadinfer_ffi.a | grep form=napi-rust`）；`#[used]` 保证进 .rodata 不被丢弃。
#[used]
pub static MADINFER_CABI_FORM_TAG: &[u8] = b"madinfer-cabi form=napi-rust abi=2\0";
/// 能力名上限（`MADINFER_CAPABILITY_MAX`）。
pub const MADINFER_CAPABILITY_MAX: usize = 64;
/// 核版本串上限（`MADINFER_VERSION_MAX`）。
pub const MADINFER_VERSION_MAX: usize = 32;
/// KvStore 值上限（`MADINFER_KV_VALUE_MAX`；核以该容量调用 `kv_get`）。
pub const MADINFER_KV_VALUE_MAX: u32 = 4096;
/// 停止串条数上限（`MADINFER_STOP_MAX`）。
pub const MADINFER_STOP_MAX: u32 = 8;
/// 宿主请求种类 CONSENT（`MADINFER_HOST_REQUEST_CONSENT`）。
pub const MADINFER_HOST_REQUEST_CONSENT: u8 = 0;

const INVALID_ARGUMENT: i32 = InferFfiError::InvalidArgument.code() as i32;
const PANICKED: i32 = InferFfiError::Panicked.code() as i32;

// ---------------------------------------------------------------------------
// §2 基础类型
// ---------------------------------------------------------------------------

/// `madinfer_str`：UTF-8 字节串（借用；不保证 NUL 结尾）。`len = 0` 时 `ptr` 可为 NULL。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawStr {
    /// 字节起点。
    pub ptr: *const u8,
    /// 字节长度。
    pub len: u32,
}

impl RawStr {
    /// 借用一段 Rust 串（回调实参形态；只在回调期间有效）。
    pub fn borrow(text: &str) -> Self {
        Self {
            ptr: text.as_ptr(),
            len: len_u32(text.len()),
        }
    }

    /// 入参 → 拥有的 `String`；空指针（len > 0）/ 非 UTF-8 → 4001。
    ///
    /// # Safety
    ///
    /// `len > 0` 时 `ptr` 必须指向 `len` 字节有效只读内存（头文件 §2 借用语义）。
    unsafe fn to_owned_string(self) -> Result<String, InferFfiError> {
        // SAFETY: 契约同本函数。
        let bytes = unsafe { slice_of(self.ptr, self.len)? };
        String::from_utf8(bytes.to_vec()).map_err(|_| InferFfiError::InvalidArgument)
    }
}

/// `struct madinfer_core`：不透明核站，C 侧只持指针（[`madinfer_core_create`] 发放、[`madinfer_core_destroy`] 回收）。
#[repr(C)]
pub struct RawCore {
    _private: [u8; 0],
}

// ---------------------------------------------------------------------------
// §3 过界 record（uniffi Record 的 C 形态；字段同名同序同宽，Option<T> → has_<field> + <field>）
// ---------------------------------------------------------------------------

/// `madinfer_trusted_key` ↔ [`TrustedKeyRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawTrustedKey {
    /// 签名文档里的 key_id。
    pub key_id: RawStr,
    /// 公钥 hex（64 字符）。
    pub public_key_hex: RawStr,
}

/// `madinfer_core_options` ↔ [`CoreOptions`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawCoreOptions {
    /// 观测实例标识。
    pub instance_name: RawStr,
    /// `madinfer_tier`。
    pub tier: u8,
    /// `madinfer_os`。
    pub os: u8,
    /// 合帧窗口毫秒（0 = 核缺省）。
    pub frame_ms: u32,
    /// 观测采样万分比。
    pub sampling_bp: u16,
    /// 注册能力词表（≥ 1 条）。
    pub capabilities: *const RawStr,
    /// 词表条数。
    pub capabilities_len: u32,
    /// Option<String> consent_endpoint 的 has_ 位。
    pub has_consent_endpoint: u8,
    /// 授权链路演示位端点。
    pub consent_endpoint: RawStr,
    /// 受信公钥表（F2 追加）。
    pub trusted_keys: *const RawTrustedKey,
    /// 受信公钥条数。
    pub trusted_keys_len: u32,
}

/// `madinfer_message` ↔ [`MessageRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawMessage {
    /// `madinfer_role`。
    pub role: u8,
    /// 内容（载荷）。
    pub content: RawStr,
}

/// `madinfer_options` ↔ [`OptionsRecord`]（has_ 为 0 = 用核缺省）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawOptions {
    /// has_ 位。
    pub has_temperature_x100: u8,
    /// temperature × 100。
    pub temperature_x100: u16,
    /// has_ 位。
    pub has_top_p_bp: u8,
    /// top_p 万分比。
    pub top_p_bp: u16,
    /// has_ 位。
    pub has_max_tokens: u8,
    /// 生成上限。
    pub max_tokens: u32,
    /// 停止串（≤ [`MADINFER_STOP_MAX`] 条）。
    pub stop: *const RawStr,
    /// 停止串条数。
    pub stop_len: u32,
    /// has_ 位。
    pub has_seed: u8,
    /// 随机种子。
    pub seed: u64,
}

/// `madinfer_request` ↔ [`RequestRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawRequest {
    /// 能力名。
    pub capability: RawStr,
    /// 业务 id（1～65535）。
    pub business: u16,
    /// `madinfer_privacy`。
    pub privacy: u8,
    /// `madinfer_qos`。
    pub qos: u8,
    /// 截止毫秒。
    pub deadline_ms: u32,
    /// 消息历史。
    pub messages: *const RawMessage,
    /// 消息条数。
    pub messages_len: u32,
    /// 采样参数。
    pub options: RawOptions,
    /// `madinfer_output_kind`（madai-w08 G4 追加）：0 Text / 1 Structured。
    pub output_kind: u8,
    /// Structured 时的 schema JSON 文本（载荷；Text 时忽略，可为空）。
    pub schema_json: RawStr,
    /// Option<String> context 的 has_ 位。
    pub has_context: u8,
    /// 生成上下文键（1～128 字节）。
    pub context: RawStr,
}

/// `madinfer_capability_entry` ↔ [`CapabilityEntryRecord`]（probe 出参，宿主提供存储）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawCapabilityEntry {
    /// 能力名字节（≤ 64）。
    pub capability: [u8; MADINFER_CAPABILITY_MAX],
    /// 能力名长度。
    pub capability_len: u32,
    /// `madinfer_cap_state`。
    pub state: u8,
    /// Option<SourceRecord> 的 has_ 位。
    pub has_source: u8,
    /// `madinfer_source`。
    pub source: u8,
    /// `madinfer_latency`。
    pub latency: u8,
    /// `madinfer_constraint_support`（madai-w08 G4 追加）。
    pub constraint: u8,
}

/// `madinfer_probe_snapshot` ↔ [`crate::ProbeRecord`]。宿主填 `entries` / `entries_capacity`，核回填其余。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawProbeSnapshot {
    /// ABI 版本。
    pub abi_version: u32,
    /// 核版本串字节（≤ 32）。
    pub core_version: [u8; MADINFER_VERSION_MAX],
    /// 核版本串长度。
    pub core_version_len: u32,
    /// `madinfer_tier`。
    pub tier: u8,
    /// 快照单调版本。
    pub snapshot_version: u64,
    /// 宿主提供的条目存储。
    pub entries: *mut RawCapabilityEntry,
    /// 存储容量（条）。
    pub entries_capacity: u32,
    /// 实际写入条数（≤ capacity）。
    pub entries_len: u32,
    /// 快照全部条数（> len 时宿主扩容重探；不算错误）。
    pub entries_total: u32,
}

/// `madinfer_started` ↔ [`StartedRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawStarted {
    /// 请求句柄。
    pub request: u64,
    /// `madinfer_source`。
    pub source: u8,
    /// Option<String> package 的 has_ 位。
    pub has_package: u8,
    /// 算法包（Bundled 时有）。
    pub package: RawStr,
    /// `madinfer_route_reason`。
    pub route_reason: u8,
    /// 排队毫秒。
    pub queued_ms: u32,
}

/// `madinfer_delta` ↔ [`DeltaRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawDelta {
    /// 请求句柄。
    pub request: u64,
    /// 帧序号（自 0 递增）。
    pub seq: u32,
    /// 文本（载荷）。
    pub text: RawStr,
    /// token 数。
    pub tokens: u32,
    /// Option<String> structured_patch 的 has_ 位（madai-w08 G4 追加；Structured 请求每帧 1）。
    pub has_structured_patch: u8,
    /// 这一帧的 RFC 6902 数组文本（可为 `[]`）。
    pub structured_patch: RawStr,
}

/// `madinfer_stats` ↔ [`crate::StatsRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawStats {
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
    /// Option<SourceRecord> 的 has_ 位。
    pub has_source: u8,
    /// `madinfer_source`。
    pub source: u8,
    /// 回退步数。
    pub fallback_steps: u8,
    /// 前缀缓存命中 0 / 1。
    pub prefix_hit: u8,
}

/// `madinfer_complete` ↔ [`CompleteRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawComplete {
    /// 请求句柄。
    pub request: u64,
    /// 完整文本（载荷；兼容字段——Structured 时与 `output` 同文）。
    pub text: RawStr,
    /// 统计。
    pub stats: RawStats,
    /// `madinfer_output_kind`（madai-w08 G4 追加）：0 Text / 1 Structured。
    pub output_kind: u8,
    /// 输出文本：Text 的全文 / Structured 的已过校验 JSON（载荷）。
    pub output: RawStr,
}

/// `madinfer_failed` ↔ [`FailedRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawFailed {
    /// 请求句柄。
    pub request: u64,
    /// 稳定错误码 1xxx～3xxx。
    pub code: u16,
    /// 协议字符串（全拼）。
    pub wire: RawStr,
    /// Option<u32> retry_after_ms 的 has_ 位。
    pub has_retry_after_ms: u8,
    /// 建议等待毫秒。
    pub retry_after_ms: u32,
    /// Option<String> partial_text 的 has_ 位。
    pub has_partial_text: u8,
    /// 已生成部分（载荷）。
    pub partial_text: RawStr,
}

/// `madinfer_telemetry_batch` ↔ [`TelemetryBatchRecord`]（宿主禁解析 payload）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawTelemetryBatch {
    /// telemetry.v1 schema 版本。
    pub schema_version: u32,
    /// 批内事件数。
    pub event_count: u32,
    /// 已序列化批次字节。
    pub payload: *const u8,
    /// 载荷长度。
    pub payload_len: u32,
}

/// `madinfer_consent` ↔ [`ConsentRecord`]。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawConsent {
    /// 能力名。
    pub capability: RawStr,
    /// `madinfer_privacy`。
    pub privacy: u8,
    /// 云端端点 id。
    pub endpoint_id: RawStr,
}

/// `madinfer_host_reply_record`：宿主请求回投，按 kind 取用对应成员。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawHostReply {
    /// `madinfer_host_request_kind`。
    pub kind: u8,
    /// kind = CONSENT：`madinfer_consent_decision`。
    pub consent_decision: u8,
}

/// `madinfer_os_signal` ↔ [`OsSignalRecord`]：kind 判别 + 扁平成员（F2 追加）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawOsSignal {
    /// `madinfer_os_signal_kind`。
    pub kind: u8,
    /// kind = THERMAL：`madinfer_thermal`。
    pub thermal_state: u8,
    /// kind = BATTERY：电量 0～100；[`MADINFER_BATTERY_LEVEL_UNKNOWN`] = 未知（101～254 → 4001）；未知进核即 None、不参与电量门控。
    pub battery_level_pct: u8,
    /// kind = BATTERY：充电中 0 / 1。
    pub battery_charging: u8,
    /// kind = BATTERY：低电量模式 0 / 1。
    pub battery_low_power_mode: u8,
    /// kind = MEMORY：`madinfer_memory_pressure`。
    pub memory_level: u8,
    /// kind = APP：`madinfer_app_state`。
    pub app_state: u8,
    /// kind = APP：Option<u32> budget_ms 的 has_ 位。
    pub has_app_budget_ms: u8,
    /// kind = APP：后台预算毫秒。
    pub app_budget_ms: u32,
    /// kind = NETWORK：`madinfer_net_state`。
    pub network_state: u8,
    /// kind = IDLE：0 / 1。
    pub idle: u8,
    /// kind = MAIN_LINK_BUSY：0 / 1。
    pub main_link_busy: u8,
}

/// `madinfer_os_signal.battery_level_pct` 的"未知"哨兵（`MADINFER_BATTERY_LEVEL_UNKNOWN`；madai-w08 G7）。
pub const MADINFER_BATTERY_LEVEL_UNKNOWN: u8 = 255;

/// 配置应用诊断文本上限（`MADINFER_APPLY_DETAIL_MAX`）。
pub const MADINFER_APPLY_DETAIL_MAX: usize = 128;

/// `madinfer_apply_outcome` ↔ [`ApplyOutcomeRecord`]：[`madinfer_apply_config`] 出参（madai-w08 I-C-07 追加）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawApplyOutcome {
    /// 已切换为活动配置 0 / 1。
    pub applied: u8,
    /// 调用后的活动配置版本。
    pub active_version: u64,
    /// `madinfer_apply_stage`。
    pub stage: u8,
    /// Option<ApplyRejectReasonRecord> 的 has_ 位。
    pub has_reject_reason: u8,
    /// `madinfer_apply_reject_reason`。
    pub reject_reason: u8,
    /// detail 有效字节数。
    pub detail_len: u32,
    /// 诊断文本 UTF-8 字节（不 NUL 结尾；超长在字符边界截断）。
    pub detail: [u8; MADINFER_APPLY_DETAIL_MAX],
}

impl RawApplyOutcome {
    fn of(record: &ApplyOutcomeRecord) -> Self {
        let mut detail = [0u8; MADINFER_APPLY_DETAIL_MAX];
        let mut cut = record.detail.len().min(MADINFER_APPLY_DETAIL_MAX);
        while !record.detail.is_char_boundary(cut) {
            cut -= 1;
        }
        detail[..cut].copy_from_slice(&record.detail.as_bytes()[..cut]);
        Self {
            applied: u8::from(record.applied),
            active_version: record.active_version,
            stage: record.stage as u8,
            has_reject_reason: u8::from(record.reject_reason.is_some()),
            reject_reason: record.reject_reason.map_or(0, |r| r as u8),
            detail_len: len_u32(cut),
            detail,
        }
    }
}

// ---------------------------------------------------------------------------
// §4 回调 vtable（函数指针 + user_data；宿主实现、核回调；Option<fn> = 可空函数指针）
// ---------------------------------------------------------------------------

/// `now_ms` 回调类型。
pub type NowMsFn = unsafe extern "C" fn(user_data: *mut c_void) -> u64;
/// `kv_get` 回调类型：返回 1 有值 / 0 无键；有值时 `*out_len` = 全长并写入 min(全长, capacity) 字节。
pub type KvGetFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    key: RawStr,
    out_value: *mut u8,
    capacity: u32,
    out_len: *mut u32,
) -> i32;
/// `kv_put` 回调类型（覆盖写）。
pub type KvPutFn =
    unsafe extern "C" fn(user_data: *mut c_void, key: RawStr, value: *const u8, len: u32);
/// `telemetry_send` 回调类型。
pub type TelemetrySendFn =
    unsafe extern "C" fn(user_data: *mut c_void, batch: *const RawTelemetryBatch);
/// `authorize` 回调类型（请求半边；宿主以同一 host_request 调 [`madinfer_host_reply`] 恰一次）。
pub type AuthorizeFn =
    unsafe extern "C" fn(user_data: *mut c_void, host_request: u64, context: *const RawConsent);
/// `verify_ed25519` 回调类型：1 验真 / 0 验假；任何异常回 0。
pub type VerifyEd25519Fn = unsafe extern "C" fn(
    user_data: *mut c_void,
    message: *const u8,
    message_len: u32,
    signature: *const u8,
    signature_len: u32,
    public_key: *const u8,
    public_key_len: u32,
) -> i32;
/// `on_worker_started` 回调类型（madai-w08 G7；`class` = `madinfer_worker_class`）：在核的该线程上调，宿主同步设优先级即返。
pub type OnWorkerStartedFn = unsafe extern "C" fn(user_data: *mut c_void, class: u8);
/// `on_started` 回调类型。
pub type OnStartedFn = unsafe extern "C" fn(user_data: *mut c_void, event: *const RawStarted);
/// `on_delta` 回调类型。
pub type OnDeltaFn = unsafe extern "C" fn(user_data: *mut c_void, event: *const RawDelta);
/// `on_complete` 回调类型。
pub type OnCompleteFn = unsafe extern "C" fn(user_data: *mut c_void, event: *const RawComplete);
/// `on_failed` 回调类型。
pub type OnFailedFn = unsafe extern "C" fn(user_data: *mut c_void, event: *const RawFailed);

/// `madinfer_host_vtable` ↔ [`HostBridge`]（madinfer-host 端口的 C 形态）。前六个函数指针任一为 NULL → create 回 4001；
/// `on_worker_started` 可为 NULL（= 不设线程优先级）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawHostVtable {
    /// 宿主不透明指针（有效期到 [`madinfer_core_destroy`] 返回）。
    pub user_data: *mut c_void,
    /// ↔ `HostBridge::now_ms`。
    pub now_ms: Option<NowMsFn>,
    /// ↔ `HostBridge::kv_get`。
    pub kv_get: Option<KvGetFn>,
    /// ↔ `HostBridge::kv_put`。
    pub kv_put: Option<KvPutFn>,
    /// ↔ `HostBridge::telemetry_send`。
    pub telemetry_send: Option<TelemetrySendFn>,
    /// ↔ `HostBridge::authorize`（请求半边）。
    pub authorize: Option<AuthorizeFn>,
    /// ↔ `HostBridge::verify_ed25519`（F2 追加）。
    pub verify_ed25519: Option<VerifyEd25519Fn>,
    /// ↔ `HostBridge::on_worker_started`（madai-w08 G7 追加；可为 NULL）。
    pub on_worker_started: Option<OnWorkerStartedFn>,
}

/// `madinfer_sink_vtable` ↔ [`EventSink`]。任一函数指针为 NULL → stream_start 回 4001。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RawSinkVtable {
    /// 宿主不透明指针（有效期到该请求的终态回调返回）。
    pub user_data: *mut c_void,
    /// ↔ `EventSink::on_started`。
    pub on_started: Option<OnStartedFn>,
    /// ↔ `EventSink::on_delta`。
    pub on_delta: Option<OnDeltaFn>,
    /// ↔ `EventSink::on_complete`。
    pub on_complete: Option<OnCompleteFn>,
    /// ↔ `EventSink::on_failed`。
    pub on_failed: Option<OnFailedFn>,
}

/// 校验后的 host vtable 拷贝（函数指针全部非空）。
#[derive(Clone, Copy)]
struct HostFns {
    user_data: *mut c_void,
    now_ms: NowMsFn,
    kv_get: KvGetFn,
    kv_put: KvPutFn,
    telemetry_send: TelemetrySendFn,
    authorize: AuthorizeFn,
    verify_ed25519: VerifyEd25519Fn,
    /// 可选（NULL = 不设优先级）。
    on_worker_started: Option<OnWorkerStartedFn>,
}

// SAFETY: `user_data` 是宿主承诺可在任意线程回传的不透明指针（头文件 §5 回调任意线程 / §4 有效期到 destroy 返回）；
// 函数指针本身无线程亲和。核只把它原样回传，不解引用。
unsafe impl Send for HostFns {}
// SAFETY: 同上；多线程并发回调是头文件契约（宿主实现只入队）。
unsafe impl Sync for HostFns {}

impl RawHostVtable {
    fn checked(&self) -> Result<HostFns, InferFfiError> {
        Ok(HostFns {
            user_data: self.user_data,
            now_ms: self.now_ms.ok_or(InferFfiError::InvalidArgument)?,
            kv_get: self.kv_get.ok_or(InferFfiError::InvalidArgument)?,
            kv_put: self.kv_put.ok_or(InferFfiError::InvalidArgument)?,
            telemetry_send: self.telemetry_send.ok_or(InferFfiError::InvalidArgument)?,
            authorize: self.authorize.ok_or(InferFfiError::InvalidArgument)?,
            verify_ed25519: self.verify_ed25519.ok_or(InferFfiError::InvalidArgument)?,
            on_worker_started: self.on_worker_started,
        })
    }
}

/// 校验后的 sink vtable 拷贝（函数指针全部非空）。
#[derive(Clone, Copy)]
struct SinkFns {
    user_data: *mut c_void,
    on_started: OnStartedFn,
    on_delta: OnDeltaFn,
    on_complete: OnCompleteFn,
    on_failed: OnFailedFn,
}

// SAFETY: 同 HostFns——sink 的 user_data 由宿主保证到该请求终态回调返回前有效、任意线程可回传。
unsafe impl Send for SinkFns {}
// SAFETY: 同上。
unsafe impl Sync for SinkFns {}

impl RawSinkVtable {
    fn checked(&self) -> Result<SinkFns, InferFfiError> {
        Ok(SinkFns {
            user_data: self.user_data,
            on_started: self.on_started.ok_or(InferFfiError::InvalidArgument)?,
            on_delta: self.on_delta.ok_or(InferFfiError::InvalidArgument)?,
            on_complete: self.on_complete.ok_or(InferFfiError::InvalidArgument)?,
            on_failed: self.on_failed.ok_or(InferFfiError::InvalidArgument)?,
        })
    }
}

// ---------------------------------------------------------------------------
// 枚举取值表（取值 = uniffi 枚举声明序；单测核对 `[i] as u8 == i`）
// ---------------------------------------------------------------------------

const TIERS: [TierRecord; 4] = [
    TierRecord::Low,
    TierRecord::Mid,
    TierRecord::Flagship,
    TierRecord::Unknown,
];
const OSES: [OsRecord; 4] = [
    OsRecord::Ios,
    OsRecord::Android,
    OsRecord::Ohos,
    OsRecord::Jvm,
];
const PRIVACIES: [PrivacyRecord; 4] = [
    PrivacyRecord::Public,
    PrivacyRecord::Internal,
    PrivacyRecord::E2eePlaintext,
    PrivacyRecord::Regulated,
];
const QOS: [QosRecord; 3] = [
    QosRecord::Interactive,
    QosRecord::Async,
    QosRecord::Background,
];
const ROLES: [RoleRecord; 4] = [
    RoleRecord::System,
    RoleRecord::User,
    RoleRecord::Assistant,
    RoleRecord::Tool,
];
const CONSENT_DECISIONS: [ConsentDecisionRecord; 3] = [
    ConsentDecisionRecord::Allow,
    ConsentDecisionRecord::AllowRemember,
    ConsentDecisionRecord::Deny,
];
const THERMALS: [ThermalRecord; 4] = [
    ThermalRecord::Nominal,
    ThermalRecord::Fair,
    ThermalRecord::Serious,
    ThermalRecord::Critical,
];
const MEMORY_PRESSURES: [MemoryPressureRecord; 3] = [
    MemoryPressureRecord::Normal,
    MemoryPressureRecord::Warning,
    MemoryPressureRecord::Critical,
];
const NET_STATES: [NetStateRecord; 3] = [
    NetStateRecord::Offline,
    NetStateRecord::Metered,
    NetStateRecord::Unmetered,
];
const APP_STATES: [AppStateRecord; 4] = [
    AppStateRecord::Foreground,
    AppStateRecord::Background,
    AppStateRecord::BackgroundTask,
    AppStateRecord::Terminating,
];

fn pick<T: Copy>(table: &[T], value: u8) -> Result<T, InferFfiError> {
    table
        .get(usize::from(value))
        .copied()
        .ok_or(InferFfiError::InvalidArgument)
}

fn len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// 入参数组 → 借用切片；`len = 0` 时 ptr 可为 NULL，`len > 0` 且 ptr 为 NULL → 4001。
///
/// # Safety
///
/// `len > 0` 时 `ptr` 必须指向 `len` 个有效、对齐的 `T`，并在返回切片的使用期内保持有效（头文件 §2 借用语义）。
unsafe fn slice_of<'a, T>(ptr: *const T, len: u32) -> Result<&'a [T], InferFfiError> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(InferFfiError::InvalidArgument);
    }
    let len = usize::try_from(len).map_err(|_| InferFfiError::InvalidArgument)?;
    // SAFETY: 已判非空；元素个数与有效期由调用方（宿主）契约保证。
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// 写出错码（`out_error_code` 可为 NULL）。
///
/// # Safety
///
/// 非空时 `out` 必须指向可写、对齐的 `int32_t`。
unsafe fn set_error(out: *mut i32, code: i32) {
    if !out.is_null() {
        // SAFETY: 已判非空；可写性由调用方保证。
        unsafe { *out = code };
    }
}

/// panic 防火墙：入口整体 `catch_unwind`，panic 时给回退值（4002）。
fn guarded<T>(fallback: impl FnOnce() -> T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(_) => fallback(),
    }
}

/// 防火墙自检（Rust 侧可见、**不是** C 符号——符号面恒八个）：在 [`guarded`] 内注入 panic，恒返回 4002 且进程存活；
/// 对齐 uniffi 面 [`crate::test_panic`] 的用途，供 tests/cabi.rs 断言 C 面入口的收口机制。
pub fn panic_firewall_probe() -> i32 {
    guarded(
        || PANICKED,
        || panic!("madinfer-ffi cabi 注入 panic（收口用例）"),
    )
}

// ---------------------------------------------------------------------------
// record 转换（入向：C → uniffi record；出向：uniffi record → C）
// ---------------------------------------------------------------------------

/// # Safety
///
/// `raw` 内全部指针 / 长度对满足头文件 §2 借用语义。
unsafe fn core_options_of(raw: &RawCoreOptions) -> Result<CoreOptions, InferFfiError> {
    let tier = pick(&TIERS, raw.tier)?;
    let os = pick(&OSES, raw.os)?;
    // SAFETY: 契约同本函数。
    let capabilities = unsafe { slice_of(raw.capabilities, raw.capabilities_len)? }
        .iter()
        // SAFETY: 同上。
        .map(|name| unsafe { name.to_owned_string() })
        .collect::<Result<Vec<_>, _>>()?;
    let consent_endpoint = if raw.has_consent_endpoint != 0 {
        // SAFETY: 同上。
        Some(unsafe { raw.consent_endpoint.to_owned_string()? })
    } else {
        None
    };
    // SAFETY: 同上。
    let trusted_keys = unsafe { slice_of(raw.trusted_keys, raw.trusted_keys_len)? }
        .iter()
        .map(|key| {
            Ok(TrustedKeyRecord {
                // SAFETY: 同上。
                key_id: unsafe { key.key_id.to_owned_string()? },
                // SAFETY: 同上。
                public_key_hex: unsafe { key.public_key_hex.to_owned_string()? },
            })
        })
        .collect::<Result<Vec<_>, InferFfiError>>()?;
    Ok(CoreOptions {
        // SAFETY: 同上。
        instance_name: unsafe { raw.instance_name.to_owned_string()? },
        tier,
        os,
        frame_ms: raw.frame_ms,
        sampling_bp: raw.sampling_bp,
        capabilities,
        consent_endpoint,
        trusted_keys,
        // C 面固定仿真核（madai-w17 I-C-16：backend / model_path 只在 uniffi 面；RawCoreOptions 不动，ABI 仍 2）。
        backend: crate::BackendKindRecord::Emulated,
        model_path: None,
    })
}

/// # Safety
///
/// `raw` 内全部指针 / 长度对满足头文件 §2 借用语义。
unsafe fn request_of(raw: &RawRequest) -> Result<RequestRecord, InferFfiError> {
    let privacy = pick(&PRIVACIES, raw.privacy)?;
    let qos = pick(&QOS, raw.qos)?;
    // SAFETY: 契约同本函数。
    let messages = unsafe { slice_of(raw.messages, raw.messages_len)? }
        .iter()
        .map(|message| {
            Ok(MessageRecord {
                role: pick(&ROLES, message.role)?,
                // SAFETY: 同上。
                content: unsafe { message.content.to_owned_string()? },
            })
        })
        .collect::<Result<Vec<_>, InferFfiError>>()?;
    let options = &raw.options;
    // 条数上限先于切片构造（越界的 stop_len 是宿主契约错误，先以 4001 拒绝而不触碰内存）。
    if options.stop_len > MADINFER_STOP_MAX {
        return Err(InferFfiError::InvalidArgument);
    }
    // SAFETY: 同上。
    let stop = unsafe { slice_of(options.stop, options.stop_len)? }
        .iter()
        // SAFETY: 同上。
        .map(|text| unsafe { text.to_owned_string() })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RequestRecord {
        // SAFETY: 同上。
        capability: unsafe { raw.capability.to_owned_string()? },
        business: raw.business,
        privacy,
        qos,
        deadline_ms: raw.deadline_ms,
        messages,
        options: OptionsRecord {
            temperature_x100: (options.has_temperature_x100 != 0)
                .then_some(options.temperature_x100),
            top_p_bp: (options.has_top_p_bp != 0).then_some(options.top_p_bp),
            max_tokens: (options.has_max_tokens != 0).then_some(options.max_tokens),
            stop,
            seed: (options.has_seed != 0).then_some(options.seed),
        },
        output: match raw.output_kind {
            0 => OutputSpecRecord::Text,
            1 => OutputSpecRecord::Structured {
                // SAFETY: 同上。
                schema_json: unsafe { raw.schema_json.to_owned_string()? },
            },
            _ => return Err(InferFfiError::InvalidArgument),
        },
        context: if raw.has_context != 0 {
            // SAFETY: 同上。
            Some(unsafe { raw.context.to_owned_string()? })
        } else {
            None
        },
    })
}

fn os_signal_of(raw: &RawOsSignal) -> Result<OsSignalRecord, InferFfiError> {
    Ok(match raw.kind {
        0 => OsSignalRecord::Thermal {
            state: pick(&THERMALS, raw.thermal_state)?,
        },
        1 => OsSignalRecord::Battery {
            level_pct: match raw.battery_level_pct {
                MADINFER_BATTERY_LEVEL_UNKNOWN => None,
                pct @ 0..=100 => Some(pct),
                _ => return Err(InferFfiError::InvalidArgument),
            },
            charging: raw.battery_charging != 0,
            low_power_mode: raw.battery_low_power_mode != 0,
        },
        2 => OsSignalRecord::Memory {
            level: pick(&MEMORY_PRESSURES, raw.memory_level)?,
        },
        3 => OsSignalRecord::App {
            state: pick(&APP_STATES, raw.app_state)?,
            budget_ms: (raw.has_app_budget_ms != 0).then_some(raw.app_budget_ms),
        },
        4 => OsSignalRecord::Network {
            state: pick(&NET_STATES, raw.network_state)?,
        },
        5 => OsSignalRecord::Idle {
            idle: raw.idle != 0,
        },
        6 => OsSignalRecord::MainLinkBusy {
            busy: raw.main_link_busy != 0,
        },
        _ => return Err(InferFfiError::InvalidArgument),
    })
}

impl RawCapabilityEntry {
    fn of(entry: &CapabilityEntryRecord) -> Self {
        let bytes = entry.capability.as_bytes();
        let n = bytes.len().min(MADINFER_CAPABILITY_MAX);
        let mut capability = [0u8; MADINFER_CAPABILITY_MAX];
        capability[..n].copy_from_slice(&bytes[..n]);
        Self {
            capability,
            capability_len: len_u32(n),
            state: entry.state as u8,
            has_source: u8::from(entry.source.is_some()),
            source: entry.source.map_or(0, |source| source as u8),
            latency: entry.latency as u8,
            constraint: entry.constraint as u8,
        }
    }
}

// ---------------------------------------------------------------------------
// 宿主 vtable → HostBridge（authorize 请求 / 回投对在专用线程上合成同步返回值）
// ---------------------------------------------------------------------------

/// vtable 宿主：回调只回传 user_data；authorize 的等待面（回投口登记表 + 存活票）也在这里。
struct VtableHost {
    fns: HostFns,
    next_host_request: AtomicU64,
    /// FFI 出入口登记表（clippy.toml 豁免协议第一类"FFI 出入口"；不是核状态）：host_request → 等待中授权线程的回投口。
    pending: RwLock<HashMap<u64, Sender<ConsentDecisionRecord>>>,
    /// 存活票：`Some` 时授权线程可克隆一张再调宿主回调；destroy 取走后新宿主回调直接拒绝，并在票据接收端等已发出的回调返回。
    alive: RwLock<Option<Sender<()>>>,
}

impl VtableHost {
    // A ticket holds destroy until the callback returns. Taking the ticket and
    // closing the gate are serialized; no host pointer is touched after closure.
    fn ticket(&self) -> Option<Sender<()>> {
        self.alive
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn reply(&self, host_request: u64, decision: ConsentDecisionRecord) -> bool {
        let waiter = self
            .pending
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&host_request);
        match waiter {
            Some(tx) => {
                // 等待线程已被 destroy 收口时 send 落空：配对本身成立，回 0。
                let _ = tx.send(decision);
                true
            }
            None => false,
        }
    }

    /// destroy 第二步：封新授权 → 等在途宿主回调（包括超时验签）返回（票据全部 Drop 使 recv 返回 Err，不轮询）→ 未回投的回投口失效。
    fn close(&self, alive_rx: &Receiver<()>) {
        let own = self
            .alive
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(own);
        while alive_rx.recv().is_ok() {}
        self.pending
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// [`HostBridge`] 实装：`Box<dyn HostBridge>` 进 [`InferCore::create`]，与核站共享同一个 [`VtableHost`]。
struct SharedHost(Arc<VtableHost>);

impl HostBridge for SharedHost {
    fn now_ms(&self) -> u64 {
        let Some(_ticket) = self.0.ticket() else {
            return 0;
        };
        let fns = &self.0.fns;
        // SAFETY: 函数指针建站时校验非空；user_data 有效期到 destroy 返回（此刻核站存活）。
        unsafe { (fns.now_ms)(fns.user_data) }
    }

    fn kv_get(&self, key: String) -> Option<Vec<u8>> {
        let _ticket = self.0.ticket()?;
        let fns = &self.0.fns;
        let mut value = vec![0u8; MADINFER_KV_VALUE_MAX as usize];
        let mut full_len: u32 = 0;
        // SAFETY: 缓冲 MADINFER_KV_VALUE_MAX 字节由本函数持有；key 借用到回调返回；out_len 指向本栈变量。
        let found = unsafe {
            (fns.kv_get)(
                fns.user_data,
                RawStr::borrow(&key),
                value.as_mut_ptr(),
                MADINFER_KV_VALUE_MAX,
                &mut full_len,
            )
        };
        if found == 0 {
            return None;
        }
        // 容量协议：*out_len 为全长，写入了 min(全长, capacity)；超 4 KB 的值按契约截断。
        value.truncate(full_len.min(MADINFER_KV_VALUE_MAX) as usize);
        Some(value)
    }

    fn kv_put(&self, key: String, value: Vec<u8>) {
        let Some(_ticket) = self.0.ticket() else {
            return;
        };
        let fns = &self.0.fns;
        // SAFETY: key / value 借用到回调返回；len 与缓冲一致。
        unsafe {
            (fns.kv_put)(
                fns.user_data,
                RawStr::borrow(&key),
                value.as_ptr(),
                len_u32(value.len()),
            )
        }
    }

    fn telemetry_send(&self, batch: TelemetryBatchRecord) {
        let Some(_ticket) = self.0.ticket() else {
            return;
        };
        let fns = &self.0.fns;
        let raw = RawTelemetryBatch {
            schema_version: batch.schema_version,
            event_count: batch.event_count,
            payload: batch.payload.as_ptr(),
            payload_len: len_u32(batch.payload.len()),
        };
        // SAFETY: raw 与 payload 借用到回调返回。
        unsafe { (fns.telemetry_send)(fns.user_data, &raw) }
    }

    fn authorize(&self, context: ConsentRecord) -> ConsentDecisionRecord {
        // 本函数在 lib.rs BridgeAuthorizer 的专用线程 madinfer-ffi-consent 上被调（允许阻塞）。
        let host = &self.0;
        let Some(ticket) = host
            .alive
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            // destroy 已封门：不再打扰宿主，保守拒绝（回执落在已关停的核上被丢弃）。
            return ConsentDecisionRecord::Deny;
        };
        let host_request = host.next_host_request.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        host.pending
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(host_request, tx);
        let raw = RawConsent {
            capability: RawStr::borrow(&context.capability),
            privacy: context.privacy as u8,
            endpoint_id: RawStr::borrow(&context.endpoint_id),
        };
        // SAFETY: raw 借用到回调返回；持存活票期间 destroy 不会返回，user_data 有效。回调期间不持任何锁
        // （宿主在回调里同步反调 madinfer_host_reply 合法：回投口已登记）。
        unsafe { (host.fns.authorize)(host.fns.user_data, host_request, &raw) };
        drop(ticket);
        // 等回投：Ok = 宿主回投恰一次；Err = destroy 丢弃了回投口（核已按 3008 收口，此处决议不再有业务效果）。
        rx.recv().unwrap_or(ConsentDecisionRecord::Deny)
    }

    fn verify_ed25519(&self, message: Vec<u8>, signature: Vec<u8>, public_key: Vec<u8>) -> bool {
        let Some(_ticket) = self.0.ticket() else {
            return false;
        };
        let fns = &self.0.fns;
        let (Ok(message_len), Ok(signature_len), Ok(public_key_len)) = (
            u32::try_from(message.len()),
            u32::try_from(signature.len()),
            u32::try_from(public_key.len()),
        ) else {
            return false;
        };
        // SAFETY: 三段缓冲借用到回调返回；长度与缓冲一致。
        let verdict = unsafe {
            (fns.verify_ed25519)(
                fns.user_data,
                message.as_ptr(),
                message_len,
                signature.as_ptr(),
                signature_len,
                public_key.as_ptr(),
                public_key_len,
            )
        };
        verdict == 1
    }

    fn on_worker_started(&self, class: crate::WorkerClassRecord) {
        let Some(_ticket) = self.0.ticket() else {
            return;
        };
        if let Some(hook) = self.0.fns.on_worker_started {
            // SAFETY: user_data 有效期到 destroy 返回；本回调在核线程启动时同步调用，destroy 前线程池尚存活。
            unsafe { hook(self.0.fns.user_data, class as u8) }
        }
    }
}

// ---------------------------------------------------------------------------
// sink vtable → EventSink（record → C 结构体，借用到回调返回；panic 收口在 lib.rs SinkAdapter 投递点）
// ---------------------------------------------------------------------------

struct VtableSink(SinkFns);

impl EventSink for VtableSink {
    fn on_started(&self, event: StartedRecord) {
        let package = event.package.as_deref();
        let raw = RawStarted {
            request: event.request,
            source: event.source as u8,
            has_package: u8::from(package.is_some()),
            package: RawStr::borrow(package.unwrap_or("")),
            route_reason: event.route_reason as u8,
            queued_ms: event.queued_ms,
        };
        // SAFETY: raw 及其借用的串活到回调返回；user_data 有效期到本请求终态回调返回。
        unsafe { (self.0.on_started)(self.0.user_data, &raw) }
    }

    fn on_delta(&self, event: DeltaRecord) {
        let patch = event.structured_patch.as_deref();
        let raw = RawDelta {
            request: event.request,
            seq: event.seq,
            text: RawStr::borrow(&event.text),
            tokens: event.tokens,
            has_structured_patch: u8::from(patch.is_some()),
            structured_patch: RawStr::borrow(patch.unwrap_or("")),
        };
        // SAFETY: 同 on_started。
        unsafe { (self.0.on_delta)(self.0.user_data, &raw) }
    }

    fn on_complete(&self, event: CompleteRecord) {
        let stats = &event.stats;
        let (output_kind, output) = match &event.output {
            OutputRecord::Text { text } => (0u8, text.as_str()),
            OutputRecord::Structured { json } => (1u8, json.as_str()),
        };
        let raw = RawComplete {
            request: event.request,
            text: RawStr::borrow(&event.text),
            output_kind,
            output: RawStr::borrow(output),
            stats: RawStats {
                ttft_ms: stats.ttft_ms,
                decode_ms: stats.decode_ms,
                prompt_tokens: stats.prompt_tokens,
                gen_tokens: stats.gen_tokens,
                tps_x100: stats.tps_x100,
                peak_memory_mb: stats.peak_memory_mb,
                has_source: u8::from(stats.source.is_some()),
                source: stats.source.map_or(0, |source| source as u8),
                fallback_steps: stats.fallback_steps,
                prefix_hit: u8::from(stats.prefix_hit),
            },
        };
        // SAFETY: 同 on_started。
        unsafe { (self.0.on_complete)(self.0.user_data, &raw) }
    }

    fn on_failed(&self, event: FailedRecord) {
        let partial = event.partial_text.as_deref();
        let raw = RawFailed {
            request: event.request,
            code: event.code,
            wire: RawStr::borrow(&event.wire),
            has_retry_after_ms: u8::from(event.retry_after_ms.is_some()),
            retry_after_ms: event.retry_after_ms.unwrap_or(0),
            has_partial_text: u8::from(partial.is_some()),
            partial_text: RawStr::borrow(partial.unwrap_or("")),
        };
        // SAFETY: 同 on_started。
        unsafe { (self.0.on_failed)(self.0.user_data, &raw) }
    }
}

// ---------------------------------------------------------------------------
// 核站（句柄背后的 Rust 对象）
// ---------------------------------------------------------------------------

struct Station {
    core: Arc<InferCore>,
    host: Arc<VtableHost>,
    /// 存活票接收端：destroy 取走 `host.alive` 后在此等全部票据 Drop（在途宿主回调（包括超时验签）返回）。
    alive_rx: Receiver<()>,
}

/// # Safety
///
/// `core` 必须是 [`madinfer_core_create`] 发放且尚未 destroy 的句柄（宿主契约：destroy 不与其余入口并发）。
unsafe fn station<'a>(core: *mut RawCore) -> Option<&'a Station> {
    if core.is_null() {
        return None;
    }
    // SAFETY: 已判非空；句柄有效性由调用方契约保证。
    Some(unsafe { &*core.cast::<Station>() })
}

// ---------------------------------------------------------------------------
// §6 十符号（W07 八符号 + W08 第 9 符号 madinfer_apply_config + W14 第 10 符号 madinfer_drop_contexts）
// ---------------------------------------------------------------------------

/// 1 ABI 版本握手（= `MADINFER_ABI_VERSION`；宿主链接 / dlsym 后第一个调用）。
#[no_mangle]
pub extern "C" fn madinfer_abi_version() -> u32 {
    INFER_ABI_VERSION
}

/// 2 建站：注入宿主 vtable + 建站参数。成功返回非 NULL 句柄；失败返回 NULL 并把 4xxx 写入 `*out_error_code`（可为 NULL）。
///
/// vtable 按值拷走；建站期即经 `kv_get` / `kv_put` 读写 install 标记键，host vtable 从本调用起必须可用。
///
/// # Safety
///
/// - `host` / `options` 为 NULL 时回 4001；非空时必须指向有效结构体，其内指针 / 长度对满足头文件 §2 借用语义；
/// - `out_error_code` 为 NULL 或指向可写 `int32_t`；
/// - `host->user_data` 有效期到 [`madinfer_core_destroy`] 返回，且可在任意线程回传。
#[no_mangle]
pub unsafe extern "C" fn madinfer_core_create(
    host: *const RawHostVtable,
    options: *const RawCoreOptions,
    out_error_code: *mut i32,
) -> *mut RawCore {
    // SAFETY: 契约同本函数。
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
        create_station(host, options, InferCore::create)
    }));
    let (handle, code) = match outcome {
        Ok(Ok(handle)) => (handle, MADINFER_OK),
        Ok(Err(error)) => (ptr::null_mut(), i32::from(error.code())),
        Err(_) => (ptr::null_mut(), PANICKED),
    };
    // SAFETY: out_error_code 为 NULL 或可写 int32_t（函数级 Safety）。
    unsafe { set_error(out_error_code, code) };
    handle
}

/// Explicit backend selection for C hosts: 0 = test emulation, 1 = native llama.
/// Native selection fails with FeatureDisabled when llama-sys is absent, never falls back.
/// # Safety
/// Same pointer/lifetime contract as `madinfer_core_create`; model_path is borrowed UTF-8.
#[no_mangle]
pub unsafe extern "C" fn madinfer_core_create_with_backend(
    host: *const RawHostVtable,
    options: *const RawCoreOptions,
    backend: u32,
    model_path: RawStr,
    out_error_code: *mut i32,
) -> *mut RawCore {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let kind = match backend {
            0 => crate::BackendKindRecord::Emulated,
            1 => crate::BackendKindRecord::Llama,
            _ => return Err(InferFfiError::InvalidArgument),
        };
        // SAFETY: caller provides a valid borrowed UTF-8 slice.
        let path = unsafe { model_path.to_owned_string()? };
        // SAFETY: same contract as core_create; all values copied before return.
        unsafe {
            create_station(host, options, |host, mut options| {
                options.backend = kind;
                options.model_path = if path.is_empty() { None } else { Some(path) };
                InferCore::create(host, options)
            })
        }
    }));
    let (handle, code) = match result {
        Ok(Ok(handle)) => (handle, MADINFER_OK),
        Ok(Err(error)) => (ptr::null_mut(), i32::from(error.code())),
        Err(_) => (ptr::null_mut(), PANICKED),
    };
    // SAFETY: caller provides writable output or null.
    unsafe { set_error(out_error_code, code) };
    handle
}

/// Query actual backend: 0 = emulated (not AI inference), 1 = native llama.
/// # Safety
/// Core must be live and output must be writable; neither may be null.
#[no_mangle]
pub unsafe extern "C" fn madinfer_backend_kind(core: *mut RawCore, out_kind: *mut u32) -> i32 {
    guarded(
        || PANICKED,
        || {
            if out_kind.is_null() {
                return INVALID_ARGUMENT;
            }
            // SAFETY: caller guarantees handle lifetime.
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            let kind = match station.core.probe().backend {
                crate::BackendKindRecord::Emulated => 0,
                crate::BackendKindRecord::Llama => 1,
            };
            // SAFETY: non-null caller-owned writable output.
            unsafe {
                *out_kind = kind;
            }
            MADINFER_OK
        },
    )
}

/// 建站主体（madai-w10 I-C-09 抽出，生产 [`madinfer_core_create`] 与调试 [`debug::madinfer_core_create_debug`] 共用）：
/// 判空 → vtable 全量校验 → options 转 record → 起 [`VtableHost`] → `build` 建核 → 装 [`Station`] 发句柄。
///
/// # Safety
///
/// 契约同 [`madinfer_core_create`]（`host` / `options` 为 NULL 时回 4001，非空时指向有效结构体）。
unsafe fn create_station(
    host: *const RawHostVtable,
    options: *const RawCoreOptions,
    build: impl FnOnce(Box<dyn HostBridge>, CoreOptions) -> Result<Arc<InferCore>, InferFfiError>,
) -> Result<*mut RawCore, InferFfiError> {
    if host.is_null() || options.is_null() {
        return Err(InferFfiError::InvalidArgument);
    }
    // SAFETY: 已判非空；调用期间有效由宿主契约保证（vtable 与 options 都按值读走）。
    let (vtable, raw_options) = unsafe { (*host, *options) };
    let fns = vtable.checked()?;
    // SAFETY: raw_options 内的指针 / 长度对满足 §2 借用语义（宿主契约）。
    let record = unsafe { core_options_of(&raw_options)? };
    let (alive_tx, alive_rx) = mpsc::channel();
    let shared = Arc::new(VtableHost {
        fns,
        next_host_request: AtomicU64::new(0),
        pending: RwLock::new(HashMap::new()),
        alive: RwLock::new(Some(alive_tx)),
    });
    let core = build(Box::new(SharedHost(Arc::clone(&shared))), record)?;
    let station = Box::new(Station {
        core,
        host: shared,
        alive_rx,
    });
    Ok(Box::into_raw(station).cast::<RawCore>())
}

/// 3 关停：发 Shutdown 并 join；在飞请求（含授权中的）以 `on_failed(3008)` 收口、观测排水刷出；再等在途 authorize
/// 回调返回并使未回投的 host_request 失效；返回后不再有任何回调。NULL 幂等。
///
/// # Safety
///
/// `core` 为 NULL 或由 [`madinfer_core_create`] 发放且未回收的句柄；本调用后句柄失效，宿主不得再传入任何入口。
#[no_mangle]
pub unsafe extern "C" fn madinfer_core_destroy(core: *mut RawCore) {
    if core.is_null() {
        return;
    }
    // SAFETY: 已判非空；句柄由 create 的 Box::into_raw 发放且宿主保证恰回收一次、不与其余入口并发。
    let station = unsafe { Box::from_raw(core.cast::<Station>()) };
    let Station {
        core,
        host,
        alive_rx,
    } = *station;
    // ① 关核：唯一持有者 Drop → CoreHandle Drop → Shutdown + join（在飞请求 Failed(3008)，观测批次刷到宿主）。
    let _ = catch_unwind(AssertUnwindSafe(move || drop(core)));
    // ② 封新授权、等在途宿主回调（包括超时验签）返回、丢弃未回投的回投口（等待线程以 Deny 收口，回执落空）。
    //    独立收口：即使 ① panic，等待中的授权线程也必须被释放。
    let _ = catch_unwind(AssertUnwindSafe(|| host.close(&alive_rx)));
}

/// 4 探测：按头文件 §3 `madinfer_probe_snapshot` 约定回填；`core` / `out_buf` 为 NULL 回 4001。
///
/// 容量协议：宿主填 `entries` / `entries_capacity`，核写入 min(total, capacity) 条并回填 `entries_len` / `entries_total`；
/// total > len 时宿主扩容重探，不算错误（`entries_capacity > 0` 而 `entries` 为 NULL 回 4001）。
///
/// # Safety
///
/// `out_buf` 为 NULL 或指向可写结构体；其 `entries` 必须指向 `entries_capacity` 个可写、对齐的条目。
#[no_mangle]
pub unsafe extern "C" fn madinfer_probe(core: *mut RawCore, out_buf: *mut RawProbeSnapshot) -> i32 {
    guarded(
        || PANICKED,
        || {
            // SAFETY: 句柄有效性由宿主契约保证（函数级 Safety）。
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            if out_buf.is_null() {
                return INVALID_ARGUMENT;
            }
            // SAFETY: 已判非空；可写性由宿主契约保证。
            let out = unsafe { &mut *out_buf };
            if out.entries_capacity > 0 && out.entries.is_null() {
                return INVALID_ARGUMENT;
            }
            let probe = station.core.probe();
            out.abi_version = probe.abi_version;
            let version = probe.core_version.as_bytes();
            let version_len = version.len().min(MADINFER_VERSION_MAX);
            out.core_version = [0u8; MADINFER_VERSION_MAX];
            out.core_version[..version_len].copy_from_slice(&version[..version_len]);
            out.core_version_len = len_u32(version_len);
            out.tier = probe.tier as u8;
            out.snapshot_version = probe.snapshot_version;
            out.entries_total = len_u32(probe.entries.len());
            let capacity = usize::try_from(out.entries_capacity).unwrap_or(usize::MAX);
            let writable = probe.entries.len().min(capacity);
            if writable > 0 {
                // SAFETY: entries 非空且宿主保证 entries_capacity（≥ writable）个可写条目。
                let slots = unsafe { std::slice::from_raw_parts_mut(out.entries, writable) };
                for (slot, entry) in slots.iter_mut().zip(&probe.entries) {
                    *slot = RawCapabilityEntry::of(entry);
                }
            }
            out.entries_len = len_u32(writable);
            MADINFER_OK
        },
    )
}

/// 5 流式生成：返回请求句柄（> 0），事件与取消都以它为锚。失败返回 0 并把 4xxx 写入 `*out_error_code`（可为 NULL）；
/// 形式非法（4001）在 Started 前拒绝，sink 不会收到任何回调。sink vtable 按值拷走。
///
/// # Safety
///
/// - `request` / `sink` 为 NULL 时回 4001；非空时必须指向有效结构体，其内指针 / 长度对满足头文件 §2 借用语义；
/// - `sink->user_data` 有效期到该请求的终态回调返回，且可在任意线程回传；
/// - `out_error_code` 为 NULL 或指向可写 `int32_t`。
#[no_mangle]
pub unsafe extern "C" fn madinfer_stream_start(
    core: *mut RawCore,
    request: *const RawRequest,
    sink: *const RawSinkVtable,
    out_error_code: *mut i32,
) -> u64 {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 句柄有效性由宿主契约保证。
        let station = unsafe { station(core) }.ok_or(InferFfiError::InvalidArgument)?;
        if request.is_null() || sink.is_null() {
            return Err(InferFfiError::InvalidArgument);
        }
        // SAFETY: 已判非空；调用期间有效由宿主契约保证（sink vtable 按值拷走）。
        let (raw_request, vtable) = unsafe { (*request, *sink) };
        let fns = vtable.checked()?;
        // SAFETY: raw_request 内的指针 / 长度对满足 §2 借用语义。
        let record = unsafe { request_of(&raw_request)? };
        station.core.stream(record, Box::new(VtableSink(fns)))
    }));
    let (handle, code) = match outcome {
        Ok(Ok(handle)) => (handle, MADINFER_OK),
        Ok(Err(error)) => (0, i32::from(error.code())),
        Err(_) => (0, PANICKED),
    };
    // SAFETY: out_error_code 为 NULL 或可写 int32_t（函数级 Safety）。
    unsafe { set_error(out_error_code, code) };
    handle
}

/// 6 取消：未完成请求恰一次 `on_failed(3008)`；已终态 / 未知句柄幂等 no-op。`core` 为 NULL 回 4001，其余恒 0。
///
/// # Safety
///
/// `core` 为 NULL 或有效句柄。
#[no_mangle]
pub unsafe extern "C" fn madinfer_stream_cancel(core: *mut RawCore, request: u64) -> i32 {
    guarded(
        || PANICKED,
        || {
            // SAFETY: 句柄有效性由宿主契约保证。
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            station.core.cancel(request);
            MADINFER_OK
        },
    )
}

/// 7 宿主请求回投：以 `host_request` 配对恰一次；重复 / 未知 id / 空指针 / kind 不符 / 取值越界回 4001 且无副作用。
///
/// # Safety
///
/// `core` 为 NULL 或有效句柄；`reply` 为 NULL 或指向有效结构体。
#[no_mangle]
pub unsafe extern "C" fn madinfer_host_reply(
    core: *mut RawCore,
    host_request: u64,
    reply: *const RawHostReply,
) -> i32 {
    guarded(
        || PANICKED,
        || {
            // SAFETY: 句柄有效性由宿主契约保证。
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            if reply.is_null() || host_request == 0 {
                return INVALID_ARGUMENT;
            }
            // SAFETY: 已判非空；结构体按值读走。
            let reply = unsafe { *reply };
            if reply.kind != MADINFER_HOST_REQUEST_CONSENT {
                return INVALID_ARGUMENT;
            }
            let Ok(decision) = pick(&CONSENT_DECISIONS, reply.consent_decision) else {
                return INVALID_ARGUMENT;
            };
            if station.host.reply(host_request, decision) {
                MADINFER_OK
            } else {
                INVALID_ARGUMENT
            }
        },
    )
}

/// 8 资源信号：热 / 电量 / 内存 / 前后台 / 网络 / 空闲 / 主链路忙进核调度门控；只入队不阻塞（邮箱满则丢并计数）。
/// `core` / `signal` 为 NULL 或 kind / 成员取值越界回 4001，其余恒 0。
///
/// # Safety
///
/// `core` 为 NULL 或有效句柄；`signal` 为 NULL 或指向有效结构体。
#[no_mangle]
pub unsafe extern "C" fn madinfer_signal(core: *mut RawCore, signal: *const RawOsSignal) -> i32 {
    guarded(
        || PANICKED,
        || {
            // SAFETY: 句柄有效性由宿主契约保证。
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            if signal.is_null() {
                return INVALID_ARGUMENT;
            }
            // SAFETY: 已判非空；结构体按值读走。
            let raw = unsafe { *signal };
            let Ok(record) = os_signal_of(&raw) else {
                return INVALID_ARGUMENT;
            };
            station.core.signal(record);
            MADINFER_OK
        },
    )
}

/// 9 配置应用（madai-w08 I-C-07 G2）：签名 config.v1 文档字节进核，同步等应用结论并回填 `*out`。
/// 返回值只表达调用本身：0 = 已拿到结论（`out->applied` 看 0 / 1）；`core` / `out` 为 NULL、或 `document_len > 0`
/// 而 `document` 为 NULL 回 4001；核已停回 4004；核内 panic 收口回 4002。宿主不得在任何回调里调本符号。
/// 线程契约（madai-w10 I-C-09；头文件 §5 / §6 同文）：核在 control 线程上经 `verify_ed25519` 回调宿主，每次 apply 至多一次，
/// 并发 apply 按入邮箱序串行回执；禁在验签回投所依赖的线程上同步调本符号（宿主验签经主循环回投时主循环直调即自锁）。
///
/// # Safety
///
/// `core` 为 NULL 或有效句柄；`document` 为 NULL（仅当 `document_len == 0`）或指向 `document_len` 个可读字节；
/// `out` 为 NULL 或指向可写结构体。
#[no_mangle]
pub unsafe extern "C" fn madinfer_apply_config(
    core: *mut RawCore,
    document: *const u8,
    document_len: u32,
    out: *mut RawApplyOutcome,
) -> i32 {
    guarded(
        || PANICKED,
        || {
            // SAFETY: 句柄有效性由宿主契约保证。
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            if out.is_null() || (document.is_null() && document_len > 0) {
                return INVALID_ARGUMENT;
            }
            let bytes = if document_len == 0 {
                Vec::new()
            } else {
                // SAFETY: 已判非空；宿主保证 document_len 个可读字节在调用期间有效（§2 借用语义）。
                unsafe {
                    std::slice::from_raw_parts(
                        document,
                        usize::try_from(document_len).unwrap_or(usize::MAX),
                    )
                }
                .to_vec()
            };
            match station.core.apply_config(bytes) {
                Ok(record) => {
                    // SAFETY: 已判非空；可写性由宿主契约保证。
                    unsafe { out.write(RawApplyOutcome::of(&record)) };
                    MADINFER_OK
                }
                Err(error) => i32::from(error.code()),
            }
        },
    )
}

/// 10 按前缀丢弃生成上下文（madai-w14 I-C-12；设计稿 R-8 / T6）：`prefix` 是 `madinfer_str`（UTF-8 字节 + 长度，借用；
/// 账户关闭时 `acct:<id>/`），核在 control 线程调 `ContextPool::drop_prefix`，**同步等**条数并回填 `*out_dropped`
/// （可为 NULL = 不要条数）。命中的键立即不可复用，在飞任务持有的上下文到任务终态才结清 KV。
/// 返回值只表达调用本身：0 = 已拿到条数（空前缀 / 无匹配也是 0 条）；`core` 为 NULL、`prefix.len > 0` 而 `ptr` 为 NULL、
/// 非 UTF-8 回 4001；核已停回 4004；核内 panic 收口回 4002。与第 9 符号同为"调用方等核回执"形态：宿主不得在任何
/// host / sink 回调里调本符号。只加符号、不改任何结构 / 枚举 → `MADINFER_ABI_VERSION` 仍 2。
///
/// # Safety
///
/// `core` 为 NULL 或有效句柄；`prefix` 满足头文件 §2 借用语义（`len > 0` 时 `ptr` 指向 `len` 个可读字节）；
/// `out_dropped` 为 NULL 或指向可写、对齐的 `uint32_t`。
#[no_mangle]
pub unsafe extern "C" fn madinfer_drop_contexts(
    core: *mut RawCore,
    prefix: RawStr,
    out_dropped: *mut u32,
) -> i32 {
    guarded(
        || PANICKED,
        || {
            // SAFETY: 句柄有效性由宿主契约保证。
            let Some(station) = (unsafe { station(core) }) else {
                return INVALID_ARGUMENT;
            };
            // SAFETY: 借用语义由宿主契约保证（§2）。
            let prefix = match unsafe { prefix.to_owned_string() } {
                Ok(text) => text,
                Err(error) => return i32::from(error.code()),
            };
            match station.core.drop_contexts(prefix) {
                Ok(dropped) => {
                    if !out_dropped.is_null() {
                        // SAFETY: 已判非空；可写性由宿主契约保证。
                        unsafe { out_dropped.write(dropped) };
                    }
                    MADINFER_OK
                }
                Err(error) => i32::from(error.code()),
            }
        },
    )
}

/// 调试符号（madai-w11 I-C-10 起两 profile 都导出、release 函数体回 4005；契约 `include/madinfer_debug.h`）。
pub mod debug;

#[cfg(test)]
mod tests;
