//! SchedCore 确定性测试：ManualClock 步进，命令进、效果出。

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use madinfer_core_types::SlotClass;
use madinfer_core_types::{
    BusinessId, CapabilityId, DegradeKind, FatalKind, InferError, QoS, RequestId, RetryKind,
};
use madinfer_host::{AppState, Clock, MemoryPressure, OsSignal, ThermalState};
use madinfer_sched::{
    CancelReason, Job, JobId, JobKind, JobState, Quota, QuotaTable, ResourceEstimate, SchedCommand,
    SchedCore, SchedEffect, SlotTable, WorkerReport,
};
use madinfer_testkit::{ManualClock, RecordingLogger};

fn job(request: u64, business: u16, qos: QoS, deadline_at_ms: u64) -> Job {
    Job {
        request: RequestId(request),
        capability: CapabilityId::new("summarize").expect("id"),
        business: BusinessId::new(business).expect("b"),
        qos,
        kind: JobKind::Generate,
        slot: SlotClass::Cpu,
        deadline_at_ms,
        priority: 0,
        estimate: ResourceEstimate {
            peak_memory_mb: 100,
            estimated_ms: 100,
        },
        passthrough: false,
    }
}

fn green_signals(core: &mut SchedCore, now: u64) {
    for s in [
        OsSignal::Thermal(ThermalState::Nominal),
        OsSignal::Battery {
            level_pct: Some(90),
            charging: true,
            low_power_mode: false,
        },
        OsSignal::Memory(MemoryPressure::Normal),
        OsSignal::App {
            state: AppState::Foreground,
            budget_ms: None,
        },
        OsSignal::Idle(true),
    ] {
        core.handle(SchedCommand::Signal(s), now);
    }
}

fn starts(effects: &[SchedEffect]) -> Vec<JobId> {
    effects
        .iter()
        .filter_map(|e| match e {
            SchedEffect::Start { ctx } => Some(ctx.job),
            _ => None,
        })
        .collect()
}

#[test]
fn interactive_preempts_background_and_requeues_victim() {
    let clock = ManualClock::at(1_000);
    let mut core = SchedCore::new(
        SlotTable::new([(SlotClass::Cpu, 1)]),
        QuotaTable::default(),
        4,
    );
    green_signals(&mut core, clock.now_ms());

    let bg = JobId(1);
    let fx = core.handle(
        SchedCommand::Submit {
            id: bg,
            job: job(1, 1, QoS::Background, 200_000),
        },
        clock.now_ms(),
    );
    assert_eq!(starts(&fx), vec![bg], "槽空，后台 Job 直接启动");

    let ia = JobId(2);
    let fx = core.handle(
        SchedCommand::Submit {
            id: ia,
            job: job(2, 1, QoS::Interactive, 5_000),
        },
        clock.now_ms(),
    );
    assert!(starts(&fx).is_empty(), "槽被占，交互 Job 先不启动");
    assert!(
        fx.contains(&SchedEffect::Cancel {
            job: bg,
            reason: CancelReason::Preempt
        }),
        "向后台占用者发抢占：{fx:?}"
    );
    assert_eq!(core.job_state(bg), Some(JobState::Preempting));
    assert_eq!(core.job_state(ia), Some(JobState::Queued));

    clock.advance(10);
    let fx = core.handle(
        SchedCommand::Report {
            job: bg,
            report: WorkerReport::Yielded {
                run_ms: 10,
                peak_memory_mb: 50,
            },
        },
        clock.now_ms(),
    );
    assert_eq!(starts(&fx), vec![ia], "让出后交互 Job 立即上槽");
    assert_eq!(core.job_state(bg), Some(JobState::Queued), "受害者重排等待");
    assert_eq!(core.stats().preempted, 1);

    let fx = core.handle(
        SchedCommand::Report {
            job: ia,
            report: WorkerReport::Finished {
                run_ms: 40,
                peak_memory_mb: 80,
            },
        },
        clock.now_ms(),
    );
    assert!(fx.contains(&SchedEffect::Done {
        job: ia,
        request: RequestId(2)
    }));
    assert_eq!(starts(&fx), vec![bg], "交互完成后受害者回到槽上");
    assert_eq!(core.slots().total_occupied(), 1);
}

#[test]
fn deadline_fails_queued_job_and_cancels_running_job() {
    let clock = ManualClock::at(0);
    let mut core = SchedCore::new(
        SlotTable::new([(SlotClass::Cpu, 1)]),
        QuotaTable::default(),
        4,
    );
    green_signals(&mut core, 0);
    let running = JobId(1);
    core.handle(
        SchedCommand::Submit {
            id: running,
            job: job(1, 1, QoS::Interactive, 300),
        },
        0,
    );
    let queued = JobId(2);
    core.handle(
        SchedCommand::Submit {
            id: queued,
            job: job(2, 1, QoS::Interactive, 200),
        },
        0,
    );
    assert_eq!(core.next_wakeup(), Some(200));

    clock.set(200);
    let fx = core.tick(clock.now_ms());
    assert_eq!(
        fx,
        vec![SchedEffect::Failed {
            job: queued,
            request: RequestId(2),
            error: InferError::Degradable {
                kind: DegradeKind::Deadline
            }
        }],
        "排队中到期 → 2001"
    );

    clock.set(300);
    let fx = core.tick(clock.now_ms());
    assert_eq!(
        fx,
        vec![SchedEffect::Cancel {
            job: running,
            reason: CancelReason::Deadline
        }],
        "运行中到期 → 向 worker 发取消"
    );
    let fx = core.handle(
        SchedCommand::Report {
            job: running,
            report: WorkerReport::Failed {
                error: InferError::Fatal {
                    kind: FatalKind::Cancelled,
                    detail: 0,
                },
                run_ms: 300,
                peak_memory_mb: 10,
            },
        },
        301,
    );
    assert_eq!(
        fx,
        vec![SchedEffect::Cancelled {
            job: running,
            request: RequestId(1),
            reason: CancelReason::Deadline
        }]
    );
    assert_eq!(core.active_jobs(), 0);
}

#[test]
fn deadline_during_preempting_cancels_by_deadline_and_during_cancelling_is_not_unexpected() {
    let clock = ManualClock::at(0);
    let mut core = SchedCore::new(
        SlotTable::new([(SlotClass::Cpu, 1)]),
        QuotaTable::default(),
        4,
    );
    green_signals(&mut core, 0);
    // 后台 Job 截止 500，被交互 Job 抢占后在让出前到期。
    let bg = JobId(1);
    core.handle(
        SchedCommand::Submit {
            id: bg,
            job: job(1, 1, QoS::Background, 500),
        },
        0,
    );
    let ia = JobId(2);
    let fx = core.handle(
        SchedCommand::Submit {
            id: ia,
            job: job(2, 1, QoS::Interactive, 5_000),
        },
        10,
    );
    assert!(fx.contains(&SchedEffect::Cancel {
        job: bg,
        reason: CancelReason::Preempt
    }));
    assert_eq!(core.job_state(bg), Some(JobState::Preempting));

    clock.set(500);
    let fx = core.tick(clock.now_ms());
    assert_eq!(
        fx,
        vec![SchedEffect::Cancel {
            job: bg,
            reason: CancelReason::Deadline
        }],
        "Preempting × DeadlineReached → 改为按截止取消"
    );
    assert_eq!(core.job_state(bg), Some(JobState::Cancelling));
    assert_eq!(core.stats().unexpected_events, 0);

    let fx = core.handle(
        SchedCommand::Report {
            job: bg,
            report: WorkerReport::Yielded {
                run_ms: 500,
                peak_memory_mb: 10,
            },
        },
        510,
    );
    assert!(
        fx.contains(&SchedEffect::Cancelled {
            job: bg,
            request: RequestId(1),
            reason: CancelReason::Deadline
        }),
        "让出后不再重排，按截止取消回执：{fx:?}"
    );
    assert_eq!(starts(&fx), vec![ia], "槽释放给交互 Job");

    // 运行中被调用方取消后截止到期：吞掉，不计非法事件。
    let fx = core.handle(
        SchedCommand::Cancel {
            job: ia,
            reason: CancelReason::Caller,
        },
        600,
    );
    assert!(fx.contains(&SchedEffect::Cancel {
        job: ia,
        reason: CancelReason::Caller
    }));
    clock.set(5_000);
    let fx = core.tick(clock.now_ms());
    assert!(fx.is_empty(), "Cancelling × DeadlineReached 无效果：{fx:?}");
    assert_eq!(core.job_state(ia), Some(JobState::Cancelling));
    assert_eq!(core.stats().unexpected_events, 0);
    let fx = core.handle(
        SchedCommand::Report {
            job: ia,
            report: WorkerReport::Failed {
                error: InferError::Fatal {
                    kind: FatalKind::Cancelled,
                    detail: 0,
                },
                run_ms: 1,
                peak_memory_mb: 1,
            },
        },
        5_001,
    );
    assert!(matches!(
        fx[0],
        SchedEffect::Cancelled {
            job,
            reason: CancelReason::Caller,
            ..
        } if job == ia
    ));
    assert_eq!(core.active_jobs(), 0);
}

#[test]
fn background_defers_until_gates_open_and_stale_ticket_is_ignored() {
    let clock = ManualClock::at(0);
    let logger = Arc::new(RecordingLogger::new());
    let mut core = SchedCore::with_policy(
        SlotTable::new([(SlotClass::Cpu, 1)]),
        QuotaTable::default(),
        4,
        Box::new(madinfer_sched::DefaultGatePolicy),
        logger.clone(),
    );
    let bg = JobId(1);
    let fx = core.handle(
        SchedCommand::Submit {
            id: bg,
            job: job(1, 1, QoS::Background, 1_000_000),
        },
        0,
    );
    assert!(
        matches!(fx[0], SchedEffect::Deferred { job, until_ms: 60_000, .. } if job == bg),
        "门未开 → 推迟 60 s：{fx:?}"
    );
    assert_eq!(core.job_state(bg), Some(JobState::Deferred));

    // 到期前用户取消：Deferred → Cancelled；旧到期票之后到达必须哑火。
    let fx = core.handle(
        SchedCommand::Cancel {
            job: bg,
            reason: CancelReason::Caller,
        },
        10,
    );
    assert!(
        matches!(fx[0], SchedEffect::Cancelled { job, reason: CancelReason::Caller, .. } if job == bg)
    );
    clock.set(60_000);
    let fx = core.tick(clock.now_ms());
    assert!(fx.is_empty());
    assert_eq!(core.stats().stale_timers, 1, "陈旧票计数，零副作用");

    // 再提交一个后台 Job，开门后到期票让它重新入队并启动。
    let bg2 = JobId(2);
    core.handle(
        SchedCommand::Submit {
            id: bg2,
            job: job(2, 1, QoS::Background, 1_000_000),
        },
        clock.now_ms(),
    );
    green_signals(&mut core, clock.now_ms());
    assert_eq!(
        core.job_state(bg2),
        Some(JobState::Deferred),
        "开门不立即唤醒，等到期票"
    );
    clock.advance(60_000);
    let fx = core.tick(clock.now_ms());
    assert_eq!(starts(&fx), vec![bg2]);
    assert!(
        logger.facts_containing("from=Deferred to=Queued").len() == 1,
        "留痕：{:?}",
        logger.lines()
    );
}

#[test]
fn interactive_is_rejected_on_critical_thermal_with_retryable_1002() {
    let mut core = SchedCore::new(
        SlotTable::new([(SlotClass::Cpu, 1)]),
        QuotaTable::default(),
        4,
    );
    core.handle(
        SchedCommand::Signal(OsSignal::Thermal(ThermalState::Critical)),
        0,
    );
    let fx = core.handle(
        SchedCommand::Submit {
            id: JobId(1),
            job: job(1, 1, QoS::Interactive, 5_000),
        },
        0,
    );
    match &fx[0] {
        SchedEffect::Failed { error, .. } => {
            assert_eq!(error.code(), 1002);
            assert!(matches!(
                error,
                InferError::Retryable {
                    kind: RetryKind::ThermalThrottled,
                    ..
                }
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(core.stats().gate_rejected, 1);
    assert_eq!(core.active_jobs(), 0);
}

#[test]
fn quota_downgrades_then_rejects() {
    let table = QuotaTable::new(
        [(
            BusinessId::new(1).expect("b"),
            Quota {
                window_ms: 3_600_000,
                budget_cost: 15_000, // 估算成本 100 ms × 100 MB = 10 000
            },
        )],
        None,
    );
    let mut core = SchedCore::new(SlotTable::new([(SlotClass::Cpu, 2)]), table, 4);
    green_signals(&mut core, 0);
    let a = JobId(1);
    let fx = core.handle(
        SchedCommand::Submit {
            id: a,
            job: job(1, 1, QoS::Interactive, 100_000),
        },
        0,
    );
    assert_eq!(starts(&fx), vec![a], "预算内直接启动");
    core.handle(
        SchedCommand::Report {
            job: a,
            report: WorkerReport::Finished {
                run_ms: 100,
                peak_memory_mb: 100,
            },
        },
        100,
    );
    // 已用 10 000，再来一个 10 000 会越过 15 000 → 降档为 Async 执行。
    let b = JobId(2);
    let fx = core.handle(
        SchedCommand::Submit {
            id: b,
            job: job(2, 1, QoS::Interactive, 100_000),
        },
        100,
    );
    assert_eq!(starts(&fx), vec![b]);
    assert_eq!(core.effective_qos(b), Some(QoS::Async), "配额越界降一档");
    assert_eq!(core.stats().quota_downgraded, 1);
    core.handle(
        SchedCommand::Report {
            job: b,
            report: WorkerReport::Finished {
                run_ms: 100,
                peak_memory_mb: 100,
            },
        },
        200,
    );
    // 已用 20 000 > 15 000 → 拒绝 1005。
    let c = JobId(3);
    let fx = core.handle(
        SchedCommand::Submit {
            id: c,
            job: job(3, 1, QoS::Interactive, 100_000),
        },
        200,
    );
    match &fx[0] {
        SchedEffect::Failed { error, .. } => assert_eq!(error.code(), 1005),
        other => panic!("unexpected {other:?}"),
    }
    // 另一业务不受影响。
    let d = JobId(4);
    let fx = core.handle(
        SchedCommand::Submit {
            id: d,
            job: job(4, 2, QoS::Interactive, 100_000),
        },
        200,
    );
    assert_eq!(starts(&fx), vec![d]);
}

#[test]
fn drain_cancels_background_then_async_and_waits_for_interactive() {
    let clock = ManualClock::at(0);
    let mut core = SchedCore::new(
        SlotTable::new([(SlotClass::Cpu, 3)]),
        QuotaTable::default(),
        4,
    );
    green_signals(&mut core, 0);
    let bg = JobId(1);
    let asy = JobId(2);
    let ia = JobId(3);
    core.handle(
        SchedCommand::Submit {
            id: bg,
            job: job(1, 1, QoS::Background, 900_000),
        },
        0,
    );
    core.handle(
        SchedCommand::Submit {
            id: asy,
            job: job(2, 1, QoS::Async, 900_000),
        },
        0,
    );
    core.handle(
        SchedCommand::Submit {
            id: ia,
            job: job(3, 1, QoS::Interactive, 900_000),
        },
        0,
    );
    assert_eq!(core.slots().total_occupied(), 3);

    let fx = core.handle(SchedCommand::Drain { timeout_ms: 1_000 }, 10);
    let cancels: Vec<JobId> = fx
        .iter()
        .filter_map(|e| match e {
            SchedEffect::Cancel {
                job,
                reason: CancelReason::Drain,
            } => Some(*job),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, vec![bg, asy], "先后台再异步；交互不动");
    assert_eq!(core.job_state(ia), Some(JobState::Running));

    // 排水期新提交 → Busy 1001。
    let fx = core.handle(
        SchedCommand::Submit {
            id: JobId(9),
            job: job(9, 1, QoS::Interactive, 900_000),
        },
        20,
    );
    assert!(matches!(&fx[0], SchedEffect::Failed { error, .. } if error.code() == 1001));

    for j in [bg, asy] {
        core.handle(
            SchedCommand::Report {
                job: j,
                report: WorkerReport::Yielded {
                    run_ms: 1,
                    peak_memory_mb: 1,
                },
            },
            30,
        );
    }
    clock.set(1_010);
    let fx = core.tick(clock.now_ms());
    assert!(
        fx.contains(&SchedEffect::Cancel {
            job: ia,
            reason: CancelReason::Drain
        }),
        "超时后交互也被取消：{fx:?}"
    );
    let fx = core.handle(
        SchedCommand::Report {
            job: ia,
            report: WorkerReport::Failed {
                error: InferError::Fatal {
                    kind: FatalKind::Cancelled,
                    detail: 0,
                },
                run_ms: 1,
                peak_memory_mb: 1,
            },
        },
        1_020,
    );
    assert!(
        fx.contains(&SchedEffect::Drained),
        "无活动 Job → Drained：{fx:?}"
    );
    assert!(core.is_drained());
}

#[test]
fn no_business_starves_within_quota() {
    // 4 个业务 × 5 个交互 Job，1 个 CPU 槽：每个 Start 立即回执 Finished，全部 20 个应在有限步内完成。
    let mut core = SchedCore::new(
        SlotTable::new([(SlotClass::Cpu, 1)]),
        QuotaTable::default(),
        4,
    );
    green_signals(&mut core, 0);
    let mut pending = Vec::new();
    let mut next = 1u64;
    let mut now = 0u64;
    for b in 1..=4u16 {
        for _ in 0..5 {
            let id = JobId(next);
            next += 1;
            let fx = core.handle(
                SchedCommand::Submit {
                    id,
                    job: job(next, b, QoS::Interactive, 1_000_000),
                },
                now,
            );
            pending.extend(starts(&fx));
        }
    }
    let mut done = 0;
    let mut steps = 0;
    while let Some(running) = pending.pop() {
        steps += 1;
        assert!(steps < 100, "步数越界");
        now += 10;
        let fx = core.handle(
            SchedCommand::Report {
                job: running,
                report: WorkerReport::Finished {
                    run_ms: 10,
                    peak_memory_mb: 10,
                },
            },
            now,
        );
        done += fx
            .iter()
            .filter(|e| matches!(e, SchedEffect::Done { .. }))
            .count();
        pending.extend(starts(&fx));
    }
    assert_eq!(done, 20);
    assert_eq!(core.active_jobs(), 0);
    assert_eq!(core.stats().max_queue_depth, 19);
    assert_eq!(core.stats().unexpected_events, 0);
}
