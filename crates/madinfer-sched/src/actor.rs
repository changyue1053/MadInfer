//! 单消费者 actor：control 线程 + 邮箱（`std::sync::mpsc`）。范式同 `Engine.kt` 的邮箱泵与 im-core `pipeline.rs` 的 `recv` 循环：
//! API 调用、worker 回执、资源信号全部走同一邮箱同序消费；无定时器时阻塞 `recv`（不轮询），有定时器时 `recv_timeout` 到最近到期。
//! 不共享锁：核状态只在本线程可见，外界经 [`SchedulerHandle`] 投递命令、经效果 sink 收结果。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{Builder, JoinHandle};
use std::time::Duration;

use madinfer_host::{Clock, OsSignal, ThreadHooks, WorkerClass};

use crate::core::{CancelReason, SchedCommand, SchedCore, SchedEffect, WorkerReport};
use crate::job::{Job, JobId};
use crate::quota::QuotaTable;

enum Envelope {
    Command(SchedCommand),
    Shutdown,
}

/// 调度 actor（只有 `spawn`）。
pub struct SchedulerActor;

/// 效果接收方（worker 池 / 回调面）；在 control 线程上被调用，实现必须只入队立即返回。
pub type EffectSink = Box<dyn FnMut(SchedEffect) + Send + 'static>;

impl SchedulerActor {
    /// 启动 control 线程。`clock` 供 `recv_timeout` 换算与命令时间戳；`hooks` 在线程启动时设 QoS。
    pub fn spawn(
        mut core: SchedCore,
        clock: Arc<dyn Clock>,
        hooks: Arc<dyn ThreadHooks>,
        mut sink: EffectSink,
    ) -> std::io::Result<SchedulerHandle> {
        let (tx, rx) = mpsc::channel::<Envelope>();
        let thread = Builder::new()
            .name("madinfer-sched".into())
            .spawn(move || {
                hooks.on_worker_started(WorkerClass::Control);
                loop {
                    let now = clock.now_ms();
                    let received = match core.next_wakeup() {
                        None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                        Some(at) if at <= now => match rx.try_recv() {
                            Ok(envelope) => Ok(envelope),
                            Err(mpsc::TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
                            Err(mpsc::TryRecvError::Disconnected) => {
                                Err(RecvTimeoutError::Disconnected)
                            }
                        },
                        Some(at) => rx.recv_timeout(Duration::from_millis(at - now)),
                    };
                    let effects = match received {
                        Ok(Envelope::Command(command)) => core.handle(command, clock.now_ms()),
                        Ok(Envelope::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => core.tick(clock.now_ms()),
                    };
                    for effect in effects {
                        sink(effect);
                    }
                }
                core
            })?;
        Ok(SchedulerHandle {
            tx,
            ids: Arc::new(AtomicU64::new(0)),
            thread: Some(thread),
        })
    }
}

/// actor 句柄：投递命令；`shutdown` 收回核（测试断言台账）。
pub struct SchedulerHandle {
    tx: mpsc::Sender<Envelope>,
    ids: Arc<AtomicU64>,
    thread: Option<JoinHandle<SchedCore>>,
}

impl SchedulerHandle {
    /// 提交；返回预分配的 JobId（回执按它归因）。actor 已退出时仍返回 id，命令被丢弃。
    pub fn submit(&self, job: Job) -> JobId {
        let id = JobId(self.ids.fetch_add(1, Ordering::Relaxed) + 1);
        self.send(SchedCommand::Submit { id, job });
        id
    }

    /// 取消。
    pub fn cancel(&self, job: JobId, reason: CancelReason) {
        self.send(SchedCommand::Cancel { job, reason });
    }

    /// worker 回执。
    pub fn report(&self, job: JobId, report: WorkerReport) {
        self.send(SchedCommand::Report { job, report });
    }

    /// 资源信号。
    pub fn signal(&self, signal: OsSignal) {
        self.send(SchedCommand::Signal(signal));
    }

    /// 配额表热更。
    pub fn replace_quota(&self, table: QuotaTable) {
        self.send(SchedCommand::ReplaceQuota(table));
    }

    /// 排水。
    pub fn drain(&self, timeout_ms: u64) {
        self.send(SchedCommand::Drain { timeout_ms });
    }

    /// 关停并收回核。
    pub fn shutdown(mut self) -> Option<SchedCore> {
        let _ = self.tx.send(Envelope::Shutdown);
        self.thread.take().and_then(|t| t.join().ok())
    }

    fn send(&self, command: SchedCommand) {
        let _ = self.tx.send(Envelope::Command(command));
    }
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Envelope::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::job::{JobKind, ResourceEstimate, SlotTable};
    use madinfer_core_types::SlotClass;
    use madinfer_core_types::{BusinessId, CapabilityId, QoS, RequestId};
    use madinfer_host::{MonotonicClock, NoopThreadHooks};

    #[test]
    fn actor_runs_one_job_through_start_and_done() {
        let core = SchedCore::new(
            SlotTable::new([(SlotClass::Cpu, 1)]),
            QuotaTable::default(),
            4,
        );
        let (fx_tx, fx_rx) = mpsc::channel::<SchedEffect>();
        let handle = SchedulerActor::spawn(
            core,
            Arc::new(MonotonicClock::new()),
            Arc::new(NoopThreadHooks),
            Box::new(move |e| {
                let _ = fx_tx.send(e);
            }),
        )
        .expect("spawn");
        let id = handle.submit(Job {
            request: RequestId(7),
            capability: CapabilityId::new("summarize").expect("id"),
            business: BusinessId::new(1).expect("b"),
            qos: QoS::Interactive,
            kind: JobKind::Generate,
            slot: SlotClass::Cpu,
            deadline_at_ms: u64::MAX / 2,
            priority: 0,
            estimate: ResourceEstimate {
                peak_memory_mb: 100,
                estimated_ms: 10,
            },
            passthrough: false,
        });
        let first = fx_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("start effect");
        match first {
            SchedEffect::Start { ctx } => assert_eq!(ctx.job, id),
            other => panic!("unexpected {other:?}"),
        }
        handle.report(
            id,
            WorkerReport::Finished {
                run_ms: 5,
                peak_memory_mb: 90,
            },
        );
        let second = fx_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("done effect");
        assert_eq!(
            second,
            SchedEffect::Done {
                job: id,
                request: RequestId(7)
            }
        );
        let core = handle.shutdown().expect("core back");
        assert_eq!(core.stats().done, 1);
        assert_eq!(core.active_jobs(), 0);
    }
}
