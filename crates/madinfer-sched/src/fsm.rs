//! Job 生命周期 FSM（表驱动，23 条迁移）。
//!
//! 状态裸名、事件过去式、效果动宾（mad 命名规范 §3.3）。进度、排队时长、抢占次数不进状态，放 owner 附属字段。
//!
//! ```text
//!  Queued ──Admitted──▶ Running ──Finished──▶ Done
//!    │  ▲                 │  │ └──Failed────▶ Failed
//!    │  │ DeferElapsed    │  └──PreemptRequested──▶ Preempting ──Yielded──▶ Queued（重排）
//!    │  Deferred          │                          └──DeadlineReached──▶ Cancelling [RequestCancel(Deadline)]
//!    │                    └──CancelRequested / DeadlineReached──▶ Cancelling ──Failed / Yielded──▶ Cancelled
//!    │                                                              │  └──Finished──▶ Done（结果先到，仍算完成）
//!    │                                                              └──DeadlineReached──▶ Cancelling []（自迁移吞掉：取消已在途）
//!    └──Rejected / DeadlineReached──▶ Failed ；──CancelRequested──▶ Cancelled
//! ```
//!
//! madai-w04 加的两条边（21 → 23）：抢占让出途中截止到期，改为按截止取消（受害者不再重排）；取消途中截止到期是常态
//! （截止票与取消回执竞速），吞掉而不计入 `unexpected_events`。
//! 轨迹样例：`Running --PreemptRequested--> Preempting @2ms`、`Preempting --DeadlineReached--> Cancelling @5ms`、
//! `Cancelling --DeadlineReached--> Cancelling @6ms`、`Cancelling --Yielded--> Cancelled @7ms`。

use madinfer_core_types::{InferError, RequestId};
use madinfer_fsm::{Fsm, FsmBuilder, FsmEvent};

use crate::core::CancelReason;

/// Job 状态（扁平枚举）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobState {
    /// 在队列里等槽 / 等门。
    Queued,
    /// 门控推迟，等到期票。
    Deferred,
    /// 在 worker 上执行。
    Running,
    /// 已向 worker 发取消，等回执。
    Cancelling,
    /// 已向 worker 发抢占让出，等回执。
    Preempting,
    /// 终态：完成。
    Done,
    /// 终态：失败。
    Failed,
    /// 终态：取消。
    Cancelled,
}

impl JobState {
    /// 是否终态。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// 是否占着槽（Running / Cancelling / Preempting）。
    pub const fn holds_slot(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling | Self::Preempting)
    }
}

/// Job 事件（携负载）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobEvent {
    /// 准入通过，槽已占。
    Admitted,
    /// 门控推迟到某时刻。
    Deferred {
        /// 到期时刻。
        until_ms: u64,
    },
    /// 推迟到期。
    DeferElapsed,
    /// 准入拒绝。
    Rejected {
        /// 错误。
        error: InferError,
    },
    /// worker 完成。
    Finished,
    /// worker 失败。
    Failed {
        /// 错误。
        error: InferError,
    },
    /// 调用方 / 排水 / 超时请求取消。
    CancelRequested {
        /// 原因。
        reason: CancelReason,
    },
    /// 交互档到达，要求让出槽。
    PreemptRequested,
    /// worker 已让出（可重排）。
    Yielded,
    /// 截止时刻到。
    DeadlineReached,
}

/// Job 事件种类（查表键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobEventKind {
    /// 见 [`JobEvent::Admitted`]。
    Admitted,
    /// 见 [`JobEvent::Deferred`]。
    Deferred,
    /// 见 [`JobEvent::DeferElapsed`]。
    DeferElapsed,
    /// 见 [`JobEvent::Rejected`]。
    Rejected,
    /// 见 [`JobEvent::Finished`]。
    Finished,
    /// 见 [`JobEvent::Failed`]。
    Failed,
    /// 见 [`JobEvent::CancelRequested`]。
    CancelRequested,
    /// 见 [`JobEvent::PreemptRequested`]。
    PreemptRequested,
    /// 见 [`JobEvent::Yielded`]。
    Yielded,
    /// 见 [`JobEvent::DeadlineReached`]。
    DeadlineReached,
}

impl FsmEvent for JobEvent {
    type Kind = JobEventKind;

    fn kind(&self) -> JobEventKind {
        match self {
            JobEvent::Admitted => JobEventKind::Admitted,
            JobEvent::Deferred { .. } => JobEventKind::Deferred,
            JobEvent::DeferElapsed => JobEventKind::DeferElapsed,
            JobEvent::Rejected { .. } => JobEventKind::Rejected,
            JobEvent::Finished => JobEventKind::Finished,
            JobEvent::Failed { .. } => JobEventKind::Failed,
            JobEvent::CancelRequested { .. } => JobEventKind::CancelRequested,
            JobEvent::PreemptRequested => JobEventKind::PreemptRequested,
            JobEvent::Yielded => JobEventKind::Yielded,
            JobEvent::DeadlineReached => JobEventKind::DeadlineReached,
        }
    }
}

/// Job 效果（数据不是动作；owner 在 control 线程执行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobEffect {
    /// 在已占的槽上启动。
    StartOnSlot,
    /// 登记推迟到期票。
    ArmDeferTimer {
        /// 到期时刻。
        until_ms: u64,
    },
    /// 释放槽。
    ReleaseSlot,
    /// 重新入队（抢占让出后）。
    Requeue,
    /// 向 worker 发取消。
    RequestCancel {
        /// 原因。
        reason: CancelReason,
    },
    /// 终态回执：完成。
    EmitDone,
    /// 终态回执：失败。
    EmitFailed {
        /// 错误。
        error: InferError,
    },
    /// 终态回执：取消。
    EmitCancelled {
        /// 原因。
        reason: CancelReason,
    },
}

fn none(_: &JobEvent) -> Vec<JobEffect> {
    Vec::new()
}

fn start(_: &JobEvent) -> Vec<JobEffect> {
    vec![JobEffect::StartOnSlot]
}

fn arm_defer(e: &JobEvent) -> Vec<JobEffect> {
    match e {
        JobEvent::Deferred { until_ms } => vec![JobEffect::ArmDeferTimer {
            until_ms: *until_ms,
        }],
        _ => Vec::new(),
    }
}

fn emit_failed(e: &JobEvent) -> Vec<JobEffect> {
    match e {
        JobEvent::Rejected { error } | JobEvent::Failed { error } => {
            vec![JobEffect::EmitFailed { error: *error }]
        }
        JobEvent::DeadlineReached => vec![JobEffect::EmitFailed {
            error: InferError::Degradable {
                kind: madinfer_core_types::DegradeKind::Deadline,
            },
        }],
        _ => Vec::new(),
    }
}

fn release_and_done(_: &JobEvent) -> Vec<JobEffect> {
    vec![JobEffect::ReleaseSlot, JobEffect::EmitDone]
}

fn release_and_failed(e: &JobEvent) -> Vec<JobEffect> {
    let mut v = vec![JobEffect::ReleaseSlot];
    v.extend(emit_failed(e));
    v
}

fn request_cancel(e: &JobEvent) -> Vec<JobEffect> {
    let reason = match e {
        JobEvent::CancelRequested { reason } => *reason,
        JobEvent::PreemptRequested => CancelReason::Preempt,
        JobEvent::DeadlineReached => CancelReason::Deadline,
        _ => CancelReason::Caller,
    };
    vec![JobEffect::RequestCancel { reason }]
}

fn emit_cancelled(e: &JobEvent) -> Vec<JobEffect> {
    let reason = match e {
        JobEvent::CancelRequested { reason } => *reason,
        _ => CancelReason::Caller,
    };
    vec![JobEffect::EmitCancelled { reason }]
}

fn release_and_cancelled(e: &JobEvent) -> Vec<JobEffect> {
    // 取消回执里的原因由 owner 从 Job 附属字段补（worker 回执不携原因），这里给缺省值。
    let mut v = vec![JobEffect::ReleaseSlot];
    v.extend(emit_cancelled(e));
    v
}

fn release_and_requeue(_: &JobEvent) -> Vec<JobEffect> {
    vec![JobEffect::ReleaseSlot, JobEffect::Requeue]
}

/// Job FSM 的迁移表条目数（单测断言锁拓扑）。
pub const JOB_FSM_TABLE_SIZE: usize = 23;

/// 建一台 Job FSM（初始 Queued）。表在构造期固化；重复注册不可能发生（表是常量代码），`unwrap` 由测试兜。
pub fn build_job_fsm() -> Fsm<JobState, JobEvent, JobEffect> {
    use JobEventKind as K;
    use JobState as S;
    let table = Ok(FsmBuilder::<S, JobEvent, JobEffect>::new(S::Queued))
        // Queued
        .and_then(|b| b.on_with(S::Queued, K::Admitted, S::Running, start))
        .and_then(|b| b.on_with(S::Queued, K::Deferred, S::Deferred, arm_defer))
        .and_then(|b| b.on_with(S::Queued, K::Rejected, S::Failed, emit_failed))
        .and_then(|b| b.on_with(S::Queued, K::CancelRequested, S::Cancelled, emit_cancelled))
        .and_then(|b| b.on_with(S::Queued, K::DeadlineReached, S::Failed, emit_failed))
        // Deferred
        .and_then(|b| b.on_with(S::Deferred, K::DeferElapsed, S::Queued, none))
        .and_then(|b| b.on_with(S::Deferred, K::Rejected, S::Failed, emit_failed))
        .and_then(|b| {
            b.on_with(
                S::Deferred,
                K::CancelRequested,
                S::Cancelled,
                emit_cancelled,
            )
        })
        .and_then(|b| b.on_with(S::Deferred, K::DeadlineReached, S::Failed, emit_failed))
        // Running
        .and_then(|b| b.on_with(S::Running, K::Finished, S::Done, release_and_done))
        .and_then(|b| b.on_with(S::Running, K::Failed, S::Failed, release_and_failed))
        .and_then(|b| {
            b.on_with(
                S::Running,
                K::CancelRequested,
                S::Cancelling,
                request_cancel,
            )
        })
        .and_then(|b| {
            b.on_with(
                S::Running,
                K::PreemptRequested,
                S::Preempting,
                request_cancel,
            )
        })
        .and_then(|b| {
            b.on_with(
                S::Running,
                K::DeadlineReached,
                S::Cancelling,
                request_cancel,
            )
        })
        // Cancelling
        .and_then(|b| b.on_with(S::Cancelling, K::Finished, S::Done, release_and_done))
        .and_then(|b| {
            b.on_with(
                S::Cancelling,
                K::Failed,
                S::Cancelled,
                release_and_cancelled,
            )
        })
        .and_then(|b| {
            b.on_with(
                S::Cancelling,
                K::Yielded,
                S::Cancelled,
                release_and_cancelled,
            )
        })
        // 取消在途时截止到期：吞掉（自迁移，无效果）。
        .and_then(|b| b.on_with(S::Cancelling, K::DeadlineReached, S::Cancelling, none))
        // Preempting
        .and_then(|b| b.on_with(S::Preempting, K::Yielded, S::Queued, release_and_requeue))
        .and_then(|b| b.on_with(S::Preempting, K::Finished, S::Done, release_and_done))
        .and_then(|b| b.on_with(S::Preempting, K::Failed, S::Failed, release_and_failed))
        .and_then(|b| {
            b.on_with(
                S::Preempting,
                K::CancelRequested,
                S::Cancelling,
                request_cancel,
            )
        })
        // 抢占让出途中截止到期：转为按截止取消，让出后不再重排。
        .and_then(|b| {
            b.on_with(
                S::Preempting,
                K::DeadlineReached,
                S::Cancelling,
                request_cancel,
            )
        });
    match table {
        Ok(builder) => builder.build(),
        // 表是常量代码，重复键只能是改表时的编程错误；单测 `job_fsm_table_is_locked` 兜住，这里退化为空表而不 panic。
        Err(_) => FsmBuilder::new(S::Queued).build(),
    }
}

/// owner 附属字段（不进状态）。
#[derive(Debug)]
pub struct JobEntry {
    /// 请求（回执归因）。
    pub request: RequestId,
    /// FSM。
    pub fsm: Fsm<JobState, JobEvent, JobEffect>,
    /// 推迟到期票的代数（每次 Deferred +1；陈旧票哑火）。
    pub defer_generation: u64,
    /// 被抢占次数。
    pub preempted_count: u32,
    /// 入队时刻（算 queued_ms）。
    pub enqueued_at_ms: u64,
    /// 最近一次取消原因（Cancelling 的回执用）。
    pub cancel_reason: Option<CancelReason>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_fsm_table_is_locked() {
        assert_eq!(build_job_fsm().table_size(), JOB_FSM_TABLE_SIZE);
    }

    #[test]
    fn happy_path_queued_running_done() {
        let mut fsm = build_job_fsm();
        let t = fsm.fire(&JobEvent::Admitted, 1);
        assert_eq!(t.effects, vec![JobEffect::StartOnSlot]);
        let t = fsm.fire(&JobEvent::Finished, 2);
        assert_eq!(t.effects, vec![JobEffect::ReleaseSlot, JobEffect::EmitDone]);
        assert_eq!(fsm.state(), JobState::Done);
        assert!(!fsm.fire(&JobEvent::Finished, 3).accepted, "终态后无出边");
        assert_eq!(fsm.unexpected_count(), 1);
    }

    #[test]
    fn preemption_requeues_after_yield() {
        let mut fsm = build_job_fsm();
        fsm.fire(&JobEvent::Admitted, 1);
        let t = fsm.fire(&JobEvent::PreemptRequested, 2);
        assert_eq!(
            t.effects,
            vec![JobEffect::RequestCancel {
                reason: CancelReason::Preempt
            }]
        );
        let t = fsm.fire(&JobEvent::Yielded, 3);
        assert_eq!(t.effects, vec![JobEffect::ReleaseSlot, JobEffect::Requeue]);
        assert_eq!(fsm.state(), JobState::Queued);
    }

    #[test]
    fn deadline_in_queue_fails_with_2001_and_running_cancels() {
        let mut fsm = build_job_fsm();
        let t = fsm.fire(&JobEvent::DeadlineReached, 1);
        match t.effects.as_slice() {
            [JobEffect::EmitFailed { error }] => assert_eq!(error.code(), 2001),
            other => panic!("unexpected {other:?}"),
        }
        let mut fsm = build_job_fsm();
        fsm.fire(&JobEvent::Admitted, 1);
        let t = fsm.fire(&JobEvent::DeadlineReached, 2);
        assert_eq!(
            t.effects,
            vec![JobEffect::RequestCancel {
                reason: CancelReason::Deadline
            }]
        );
        assert_eq!(fsm.state(), JobState::Cancelling);
        let t = fsm.fire(
            &JobEvent::Failed {
                error: InferError::Fatal {
                    kind: madinfer_core_types::FatalKind::Cancelled,
                    detail: 0,
                },
            },
            3,
        );
        assert!(matches!(t.effects[1], JobEffect::EmitCancelled { .. }));
        assert_eq!(fsm.state(), JobState::Cancelled);
    }

    #[test]
    fn deadline_while_preempting_cancels_and_while_cancelling_is_swallowed() {
        let mut fsm = build_job_fsm();
        fsm.fire(&JobEvent::Admitted, 1);
        fsm.fire(&JobEvent::PreemptRequested, 2);
        let t = fsm.fire(&JobEvent::DeadlineReached, 5);
        assert!(t.accepted);
        assert_eq!(
            t.effects,
            vec![JobEffect::RequestCancel {
                reason: CancelReason::Deadline
            }],
            "抢占让出途中到期 → 改为按截止取消"
        );
        assert_eq!(fsm.state(), JobState::Cancelling);
        let t = fsm.fire(&JobEvent::DeadlineReached, 6);
        assert!(t.accepted, "取消在途时到期是常态，不是非法事件");
        assert!(t.effects.is_empty());
        assert_eq!(fsm.state(), JobState::Cancelling);
        assert_eq!(fsm.unexpected_count(), 0);
        let t = fsm.fire(&JobEvent::Yielded, 7);
        assert_eq!(fsm.state(), JobState::Cancelled, "让出后不再重排");
        assert!(matches!(t.effects[1], JobEffect::EmitCancelled { .. }));
        let trace: Vec<String> = fsm.trace().map(|r| r.describe()).collect();
        assert_eq!(
            trace,
            vec![
                "Queued --Admitted--> Running @1ms",
                "Running --PreemptRequested--> Preempting @2ms",
                "Preempting --DeadlineReached--> Cancelling @5ms",
                "Cancelling --DeadlineReached--> Cancelling @6ms",
                "Cancelling --Yielded--> Cancelled @7ms",
            ]
        );
    }
}
