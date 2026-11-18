//! 核 actor（设计文档 C2 `Core` + C4 actor 的合体，I2b 形态）：一个 control 线程独占 [`SchedCore`]、每请求一台请求 FSM、
//! 路由决策、[`Framer`]、[`CapabilityIndex`]、[`ConfigStore`]、[`Telemetry`]、[`BackendRegistry`]、[`ModelRegistry`]；
//! 外界经 [`CoreHandle`] 投命令，worker 经同一邮箱回投 token 与回执——命令 / 回执 / 资源信号同序消费。
//!
//! 泵（泳道规则第 8 条）：邮箱 `std::sync::mpsc::sync_channel(1024)`；命令与 worker 回执 `send`（阻塞式保交付），
//! 资源信号 `try_send`（满则丢并计数）；无定时器 `recv` 阻塞，有定时器 `recv_timeout` 到最近到期
//! （`SchedCore::next_wakeup`、合帧到期票、冷却到期票、观测定时刷出四者取最小）；不轮询。
//!
//! **本批 Framer 与业务回调都在 control 线程执行**（`EventSink::on_event` 必须只入队立即返回）；I3 把合帧与投递移到 delivery 线程，
//! 慢 sink 计 `slow_sink` 观测。事件顺序保证由请求 FSM 的表保证：Started 至多一次且先于任何 Delta / Complete，终态恰一次。
//!
//! 上下文池（madai-w15 I-C-13 接进管线；madai-w14 I-C-12 只承接 `drop_contexts`）：`ContextPool` 由 control 线程持有，请求带
//! `context` 键时 Job 首次投递即 `reserve`（同键 `hits` +1、亲和 worker 作投递偏好）、Job 终态 `release`；预算取 config
//! `thresholds.kv_budget_mb[tier]`（建站按内置缺省 0，`Broadcast` 时换）；被丢 / 被腾 / 被压力回收的键经 `WorkerPool::drop_contexts`
//! 广播给 worker 释放后端 KV。
//!
//! 后端驻留（madai-w16 I-C-14）：池的裁决随工单下发（`ContextReservation`），worker 按键驻留 `Context`、命中即复用、收到回键即 Drop
//! （`workers.rs` 模块注）。每上下文 KV 取**后端真值**：`Model::context_kv_mb`（加载后）→ `Backend::context_kv_mb`（加载前）；
//! [`CoreBuilder::context_kv_mb`] 只是覆盖位（用例定数）。预算 0（本档未配置）→ `OverBudget` 按无 KV 模式跑、不驻留——生产站在
//! 配置带 `kv_budget_mb` 之前，行为与逐轮开关相同。worker 回投的两项事实让池与实际驻留一致：终态未驻留（`context_retained` 假）
//! 与腾席位丢弃（`evicted_contexts`）都让键离池；模型卸载随带其上下文键。
//!
//! 中间态（随 I3 / 资产接入替换）：模型键的权重 id 是包引用派生的占位 id；配置持久化（`PersistActive` / `PersistPrevious`）
//! 与 KvStore 端口未接；抢占重排的 Generate 型 Job 无 KV snapshot / restore，从头生成；
//! Retryable 错误不做原地重排（直接按请求失败上抛，业务按 `retry_after_ms` 重试）；回退阶段不弹授权（需授权的目标在回退时跳过）；
//! `Authorizer` 若在调用线程同步回执（测试替身 `ScriptedAuthorizer`），回执经 `send` 入邮箱，生产宿主的授权是异步弹窗、在别的线程回执。

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{Builder, JoinHandle};
use std::time::Duration;

use madinfer_backend::{
    Backend, BackendError, BackendRegistry, CancelToken, ConstraintSupport, ContextOptions,
    LoadOpts, MemoryFootprint, RegistryStats,
};
use madinfer_config::{
    ApplyOutcome, ConfigEffect, ConfigStore, Registry, SignatureVerifier, TrustAllVerifier,
};
use madinfer_core_types::{
    BackendId, BusinessId, ContextKey, DegradeKind, FatalKind, InferError, Os, PackageRef,
    RequestId, RetryKind, Source, Tier,
};
use madinfer_fsm::Fsm;
use madinfer_host::{
    Authorizer, Clock, ConsentContext, ConsentDecision, LogLevel, Logger, MonotonicClock,
    NoopLogger, NoopThreadHooks, OsSignal, TelemetrySink, ThermalState, ThreadHooks, WorkerClass,
};
use madinfer_observe::{
    Batch, BatchMeta, Constraint as ObservedConstraint, Decision, Gate, IdRegistry, Outcome,
    RegisteredId, Telemetry, TelemetryEvent, Thermal,
};
use madinfer_router::{
    advance, resolve, Advance, AvailabilityView, MappingTable, RouteDecision, RouteInput,
    RouteReason,
};
use madinfer_runtime::{
    Acquire, ContextPool, ContextPoolStats, KvBudget, ModelKey, ModelRegistry, Reserve,
    RuntimeEffect, RuntimeStats,
};
use madinfer_sched::{
    CancelReason as SchedCancelReason, GateName, Job, JobCtx, JobId, JobKind, QuotaTable,
    ResourceEstimate, SchedCommand, SchedCore, SchedEffect, SchedStats, SlotTable,
};
use semver::Version;

use crate::capability_index::{CapabilityIndex, IndexInputs, PackageReadiness};
use crate::framer::{Delta, Framer, FramerEffect, FramerStats, DEFAULT_FRAME_MS};
use crate::pipeline::{
    backend_of, model_key_of, package_of, slot_of, source_of, Event, EventSink, GenRequest,
    GenStats, Output,
};
use crate::request::{
    build_request_fsm, RequestEffect, RequestEvent, RequestState, REQUEST_FSM_TABLE_SIZE,
};
use crate::structured::{OutputSpec, StructuredTracker};
use crate::workers::{
    ContextReservation, DispatchError, WorkOrder, WorkOutcome, WorkerIndex, WorkerPool,
};

/// 邮箱缺省容量。
pub const DEFAULT_MAILBOX_CAPACITY: usize = 1_024;
/// 模型冷却缺省时长。
pub const DEFAULT_COOL_MS: u64 = 30_000;

/// 核命令（邮箱信封）。`Stream` / `Cancel` / `Signal` / `ConfigArrived` / `ApplyConfig` / `DropContexts` / `Stats` / `Shutdown`
/// 来自 [`CoreHandle`]，其余是 worker 回执与授权回执。
pub enum CoreCommand {
    /// 发起一次流式生成。
    Stream {
        /// 句柄预分配的请求 id。
        id: RequestId,
        /// 请求。
        request: GenRequest,
        /// 事件接收方。
        sink: Box<dyn EventSink>,
    },
    /// 取消。
    Cancel {
        /// 请求。
        request: RequestId,
    },
    /// 宿主资源信号。
    Signal(OsSignal),
    /// 配置文档字节到达（config.v1，已签名）；只投不等。
    ConfigArrived(Vec<u8>),
    /// 配置文档字节到达并要回执（madai-w08 G2 `apply_config`）：应用结论经 `reply` 回投调用方。
    ApplyConfig {
        /// 已签名文档字节。
        bytes: Vec<u8>,
        /// 回执通道（调用方阻塞等）。
        reply: mpsc::Sender<ApplyReport>,
    },
    /// 按前缀丢弃上下文并要回执（madai-w14 I-C-12 `drop_contexts`）：control 线程调 `ContextPool::drop_prefix`，条数经 `reply` 回投。
    DropContexts {
        /// 键前缀（账户段 `acct:<id>/`）。
        prefix: String,
        /// 回执通道（调用方阻塞等）。
        reply: mpsc::Sender<u32>,
    },
    /// 读一份台账快照（madai-w15 I-C-13 `stats`）：control 线程在两条命令之间取 [`CoreStats`]（子台账 + 在飞 / 上下文池量规），
    /// 经 `reply` 回投；不改任何状态。
    Stats {
        /// 回执通道（调用方阻塞等）。
        reply: mpsc::Sender<CoreStats>,
    },
    /// 授权回执。
    ConsentDecided {
        /// 请求。
        request: RequestId,
        /// 结论。
        decision: ConsentDecision,
    },
    /// worker 回执。
    WorkerReport {
        /// Job。
        job: JobId,
        /// 请求。
        request: RequestId,
        /// worker。
        worker: WorkerIndex,
        /// 结果。
        outcome: WorkOutcome,
    },
    /// token 片到达。
    TokenArrived {
        /// 请求。
        request: RequestId,
        /// Job（陈旧目标的片按它剔除）。
        job: JobId,
        /// 片。
        piece: madinfer_backend::TokenPiece,
    },
    /// worker 加载完成。
    ModelLoaded {
        /// 模型。
        key: ModelKey,
        /// 占用。
        footprint: MemoryFootprint,
        /// 持有它的 worker。
        worker: WorkerIndex,
        /// 加载后的每上下文 KV 读数（`Model::context_kv_mb`；madai-w16 I-C-14）。0 = 不申报，池沿用 `Backend::context_kv_mb`。
        context_kv_mb: u32,
    },
    /// worker 加载失败。
    ModelLoadFailed {
        /// 模型。
        key: ModelKey,
        /// 错误。
        error: BackendError,
    },
    /// worker 卸载完成。
    ModelUnloaded {
        /// 模型。
        key: ModelKey,
        /// 随模型一并丢弃的驻留上下文键（madai-w16 I-C-14；池据此让键离池）。
        contexts: Vec<ContextKey>,
    },
    /// 关停：取消全部在途请求、排水 worker、刷出观测。
    Shutdown,
}

/// `apply_config` 回执：ConfigStore 的应用结论 + 调用后活动配置版本（madai-w08 G2）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyReport {
    /// 应用结论（已应用 / 拒绝原因 + 阶段）。
    pub outcome: ApplyOutcome,
    /// 调用后的活动配置版本（被拒时仍是旧版本）。
    pub active_version: u64,
}

/// 计数台账（`CoreHandle::shutdown` 返回；madai-w15 I-C-13 起 [`CoreHandle::stats`] 在运行期也能取同一份快照；断言面）。
/// 子台账（`sched` / `framer` / `models` / `contexts` / `backends`）与量规（`inflight_jobs` / `context_*` / `uptime_ms`）
/// 在 control 线程一次取齐，彼此一致。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoreStats {
    /// 调度核台账。
    pub sched: SchedStats,
    /// 合帧台账。
    pub framer: FramerStats,
    /// 运行时台账。
    pub models: RuntimeStats,
    /// 上下文池台账（madai-w14 I-C-12：`dropped` 计按前缀丢弃数）。
    pub contexts: ContextPoolStats,
    /// 池内上下文数（量规；被 `drop_prefix` 在飞命中、尚待终态结清的条目不计）。
    pub context_entries: u32,
    /// 池内 KV 已用 MB（量规；含在飞延迟结清的记账）。
    pub context_kv_mb: u32,
    /// 池 KV 预算上限 MB（量规；config `thresholds.kv_budget_mb[tier]`，未配置 0）。
    pub context_kv_budget_mb: u32,
    /// 在飞 Job 数（量规：已提交调度、尚未终态，含排队与执行中）。
    pub inflight_jobs: u32,
    /// 自建站起的毫秒数（量规；注入的 `Clock`）。
    pub uptime_ms: u64,
    /// 后端注册表台账。
    pub backends: RegistryStats,
    /// 收到的请求。
    pub requests_accepted: u64,
    /// 完成。
    pub completed: u64,
    /// 失败（含取消）。
    pub failed: u64,
    /// 其中取消。
    pub cancelled: u64,
    /// 降级链推进次数。
    pub fallbacks: u64,
    /// 投递的事件数。
    pub events_delivered: u64,
    /// 资源信号满则丢的次数。
    pub dropped_signals: u64,
    /// 刷出的观测批次。
    pub telemetry_batches: u64,
    /// 请求已终态后到达的 token 片。
    pub late_tokens: u64,
    /// 结构化输出终态校验未过（2003）的次数（madai-w08 G4）。
    pub structured_rejected: u64,
    /// 请求 FSM 非法事件累计。
    pub unexpected_request_events: u64,
    /// 能力快照版本。
    pub capability_version: u64,
    /// 请求 FSM 表大小（锁拓扑）。
    pub request_fsm_table_size: usize,
}

/// 核（只有构建器入口）。
pub struct Core;

impl Core {
    /// 构建器。
    pub fn builder() -> CoreBuilder {
        CoreBuilder::default()
    }
}

struct NoopTelemetrySink;

impl TelemetrySink for NoopTelemetrySink {
    fn emit(&self, _batch_json: &str) {}
}

struct DenyAuthorizer;

impl Authorizer for DenyAuthorizer {
    fn request(&self, _context: ConsentContext, reply: Box<dyn FnOnce(ConsentDecision) + Send>) {
        reply(ConsentDecision::Deny);
    }
}

/// 核构建器：注入端口、槽表、配额表、初始映射表（测试直接给；生产由 ConfigStore 给）、后端列表。
pub struct CoreBuilder {
    clock: Arc<dyn Clock>,
    logger: Arc<dyn Logger>,
    telemetry_sink: Arc<dyn TelemetrySink>,
    authorizer: Arc<dyn Authorizer>,
    hooks: Arc<dyn ThreadHooks>,
    slots: SlotTable,
    quota: QuotaTable,
    mapping: MappingTable,
    backends: Vec<Arc<dyn Backend>>,
    installed_packages: BTreeSet<PackageRef>,
    tier: Tier,
    os: Os,
    big_cores: u8,
    frame_ms: u64,
    cool_ms: u64,
    telemetry_meta: Option<BatchMeta>,
    sampling_bp: u16,
    mailbox_capacity: usize,
    verifier: Option<Box<dyn SignatureVerifier>>,
    bucket: u16,
    sdk_version: String,
    context_kv_mb: Option<u32>,
    weights_path: Option<PathBuf>,
}

impl Default for CoreBuilder {
    fn default() -> Self {
        Self {
            clock: Arc::new(MonotonicClock::new()),
            logger: Arc::new(NoopLogger),
            telemetry_sink: Arc::new(NoopTelemetrySink),
            authorizer: Arc::new(DenyAuthorizer),
            hooks: Arc::new(NoopThreadHooks),
            slots: SlotTable::new([(madinfer_core_types::SlotClass::Cpu, 1)]),
            quota: QuotaTable::default(),
            mapping: MappingTable::default(),
            backends: Vec::new(),
            installed_packages: BTreeSet::new(),
            tier: Tier::Unknown,
            os: Os::Jvm,
            big_cores: 4,
            frame_ms: DEFAULT_FRAME_MS,
            cool_ms: DEFAULT_COOL_MS,
            telemetry_meta: None,
            sampling_bp: 10_000,
            mailbox_capacity: DEFAULT_MAILBOX_CAPACITY,
            verifier: None,
            bucket: 0,
            sdk_version: env!("CARGO_PKG_VERSION").to_owned(),
            context_kv_mb: None,
            weights_path: None,
        }
    }
}

impl CoreBuilder {
    /// 时钟。
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// 日志。
    pub fn logger(mut self, logger: Arc<dyn Logger>) -> Self {
        self.logger = logger;
        self
    }

    /// 观测出口。
    pub fn telemetry_sink(mut self, sink: Arc<dyn TelemetrySink>) -> Self {
        self.telemetry_sink = sink;
        self
    }

    /// 授权端口。
    pub fn authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    /// 线程钩子。
    pub fn thread_hooks(mut self, hooks: Arc<dyn ThreadHooks>) -> Self {
        self.hooks = hooks;
        self
    }

    /// 槽表（worker 池按它起线程）。
    pub fn slots(mut self, slots: SlotTable) -> Self {
        self.slots = slots;
        self
    }

    /// 配额表。
    pub fn quota(mut self, quota: QuotaTable) -> Self {
        self.quota = quota;
        self
    }

    /// 初始映射表（配置中心下发前的兜底）。
    pub fn mapping(mut self, mapping: MappingTable) -> Self {
        self.mapping = mapping;
        self
    }

    /// 注册一个后端。
    pub fn backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backends.push(backend);
        self
    }

    /// 已安装激活的算法包（资产层接入前由装配方给）。
    pub fn installed_package(mut self, package: PackageRef) -> Self {
        self.installed_packages.insert(package);
        self
    }

    /// 设备档位。
    pub fn tier(mut self, tier: Tier) -> Self {
        self.tier = tier;
        self
    }

    /// 操作系统。
    pub fn os(mut self, os: Os) -> Self {
        self.os = os;
        self
    }

    /// 大核数（线程数建议）。
    pub fn big_cores(mut self, cores: u8) -> Self {
        self.big_cores = cores;
        self
    }

    /// 合帧窗口。
    pub fn frame_ms(mut self, ms: u64) -> Self {
        self.frame_ms = ms;
        self
    }

    /// 模型冷却时长。
    pub fn cool_ms(mut self, ms: u64) -> Self {
        self.cool_ms = ms;
        self
    }

    /// 观测批次头。
    pub fn telemetry_meta(mut self, meta: BatchMeta) -> Self {
        self.telemetry_meta = Some(meta);
        self
    }

    /// 观测采样万分比。
    pub fn sampling_bp(mut self, bp: u16) -> Self {
        self.sampling_bp = bp;
        self
    }

    /// 邮箱容量。
    pub fn mailbox_capacity(mut self, capacity: usize) -> Self {
        self.mailbox_capacity = capacity.max(1);
        self
    }

    /// 配置验签器（生产必填；调试构建缺省 `TrustAllVerifier`）。
    pub fn verifier(mut self, verifier: Box<dyn SignatureVerifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// 设备灰度桶。
    pub fn bucket(mut self, bucket: u16) -> Self {
        self.bucket = bucket;
        self
    }

    /// SDK 版本（config.v1 min_sdk 比较用）。
    pub fn sdk_version(mut self, version: &str) -> Self {
        self.sdk_version = version.to_owned();
        self
    }

    /// 每上下文 KV MB 的**覆盖位**（madai-w15 I-C-13 引入为估算；madai-w16 I-C-14 起不设 = 取后端真值：加载后 `Model::context_kv_mb`、
    /// 加载前 `Backend::context_kv_mb`）。`reserve` 以它对着 config `thresholds.kv_budget_mb[tier]` 算预算、腾 LRU 空闲条目或判
    /// `OverBudget`（按无 KV 模式跑、不驻留）。用例给定数用；生产站不设。
    pub fn context_kv_mb(mut self, mb: u32) -> Self {
        self.context_kv_mb = Some(mb);
        self
    }

    /// 权重文件路径（madai-w17 I-C-16）：随每张工单进 `LoadOpts::weights_path`，真实引擎后端在 `load` 时读它。资产层接入前由装配方
    /// （madinfer-ffi `CoreOptions.model_path`）直给；不设 = `None`，仿真核不受影响、真实引擎加载回 `ModelMissing`（3004，走既有失败路径）。
    /// 单后端单包形态下站内所有 Bundled 目标共用这一条路径；多包由资产层按 `weights` id 解析（归 I4）。
    pub fn weights_path(mut self, path: PathBuf) -> Self {
        self.weights_path = Some(path);
        self
    }

    /// 起 control 线程与 worker 池，返回句柄。装配期断言：生产构建拒绝缺省的 `TrustAllVerifier`（泳道规则第 14 条）。
    pub fn spawn(self) -> io::Result<CoreHandle> {
        let verifier: Box<dyn SignatureVerifier> = match self.verifier {
            Some(v) => v,
            None if cfg!(debug_assertions) => Box::new(TrustAllVerifier),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "生产构建必须注入 SignatureVerifier",
                ))
            }
        };
        let sdk = Version::parse(&self.sdk_version)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        let mut backends = BackendRegistry::with_logger(Arc::clone(&self.logger));
        for backend in self.backends {
            backends
                .register(backend)
                .map_err(|e| io::Error::new(io::ErrorKind::AlreadyExists, e.to_string()))?;
        }
        let snapshot = Arc::new(backends.snapshot());
        let registry = Registry {
            capabilities: BTreeSet::new(),
            backends: backends.ids().cloned().collect(),
            endpoints: BTreeSet::new(),
        };
        let config = ConfigStore::new(self.bucket, sdk, verifier, registry);
        // 建站取内置缺省配置的预算（未配置的档位 0）；apply_config Broadcast 时随活动配置换。
        let kv_budget_mb = kv_budget_mb_of(&config.active().thresholds, self.tier);

        let (tx, rx) = mpsc::sync_channel::<CoreCommand>(self.mailbox_capacity);
        let classes = self.slots.classes();
        let pool = WorkerPool::spawn(
            &self.slots,
            &classes,
            Arc::clone(&snapshot),
            Arc::clone(&self.clock),
            Arc::clone(&self.hooks),
            tx.clone(),
        )?;

        let now = self.clock.now_ms();
        let mut ids = IdRegistry::new();
        let none_id = ids
            .register("none")
            .unwrap_or_else(|| unreachable!("词表字符"));
        let telemetry_meta = self.telemetry_meta.unwrap_or_else(|| BatchMeta {
            instance: "madinfer".to_owned(),
            sdk: self.sdk_version.clone(),
            tier: self.tier,
            os: self.os,
            os_version: String::new(),
        });
        let actor = CoreActor {
            clock: Arc::clone(&self.clock),
            logger: self.logger,
            telemetry_sink: self.telemetry_sink,
            authorizer: self.authorizer,
            hooks: self.hooks,
            mailbox: tx.clone(),
            sched: SchedCore::new(self.slots, self.quota, self.big_cores),
            framer: Framer::new(self.frame_ms),
            framer_timers: TimerQueue::default(),
            cool_timers: TimerQueue::default(),
            requests: HashMap::new(),
            jobs: HashMap::new(),
            next_job: 0,
            capability_index: CapabilityIndex::new(),
            config,
            mapping: self.mapping,
            telemetry: Telemetry::new(self.sampling_bp, now),
            telemetry_meta,
            ids,
            none_id,
            backends,
            models: ModelRegistry::new(self.cool_ms),
            contexts: ContextPool::new(KvBudget::new(kv_budget_mb)),
            context_kv_mb: self.context_kv_mb,
            model_context_kv_mb: HashMap::new(),
            weights_path: self.weights_path,
            affinity: HashMap::new(),
            pool: Some(pool),
            installed_packages: self.installed_packages,
            tier: self.tier,
            os: self.os,
            started_at_ms: now,
            stats: CoreStats {
                request_fsm_table_size: REQUEST_FSM_TABLE_SIZE,
                ..CoreStats::default()
            },
        };
        let thread = Builder::new()
            .name("madinfer-core".into())
            .spawn(move || actor.run(rx))?;
        Ok(CoreHandle {
            tx,
            next_request: AtomicU64::new(0),
            dropped_signals: Arc::new(AtomicU64::new(0)),
            thread: Some(thread),
        })
    }
}

/// 核句柄：任意线程可调；命令 `send` 阻塞式保交付，资源信号 `try_send` 满则丢并计数。
pub struct CoreHandle {
    tx: SyncSender<CoreCommand>,
    next_request: AtomicU64,
    dropped_signals: Arc<AtomicU64>,
    thread: Option<JoinHandle<CoreStats>>,
}

impl CoreHandle {
    /// 发起流式生成；返回请求 id（事件与取消都以它为锚）。
    pub fn stream(&self, request: GenRequest, sink: Box<dyn EventSink>) -> RequestId {
        let id = RequestId(self.next_request.fetch_add(1, Ordering::Relaxed) + 1);
        let _ = self.tx.send(CoreCommand::Stream { id, request, sink });
        id
    }

    /// 取消。
    pub fn cancel(&self, request: RequestId) {
        let _ = self.tx.send(CoreCommand::Cancel { request });
    }

    /// 资源信号（满则丢并计数）。
    pub fn signal(&self, signal: OsSignal) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(CoreCommand::Signal(signal)) {
            self.dropped_signals.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 配置文档到达（只投不等）。
    pub fn config(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(CoreCommand::ConfigArrived(bytes));
    }

    /// 送一份已签名 config.v1 文档进核并等应用结论（madai-w08 G2 `apply_config`）：验签（`SignatureVerifier`）→ 解析 /
    /// 版本 / min_sdk / 分桶 / 语义 → 原子切换，全部在 control 线程上由 `ConfigStore` 完成；本调用阻塞到回执。
    /// control 线程已退出（关停 / panic）时返回 `None`。
    pub fn apply_config(&self, bytes: Vec<u8>) -> Option<ApplyReport> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(CoreCommand::ApplyConfig { bytes, reply })
            .ok()?;
        rx.recv().ok()
    }

    /// 按前缀丢弃上下文并等条数（madai-w14 I-C-12 `drop_contexts`；账户关闭时前缀 `acct:<id>/`）：control 线程上调
    /// `ContextPool::drop_prefix`——命中的键立即不可复用，在飞条目的 KV 到任务终态才结清；本调用阻塞到回执。
    /// control 线程已退出（关停 / panic）时返回 `None`（ffi 映射 4004）。
    pub fn drop_contexts(&self, prefix: String) -> Option<u32> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(CoreCommand::DropContexts { prefix, reply })
            .ok()?;
        rx.recv().ok()
    }

    /// 运行期取一份台账快照（madai-w15 I-C-13 `stats`）：经邮箱在 control 线程取 [`CoreStats`]（与 [`Self::shutdown`] 同一
    /// 类型、同一取法），子台账与量规在两条命令之间一次取齐；不改任何状态；本调用阻塞到回执（等的是核自己的 control 线程）。
    /// control 线程已退出（关停 / panic）时返回 `None`（ffi 映射 4004）。
    pub fn stats(&self) -> Option<CoreStats> {
        let (reply, rx) = mpsc::channel();
        self.tx.send(CoreCommand::Stats { reply }).ok()?;
        let mut stats = rx.recv().ok()?;
        stats.dropped_signals = self.dropped_signals.load(Ordering::Relaxed);
        Some(stats)
    }

    /// 已丢弃的资源信号数。
    pub fn dropped_signals(&self) -> u64 {
        self.dropped_signals.load(Ordering::Relaxed)
    }

    /// 调试入口（只在 `debug_assertions` 构建存在；madai-w10 I-C-09 J5-a，I-S-05 U-4）：让 control 线程退出但保留句柄，
    /// 复现"核已停、对象仍在"的故障态——之后 [`Self::apply_config`] 恒 `None`（ffi 映射 4004），`stream` / `cancel` /
    /// `signal` / `config` 静默丢弃（无任何回调）。幂等；不 join（[`Self::shutdown`] / Drop 仍负责 join）。
    ///
    /// 返回即 4004 已成立：发 Shutdown 后再投一份空回执命令作探针并等其回执通道断开。邮箱 FIFO 保证 Shutdown 先于探针被受理，
    /// 关停排水阶段只处理 worker 回执、丢弃其余命令（探针连同回执发送端一起被丢 → 接收端断开）；线程已退出时 send 直接失败。
    /// 探针不会被应用（若竟收到回执即 FIFO 被破坏，调试构建断言）。
    #[cfg(debug_assertions)]
    pub fn stop_control(&self) {
        let _ = self.tx.send(CoreCommand::Shutdown);
        let (reply, rx) = mpsc::channel();
        if self
            .tx
            .send(CoreCommand::ApplyConfig {
                bytes: Vec::new(),
                reply,
            })
            .is_ok()
        {
            debug_assert!(rx.recv().is_err(), "stop_control 探针不应被应用");
        }
    }

    /// 关停并收回台账。
    pub fn shutdown(mut self) -> Option<CoreStats> {
        let _ = self.tx.send(CoreCommand::Shutdown);
        let dropped = self.dropped_signals.load(Ordering::Relaxed);
        self.thread.take().and_then(|t| t.join().ok()).map(|mut s| {
            s.dropped_signals = dropped;
            s
        })
    }
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = self.tx.send(CoreCommand::Shutdown);
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// actor 内部
// ---------------------------------------------------------------------------

/// 带代数的定时器队列（少量条目，线性扫描）。
struct TimerQueue<T> {
    items: Vec<(u64, u64, T)>,
    seq: u64,
}

impl<T> Default for TimerQueue<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            seq: 0,
        }
    }
}

impl<T> TimerQueue<T> {
    fn arm(&mut self, at_ms: u64, item: T) {
        self.seq += 1;
        self.items.push((at_ms, self.seq, item));
    }

    fn next_at(&self) -> Option<u64> {
        self.items.iter().map(|(at, _, _)| *at).min()
    }

    fn pop_due(&mut self, now_ms: u64) -> Vec<T> {
        let mut due: Vec<(u64, u64, T)> = Vec::new();
        let mut rest = Vec::with_capacity(self.items.len());
        for item in self.items.drain(..) {
            if item.0 <= now_ms {
                due.push(item);
            } else {
                rest.push(item);
            }
        }
        self.items = rest;
        due.sort_by_key(|(at, seq, _)| (*at, *seq));
        due.into_iter().map(|(_, _, t)| t).collect()
    }
}

/// 请求的 owner 附属字段（不进 FSM 状态）。
struct RequestEntry {
    request: GenRequest,
    sink: Box<dyn EventSink>,
    fsm: Fsm<RequestState, RequestEvent, RequestEffect>,
    decision: Option<RouteDecision>,
    source: Option<Source>,
    job: Option<JobId>,
    consent_granted: bool,
    text: String,
    accepted_at_ms: u64,
    started_at_ms: Option<u64>,
    first_token_at_ms: Option<u64>,
    queued_ms: u32,
    prompt_tokens: u32,
    gen_tokens: u32,
    peak_memory_mb: u32,
    fallback_steps: u8,
    capability_id: RegisteredId,
    backend_id: Option<RegisteredId>,
    package_id: Option<RegisteredId>,
    constraint_support: Option<ConstraintSupport>,
    last_error: Option<InferError>,
    /// 结构化输出跟踪器（`output = Structured` 时有；madai-w08 G4）。
    structured: Option<StructuredTracker>,
    /// 终态校验通过的 JSON（Complete 的 `Output::Structured`）。
    structured_json: Option<String>,
}

/// Job 的 owner 附属字段。
struct JobRecord {
    request: RequestId,
    backend: BackendId,
    cancel: CancelToken,
    model_key: Option<ModelKey>,
    acquired: bool,
    worker: Option<WorkerIndex>,
    /// 本 Job 在池里持有的上下文预留（首次投递成功时 `reserve`，终态 `release`）；无 `context` 键或 `OverBudget` 为 None。
    context: Option<ContextKey>,
}

struct CoreActor {
    clock: Arc<dyn Clock>,
    logger: Arc<dyn Logger>,
    telemetry_sink: Arc<dyn TelemetrySink>,
    authorizer: Arc<dyn Authorizer>,
    hooks: Arc<dyn ThreadHooks>,
    mailbox: SyncSender<CoreCommand>,
    sched: SchedCore,
    framer: Framer,
    framer_timers: TimerQueue<(RequestId, u64)>,
    cool_timers: TimerQueue<(ModelKey, u64)>,
    requests: HashMap<RequestId, RequestEntry>,
    jobs: HashMap<JobId, JobRecord>,
    next_job: u64,
    capability_index: CapabilityIndex,
    config: ConfigStore,
    mapping: MappingTable,
    telemetry: Telemetry,
    telemetry_meta: BatchMeta,
    ids: IdRegistry,
    none_id: RegisteredId,
    backends: BackendRegistry,
    models: ModelRegistry,
    /// 上下文池：`DropContexts` 按前缀丢弃；Job 首次投递 `reserve`、终态 `release`（模块头"上下文池"）。
    contexts: ContextPool,
    /// 每上下文 KV MB 覆盖位（[`CoreBuilder::context_kv_mb`]；None = 取后端读数，见 [`Self::context_kv_mb_of`]）。
    context_kv_mb: Option<u32>,
    /// 加载后各模型申报的每上下文 KV（`ModelLoaded` 携带；卸载时移除）。
    model_context_kv_mb: HashMap<ModelKey, u32>,
    /// 权重文件路径（[`CoreBuilder::weights_path`]）：逐单进 `LoadOpts::weights_path`。
    weights_path: Option<PathBuf>,
    affinity: HashMap<ModelKey, WorkerIndex>,
    /// 关停时取走（`Option` 只为所有权转移，运行期恒 `Some`）。
    pool: Option<WorkerPool>,
    installed_packages: BTreeSet<PackageRef>,
    tier: Tier,
    os: Os,
    /// 建站时刻（`uptime_ms` 基点）。
    started_at_ms: u64,
    stats: CoreStats,
}

const CANCELLED: InferError = InferError::Fatal {
    kind: FatalKind::Cancelled,
    detail: 0,
};

impl CoreActor {
    fn run(mut self, rx: Receiver<CoreCommand>) -> CoreStats {
        self.hooks.on_worker_started(WorkerClass::Control);
        self.refresh_capability_index();
        loop {
            let now = self.clock.now_ms();
            let received = match self.next_wakeup() {
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(at) if at <= now => match rx.try_recv() {
                    Ok(command) => Ok(command),
                    Err(TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
                    Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
                },
                Some(at) => rx.recv_timeout(Duration::from_millis(at - now)),
            };
            match received {
                Ok(CoreCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => self.tick(),
            }
        }
        self.shutdown(rx)
    }

    fn next_wakeup(&self) -> Option<u64> {
        let mut next = self.sched.next_wakeup();
        for candidate in [
            self.framer_timers.next_at(),
            self.cool_timers.next_at(),
            (self.telemetry.pending() > 0).then(|| self.telemetry.next_flush_ms()),
        ]
        .into_iter()
        .flatten()
        {
            next = Some(next.map_or(candidate, |n: u64| n.min(candidate)));
        }
        next
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    // ------------------------------------------------------------------ 命令

    fn handle(&mut self, command: CoreCommand) {
        match command {
            CoreCommand::Stream { id, request, sink } => self.on_stream(id, request, sink),
            CoreCommand::Cancel { request } => {
                self.fire_request(request, RequestEvent::CancelRequested, None)
            }
            CoreCommand::Signal(signal) => self.on_signal(signal),
            CoreCommand::ConfigArrived(bytes) => {
                let _ = self.on_config(&bytes);
            }
            CoreCommand::ApplyConfig { bytes, reply } => {
                let outcome = self.on_config(&bytes);
                // 调用方已放弃等待（send 失败）不是错误：结论已落在 ConfigStore 与观测里。
                let _ = reply.send(ApplyReport {
                    outcome,
                    active_version: self.config.active().version,
                });
            }
            CoreCommand::DropContexts { prefix, reply } => {
                // 键先离池（同键再来必是新建），再让 worker 释放后端 KV；在飞条目的记账到其 Job 终态 release 才结清。
                let keys = self.contexts.drop_prefix_keys(&prefix);
                self.release_backend_contexts(&keys);
                // 调用方已放弃等待（send 失败）不是错误：丢弃已落在池与台账里。
                let _ = reply.send(count_u32(keys.len()));
            }
            CoreCommand::Stats { reply } => {
                // 只读快照；调用方已放弃等待（send 失败）不是错误。
                let snapshot = self.snapshot();
                let _ = reply.send(snapshot);
            }
            CoreCommand::ConsentDecided { request, decision } => self.on_consent(request, decision),
            CoreCommand::WorkerReport {
                job,
                request,
                worker,
                outcome,
            } => self.on_worker_report(job, request, worker, outcome),
            CoreCommand::TokenArrived {
                request,
                job,
                piece,
            } => self.on_token(request, job, piece),
            CoreCommand::ModelLoaded {
                key,
                footprint,
                worker,
                context_kv_mb,
            } => self.on_model_loaded(&key, footprint, worker, context_kv_mb),
            CoreCommand::ModelLoadFailed { key, .. } => {
                let now = self.now();
                self.models.on_load_failed(&key, now);
                self.affinity.remove(&key);
            }
            CoreCommand::ModelUnloaded { key, contexts } => {
                let now = self.now();
                self.models.on_unload_finished(&key, now);
                self.affinity.remove(&key);
                self.model_context_kv_mb.remove(&key);
                self.detach_dropped_contexts(&contexts);
            }
            CoreCommand::Shutdown => {}
        }
    }

    fn tick(&mut self) {
        let now = self.now();
        let effects = self.sched.tick(now);
        self.apply_sched_effects(effects);
        for (request, generation) in self.framer_timers.pop_due(now) {
            let effects = self.framer.on_window_elapsed(request, generation, now);
            self.apply_framer_effects(request, effects);
        }
        for (key, generation) in self.cool_timers.pop_due(now) {
            let effects = self.models.on_cool_elapsed(&key, generation, now);
            self.apply_runtime_effects(effects);
        }
        if let Some(batch) = self.telemetry.flush_due(now) {
            self.emit_batch(&batch);
        }
    }

    fn shutdown(mut self, rx: Receiver<CoreCommand>) -> CoreStats {
        // 在途请求全部按取消收口（业务收到 Failed(3008)，Job 经 CancelJob 翻旗），再等 worker 退出。
        let active: Vec<RequestId> = self.requests.keys().copied().collect();
        for request in active {
            self.fire_request(request, RequestEvent::CancelRequested, None);
        }
        // worker 可能正阻塞在向邮箱回投 token，也会补最后一份回执：边消费边等它们退出
        // （有界等待，不是轮询：每轮阻塞在 recv_timeout 上）。回执照常进调度核，台账因此完整；新命令不再受理。
        if let Some(pool) = self.pool.take() {
            pool.shutdown_draining(|| loop {
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(
                        command @ (CoreCommand::WorkerReport { .. }
                        | CoreCommand::TokenArrived { .. }
                        | CoreCommand::ModelLoaded { .. }
                        | CoreCommand::ModelLoadFailed { .. }
                        | CoreCommand::ModelUnloaded { .. }),
                    ) => self.handle(command),
                    Ok(_) => continue,
                    Err(RecvTimeoutError::Timeout) => return true,
                    Err(RecvTimeoutError::Disconnected) => return false,
                }
            });
        }
        let now = self.now();
        if let Some(batch) = self.telemetry.drain(now) {
            self.emit_batch(&batch);
        }
        self.snapshot()
    }

    /// 台账快照：子台账（调度 / 合帧 / 运行时 / 上下文池 / 后端表）与量规（在飞 Job、池条目 / KV / 预算、运行时长）在 control
    /// 线程上一次取齐。`Stats` 命令与关停收口同一取法。
    fn snapshot(&mut self) -> CoreStats {
        self.stats.sched = self.sched.stats();
        self.stats.framer = self.framer.stats();
        self.stats.models = self.models.stats();
        self.stats.contexts = self.contexts.stats();
        self.stats.backends = self.backends.stats();
        self.stats.context_entries = count_u32(self.contexts.len());
        self.stats.context_kv_mb = self.contexts.budget().used_mb;
        self.stats.context_kv_budget_mb = self.contexts.budget().limit_mb;
        self.stats.inflight_jobs = count_u32(self.jobs.len());
        self.stats.uptime_ms = self.now().saturating_sub(self.started_at_ms);
        self.stats.clone()
    }

    // ------------------------------------------------------------------ 请求管线

    fn on_stream(&mut self, id: RequestId, mut request: GenRequest, sink: Box<dyn EventSink>) {
        let now = self.now();
        self.stats.requests_accepted += 1;
        // G4：Structured → 受理时编 GBNF（schema 是载荷，编不过只记原因词，Started 前以 2003 拒绝）。
        let mut structured = None;
        let mut schema_invalid = false;
        if let OutputSpec::Structured { schema_json } = &request.output {
            match StructuredTracker::compile(schema_json) {
                Ok((constraint, tracker)) => {
                    request.constraint = Some(constraint);
                    structured = Some(tracker);
                }
                Err(_) => schema_invalid = true,
            }
        }
        let capability_id = self
            .ids
            .register(request.capability.as_str())
            .unwrap_or_else(|| self.none_id.clone());
        self.observe(TelemetryEvent::RequestStart {
            trace: request.trace.map_or([0; 16], |t| t.trace_id),
            capability: capability_id.clone(),
            business: request.business.get(),
            qos: request.budget.qos,
            privacy: request.privacy,
        });
        let empty_turn = request.turn.messages.is_empty();
        let input = RouteInput {
            capability: request.capability.clone(),
            privacy: request.privacy,
            qos: request.budget.qos,
            tier: self.tier,
            os: self.os,
        };
        self.requests.insert(
            id,
            RequestEntry {
                request,
                sink,
                fsm: build_request_fsm(),
                decision: None,
                source: None,
                job: None,
                consent_granted: false,
                text: String::new(),
                accepted_at_ms: now,
                started_at_ms: None,
                first_token_at_ms: None,
                queued_ms: 0,
                prompt_tokens: 0,
                gen_tokens: 0,
                peak_memory_mb: 0,
                fallback_steps: 0,
                capability_id: capability_id.clone(),
                backend_id: None,
                package_id: None,
                constraint_support: None,
                last_error: None,
                structured,
                structured_json: None,
            },
        );
        if empty_turn {
            self.fire_request(
                id,
                RequestEvent::RouteFailed {
                    error: InferError::Fatal {
                        kind: FatalKind::InvalidRequest,
                        detail: 0,
                    },
                },
                None,
            );
            return;
        }
        if schema_invalid {
            self.fire_request(
                id,
                RequestEvent::RouteFailed {
                    error: InferError::Degradable {
                        kind: DegradeKind::SchemaInvalid,
                    },
                },
                None,
            );
            return;
        }
        let view = self.availability_view();
        let decision = resolve(&input, &self.mapping, &view).and_then(|mut d| {
            // 不变量（设计文档 R10）：supports_abort = false 的后端不接交互档 Job。
            if input.qos == madinfer_core_types::QoS::Interactive {
                d.chain
                    .retain(|t| self.backends.accepts_interactive(&backend_of(&t.target)));
            }
            if d.chain.is_empty() {
                Err(InferError::Fatal {
                    kind: FatalKind::UnsupportedDevice,
                    detail: 0,
                })
            } else {
                Ok(d)
            }
        });
        match decision {
            Err(error) => self.fire_request(id, RequestEvent::RouteFailed { error }, None),
            Ok(decision) => {
                let chain_len = u8::try_from(decision.chain.len()).unwrap_or(u8::MAX);
                let chosen = decision
                    .current()
                    .map_or(Source::Bundled, |t| source_of(&t.target));
                let requires_consent = decision.current().is_some_and(|t| t.requires_consent);
                let privacy = input.privacy;
                let reason = decision.reason;
                if let Some(entry) = self.requests.get_mut(&id) {
                    entry.decision = Some(decision);
                }
                self.observe(TelemetryEvent::RouteDecision {
                    capability: capability_id,
                    privacy,
                    reason: reason.wire(),
                    chain_len,
                    chosen,
                });
                if requires_consent {
                    self.fire_request(id, RequestEvent::ConsentRequired, None);
                } else {
                    self.fire_request(id, RequestEvent::Routed, None);
                }
            }
        }
    }

    fn availability_view(&self) -> AvailabilityView {
        let available = self.backends.available_backends();
        AvailabilityView {
            reachable_endpoints: available.iter().map(|b| b.0.clone()).collect(),
            available_backends: available,
            installed_packages: self.installed_packages.clone(),
            quarantined: self.backends.quarantined().clone(),
            switches: self.config.active().router_switches(),
        }
    }

    fn refresh_capability_index(&mut self) {
        let switches = self.config.active().router_switches();
        let inputs = IndexInputs {
            mapping: self.mapping.clone(),
            tier: self.tier,
            os: Some(self.os),
            available_backends: self.backends.available_backends(),
            quarantined: self.backends.quarantined().clone(),
            packages: self
                .installed_packages
                .iter()
                .map(|p| (p.clone(), PackageReadiness::Installed))
                .collect(),
            disabled: switches.disabled_capabilities,
            all_off: switches.all_off,
            cloud_reachable: false,
        };
        if let Some(snapshot) = self.capability_index.update(&inputs) {
            self.stats.capability_version = snapshot.version;
            self.log(format!(
                "core=capabilities version={} entries={}",
                snapshot.version,
                snapshot.entries.len()
            ));
        }
    }

    /// 投一条请求事件；接受则执行效果，终态则收尾。`delta` 是 DeltaArrived 携带的已合帧数据。
    fn fire_request(&mut self, id: RequestId, event: RequestEvent, delta: Option<Delta>) {
        let now = self.now();
        let Some(entry) = self.requests.get_mut(&id) else {
            return;
        };
        if let RequestEvent::Started { source } = &event {
            entry.source = Some(*source);
        }
        let transition = entry.fsm.fire(&event, now);
        if !transition.accepted {
            self.stats.unexpected_request_events += 1;
            return;
        }
        self.log(format!(
            "core=request request={} from={:?} to={:?}",
            id.0, transition.from, transition.to
        ));
        self.execute_effects(id, transition.effects, delta);
        let terminal = self
            .requests
            .get(&id)
            .is_some_and(|e| e.fsm.state().is_terminal());
        if terminal {
            self.finish_request(id);
        }
    }

    fn execute_effects(
        &mut self,
        id: RequestId,
        effects: Vec<RequestEffect>,
        delta: Option<Delta>,
    ) {
        let mut delta = delta;
        for effect in effects {
            match effect {
                RequestEffect::SubmitJob => self.submit_job(id),
                RequestEffect::CancelJob => {
                    let job = self.requests.get(&id).and_then(|e| e.job);
                    if let Some(job) = job {
                        let now = self.now();
                        let effects = self.sched.handle(
                            SchedCommand::Cancel {
                                job,
                                reason: SchedCancelReason::Caller,
                            },
                            now,
                        );
                        self.apply_sched_effects(effects);
                    }
                }
                RequestEffect::RequestConsent => self.request_consent(id),
                RequestEffect::EmitStarted => {
                    let event = self.requests.get(&id).map(|e| Event::Started {
                        request: id,
                        source: e.source.unwrap_or(Source::Bundled),
                        package: e
                            .decision
                            .as_ref()
                            .and_then(|d| d.current())
                            .and_then(|t| package_of(&t.target)),
                        route_reason: e
                            .decision
                            .as_ref()
                            .map_or(RouteReason::Default, |d| d.reason),
                        queued_ms: e.queued_ms,
                    });
                    if let Some(event) = event {
                        self.deliver(id, event);
                    }
                }
                RequestEffect::EmitDelta => {
                    if let Some(d) = delta.take() {
                        self.deliver(id, Event::Delta(d));
                    }
                }
                RequestEffect::EmitComplete => {
                    let event = self.requests.get(&id).map(|e| Event::Complete {
                        request: id,
                        output: match &e.structured_json {
                            Some(json) => Output::Structured(json.clone()),
                            None => Output::Text(e.text.clone()),
                        },
                        stats: self.gen_stats(e),
                    });
                    if let Some(event) = event {
                        self.stats.completed += 1;
                        self.deliver(id, event);
                    }
                }
                RequestEffect::EmitFailed { error } => {
                    let event = self.requests.get_mut(&id).map(|e| {
                        e.last_error = Some(error);
                        Event::Failed {
                            request: id,
                            error,
                            partial: (!e.text.is_empty()).then(|| Output::Text(e.text.clone())),
                        }
                    });
                    if let Some(event) = event {
                        self.stats.failed += 1;
                        if error.code() == CANCELLED.code() {
                            self.stats.cancelled += 1;
                        }
                        self.deliver(id, event);
                    }
                }
                RequestEffect::FinishFramer => {
                    for frame in self.framer.finish(id) {
                        if let FramerEffect::Emit(d) = frame {
                            self.deliver(id, Event::Delta(d));
                        }
                    }
                }
            }
        }
    }

    fn deliver(&mut self, id: RequestId, event: Event) {
        if let Some(entry) = self.requests.get_mut(&id) {
            entry.sink.on_event(event);
            self.stats.events_delivered += 1;
        }
    }

    fn gen_stats(&self, entry: &RequestEntry) -> GenStats {
        let now = self.now();
        let started = entry.started_at_ms.unwrap_or(entry.accepted_at_ms);
        let first = entry.first_token_at_ms.unwrap_or(now);
        let ttft_ms = to_u32(first.saturating_sub(started));
        let decode_ms = to_u32(now.saturating_sub(first));
        let tps_x100 = if decode_ms == 0 {
            0
        } else {
            to_u32(u64::from(entry.gen_tokens) * 100_000 / u64::from(decode_ms))
        };
        GenStats {
            ttft_ms,
            decode_ms,
            prompt_tokens: entry.prompt_tokens,
            gen_tokens: entry.gen_tokens,
            tps_x100,
            peak_memory_mb: entry.peak_memory_mb,
            source: entry.source,
            fallback_steps: entry.fallback_steps,
            prefix_hit: false,
        }
    }

    fn finish_request(&mut self, id: RequestId) {
        let now = self.now();
        let Some(entry) = self.requests.remove(&id) else {
            return;
        };
        // 终态后合帧器不得再有 pending（FinishFramer 已放行）；若有即协议错误，只计数不投递（终态后无事件）。
        let leaked = self.framer.finish(id).len();
        if leaked > 0 {
            self.log(format!("core=framer_leak request={} frames={leaked}", id.0));
        }
        let stats = self.gen_stats(&entry);
        let outcome = match (entry.fsm.state(), entry.last_error) {
            (RequestState::Completed, _) => Outcome::Complete,
            (RequestState::Cancelled, _) => Outcome::Cancelled,
            (_, Some(e)) if e.code() == CANCELLED.code() => Outcome::Cancelled,
            _ => Outcome::Failed,
        };
        let thermal = thermal_of(self.sched.gates().thermal);
        let source = entry.source.or_else(|| {
            entry
                .decision
                .as_ref()
                .and_then(|d| d.current())
                .map(|t| source_of(&t.target))
        });
        self.observe(TelemetryEvent::RequestEnd {
            trace: entry.request.trace.map_or([0; 16], |t| t.trace_id),
            capability: entry.capability_id.clone(),
            business: entry.request.business.get(),
            qos: entry.request.budget.qos,
            source: source.unwrap_or(Source::Bundled),
            backend: entry.backend_id.clone(),
            package: entry.package_id.clone(),
            outcome,
            code: entry.last_error.map_or(0, |e| e.code()),
            queued_ms: entry.queued_ms,
            ttft_ms: stats.ttft_ms,
            total_ms: to_u32(now.saturating_sub(entry.accepted_at_ms)),
            prompt_tokens: entry.prompt_tokens,
            gen_tokens: entry.gen_tokens,
            tps_x100: stats.tps_x100,
            peak_memory_mb: entry.peak_memory_mb,
            thermal,
            fallback_steps: entry.fallback_steps,
            prefix_hit: false,
            cold_page_in: false,
            constraint: match entry.constraint_support {
                Some(ConstraintSupport::Native) => ObservedConstraint::Native,
                Some(ConstraintSupport::Emulated) => ObservedConstraint::Emulated,
                Some(ConstraintSupport::None) | None => ObservedConstraint::None,
            },
            adopted: None,
        });
        if let Some(error) = entry.last_error {
            if outcome == Outcome::Failed {
                self.observe(TelemetryEvent::Error {
                    code: error.code(),
                    kind: error.kind_wire(),
                    backend: entry.backend_id.clone(),
                    package: entry.package_id.clone(),
                });
            }
        }
        // sink 在此 Drop（control 线程；I3 起在 delivery 线程）。
        drop(entry);
    }

    fn request_consent(&mut self, id: RequestId) {
        let Some(entry) = self.requests.get(&id) else {
            return;
        };
        let endpoint_id = entry
            .decision
            .as_ref()
            .and_then(|d| d.current())
            .map_or_else(String::new, |t| backend_of(&t.target).0);
        let context = ConsentContext {
            capability: entry.request.capability.clone(),
            privacy: entry.request.privacy,
            endpoint_id,
        };
        let mailbox = self.mailbox.clone();
        self.authorizer.request(
            context,
            Box::new(move |decision| {
                let _ = mailbox.send(CoreCommand::ConsentDecided {
                    request: id,
                    decision,
                });
            }),
        );
    }

    fn on_consent(&mut self, id: RequestId, decision: ConsentDecision) {
        let Some(entry) = self.requests.get_mut(&id) else {
            return;
        };
        let capability = entry.capability_id.clone();
        let (event, observed) = match decision {
            ConsentDecision::Allow => (RequestEvent::ConsentGranted, Decision::Allow),
            ConsentDecision::AllowRemember => {
                (RequestEvent::ConsentGranted, Decision::AllowRemember)
            }
            ConsentDecision::Deny => (RequestEvent::ConsentDenied, Decision::Deny),
        };
        entry.consent_granted = matches!(event, RequestEvent::ConsentGranted);
        self.observe(TelemetryEvent::Consent {
            capability,
            decision: observed,
        });
        self.fire_request(id, event, None);
    }

    fn submit_job(&mut self, id: RequestId) {
        let now = self.now();
        let Some(entry) = self.requests.get_mut(&id) else {
            return;
        };
        let Some(target) = entry
            .decision
            .as_ref()
            .and_then(|d| d.current())
            .map(|t| t.target.clone())
        else {
            self.fire_request(
                id,
                RequestEvent::Failed {
                    error: InferError::Fatal {
                        kind: FatalKind::Internal,
                        detail: 2,
                    },
                },
                None,
            );
            return;
        };
        self.next_job += 1;
        let job_id = JobId(self.next_job);
        let backend = backend_of(&target);
        let job = Job {
            request: id,
            capability: entry.request.capability.clone(),
            business: entry.request.business,
            qos: entry.request.budget.qos,
            kind: JobKind::Generate,
            slot: slot_of(&target),
            deadline_at_ms: entry
                .accepted_at_ms
                .saturating_add(u64::from(entry.request.budget.deadline_ms)),
            priority: 0,
            // 资源估算随资产层 manifest.compat.memory_mb 接入；此刻成本 0 = 配额只按实际记账。
            estimate: ResourceEstimate {
                peak_memory_mb: 0,
                estimated_ms: 0,
            },
            passthrough: false,
        };
        entry.job = Some(job_id);
        entry.backend_id = self.ids.register(&backend.0);
        entry.package_id = package_of(&target).and_then(|p| self.ids.register(&p.to_wire()));
        entry.constraint_support = self.backends.caps(&backend).map(|c| c.constraint);
        self.jobs.insert(
            job_id,
            JobRecord {
                request: id,
                backend,
                cancel: CancelToken::new(),
                model_key: None,
                acquired: false,
                worker: None,
                context: None,
            },
        );
        let effects = self
            .sched
            .handle(SchedCommand::Submit { id: job_id, job }, now);
        self.apply_sched_effects(effects);
    }

    // ------------------------------------------------------------------ 调度效果

    fn apply_sched_effects(&mut self, effects: Vec<SchedEffect>) {
        for effect in effects {
            match effect {
                SchedEffect::Start { ctx } => self.on_job_start(ctx),
                SchedEffect::Cancel { job, reason } => {
                    if let Some(record) = self.jobs.get(&job) {
                        record.cancel.cancel(cancel_reason_of(reason));
                    }
                }
                SchedEffect::Deferred {
                    job,
                    until_ms,
                    gate,
                    ..
                } => {
                    let qos = self
                        .jobs
                        .get(&job)
                        .and_then(|r| self.requests.get(&r.request))
                        .map(|e| e.request.budget.qos);
                    if let (Some(qos), Some(gate)) = (qos, gate_of(gate)) {
                        let now = self.now();
                        self.observe(TelemetryEvent::SchedWait {
                            qos,
                            wait_ms: to_u32(until_ms.saturating_sub(now)),
                            admission: madinfer_observe::AdmissionKind::Defer,
                            gate,
                        });
                    }
                }
                SchedEffect::Done { job, request } => {
                    self.finish_job(job);
                    // G4：结构化请求终态前校验全文；不过 → 2003（partial 带已生成文本），不发 Complete。
                    let verdict = self
                        .requests
                        .get(&request)
                        .and_then(|e| e.structured.as_ref())
                        .map(StructuredTracker::finish);
                    match verdict {
                        Some(Ok(json)) => {
                            if let Some(entry) = self.requests.get_mut(&request) {
                                entry.structured_json = Some(json);
                            }
                            self.fire_request(request, RequestEvent::Completed, None);
                        }
                        Some(Err(_)) => {
                            self.stats.structured_rejected += 1;
                            self.fire_request(
                                request,
                                RequestEvent::Failed {
                                    error: InferError::Degradable {
                                        kind: DegradeKind::SchemaInvalid,
                                    },
                                },
                                None,
                            );
                        }
                        None => self.fire_request(request, RequestEvent::Completed, None),
                    }
                }
                SchedEffect::Failed {
                    job,
                    request,
                    error,
                } => {
                    self.finish_job(job);
                    self.on_job_failed(request, error);
                }
                SchedEffect::Cancelled {
                    job,
                    request,
                    reason,
                } => {
                    self.finish_job(job);
                    let error = match reason {
                        // 调用方取消：请求 FSM 已在 CancelRequested 时到终态，这里无事可做。
                        SchedCancelReason::Caller => continue,
                        SchedCancelReason::Deadline => InferError::Degradable {
                            kind: DegradeKind::Deadline,
                        },
                        SchedCancelReason::Pressure => InferError::Retryable {
                            kind: RetryKind::MemoryPressure,
                            retry_after_ms: 1_000,
                        },
                        SchedCancelReason::Preempt => InferError::Retryable {
                            kind: RetryKind::Busy,
                            retry_after_ms: 200,
                        },
                        SchedCancelReason::Drain => CANCELLED,
                    };
                    self.fire_request(request, RequestEvent::Failed { error }, None);
                }
                SchedEffect::Drained => {}
            }
        }
    }

    fn on_job_start(&mut self, ctx: JobCtx) {
        let now = self.now();
        let Some(record) = self.jobs.get(&ctx.job) else {
            return;
        };
        let request = record.request;
        let cancel = record.cancel.clone();
        let target = self
            .requests
            .get(&request)
            .and_then(|e| e.decision.as_ref())
            .and_then(|d| d.current())
            .map(|t| t.target.clone());
        let Some(target) = target else {
            // 请求已终态（调用方取消）：合成失败回执释放槽。
            self.synthesize_failure(ctx.job, BackendError::Cancelled);
            return;
        };
        let backend = backend_of(&target);
        let Some(caps) = self.backends.caps(&backend).copied() else {
            self.synthesize_failure(ctx.job, BackendError::ModelMissing);
            return;
        };
        let key = model_key_of(&target);
        let (acquire, _) = self.models.acquire(&key, now);
        let acquired = match acquire {
            Acquire::Held => true,
            Acquire::NotLoaded => {
                let _ = self.models.begin_load(key.clone(), now);
                false
            }
            Acquire::Loading => false,
            Acquire::BackboneBusy { .. } | Acquire::Unloaded => {
                self.synthesize_failure(ctx.job, BackendError::Busy);
                return;
            }
        };
        // 投递偏好：上下文亲和（KV 在那个 worker 上）优先于模型亲和；两者同槽类别。
        let context_affinity = self
            .requests
            .get(&request)
            .and_then(|e| e.request.context.as_ref())
            .and_then(|k| self.contexts.worker_of(k))
            .map(|w| WorkerIndex(u16::from(w)));
        let prefer = context_affinity.or_else(|| self.affinity.get(&key).copied());
        let mut order = match self.requests.get_mut(&request) {
            Some(entry) => {
                entry.started_at_ms = Some(now);
                entry.queued_ms = to_u32(ctx.queued_ms);
                Box::new(WorkOrder {
                    job: ctx.job,
                    request,
                    ctx: ctx.clone(),
                    backend: backend.clone(),
                    model_key: key.clone(),
                    load: LoadOpts {
                        package: package_of(&target),
                        weights: Some(key.weights),
                        weights_path: self.weights_path.clone(),
                        threads: ctx.threads,
                        mmap: true,
                    },
                    context_options: ContextOptions {
                        max_context_tokens: caps.max_context_tokens,
                        threads: ctx.threads,
                    },
                    turn: entry.request.turn.clone(),
                    options: entry.request.options.clone(),
                    constraint: entry.request.constraint.clone(),
                    cancel,
                    context: None,
                })
            }
            None => {
                self.synthesize_failure(ctx.job, BackendError::Cancelled);
                return;
            }
        };
        // 先选 worker、再预留上下文（池的裁决随单下发，madai-w16 I-C-14），最后投递；投不出去时刚新建的预留随即离池。
        let picked = self.pool.as_ref().and_then(|p| p.pick(ctx.slot, prefer));
        let mut created_now: Option<ContextKey> = None;
        let dispatched = match picked {
            None => Err(DispatchError::NoWorker(order)),
            Some(worker) => {
                let (reservation, created) =
                    self.reserve_context(ctx.job, request, &backend, &key, worker, now);
                if created {
                    created_now = reservation.as_ref().map(|r| r.key.clone());
                }
                order.context = reservation;
                match self.pool.as_mut() {
                    Some(pool) => pool.dispatch_to(worker, order).map(|()| worker),
                    None => Err(DispatchError::NoWorker(order)),
                }
            }
        };
        match dispatched {
            Ok(worker) => {
                self.affinity.entry(key.clone()).or_insert(worker);
                if let Some(record) = self.jobs.get_mut(&ctx.job) {
                    record.worker = Some(worker);
                    record.model_key = Some(key);
                    record.acquired = acquired;
                }
                let source = source_of(&target);
                let state = self.requests.get(&request).map(|e| e.fsm.state());
                if state == Some(RequestState::Submitted) {
                    self.fire_request(request, RequestEvent::Started { source }, None);
                }
            }
            Err(error) => {
                if let Some(key) = created_now {
                    // worker 没接单、句柄从未打开：键离池（在飞 → 延迟记账，Job 终态 release 结清）。
                    self.contexts.drop_context(&key);
                }
                if acquired {
                    let effects = self.models.release(&key, now);
                    self.apply_runtime_effects(effects);
                }
                let reason = match &error {
                    DispatchError::NoWorker(_) => "no_worker",
                    DispatchError::Full(_) => "mailbox_full",
                    DispatchError::Gone(_) => "worker_gone",
                };
                self.log(format!(
                    "core=dispatch_failed job={} reason={reason}",
                    ctx.job.0
                ));
                self.synthesize_failure(ctx.job, BackendError::Busy);
            }
        }
    }

    /// Job 投递前把请求的 `context` 键预留进池（madai-w15 I-C-13；madai-w16 I-C-14 起裁决随单下发给 worker）：同键复用计 `hits`
    /// → `reused`；新建计 `misses`，为腾预算回收的空闲键回给 worker 释放后端 KV；`OverBudget`（KV 读数 > 预算且无可回收）按无 KV
    /// 模式跑——不记条目、不驻留，池 `rejected` 计数（预算 > 0 时留痕一行；预算 0 是"本档不留 KV"的配置态，不留痕）。
    /// 重排再启动（`Yielded` 后）的 Job 已持有预留：键仍在池 → 按新建口径下发（worker 让出时未驻留、从头开）；键已离池 → 不带预留。
    /// 返回 `(随单下发的预留, 本次是否新建)`。池的 worker 位是 `u8`（槽表容量口径），编号截断到 255。
    fn reserve_context(
        &mut self,
        job: JobId,
        request: RequestId,
        backend: &BackendId,
        model: &ModelKey,
        worker: WorkerIndex,
        now: u64,
    ) -> (Option<ContextReservation>, bool) {
        let key = self
            .requests
            .get(&request)
            .and_then(|e| e.request.context.clone());
        let Some(key) = key else {
            return (None, false);
        };
        if self.jobs.get(&job).is_some_and(|r| r.context.is_some()) {
            let reservation = self
                .contexts
                .worker_of(&key)
                .is_some()
                .then_some(ContextReservation { key, reused: false });
            return (reservation, false);
        }
        let worker_slot = u8::try_from(worker.0).unwrap_or(u8::MAX);
        let kv_mb = self.context_kv_mb_of(backend, model);
        let reservation = match self.contexts.reserve(&key, kv_mb, worker_slot, now) {
            Reserve::Reused { worker: held_on } => ContextReservation {
                key: key.clone(),
                // 池记的亲和 worker 与本次投递不同（形态上不可达：亲和是投递偏好）→ 当新建下发，worker 从头开。
                reused: held_on == worker_slot,
            },
            Reserve::Created { evicted, .. } => {
                self.release_backend_contexts(&evicted);
                ContextReservation {
                    key: key.clone(),
                    reused: false,
                }
            }
            Reserve::OverBudget => {
                let budget_mb = self.contexts.budget().limit_mb;
                if budget_mb > 0 {
                    self.log(format!(
                        "core=context_over_budget job={} kv_mb={kv_mb} budget_mb={budget_mb}",
                        job.0
                    ));
                }
                return (None, false);
            }
        };
        let created = !reservation.reused;
        if let Some(record) = self.jobs.get_mut(&job) {
            record.context = Some(key);
        }
        (Some(reservation), created)
    }

    /// 每上下文 KV MB：覆盖位（[`CoreBuilder::context_kv_mb`]）→ 加载后模型申报（`ModelLoaded`）→ 加载前后端申报
    /// （`Backend::context_kv_mb`）；后端也不申报则 0（池只记亲和与命中）。
    fn context_kv_mb_of(&self, backend: &BackendId, model: &ModelKey) -> u32 {
        if let Some(mb) = self.context_kv_mb {
            return mb;
        }
        match self.model_context_kv_mb.get(model).copied() {
            Some(mb) if mb > 0 => mb,
            _ => self.backends.get(backend).map_or(0, |b| b.context_kv_mb()),
        }
    }

    /// worker 已自行丢弃的驻留上下文（腾席位 / 随模型卸载）→ 键离池，读数与实际一致。键若正被在飞 Job 持有（该 Job 排在
    /// worker 队列里、执行时会重新打开并驻留），不动池。
    fn detach_dropped_contexts(&mut self, keys: &[ContextKey]) {
        for key in keys {
            let in_flight = self
                .jobs
                .values()
                .any(|r| r.context.as_ref() == Some(key) && r.worker.is_some());
            if !in_flight {
                self.contexts.drop_context(key);
            }
        }
    }

    /// Job 终态：移除记录并归还它持有的上下文预留——`release` 置闲（KV 保留，可被 LRU 回收），键若已被 `drop_prefix` 在飞命中
    /// 则在此结清延迟记账、不写回。
    fn finish_job(&mut self, job: JobId) {
        if let Some(record) = self.jobs.remove(&job) {
            if let Some(key) = record.context {
                let now = self.now();
                self.contexts.release(&key, now);
            }
        }
    }

    /// 已离池的键交给 worker 释放后端 KV（`drop_prefix` / 腾预算 / 压力回收三处同一出口）；未送达的 lane 数留痕。
    fn release_backend_contexts(&mut self, keys: &[ContextKey]) {
        if keys.is_empty() {
            return;
        }
        let undelivered = self.pool.as_ref().map_or(0, |p| p.drop_contexts(keys));
        if undelivered > 0 {
            self.log(format!(
                "core=drop_contexts_undelivered lanes={undelivered} keys={}",
                keys.len()
            ));
        }
    }

    /// 没有 worker 会回执时，由核自己给调度器一份失败回执（保住"每 Job 终态回执恰一次 / 槽必释放"）。
    fn synthesize_failure(&mut self, job: JobId, error: BackendError) {
        let now = self.now();
        let effects = self.sched.handle(
            SchedCommand::Report {
                job,
                report: madinfer_sched::WorkerReport::Failed {
                    error: error.into(),
                    run_ms: 0,
                    peak_memory_mb: 0,
                },
            },
            now,
        );
        self.apply_sched_effects(effects);
    }

    fn on_job_failed(&mut self, request: RequestId, error: InferError) {
        let Some(entry) = self.requests.get_mut(&request) else {
            return;
        };
        let may_fallback = matches!(
            entry.fsm.state(),
            RequestState::Submitted | RequestState::Running
        );
        let mut chosen_step = None;
        if may_fallback {
            let consent_granted = entry.consent_granted;
            if let Some(decision) = entry.decision.as_mut() {
                let chosen = loop {
                    match advance(decision, &error) {
                        // 回退阶段不弹授权：需授权的目标跳过（I3 加 Running × ConsentRequired 边后放开）。
                        Advance::Next(t) if t.requires_consent && !consent_granted => continue,
                        Advance::Next(_) => break true,
                        Advance::RetryInPlace | Advance::Exhausted | Advance::Stop => break false,
                    }
                };
                if chosen {
                    entry.fallback_steps = entry.fallback_steps.saturating_add(1);
                    entry.first_token_at_ms = None;
                    chosen_step = Some(entry.fallback_steps);
                }
            }
        }
        if let Some(step) = chosen_step {
            self.stats.fallbacks += 1;
            self.log(format!(
                "core=fallback request={} code={} step={step}",
                request.0,
                error.code()
            ));
            self.fire_request(request, RequestEvent::FallbackChosen, None);
            return;
        }
        self.fire_request(request, RequestEvent::Failed { error }, None);
    }

    // ------------------------------------------------------------------ worker 回执

    fn on_worker_report(
        &mut self,
        job: JobId,
        request: RequestId,
        worker: WorkerIndex,
        outcome: WorkOutcome,
    ) {
        let now = self.now();
        if let Some(pool) = self.pool.as_mut() {
            pool.mark_reported(worker);
        }
        let mut backend: Option<BackendId> = None;
        let mut release_effects = Vec::new();
        let mut not_retained: Option<ContextKey> = None;
        if let Some(record) = self.jobs.get_mut(&job) {
            backend = Some(record.backend.clone());
            if record.acquired {
                if let Some(key) = record.model_key.clone() {
                    release_effects = self.models.release(&key, now);
                }
                record.acquired = false;
            }
            record.worker = None;
            // 终态回执且 worker 未驻留（失败 / 取消）→ 键离池；让出（Yielded）的 Job 保留预留，重排后 worker 从头开再驻留。
            if !outcome.context_retained
                && !matches!(outcome.report, madinfer_sched::WorkerReport::Yielded { .. })
            {
                not_retained = record.context.clone();
            }
        }
        self.apply_runtime_effects(release_effects);
        // worker 回投的驻留事实对账（madai-w16 I-C-14）：腾席位丢掉的键、以及本 Job 未驻留的键都离池，`release` 到终态结清。
        self.detach_dropped_contexts(&outcome.evicted_contexts);
        if let Some(key) = not_retained {
            self.contexts.drop_context(&key);
        }
        if let Some(backend) = backend {
            match outcome.backend_error {
                Some(error) if error.counts_as_crash() => {
                    if self.backends.record_crash(&backend) {
                        if let Some(id) = self.ids.register(&backend.0) {
                            self.observe(TelemetryEvent::Quarantine {
                                backend: id,
                                reason: "crash_x3",
                            });
                        }
                        self.refresh_capability_index();
                    }
                }
                Some(_) => {}
                None => self.backends.record_success(&backend),
            }
        }
        let peak = match outcome.report {
            madinfer_sched::WorkerReport::Finished { peak_memory_mb, .. }
            | madinfer_sched::WorkerReport::Failed { peak_memory_mb, .. }
            | madinfer_sched::WorkerReport::Yielded { peak_memory_mb, .. } => peak_memory_mb,
        };
        if let Some(entry) = self.requests.get_mut(&request) {
            if let Some(stats) = outcome.stats {
                entry.prompt_tokens = stats.prompt_tokens;
            }
            entry.peak_memory_mb = entry.peak_memory_mb.max(peak);
        }
        let effects = self.sched.handle(
            SchedCommand::Report {
                job,
                report: outcome.report,
            },
            now,
        );
        self.apply_sched_effects(effects);
    }

    fn on_token(&mut self, request: RequestId, job: JobId, piece: madinfer_backend::TokenPiece) {
        let now = self.now();
        let Some(entry) = self.requests.get_mut(&request) else {
            self.stats.late_tokens += 1;
            return;
        };
        if entry.job != Some(job) || entry.fsm.state().is_terminal() {
            self.stats.late_tokens += 1;
            return;
        }
        if entry.first_token_at_ms.is_none() {
            entry.first_token_at_ms = Some(now);
        }
        entry.gen_tokens = entry.gen_tokens.saturating_add(piece.tokens);
        entry.text.push_str(&piece.text);
        let patch = entry
            .structured
            .as_mut()
            .map_or_else(Vec::new, |tracker| tracker.push(&piece.text));
        let effects = self.framer.offer(
            request,
            crate::framer::TokenPiece {
                text: piece.text,
                tokens: piece.tokens,
                patch,
            },
            now,
        );
        self.apply_framer_effects(request, effects);
    }

    fn apply_framer_effects(&mut self, request: RequestId, effects: Vec<FramerEffect>) {
        for effect in effects {
            match effect {
                FramerEffect::Emit(delta) => {
                    self.fire_request(request, RequestEvent::DeltaArrived, Some(delta))
                }
                FramerEffect::ArmTimer {
                    request,
                    generation,
                    at_ms,
                } => self.framer_timers.arm(at_ms, (request, generation)),
            }
        }
    }

    // ------------------------------------------------------------------ 运行时 / 后端

    fn on_model_loaded(
        &mut self,
        key: &ModelKey,
        footprint: MemoryFootprint,
        worker: WorkerIndex,
        context_kv_mb: u32,
    ) {
        let now = self.now();
        self.models.on_loaded(key, footprint.mapped_mb, now);
        self.affinity.insert(key.clone(), worker);
        // 加载后的每上下文 KV 真值（madai-w16 I-C-14）：之后的 reserve 以它记账；0 = 模型不申报，沿用后端读数。
        if context_kv_mb > 0 {
            self.model_context_kv_mb.insert(key.clone(), context_kv_mb);
        }
        // 加载期间投给该 worker 的工单此刻补引用计数。
        let pending: Vec<JobId> = self
            .jobs
            .iter()
            .filter(|(_, r)| r.model_key.as_ref() == Some(key) && !r.acquired && r.worker.is_some())
            .map(|(j, _)| *j)
            .collect();
        for job in pending {
            let (acquire, _) = self.models.acquire(key, now);
            if acquire == Acquire::Held {
                if let Some(record) = self.jobs.get_mut(&job) {
                    record.acquired = true;
                }
            }
        }
        let package = self
            .jobs
            .values()
            .find(|r| r.model_key.as_ref() == Some(key))
            .and_then(|r| self.requests.get(&r.request))
            .and_then(|e| e.package_id.clone());
        if let Some(backend) = self.ids.register(&key.backend.0) {
            self.observe(TelemetryEvent::ModelLoad {
                backend,
                package,
                phase: madinfer_observe::Phase::Load,
                ms: 0,
                aot_hit: false,
                resident_mb: footprint.resident_mb,
                mapped_mb: footprint.mapped_mb,
            });
        }
    }

    fn apply_runtime_effects(&mut self, effects: Vec<RuntimeEffect>) {
        for effect in effects {
            match effect {
                RuntimeEffect::ArmCoolTimer {
                    key,
                    generation,
                    at_ms,
                } => self.cool_timers.arm(at_ms, (key, generation)),
                RuntimeEffect::Unload { key } => {
                    let sent = self.affinity.get(&key).copied().is_some_and(|worker| {
                        self.pool
                            .as_ref()
                            .is_some_and(|p| p.unload(worker, key.clone()))
                    });
                    match sent {
                        true => {}
                        false => {
                            // 没有 worker 持有它：直接记完成。
                            let now = self.now();
                            self.models.on_unload_finished(&key, now);
                            self.affinity.remove(&key);
                        }
                    }
                }
                RuntimeEffect::DropContexts { keys } => self.release_backend_contexts(&keys),
                RuntimeEffect::Load { .. } | RuntimeEffect::CancelBackgroundJobs => {}
            }
        }
    }

    fn on_signal(&mut self, signal: OsSignal) {
        let now = self.now();
        if let OsSignal::Thermal(state) = signal {
            self.observe(TelemetryEvent::ThermalChange {
                thermal: thermal_of(Some(state)),
            });
        }
        let effects = self.sched.handle(SchedCommand::Signal(signal), now);
        self.apply_sched_effects(effects);
    }

    fn on_config(&mut self, bytes: &[u8]) -> ApplyOutcome {
        let now = self.now();
        let (outcome, effects) = self.config.apply(bytes, now);
        for effect in effects {
            match effect {
                ConfigEffect::Broadcast { version } => {
                    self.mapping = self.config.active_table().clone();
                    let quota = quota_table_of(self.config.active().thresholds.quotas.iter());
                    let effects = self.sched.handle(SchedCommand::ReplaceQuota(quota), now);
                    self.apply_sched_effects(effects);
                    // KV 预算随活动配置换档（超出部分由下一次回收处理，不在此驱逐）。
                    let kv_budget_mb = kv_budget_mb_of(&self.config.active().thresholds, self.tier);
                    self.contexts.set_limit(kv_budget_mb);
                    self.refresh_capability_index();
                    self.log(format!(
                        "core=config_applied version={version} kv_budget_mb={kv_budget_mb}"
                    ));
                }
                ConfigEffect::RecordApplied { version } => {
                    self.observe(TelemetryEvent::ConfigApply {
                        version,
                        outcome: Outcome::Complete,
                    });
                }
                ConfigEffect::RecordRejected { version, reason } => {
                    self.observe(TelemetryEvent::ConfigRejected {
                        version: version.unwrap_or(0),
                        reason,
                    });
                }
                // 持久化经 KvStore 端口（I3 接入）；此刻只留痕。
                ConfigEffect::PersistActive(_) | ConfigEffect::PersistPrevious(_) => {}
            }
        }
        outcome
    }

    // ------------------------------------------------------------------ 观测与留痕

    fn observe(&mut self, event: TelemetryEvent) {
        let now = self.now();
        if let Some(batch) = self.telemetry.offer(event, now) {
            self.emit_batch(&batch);
        }
    }

    fn emit_batch(&mut self, batch: &Batch) {
        self.stats.telemetry_batches += 1;
        self.telemetry_sink
            .emit(&batch.to_json(&self.telemetry_meta));
    }

    fn log(&self, facts: String) {
        self.logger.log(LogLevel::Info, "madinfer-api", &facts);
    }
}

fn to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn count_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// config `thresholds.kv_budget_mb[tier]`（键 = `Tier::wire()`：`low` / `mid` / `flagship`）；未配置的档位（含 `unknown`）0 = 不留 KV。
/// 池 KV 预算（MB）：config `thresholds.kv_budget_mb[tier]` 显式给了就用它（显式 0 = 本档关池）；**未配置该档**回内置缺省
/// [`DEFAULT_KV_BUDGET_MB`]（madai-w16 首日 K3：0.14.0 起池条目 = 后端实际驻留，若无配置的站预算为 0 则上下文永不驻留、
/// T6 命中读数恒 0——生产站不能依赖"配置到达"才有有界复用；缺省按档位保守给，config 到达即换档）。
fn kv_budget_mb_of(thresholds: &madinfer_config::Thresholds, tier: Tier) -> u32 {
    thresholds
        .kv_budget_mb
        .get(tier.wire())
        .copied()
        .unwrap_or_else(|| default_kv_budget_mb(tier))
}

/// 内置缺省 KV 预算（MB，按档位；config 未给该档时生效）：Low / Unknown 32、Mid 64、Flagship 128——
/// 135M～0.5B 量级模型单上下文 KV 约 8～24 MB，缺省能驻留 2～8 个会话上下文；上限由 config 按机型实测调。
pub const DEFAULT_KV_BUDGET_MB: [(Tier, u32); 4] = [
    (Tier::Low, 32),
    (Tier::Mid, 64),
    (Tier::Flagship, 128),
    (Tier::Unknown, 32),
];

fn default_kv_budget_mb(tier: Tier) -> u32 {
    DEFAULT_KV_BUDGET_MB
        .iter()
        .find(|(t, _)| *t == tier)
        .map(|(_, mb)| *mb)
        .unwrap_or(32)
}

const fn cancel_reason_of(reason: SchedCancelReason) -> madinfer_backend::CancelReason {
    match reason {
        SchedCancelReason::Caller => madinfer_backend::CancelReason::Caller,
        SchedCancelReason::Deadline => madinfer_backend::CancelReason::Deadline,
        SchedCancelReason::Preempt => madinfer_backend::CancelReason::Preempt,
        SchedCancelReason::Drain => madinfer_backend::CancelReason::Shutdown,
        SchedCancelReason::Pressure => madinfer_backend::CancelReason::Pressure,
    }
}

const fn thermal_of(state: Option<ThermalState>) -> Thermal {
    match state {
        None | Some(ThermalState::Nominal) => Thermal::Nominal,
        Some(ThermalState::Fair) => Thermal::Fair,
        Some(ThermalState::Serious) => Thermal::Serious,
        Some(ThermalState::Critical) => Thermal::Critical,
    }
}

const fn gate_of(gate: GateName) -> Option<Gate> {
    match gate {
        GateName::Thermal => Some(Gate::Thermal),
        GateName::Memory => Some(Gate::Memory),
        GateName::Charging => Some(Gate::Charging),
        GateName::Idle => Some(Gate::Idle),
        GateName::AppWindow | GateName::HostBusy | GateName::LowPower => None,
    }
}

fn quota_table_of<'a>(
    quotas: impl Iterator<Item = (&'a String, &'a madinfer_config::QuotaDoc)>,
) -> QuotaTable {
    let mut per_business = Vec::new();
    let mut default = None;
    for (business, quota) in quotas {
        let q = madinfer_sched::Quota {
            window_ms: u64::from(quota.window_s) * 1_000,
            budget_cost: quota.cost_ms_mb,
        };
        match business.parse::<u16>().ok().and_then(BusinessId::new) {
            Some(b) => per_business.push((b, q)),
            None if business == "default" => default = Some(q),
            None => {}
        }
    }
    QuotaTable::new(per_business, default)
}
