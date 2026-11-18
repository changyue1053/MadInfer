//! worker 池：按 [`SlotClass`] 各起 N 个 `std::thread`（容量来自 [`SlotTable`]），每个 worker 一条有界 `mpsc` 收 [`WorkOrder`]。
//! worker 持 `Arc<RegistrySnapshot>` 找后端，`load` / `open_context` / `generate`，token 以 [`CoreCommand::TokenArrived`] 回投，
//! 结束以 [`CoreCommand::WorkerReport`] 回投；取消 = 核 actor 翻 `CancelToken`，worker 不被打断、自己在下一 token 边界退出。
//!
//! 模型归属：`Box<dyn Model>` 由加载它的 worker 独占缓存（`Model::unload(self: Box<Self>)` 需要所有权），核 actor 记 `ModelKey → WorkerIndex`
//! 亲和并把同一模型的 Job 投到同一 worker；跨 worker 共享句柄随 I3 的 `Arc<dyn Model>` 形态落地。
//! 时间只从注入的 `Clock` 读（run_ms）；片间 `thread::sleep` 是替身的事，worker 本身不睡。
//!
//! 上下文驻留（madai-w16 I-C-14）：worker 按 [`ContextKey`] 驻留 `Box<dyn Context>`（`Send` 不 `Sync`，只在本线程调用）。池（control 线程）
//! 是记账方、worker 是持有方——工单携带池的裁决 [`ContextReservation`]：`reused` 命中即复用（不再 `open_context`），新建则开后驻留；
//! 无预留（请求无键 / 池 `OverBudget`）逐轮开关不驻留。释放方向单向：池回键（`drop_prefix_keys` / 腾预算 / 压力回收）→
//! [`WorkerEnvelope::DropContexts`] → worker Drop 句柄（真释放后端 KV）。worker 自己只在两处丢句柄并把**事实**回投让池对账：
//! ① 后端席位（`caps.concurrency`）不够时按本 worker LRU 腾（[`WorkOutcome::evicted_contexts`]）；② 生成失败 / 取消不驻留
//! （[`WorkOutcome::context_retained`] 为假）；模型卸载时随模型丢（[`CoreCommand::ModelUnloaded`] 携键）。

use std::collections::HashMap;
use std::io;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{Builder, JoinHandle};

use madinfer_backend::{
    BackendError, CancelReason, CancelToken, Constraint, Context, ContextOptions, GenerateOptions,
    GenerateStats, LoadOpts, Model, RegistrySnapshot, SinkControl, TokenPiece, TokenSink, Turn,
};
use madinfer_core_types::SlotClass;
use madinfer_core_types::{BackendId, ContextKey, DegradeKind, FatalKind, InferError, RequestId};
use madinfer_host::{Clock, ThreadHooks, WorkerClass};
use madinfer_runtime::ModelKey;
use madinfer_sched::{JobCtx, JobId, SlotTable, WorkerReport};

use crate::core::CoreCommand;

/// 每个 worker 邮箱容量：同一模型（亲和）的 Job 排在这里等；满则核 actor 以 Busy 拒绝。
pub const WORKER_MAILBOX_CAPACITY: usize = 64;

/// worker 编号（全池唯一）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerIndex(pub u16);

/// 一份工单：核 actor 组装，worker 执行。
#[derive(Debug)]
pub struct WorkOrder {
    /// Job。
    pub job: JobId,
    /// 请求。
    pub request: RequestId,
    /// 调度上下文。
    pub ctx: JobCtx,
    /// 后端。
    pub backend: BackendId,
    /// 模型键（worker 缓存键）。
    pub model_key: ModelKey,
    /// 加载参数（缓存未命中时用）。
    pub load: LoadOpts,
    /// 上下文参数。
    pub context_options: ContextOptions,
    /// 输入（载荷）。
    pub turn: Turn,
    /// 采样参数。
    pub options: GenerateOptions,
    /// 约束。
    pub constraint: Option<Constraint>,
    /// 取消令牌（核 actor 持另一份）。
    pub cancel: CancelToken,
    /// 池为本 Job 预留的上下文（madai-w16 I-C-14）；None = 逐轮开关、不驻留。
    pub context: Option<ContextReservation>,
}

/// 工单携带的上下文预留（madai-w16 I-C-14；池的裁决随单下发，worker 不自己决定驻留）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextReservation {
    /// 键（请求的 `context`）。
    pub key: ContextKey,
    /// 池判为复用（`Reserve::Reused`，该键驻留在本 worker）：命中即复用、不再 `open_context`。为假 = 池刚新建——本 worker 若仍持
    /// 同键旧句柄（`DropContexts` 未送达的残留），先丢再开：关户后的键不得复用旧 KV（T6）。
    pub reused: bool,
}

/// worker 收到的信封。
pub enum WorkerEnvelope {
    /// 执行一份工单。
    Order(Box<WorkOrder>),
    /// 卸载缓存里的模型（回投 `ModelUnloaded`）。
    Unload(ModelKey),
    /// 释放这些生成上下文的后端 KV（madai-w15 I-C-13；池 `drop_prefix` / 腾预算 / 压力回收给出的键）。不回投。
    DropContexts(Vec<ContextKey>),
    /// 退出。
    Shutdown,
}

/// 投递失败。
#[derive(Debug)]
pub enum DispatchError {
    /// 该槽类别没有 worker（SlotTable 未登记容量）。
    NoWorker(Box<WorkOrder>),
    /// 目标 worker 邮箱已满。
    Full(Box<WorkOrder>),
    /// 目标 worker 已退出。
    Gone(Box<WorkOrder>),
}

impl DispatchError {
    /// 取回工单。
    pub fn into_order(self) -> Box<WorkOrder> {
        match self {
            Self::NoWorker(o) | Self::Full(o) | Self::Gone(o) => o,
        }
    }
}

struct Lane {
    index: WorkerIndex,
    slot: SlotClass,
    tx: SyncSender<WorkerEnvelope>,
    /// 已投未回执的工单数（核 actor 视角）。
    pending: u32,
}

/// worker 池（核 actor 线程独占）。
pub struct WorkerPool {
    lanes: Vec<Lane>,
    by_slot: HashMap<SlotClass, Vec<usize>>,
    threads: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    /// 按槽表容量起线程；`mailbox` 是核 actor 邮箱的发送端。
    pub fn spawn(
        slots: &SlotTable,
        classes: &[SlotClass],
        snapshot: Arc<RegistrySnapshot>,
        clock: Arc<dyn Clock>,
        hooks: Arc<dyn ThreadHooks>,
        mailbox: SyncSender<CoreCommand>,
    ) -> io::Result<Self> {
        let mut pool = Self {
            lanes: Vec::new(),
            by_slot: HashMap::new(),
            threads: Vec::new(),
        };
        let mut next = 0u16;
        for slot in classes {
            for _ in 0..slots.capacity(*slot) {
                let index = WorkerIndex(next);
                next = next.saturating_add(1);
                let (tx, rx) = mpsc::sync_channel::<WorkerEnvelope>(WORKER_MAILBOX_CAPACITY);
                let worker = Worker {
                    index,
                    class: worker_class(*slot),
                    snapshot: Arc::clone(&snapshot),
                    clock: Arc::clone(&clock),
                    hooks: Arc::clone(&hooks),
                    mailbox: mailbox.clone(),
                    models: HashMap::new(),
                    resident: HashMap::new(),
                    use_seq: 0,
                };
                let thread = Builder::new()
                    .name(format!("madinfer-worker-{}", index.0))
                    .spawn(move || worker.run(rx))?;
                pool.threads.push(thread);
                pool.by_slot
                    .entry(*slot)
                    .or_default()
                    .push(pool.lanes.len());
                pool.lanes.push(Lane {
                    index,
                    slot: *slot,
                    tx,
                    pending: 0,
                });
            }
        }
        Ok(pool)
    }

    /// worker 总数。
    pub fn len(&self) -> usize {
        self.lanes.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// 投递：有亲和则投亲和 worker；否则投该槽类别里在途最少的 worker。返回接单的 worker（= [`Self::pick`] + [`Self::dispatch_to`]）。
    pub fn dispatch(
        &mut self,
        order: Box<WorkOrder>,
        prefer: Option<WorkerIndex>,
    ) -> Result<WorkerIndex, DispatchError> {
        match self.pick(order.ctx.slot, prefer) {
            Some(worker) => self.dispatch_to(worker, order).map(|()| worker),
            None => Err(DispatchError::NoWorker(order)),
        }
    }

    /// 选 worker 不投递（madai-w16 I-C-14：核 actor先选后 `reserve` 上下文，再把池的裁决随单下发）：有亲和且 lane 在 → 亲和
    /// worker；否则该槽类别里在途最少的；该槽无 worker → None。
    pub fn pick(&self, slot: SlotClass, prefer: Option<WorkerIndex>) -> Option<WorkerIndex> {
        let lane = match prefer.and_then(|w| self.lanes.iter().position(|l| l.index == w)) {
            Some(i) => Some(i),
            None => self
                .by_slot
                .get(&slot)
                .and_then(|idxs| idxs.iter().copied().min_by_key(|i| self.lanes[*i].pending)),
        };
        lane.map(|i| self.lanes[i].index)
    }

    /// 投给指定 worker（[`Self::pick`] 选出的）。
    pub fn dispatch_to(
        &mut self,
        worker: WorkerIndex,
        order: Box<WorkOrder>,
    ) -> Result<(), DispatchError> {
        let Some(i) = self.lanes.iter().position(|l| l.index == worker) else {
            return Err(DispatchError::NoWorker(order));
        };
        match self.lanes[i].tx.try_send(WorkerEnvelope::Order(order)) {
            Ok(()) => {
                self.lanes[i].pending += 1;
                Ok(())
            }
            Err(TrySendError::Full(WorkerEnvelope::Order(o))) => Err(DispatchError::Full(o)),
            Err(TrySendError::Disconnected(WorkerEnvelope::Order(o))) => {
                Err(DispatchError::Gone(o))
            }
            Err(_) => unreachable!("只投了 Order"),
        }
    }

    /// 工单回执已到：在途计数减一。
    pub fn mark_reported(&mut self, worker: WorkerIndex) {
        if let Some(l) = self.lanes.iter_mut().find(|l| l.index == worker) {
            l.pending = l.pending.saturating_sub(1);
        }
    }

    /// 让某 worker 卸载模型（阻塞式保交付：卸载不可丢）。
    pub fn unload(&self, worker: WorkerIndex, key: ModelKey) -> bool {
        self.lanes
            .iter()
            .find(|l| l.index == worker)
            .is_some_and(|l| l.tx.send(WorkerEnvelope::Unload(key)).is_ok())
    }

    /// 让全部 worker 释放这些上下文的后端 KV（madai-w15 I-C-13）：广播而不按池亲和定点——上下文可能因重排 / 抢占换过 worker，
    /// 没有的 worker 收到即无事。`try_send` 不阻塞 control 线程（worker 正阻塞在向核邮箱回投 token 时 `send` 会互等）；
    /// 邮箱满 / 已退出的 lane 计入返回值（未送达 lane 数），由 owner 留痕。
    pub fn drop_contexts(&self, keys: &[ContextKey]) -> usize {
        if keys.is_empty() {
            return 0;
        }
        self.lanes
            .iter()
            .filter(|l| {
                l.tx.try_send(WorkerEnvelope::DropContexts(keys.to_vec()))
                    .is_err()
            })
            .count()
    }

    /// 某 worker 的槽类别。
    pub fn slot_of(&self, worker: WorkerIndex) -> Option<SlotClass> {
        self.lanes
            .iter()
            .find(|l| l.index == worker)
            .map(|l| l.slot)
    }

    /// 关停：发 Shutdown、丢弃发送端（worker 的 `recv` 随之返回 Disconnected）、join 全部线程。
    /// worker 可能正阻塞在向核邮箱回投 token，调用方须保证邮箱仍被消费；否则用 [`WorkerPool::shutdown_draining`]。
    pub fn shutdown(self) {
        self.shutdown_draining(|| true);
    }

    /// 关停并排水：每轮调用 `drain`（消费核邮箱一小段时间；返回 false 表示邮箱已断开）直到全部线程退出，再 join。
    pub fn shutdown_draining(self, mut drain: impl FnMut() -> bool) {
        for lane in &self.lanes {
            let _ = lane.tx.try_send(WorkerEnvelope::Shutdown);
        }
        drop(self.lanes);
        while !self.threads.iter().all(|t| t.is_finished()) {
            if !drain() {
                break;
            }
        }
        for t in self.threads {
            let _ = t.join();
        }
    }
}

/// 线程类别按槽位定、不按请求 QoS：Cpu / Gpu / Npu 槽（内联引擎）→ `Interactive`，External 槽（系统模型 / 云端适配器）→ `Background`。
/// 三档 QoS 的 Job 都可能投到任一类 worker——Async / Background 的区分由 sched（门控准入 / 配额 / 抢占）承担，不靠线程优先级
/// （madai-w09 I-C-08 总控裁决：不按 QoS 拆池，等真 llama 后端出现两档争用读数再议；宿主映射口径见 madinfer.h §5b）。
const fn worker_class(slot: SlotClass) -> WorkerClass {
    match slot {
        SlotClass::Cpu | SlotClass::Gpu | SlotClass::Npu => WorkerClass::Interactive,
        SlotClass::External(_) => WorkerClass::Background,
    }
}

struct Worker {
    index: WorkerIndex,
    class: WorkerClass,
    snapshot: Arc<RegistrySnapshot>,
    clock: Arc<dyn Clock>,
    hooks: Arc<dyn ThreadHooks>,
    mailbox: SyncSender<CoreCommand>,
    models: HashMap<ModelKey, Box<dyn Model>>,
    /// 按键驻留的生成上下文（madai-w16 I-C-14；模块注"上下文驻留"）。
    resident: HashMap<ContextKey, ResidentContext>,
    /// 驻留上下文的使用序（worker 内 LRU 依据；不读时钟——同毫秒多次使用也要有序）。
    use_seq: u64,
}

/// worker 驻留的一个生成上下文：后端句柄 + 所属模型（腾席位按模型计、卸载随模型丢）+ 最近使用序。
struct ResidentContext {
    model: ModelKey,
    context: Box<dyn Context>,
    used_seq: u64,
}

/// worker 的一次执行结果（进 `CoreCommand::WorkerReport`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkOutcome {
    /// 调度回执。
    pub report: WorkerReport,
    /// 生成统计（Finished 时有）。
    pub stats: Option<GenerateStats>,
    /// 后端原始错误（崩溃计数用）。
    pub backend_error: Option<BackendError>,
    /// 本轮结束后 worker 仍按工单的键驻留该上下文（madai-w16 I-C-14）：有预留且生成成功 → 真；失败 / 取消 / 让出 / 无预留 → 假。
    /// 池据此对账：终态回执为假而池仍有该键 → 键离池（读数与后端实际驻留一致）。
    pub context_retained: bool,
    /// 为给本轮腾后端席位（`caps.concurrency` 上限）而被本 worker 丢弃的驻留上下文键（LRU；madai-w16 I-C-14）。池据此让键离池。
    pub evicted_contexts: Vec<ContextKey>,
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<WorkerEnvelope>) {
        self.hooks.on_worker_started(self.class);
        loop {
            match rx.recv() {
                Ok(WorkerEnvelope::Order(order)) => {
                    let outcome = self.execute(&order);
                    if self
                        .mailbox
                        .send(CoreCommand::WorkerReport {
                            job: order.job,
                            request: order.request,
                            worker: self.index,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(WorkerEnvelope::Unload(key)) => {
                    // 上下文句柄依赖模型：先丢该模型的驻留上下文（键随回执回池对账），再卸载。
                    let contexts = self.drop_model_contexts(&key);
                    if let Some(model) = self.models.remove(&key) {
                        model.unload();
                    }
                    if self
                        .mailbox
                        .send(CoreCommand::ModelUnloaded { key, contexts })
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(WorkerEnvelope::DropContexts(keys)) => self.drop_contexts(&keys),
                Ok(WorkerEnvelope::Shutdown) | Err(_) => break,
            }
        }
        // 关停：句柄先于模型释放。
        self.resident.clear();
        for (_, model) in self.models.drain() {
            model.unload();
        }
    }

    fn execute(&mut self, order: &WorkOrder) -> WorkOutcome {
        let started = self.clock.now_ms();
        let Some(backend) = self.snapshot.get(&order.backend) else {
            return self.failed(BackendError::ModelMissing, started, 0, Vec::new());
        };
        if !self.models.contains_key(&order.model_key) {
            match backend.load(&order.load) {
                Ok(model) => {
                    let footprint = model.footprint();
                    let context_kv_mb = model.context_kv_mb();
                    self.models.insert(order.model_key.clone(), model);
                    let _ = self.mailbox.send(CoreCommand::ModelLoaded {
                        key: order.model_key.clone(),
                        footprint,
                        worker: self.index,
                        context_kv_mb,
                    });
                }
                Err(error) => {
                    let _ = self.mailbox.send(CoreCommand::ModelLoadFailed {
                        key: order.model_key.clone(),
                        error,
                    });
                    return self.failed(error, started, 0, Vec::new());
                }
            }
        }
        // 上下文：池判复用且本 worker 持同键同模型句柄 → 直接用；否则（新建 / 无预留 / 句柄不在——`DropContexts` 未送达的同键
        // 旧句柄在此一并丢掉）先按后端席位上限腾，再 `open_context`。
        let reused = order.context.as_ref().and_then(|reservation| {
            let held = self.resident.remove(&reservation.key)?;
            (reservation.reused && held.model == order.model_key).then_some(held.context)
        });
        let mut evicted = Vec::new();
        let (mut context, peak) = match reused {
            Some(context) => {
                let peak = self
                    .models
                    .get(&order.model_key)
                    .map_or(0, |m| m.footprint().resident_mb);
                (context, peak)
            }
            None => {
                let concurrency = self
                    .snapshot
                    .caps(&order.backend)
                    .map_or(1, |caps| caps.concurrency);
                evicted = self.vacate(&order.model_key, concurrency);
                let Some(model) = self.models.get(&order.model_key) else {
                    return self.failed(BackendError::ModelMissing, started, 0, evicted);
                };
                let peak = model.footprint().resident_mb;
                match model.open_context(&order.context_options) {
                    Ok(context) => (context, peak),
                    Err(error) => return self.failed(error, started, peak, evicted),
                }
            }
        };
        let mut sink = MailboxSink {
            mailbox: &self.mailbox,
            request: order.request,
            job: order.job,
        };
        let result = context.generate(
            &order.turn,
            &order.options,
            order.constraint.as_ref(),
            &mut sink,
            &order.cancel,
        );
        // 驻留只在"有预留且生成成功"：失败 / 取消 / 让出的句柄 KV 处于半途，不留（重排的 Job 从头生成，既有中间态）。
        let context_retained = match (&result, order.context.as_ref()) {
            (Ok(_), Some(reservation)) => {
                self.use_seq += 1;
                self.resident.insert(
                    reservation.key.clone(),
                    ResidentContext {
                        model: order.model_key.clone(),
                        context,
                        used_seq: self.use_seq,
                    },
                );
                true
            }
            _ => {
                drop(context);
                false
            }
        };
        let run_ms = elapsed(self.clock.now_ms(), started);
        let mut outcome = match result {
            Ok(stats) => WorkOutcome {
                report: WorkerReport::Finished {
                    run_ms,
                    peak_memory_mb: peak,
                },
                stats: Some(stats),
                backend_error: None,
                context_retained: false,
                evicted_contexts: Vec::new(),
            },
            Err(BackendError::Cancelled) => match order.cancel.reason() {
                // 抢占 / 压力：让出，调度器重排；不上抛。
                Some(CancelReason::Preempt | CancelReason::Pressure) => WorkOutcome {
                    report: WorkerReport::Yielded {
                        run_ms,
                        peak_memory_mb: peak,
                    },
                    stats: None,
                    backend_error: Some(BackendError::Cancelled),
                    context_retained: false,
                    evicted_contexts: Vec::new(),
                },
                Some(CancelReason::Deadline) => WorkOutcome {
                    report: WorkerReport::Failed {
                        error: InferError::Degradable {
                            kind: DegradeKind::Deadline,
                        },
                        run_ms,
                        peak_memory_mb: peak,
                    },
                    stats: None,
                    backend_error: Some(BackendError::Cancelled),
                    context_retained: false,
                    evicted_contexts: Vec::new(),
                },
                Some(CancelReason::Caller | CancelReason::Shutdown) | None => WorkOutcome {
                    report: WorkerReport::Failed {
                        error: InferError::Fatal {
                            kind: FatalKind::Cancelled,
                            detail: 0,
                        },
                        run_ms,
                        peak_memory_mb: peak,
                    },
                    stats: None,
                    backend_error: Some(BackendError::Cancelled),
                    context_retained: false,
                    evicted_contexts: Vec::new(),
                },
            },
            Err(error) => WorkOutcome {
                report: WorkerReport::Failed {
                    error: error.into(),
                    run_ms,
                    peak_memory_mb: peak,
                },
                stats: None,
                backend_error: Some(error),
                context_retained: false,
                evicted_contexts: Vec::new(),
            },
        };
        outcome.context_retained = context_retained;
        outcome.evicted_contexts = evicted;
        outcome
    }

    /// 给本模型腾一个后端席位（`caps.concurrency` 上限，madai-w16 I-C-14）：该模型的驻留数 + 1 > 上限时按本 worker LRU 丢驻留
    /// 上下文（句柄 Drop 即释放 KV 与席位），返回被丢的键让池对账。仿真核 / testkit 缺省 `concurrency = 1` → 同模型最多驻留 1 个，
    /// 换键即腾；真 llama 后端按其并发席位申报。
    fn vacate(&mut self, model: &ModelKey, concurrency: u8) -> Vec<ContextKey> {
        let limit = usize::from(concurrency).max(1);
        let mut evicted = Vec::new();
        loop {
            let held = self.resident.values().filter(|r| r.model == *model).count();
            if held < limit {
                break;
            }
            let victim = self
                .resident
                .iter()
                .filter(|(_, r)| r.model == *model)
                .min_by_key(|(k, r)| (r.used_seq, k.as_str().to_owned()))
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else {
                break;
            };
            self.resident.remove(&victim);
            evicted.push(victim);
        }
        evicted
    }

    /// 释放这些键驻留的后端 KV——池（control 线程）判定这些键已离池后的落地点（madai-w15 I-C-13 空落地点，madai-w16 I-C-14 填）：
    /// 句柄 Drop 即释放；本 worker 没有的键无事（`WorkerPool::drop_contexts` 是广播）。不回投。
    fn drop_contexts(&mut self, keys: &[ContextKey]) {
        for key in keys {
            self.resident.remove(key);
        }
    }

    /// 丢该模型的全部驻留上下文（卸载前），返回键（字典序）让池对账。
    fn drop_model_contexts(&mut self, model: &ModelKey) -> Vec<ContextKey> {
        let mut keys: Vec<ContextKey> = self
            .resident
            .iter()
            .filter(|(_, r)| r.model == *model)
            .map(|(k, _)| k.clone())
            .collect();
        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for key in &keys {
            self.resident.remove(key);
        }
        keys
    }

    fn failed(
        &self,
        error: BackendError,
        started: u64,
        peak: u32,
        evicted_contexts: Vec<ContextKey>,
    ) -> WorkOutcome {
        WorkOutcome {
            report: WorkerReport::Failed {
                error: error.into(),
                run_ms: elapsed(self.clock.now_ms(), started),
                peak_memory_mb: peak,
            },
            stats: None,
            backend_error: Some(error),
            context_retained: false,
            evicted_contexts,
        }
    }
}

fn elapsed(now: u64, started: u64) -> u32 {
    u32::try_from(now.saturating_sub(started)).unwrap_or(u32::MAX)
}

/// 把 token 片回投核 actor 邮箱（阻塞式 `send`：token 不可丢；核 actor 只消费不阻塞在 worker 上，不会互等）。
struct MailboxSink<'a> {
    mailbox: &'a SyncSender<CoreCommand>,
    request: RequestId,
    job: JobId,
}

impl TokenSink for MailboxSink<'_> {
    fn on_token(&mut self, piece: TokenPiece) -> SinkControl {
        match self.mailbox.send(CoreCommand::TokenArrived {
            request: self.request,
            job: self.job,
            piece,
        }) {
            Ok(()) => SinkControl::Continue,
            Err(_) => SinkControl::Stop,
        }
    }
}
