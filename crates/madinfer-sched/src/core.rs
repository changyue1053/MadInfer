//! sans-io 调度核：命令进、效果出；不碰线程、IO、墙钟。
//!
//! 单消费者语义由 owner（[`crate::SchedulerActor`] 或测试）保证：同一时刻只有一个调用方在 `handle` / `tick`。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use madinfer_core_types::{InferError, QoS, RequestId, RetryKind};
use madinfer_host::{LogLevel, Logger, NoopLogger, OsSignal};

use crate::fsm::{build_job_fsm, JobEffect, JobEntry, JobEvent, JobState};
use crate::gates::{Admission, DefaultGatePolicy, GateName, GatePolicy, Gates};
use crate::job::{threads_for, Job, JobCtx, JobId, SlotTable};
use crate::quota::{downgrade, QuotaLedger, QuotaTable, QuotaVerdict};
use madinfer_core_types::SlotClass;

/// 取消原因（进 worker 的 CancelToken 与回执）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelReason {
    /// 调用方取消。
    Caller,
    /// 截止时刻到。
    Deadline,
    /// 被交互档抢占。
    Preempt,
    /// 关停排水。
    Drain,
    /// 内存 / 热压力驱逐。
    Pressure,
}

/// worker 回执。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerReport {
    /// 完成。
    Finished {
        /// 实际运行毫秒。
        run_ms: u32,
        /// 峰值内存 MB。
        peak_memory_mb: u32,
    },
    /// 失败（含取消回执：error = Fatal(Cancelled)）。
    Failed {
        /// 错误。
        error: InferError,
        /// 实际运行毫秒。
        run_ms: u32,
        /// 峰值内存 MB。
        peak_memory_mb: u32,
    },
    /// 已让出（抢占后可重排）。
    Yielded {
        /// 实际运行毫秒。
        run_ms: u32,
        /// 峰值内存 MB。
        peak_memory_mb: u32,
    },
}

impl WorkerReport {
    const fn cost(&self) -> u64 {
        match *self {
            Self::Finished {
                run_ms,
                peak_memory_mb,
            }
            | Self::Failed {
                run_ms,
                peak_memory_mb,
                ..
            }
            | Self::Yielded {
                run_ms,
                peak_memory_mb,
            } => run_ms as u64 * peak_memory_mb as u64,
        }
    }
}

/// 调度命令。
#[derive(Clone, Debug)]
pub enum SchedCommand {
    /// 提交（JobId 由调用方从 `JobIdSource` 预分配，回执按它归因）。
    Submit {
        /// 预分配 id。
        id: JobId,
        /// Job。
        job: Job,
    },
    /// 取消。
    Cancel {
        /// Job。
        job: JobId,
        /// 原因。
        reason: CancelReason,
    },
    /// worker 回执。
    Report {
        /// Job。
        job: JobId,
        /// 回执。
        report: WorkerReport,
    },
    /// 资源信号。
    Signal(OsSignal),
    /// 配额表热更。
    ReplaceQuota(QuotaTable),
    /// 排水：先取消后台、再异步、交互等到超时。
    Drain {
        /// 超时毫秒。
        timeout_ms: u64,
    },
}

/// 调度效果（数据；由 actor 交给 worker 池 / 回调面）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedEffect {
    /// 在槽上启动。
    Start {
        /// 上下文。
        ctx: JobCtx,
    },
    /// 向 worker 发取消。
    Cancel {
        /// Job。
        job: JobId,
        /// 原因。
        reason: CancelReason,
    },
    /// 推迟（观测 sched_wait）。
    Deferred {
        /// Job。
        job: JobId,
        /// 到期时刻。
        until_ms: u64,
        /// 门。
        gate: GateName,
    },
    /// 终态：完成。
    Done {
        /// Job。
        job: JobId,
        /// 请求。
        request: RequestId,
    },
    /// 终态：失败。
    Failed {
        /// Job。
        job: JobId,
        /// 请求。
        request: RequestId,
        /// 错误。
        error: InferError,
    },
    /// 终态：取消。
    Cancelled {
        /// Job。
        job: JobId,
        /// 请求。
        request: RequestId,
        /// 原因。
        reason: CancelReason,
    },
    /// 排水完成（无活动 Job）。
    Drained,
}

/// 计数台账（观测 / 断言面）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedStats {
    /// 提交。
    pub submitted: u64,
    /// 启动。
    pub started: u64,
    /// 完成。
    pub done: u64,
    /// 失败（含拒绝）。
    pub failed: u64,
    /// 取消。
    pub cancelled: u64,
    /// 推迟次数。
    pub deferred: u64,
    /// 门控拒绝。
    pub gate_rejected: u64,
    /// 抢占发起。
    pub preempted: u64,
    /// 配额降档。
    pub quota_downgraded: u64,
    /// 配额拒绝。
    pub quota_rejected: u64,
    /// 陈旧定时器票（哑火）。
    pub stale_timers: u64,
    /// 队列深度峰值。
    pub max_queue_depth: usize,
    /// FSM 非法事件累计。
    pub unexpected_events: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct QueueKey {
    deadline_at_ms: u64,
    priority: u8,
    seq: u64,
    job: JobId,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerKind {
    Deadline { job: JobId },
    Defer { job: JobId, generation: u64 },
    Drain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Timer {
    at_ms: u64,
    seq: u64,
    kind: TimerKind,
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.at_ms, self.seq).cmp(&(other.at_ms, other.seq))
    }
}

struct Slot {
    job: Job,
    effective_qos: QoS,
    entry: JobEntry,
    queue_generation: u64,
    /// 已因配额降过一档：再判 Downgrade 不再降（"超额降一档，再超额才 Reject"）。
    downgraded: bool,
}

/// 调度核。
pub struct SchedCore {
    policy: Box<dyn GatePolicy>,
    slots: SlotTable,
    quota: QuotaLedger,
    gates: Gates,
    jobs: HashMap<JobId, Slot>,
    queues: [BinaryHeap<Reverse<QueueKey>>; 3],
    seq: u64,
    timers: BinaryHeap<Reverse<Timer>>,
    timer_seq: u64,
    draining: Option<u64>,
    drained_emitted: bool,
    big_cores: u8,
    logger: Arc<dyn Logger>,
    stats: SchedStats,
}

/// 门控拒绝的建议重试间隔。
pub const GATE_REJECT_RETRY_AFTER_MS: u32 = 2_000;
/// 配额拒绝的建议重试间隔。
pub const QUOTA_REJECT_RETRY_AFTER_MS: u32 = 60_000;

fn queue_index(qos: QoS) -> usize {
    match qos {
        QoS::Interactive => 0,
        QoS::Async => 1,
        QoS::Background => 2,
    }
}

impl SchedCore {
    /// 建核：槽表 + 配额表 + 缺省门控策略。
    pub fn new(slots: SlotTable, quota: QuotaTable, big_cores: u8) -> Self {
        Self::with_policy(
            slots,
            quota,
            big_cores,
            Box::new(DefaultGatePolicy),
            Arc::new(NoopLogger),
        )
    }

    /// 建核：自定义门控策略与日志。
    pub fn with_policy(
        slots: SlotTable,
        quota: QuotaTable,
        big_cores: u8,
        policy: Box<dyn GatePolicy>,
        logger: Arc<dyn Logger>,
    ) -> Self {
        Self {
            policy,
            slots,
            quota: QuotaLedger::new(quota),
            gates: Gates::default(),
            jobs: HashMap::new(),
            queues: [BinaryHeap::new(), BinaryHeap::new(), BinaryHeap::new()],
            seq: 0,
            timers: BinaryHeap::new(),
            timer_seq: 0,
            draining: None,
            drained_emitted: false,
            big_cores,
            logger,
            stats: SchedStats::default(),
        }
    }

    /// 台账。
    pub fn stats(&self) -> SchedStats {
        self.stats
    }

    /// 门控读数。
    pub fn gates(&self) -> &Gates {
        &self.gates
    }

    /// 槽表。
    pub fn slots(&self) -> &SlotTable {
        &self.slots
    }

    /// 某 Job 的状态（不存在 = 已终态并移除）。
    pub fn job_state(&self, job: JobId) -> Option<JobState> {
        self.jobs.get(&job).map(|s| s.entry.fsm.state())
    }

    /// 某 Job 的实际执行档。
    pub fn effective_qos(&self, job: JobId) -> Option<QoS> {
        self.jobs.get(&job).map(|s| s.effective_qos)
    }

    /// 活动 Job 数（非终态）。
    pub fn active_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// 下一个定时器到期时刻（actor 据此 `recv_timeout`；None = 无定时器，阻塞等命令）。
    pub fn next_wakeup(&self) -> Option<u64> {
        self.timers.peek().map(|Reverse(t)| t.at_ms)
    }

    /// 是否已排水完毕。
    pub fn is_drained(&self) -> bool {
        self.draining.is_some() && self.jobs.is_empty()
    }

    /// 处理一条命令。
    pub fn handle(&mut self, command: SchedCommand, now_ms: u64) -> Vec<SchedEffect> {
        let mut out = Vec::new();
        match command {
            SchedCommand::Submit { id, job } => self.submit(id, job, now_ms, &mut out),
            SchedCommand::Cancel { job, reason } => {
                self.fire(job, &JobEvent::CancelRequested { reason }, now_ms, &mut out)
            }
            SchedCommand::Report { job, report } => self.report(job, report, now_ms, &mut out),
            SchedCommand::Signal(signal) => self.gates.apply(signal, now_ms),
            SchedCommand::ReplaceQuota(table) => self.quota.replace_table(table),
            SchedCommand::Drain { timeout_ms } => self.drain(timeout_ms, now_ms, &mut out),
        }
        self.try_schedule(now_ms, &mut out);
        self.emit_drained_if_due(&mut out);
        out
    }

    /// 时间推进：到期定时器 → 重新调度。
    pub fn tick(&mut self, now_ms: u64) -> Vec<SchedEffect> {
        let mut out = Vec::new();
        while let Some(Reverse(timer)) = self.timers.peek().copied() {
            if timer.at_ms > now_ms {
                break;
            }
            self.timers.pop();
            match timer.kind {
                TimerKind::Deadline { job } => {
                    if self.jobs.contains_key(&job) {
                        self.fire(job, &JobEvent::DeadlineReached, now_ms, &mut out);
                    }
                }
                TimerKind::Defer { job, generation } => {
                    let live = self.jobs.get(&job).is_some_and(|s| {
                        s.entry.defer_generation == generation
                            && s.entry.fsm.state() == JobState::Deferred
                    });
                    if live {
                        self.fire(job, &JobEvent::DeferElapsed, now_ms, &mut out);
                        self.enqueue(job, now_ms);
                    } else {
                        self.stats.stale_timers += 1;
                    }
                }
                TimerKind::Drain => {
                    let remaining: Vec<JobId> = self.jobs.keys().copied().collect();
                    for job in remaining {
                        self.fire(
                            job,
                            &JobEvent::CancelRequested {
                                reason: CancelReason::Drain,
                            },
                            now_ms,
                            &mut out,
                        );
                    }
                }
            }
        }
        self.try_schedule(now_ms, &mut out);
        self.emit_drained_if_due(&mut out);
        out
    }

    // ------------------------------------------------------------------ 命令

    fn submit(&mut self, id: JobId, job: Job, now_ms: u64, out: &mut Vec<SchedEffect>) {
        self.stats.submitted += 1;
        if self.jobs.contains_key(&id) {
            self.log(format!("sched=submit job={} outcome=duplicate_id", id.0));
            return;
        }
        let request = job.request;
        let qos = job.qos;
        let passthrough =
            job.passthrough && job.slot.preemptible() && self.slots.has_free(job.slot);
        self.jobs.insert(
            id,
            Slot {
                job,
                effective_qos: qos,
                entry: JobEntry {
                    request,
                    fsm: build_job_fsm(),
                    defer_generation: 0,
                    preempted_count: 0,
                    enqueued_at_ms: now_ms,
                    cancel_reason: None,
                },
                queue_generation: 0,
                downgraded: false,
            },
        );
        if let Some(drain_deadline) = self.draining {
            let retry_after_ms =
                u32::try_from(drain_deadline.saturating_sub(now_ms)).unwrap_or(u32::MAX);
            self.fire(
                id,
                &JobEvent::Rejected {
                    error: InferError::Retryable {
                        kind: RetryKind::Busy,
                        retry_after_ms,
                    },
                },
                now_ms,
                out,
            );
            return;
        }
        let deadline_at_ms = self.jobs.get(&id).map_or(now_ms, |s| s.job.deadline_at_ms);
        self.arm_timer(deadline_at_ms, TimerKind::Deadline { job: id });
        if passthrough {
            // 零拷贝直通：跳过排队，仍占槽计数。
            let slot = self.jobs.get(&id).map(|s| s.job.slot);
            if let Some(slot) = slot {
                if self.slots.acquire(slot, id) {
                    self.fire(id, &JobEvent::Admitted, now_ms, out);
                    return;
                }
            }
        }
        self.enqueue(id, now_ms);
    }

    fn report(
        &mut self,
        job: JobId,
        report: WorkerReport,
        now_ms: u64,
        out: &mut Vec<SchedEffect>,
    ) {
        let Some(business) = self.jobs.get(&job).map(|s| s.job.business) else {
            self.log(format!("sched=report job={} outcome=unknown_job", job.0));
            return;
        };
        self.quota.charge(business, report.cost(), now_ms);
        let event = match report {
            WorkerReport::Finished { .. } => JobEvent::Finished,
            WorkerReport::Failed { error, .. } => JobEvent::Failed { error },
            WorkerReport::Yielded { .. } => JobEvent::Yielded,
        };
        self.fire(job, &event, now_ms, out);
    }

    fn drain(&mut self, timeout_ms: u64, now_ms: u64, out: &mut Vec<SchedEffect>) {
        if self.draining.is_some() {
            return;
        }
        let deadline = now_ms.saturating_add(timeout_ms);
        self.draining = Some(deadline);
        self.arm_timer(deadline, TimerKind::Drain);
        for qos in [QoS::Background, QoS::Async] {
            let victims: Vec<JobId> = self
                .jobs
                .iter()
                .filter(|(_, s)| s.effective_qos == qos)
                .map(|(id, _)| *id)
                .collect();
            for job in victims {
                self.fire(
                    job,
                    &JobEvent::CancelRequested {
                        reason: CancelReason::Drain,
                    },
                    now_ms,
                    out,
                );
            }
        }
    }

    // ------------------------------------------------------------------ 调度循环

    fn try_schedule(&mut self, now_ms: u64, out: &mut Vec<SchedEffect>) {
        for qos in [QoS::Interactive, QoS::Async, QoS::Background] {
            while let Some(head) = self.peek_live_head(qos) {
                let Some(slot) = self.jobs.get(&head) else {
                    break;
                };
                let business = slot.job.business;
                let estimate_cost = slot.job.estimate.cost();
                let slot_class = slot.job.slot;
                let already_downgraded = slot.downgraded;

                // ① 门控
                match self.policy.admit(qos, &self.gates, now_ms) {
                    Admission::Reject { kind, gate } => {
                        self.pop_head(qos);
                        self.stats.gate_rejected += 1;
                        self.log(format!(
                            "sched=reject job={} gate={gate:?} kind={kind:?}",
                            head.0
                        ));
                        self.fire(
                            head,
                            &JobEvent::Rejected {
                                error: InferError::Retryable {
                                    kind,
                                    retry_after_ms: GATE_REJECT_RETRY_AFTER_MS,
                                },
                            },
                            now_ms,
                            out,
                        );
                        continue;
                    }
                    Admission::Defer { ms, gate } => {
                        self.pop_head(qos);
                        let until_ms = now_ms.saturating_add(ms);
                        self.stats.deferred += 1;
                        out.push(SchedEffect::Deferred {
                            job: head,
                            until_ms,
                            gate,
                        });
                        self.fire(head, &JobEvent::Deferred { until_ms }, now_ms, out);
                        continue;
                    }
                    Admission::Run => {}
                }

                // ② 配额
                match self.quota.verdict(business, estimate_cost, now_ms) {
                    QuotaVerdict::Exceeded => {
                        self.pop_head(qos);
                        self.stats.quota_rejected += 1;
                        self.fire(
                            head,
                            &JobEvent::Rejected {
                                error: InferError::Retryable {
                                    kind: RetryKind::QuotaExceeded,
                                    retry_after_ms: QUOTA_REJECT_RETRY_AFTER_MS,
                                },
                            },
                            now_ms,
                            out,
                        );
                        continue;
                    }
                    QuotaVerdict::Downgrade if already_downgraded => {
                        // 已降过一档：按降后档执行，不再往下降。
                    }
                    QuotaVerdict::Downgrade => {
                        self.pop_head(qos);
                        match downgrade(qos) {
                            Some(lower) => {
                                self.stats.quota_downgraded += 1;
                                if let Some(s) = self.jobs.get_mut(&head) {
                                    s.effective_qos = lower;
                                    s.downgraded = true;
                                }
                                self.log(format!(
                                    "sched=downgrade job={} from={qos:?} to={lower:?}",
                                    head.0
                                ));
                                self.enqueue(head, now_ms);
                            }
                            None => {
                                self.stats.quota_rejected += 1;
                                self.fire(
                                    head,
                                    &JobEvent::Rejected {
                                        error: InferError::Retryable {
                                            kind: RetryKind::QuotaExceeded,
                                            retry_after_ms: QUOTA_REJECT_RETRY_AFTER_MS,
                                        },
                                    },
                                    now_ms,
                                    out,
                                );
                            }
                        }
                        continue;
                    }
                    QuotaVerdict::Within => {}
                }

                // ③ 槽
                if self.slots.acquire(slot_class, head) {
                    self.pop_head(qos);
                    self.fire(head, &JobEvent::Admitted, now_ms, out);
                    continue;
                }
                if qos == QoS::Interactive && slot_class.preemptible() {
                    if let Some(victim) = self.pick_preempt_victim(slot_class) {
                        self.stats.preempted += 1;
                        self.log(format!(
                            "sched=preempt victim={} for={} slot={slot_class:?}",
                            victim.0, head.0
                        ));
                        self.fire(victim, &JobEvent::PreemptRequested, now_ms, out);
                    }
                }
                // 队首等槽：同档后续 Job 不越过队首（deadline 序）。
                break;
            }
        }
    }

    fn pick_preempt_victim(&self, slot: SlotClass) -> Option<JobId> {
        // 先后台、再异步；同档取最晚入队者；已在 Preempting / Cancelling 的不再打。
        let mut best: Option<(u8, u64, JobId)> = None;
        for id in self.slots.occupants(slot) {
            let Some(s) = self.jobs.get(id) else {
                continue;
            };
            if s.entry.fsm.state() != JobState::Running {
                continue;
            }
            let rank = match s.effective_qos {
                QoS::Background => 2,
                QoS::Async => 1,
                QoS::Interactive => continue,
            };
            let key = (rank, s.entry.enqueued_at_ms, *id);
            if best.is_none_or(|b| key > b) {
                best = Some(key);
            }
        }
        best.map(|(_, _, id)| id)
    }

    fn peek_live_head(&mut self, qos: QoS) -> Option<JobId> {
        let idx = queue_index(qos);
        loop {
            let Reverse(key) = *self.queues[idx].peek()?;
            let live = self.jobs.get(&key.job).is_some_and(|s| {
                s.queue_generation == key.generation
                    && s.entry.fsm.state() == JobState::Queued
                    && s.effective_qos == qos
            });
            if live {
                return Some(key.job);
            }
            self.queues[idx].pop();
        }
    }

    fn pop_head(&mut self, qos: QoS) {
        self.queues[queue_index(qos)].pop();
    }

    fn enqueue(&mut self, job: JobId, now_ms: u64) {
        self.seq += 1;
        let seq = self.seq;
        let Some(s) = self.jobs.get_mut(&job) else {
            return;
        };
        s.queue_generation += 1;
        s.entry.enqueued_at_ms = now_ms;
        let key = QueueKey {
            deadline_at_ms: s.job.deadline_at_ms,
            priority: s.job.priority,
            seq,
            job,
            generation: s.queue_generation,
        };
        let idx = queue_index(s.effective_qos);
        self.queues[idx].push(Reverse(key));
        let depth: usize = self.queues.iter().map(BinaryHeap::len).sum();
        self.stats.max_queue_depth = self.stats.max_queue_depth.max(depth);
    }

    fn arm_timer(&mut self, at_ms: u64, kind: TimerKind) {
        self.timer_seq += 1;
        self.timers.push(Reverse(Timer {
            at_ms,
            seq: self.timer_seq,
            kind,
        }));
    }

    // ------------------------------------------------------------------ FSM 投递与效果执行

    fn fire(&mut self, job: JobId, event: &JobEvent, now_ms: u64, out: &mut Vec<SchedEffect>) {
        let Some(s) = self.jobs.get_mut(&job) else {
            return;
        };
        let transition = s.entry.fsm.fire(event, now_ms);
        if !transition.accepted {
            self.stats.unexpected_events += 1;
            return;
        }
        let request = s.entry.request;
        self.log(format!(
            "sched=transition job={} request={} from={:?} to={:?}",
            job.0, request.0, transition.from, transition.to
        ));
        let mut remove = false;
        for effect in transition.effects {
            match effect {
                JobEffect::StartOnSlot => {
                    self.stats.started += 1;
                    if let Some(s) = self.jobs.get(&job) {
                        out.push(SchedEffect::Start {
                            ctx: JobCtx {
                                job,
                                request,
                                qos: s.effective_qos,
                                slot: s.job.slot,
                                deadline_at_ms: s.job.deadline_at_ms,
                                threads: threads_for(s.effective_qos, self.big_cores),
                                queued_ms: now_ms.saturating_sub(s.entry.enqueued_at_ms),
                            },
                        });
                    }
                }
                JobEffect::ArmDeferTimer { until_ms } => {
                    if let Some(s) = self.jobs.get_mut(&job) {
                        s.entry.defer_generation += 1;
                        let generation = s.entry.defer_generation;
                        self.arm_timer(until_ms, TimerKind::Defer { job, generation });
                    }
                }
                JobEffect::ReleaseSlot => {
                    if let Some(slot) = self.jobs.get(&job).map(|s| s.job.slot) {
                        self.slots.release(slot, job);
                    }
                }
                JobEffect::Requeue => {
                    if let Some(s) = self.jobs.get_mut(&job) {
                        s.entry.preempted_count += 1;
                    }
                    self.enqueue(job, now_ms);
                }
                JobEffect::RequestCancel { reason } => {
                    if let Some(s) = self.jobs.get_mut(&job) {
                        s.entry.cancel_reason = Some(reason);
                    }
                    out.push(SchedEffect::Cancel { job, reason });
                }
                JobEffect::EmitDone => {
                    self.stats.done += 1;
                    out.push(SchedEffect::Done { job, request });
                    remove = true;
                }
                JobEffect::EmitFailed { error } => {
                    self.stats.failed += 1;
                    out.push(SchedEffect::Failed {
                        job,
                        request,
                        error,
                    });
                    remove = true;
                }
                JobEffect::EmitCancelled { reason } => {
                    self.stats.cancelled += 1;
                    let reason = self
                        .jobs
                        .get(&job)
                        .and_then(|s| s.entry.cancel_reason)
                        .unwrap_or(reason);
                    out.push(SchedEffect::Cancelled {
                        job,
                        request,
                        reason,
                    });
                    remove = true;
                }
            }
        }
        if remove {
            if let Some(s) = self.jobs.remove(&job) {
                // 终态兜底：槽若仍被占（异常路径）在此释放，保住"每槽最多一个运行中 Job"的不变量。
                self.slots.release(s.job.slot, job);
            }
        }
    }

    fn emit_drained_if_due(&mut self, out: &mut Vec<SchedEffect>) {
        if self.is_drained() && !self.drained_emitted {
            self.drained_emitted = true;
            out.push(SchedEffect::Drained);
        }
    }

    fn log(&self, facts: String) {
        self.logger.log(LogLevel::Info, "madinfer-sched", &facts);
    }
}
