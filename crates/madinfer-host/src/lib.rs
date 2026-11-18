//! madinfer-host —— 宿主端口（设计文档 §4.3 C16）。
//!
//! 只有 trait 与纯数据类型。方向约定：宿主实现、内核消费；回调型端口的回调可在任意线程调用，内核侧只入队。
//! 端口方法非阻塞，或明确标注可阻塞（仅 [`KvStore`] / [`SecureStore`] 的读写允许阻塞 ≤ 50 ms）。
//! 端口 trait 的方法只加不改（对象安全，`dyn` 使用）。
//!
//! 时间纪律（imcore 规则第 7 条同款）：内核任何地方不得直读墙钟或单调钟，一律经 [`Clock`]；
//! [`MonotonicClock`] 是本工作区唯一的 `Instant::now` 豁免点（clippy.toml disallowed-methods）。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::time::Instant;

use madinfer_core_types::{CapabilityId, Os, PrivacyTag};

// ---------------------------------------------------------------------------
// 时钟与日志
// ---------------------------------------------------------------------------

/// 注入时钟：只保证单调不减，不承诺与挂钟对齐。测试用 `madinfer_testkit::ManualClock` 手拨。
pub trait Clock: Send + Sync {
    /// 当前毫秒。
    fn now_ms(&self) -> u64;
}

/// 生产时钟：进程内单调毫秒，以构造时刻为原点。
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    /// 以当前时刻为原点。
    #[allow(clippy::disallowed_methods)] // 豁免点：本工作区唯一的 Instant::now 读取处（imcore 规则第 7 条形态一）
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    #[allow(clippy::disallowed_methods)] // 豁免点同上：elapsed 内部读 Instant::now
    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// 日志级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// 调试构建才落地。
    Debug,
    /// 事实留痕。
    Info,
    /// 需要人看一眼。
    Warn,
    /// 失败。
    Error,
}

/// 事实留痕端口：`facts` 是 `key=value` 串，只含状态名、注册制 id、计数与毫秒；**用户内容禁入**（数据纪律）。
pub trait Logger: Send + Sync {
    /// 落一行。
    fn log(&self, level: LogLevel, target: &'static str, facts: &str);
}

/// 丢弃一切的日志端口（缺省）。
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn log(&self, _level: LogLevel, _target: &'static str, _facts: &str) {}
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

/// 小键值存储（install_id、授权记忆、配置版本号等；值 ≤ 4 KB）；读写可阻塞 ≤ 50 ms。
pub trait KvStore: Send + Sync {
    /// 读。
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// 写（覆盖）。
    fn put(&self, key: &str, value: &[u8]);
    /// 删。
    fn delete(&self, key: &str);
}

/// 受保护键值（密钥材料、算法包解密密钥）；同 [`KvStore`] 契约，落地在系统钥匙串 / Keystore。
pub trait SecureStore: Send + Sync {
    /// 读。
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// 写。
    fn put(&self, key: &str, value: &[u8]);
    /// 删。
    fn delete(&self, key: &str);
}

/// 目录（init.v1 `paths`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// 可被系统清理：blob 库、AOT 缓存。
    pub cache_dir: String,
    /// 不被清理：packages/、config、install_id、授权记忆。
    pub protected_dir: String,
    /// 临时目录。
    pub tmp_dir: String,
}

// ---------------------------------------------------------------------------
// 资源信号（signal.v1）与宿主状态
// ---------------------------------------------------------------------------

/// 热状态（Apple 语义；Android / 鸿蒙映射见 KMP 侧 HostPorts）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThermalState {
    /// 正常。
    Nominal,
    /// 偏热。
    Fair,
    /// 严重。
    Serious,
    /// 临界。
    Critical,
}

/// 内存压力。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryPressure {
    /// 正常。
    Normal,
    /// 警告。
    Warning,
    /// 临界。
    Critical,
}

/// 网络计费形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NetState {
    /// 离线。
    Offline,
    /// 计费网络。
    Metered,
    /// 非计费。
    Unmetered,
}

/// 应用前后台状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppState {
    /// 前台。
    Foreground,
    /// 后台（无执行窗口）。
    Background,
    /// 后台任务窗口（携预算）。
    BackgroundTask,
    /// 即将终止。
    Terminating,
}

/// 宿主 → 核的资源信号（过去式事实；缺省全无 = 按不可知运行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsSignal {
    /// 热状态变化。
    Thermal(ThermalState),
    /// 电量。
    Battery {
        /// 0～100；`None` = 未知（宿主读不到电量读数：模拟器 / 未开监测 / 平台不给），此时 `charging` 与
        /// `low_power_mode` 仍按实报。未知**不参与电量门控**：调度只把它记成"不可知"，任何策略不得以满电或空电
        /// 代入（madai-w09 I-C-08——此前 ffi 面把 None 透传成 100，真机低电但壳读数缺席时核会把"未知"当"满电"）。
        /// 范围校验归入口面：C 面 101～254 回 4001、255 为未知哨兵；uniffi 面由壳把越界读数映射为 None。
        level_pct: Option<u8>,
        /// 是否充电。
        charging: bool,
        /// 低电量模式。
        low_power_mode: bool,
    },
    /// 内存压力。
    Memory(MemoryPressure),
    /// 前后台。
    App {
        /// 状态。
        state: AppState,
        /// 后台任务窗口预算毫秒（仅 BackgroundTask）。
        budget_ms: Option<u32>,
    },
    /// 网络。
    Network(NetState),
    /// 用户空闲。
    Idle(bool),
    /// 宿主主链路忙（IM 收发高峰 / 列表滚动）；5 s 无更新自动解除。
    MainLinkBusy(bool),
}

// ---------------------------------------------------------------------------
// 观测出口
// ---------------------------------------------------------------------------

/// telemetry.v1 批次出口；实现禁止把批次写进日志。
pub trait TelemetrySink: Send + Sync {
    /// 交付一批（已序列化 JSON）。
    fn emit(&self, batch_json: &str);
}

// ---------------------------------------------------------------------------
// 下载（download.v1）
// ---------------------------------------------------------------------------

/// 网络策略。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NetPolicy {
    /// 只走 Wi-Fi。
    pub wifi_only: bool,
    /// 允许计费网络。
    pub allow_metered: bool,
    /// 只在充电时。
    pub charging_only: bool,
    /// 限速字节 / 秒。
    pub max_bytes_per_second: Option<u64>,
}

/// 下载请求；目标路径由资产层从内容 hash 派生。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadReq {
    /// 传输 id（资产层分配，同 id 的 start 幂等）。
    pub id: String,
    /// 源。
    pub url: String,
    /// 落点（`<tmp_dir>/<sha256>.part`）。
    pub dest: String,
    /// 期望 sha256（`sha256:` 前缀形式）。
    pub expected_sha256: String,
    /// 大小。
    pub size: u64,
    /// 网络策略。
    pub policy: NetPolicy,
    /// 优先级（越小越先）。
    pub priority: u8,
}

/// 传输事件（宿主 → 核）；`error_code` < 1000 瞬时可重试、≥ 1000 永久。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadEvent {
    /// 进度。
    Progress {
        /// 传输 id。
        id: String,
        /// 已传字节。
        transferred: u64,
        /// 总字节。
        total: u64,
    },
    /// 完成。
    Completed {
        /// 传输 id。
        id: String,
        /// 落点。
        path: String,
    },
    /// 失败。
    Failed {
        /// 传输 id。
        id: String,
        /// 错误码。
        error_code: u32,
    },
}

/// 后台传输端口；事件经 [`DownloadCallback`] 投递（任意线程，内核只入队）。
pub trait Downloader: Send + Sync {
    /// 开始（同 id 幂等）。
    fn start(&self, req: DownloadReq);
    /// 暂停。
    fn pause(&self, id: &str);
    /// 恢复。
    fn resume(&self, id: &str);
    /// 取消。
    fn cancel(&self, id: &str);
}

/// 传输事件回调（核实现、宿主调用）。
pub trait DownloadCallback: Send + Sync {
    /// 投递一条事件；实现必须只入队立即返回。
    fn on_event(&self, event: DownloadEvent);
}

// ---------------------------------------------------------------------------
// 流式 HTTP（http.v1）
// ---------------------------------------------------------------------------

/// HTTP 请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpReq {
    /// 方法。
    pub method: String,
    /// URL。
    pub url: String,
    /// 头。
    pub headers: Vec<(String, String)>,
    /// 体。
    pub body: Option<Vec<u8>>,
    /// 超时毫秒。
    pub timeout_ms: u32,
}

/// 流式响应块；`is_end` 为真的块是最后一块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpChunk {
    /// 状态码（首块有效）。
    pub status: u16,
    /// 头（首块有效）。
    pub headers: Vec<(String, String)>,
    /// 字节。
    pub bytes: Vec<u8>,
    /// 是否结束。
    pub is_end: bool,
}

/// 流式响应接收方（核实现、宿主调用；只入队）。
pub trait HttpSink: Send + Sync {
    /// 一块到达。
    fn on_chunk(&self, chunk: HttpChunk);
    /// 传输层失败（非 HTTP 状态码）。
    fn on_failure(&self, error_code: u32);
}

/// 流式 HTTP 端口；返回的请求 id 供取消。
pub trait HttpClient: Send + Sync {
    /// 发起；块经 sink 投递。
    fn start(&self, req: HttpReq, sink: Box<dyn HttpSink>) -> u64;
    /// 断开。
    fn cancel(&self, request: u64);
}

// ---------------------------------------------------------------------------
// 授权（升云前的一次性确认）
// ---------------------------------------------------------------------------

/// 授权上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentContext {
    /// 能力。
    pub capability: CapabilityId,
    /// 隐私标签。
    pub privacy: PrivacyTag,
    /// 云端端点 id。
    pub endpoint_id: String,
}

/// 授权结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentDecision {
    /// 本次允许。
    Allow,
    /// 允许并记住。
    AllowRemember,
    /// 拒绝。
    Deny,
}

/// 授权端口：非阻塞；宿主弹窗后经 `reply` 回投（任意线程）。
pub trait Authorizer: Send + Sync {
    /// 发起确认。
    fn request(&self, context: ConsentContext, reply: Box<dyn FnOnce(ConsentDecision) + Send>);
}

// ---------------------------------------------------------------------------
// 设备事实（init.v1 facts）
// ---------------------------------------------------------------------------

/// SoC。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Soc {
    /// 厂商。
    pub vendor: String,
    /// 家族。
    pub family: String,
}

/// NPU。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Npu {
    /// 厂商。
    pub vendor: String,
    /// API（core_ai / qnn / aicore / hiai …）。
    pub api: String,
    /// API 版本。
    pub api_version: String,
}

/// 宿主上报的硬件事实；不含任何标识符。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceFacts {
    /// 操作系统。
    pub os: Os,
    /// OS 版本。
    pub os_version: String,
    /// 机型类（不是型号）。
    pub model_class: String,
    /// SoC。
    pub soc: Soc,
    /// RAM MB。
    pub ram_mb: u32,
    /// NPU。
    pub npu: Option<Npu>,
}

// ---------------------------------------------------------------------------
// 线程钩子
// ---------------------------------------------------------------------------

/// 工作线程类别（宿主经 [`ThreadHooks`] 按类别设线程 QoS）。
///
/// 类别按**槽位**分派、不按请求 QoS（madinfer-api `WorkerPool`：Cpu / Gpu / Npu 槽 → `Interactive`，External 槽 → `Background`）；
/// 三档 `QoS` 的 Job 都可能落在任一类 worker 上，Async 与 Background（批量）两档的区分在调度（门控准入 / 配额 / 抢占），
/// 不在线程优先级。madai-w09 I-C-08 总控裁决：不拆、不升 ABI；宿主映射口径见 madinfer.h §5b（`Background` worker →
/// utility 档，禁 background 档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkerClass {
    /// 控制线程（调度 actor）。
    Control,
    /// Cpu / Gpu / Npu 槽（内联引擎）worker；承接三档 QoS 的本地推理。
    Interactive,
    /// External 槽（系统模型 / 云端适配器）worker；宿主映射 utility 档，禁 background 档（background 档在热 / 低电下会被
    /// 系统压到零配额，把同一 worker 上的 Async 请求随批量一起饿死）。
    Background,
    /// 投递线程（预留：合帧 / 回调今日仍在 control 线程，尚未拆出）。
    Delivery,
}

/// 线程钩子：宿主为核的工作线程设 QoS / 名字（iOS QoS class、Android 线程优先级、鸿蒙 QoS）。
pub trait ThreadHooks: Send + Sync {
    /// 线程启动时调用（在该线程上）。
    fn on_worker_started(&self, class: WorkerClass);
}

/// 不做任何事的线程钩子。
pub struct NoopThreadHooks;

impl ThreadHooks for NoopThreadHooks {
    fn on_worker_started(&self, _class: WorkerClass) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_clock_never_goes_backwards() {
        let clock = MonotonicClock::new();
        let a = clock.now_ms();
        let b = clock.now_ms();
        assert!(b >= a);
    }

    #[test]
    fn thermal_and_memory_are_ordered_for_gate_comparisons() {
        assert!(
            ThermalState::Nominal < ThermalState::Fair
                && ThermalState::Fair < ThermalState::Serious
        );
        assert!(ThermalState::Serious < ThermalState::Critical);
        assert!(
            MemoryPressure::Normal < MemoryPressure::Warning
                && MemoryPressure::Warning < MemoryPressure::Critical
        );
    }
}
