//! 请求生命周期 FSM（20 条迁移）：把事件顺序保证做成表——**Started 至多一次，且有 Delta / Complete 必先有 Started；终态恰一次；
//! 链推进只在首个 Delta 前**（泳道规则第 9 条、设计文档 §7.2 madai-W03 勘误）。
//!
//! ```text
//!  Accepted ──Routed──▶ Submitted ──Started──▶ Running ──DeltaArrived──▶ Streaming ──DeltaArrived──▶ Streaming
//!     │  │                 │  ▲                 │  │                         │
//!     │  │                 └──┘ FallbackChosen  │  └──FallbackChosen──▶ Submitted（首个 Delta 前可回退）
//!     │  │                                      ├──Completed──▶ Completed  ├──Completed──▶ Completed
//!     │  └──ConsentRequired──▶ AwaitingConsent  └──Failed / CancelRequested└──Failed / CancelRequested
//!     │                          ├──ConsentGranted──▶ Submitted
//!     │                          ├──ConsentDenied──▶ Failed(3007)
//!     │                          └──CancelRequested──▶ Cancelled
//!     └──RouteFailed──▶ Failed ；──CancelRequested──▶ Cancelled
//! ```
//!
//! **Started 延迟发布**：worker 开始执行时 `Started{source}` 只把状态推到 `Running`（无效果，owner 把 source 记在附属字段）；
//! `EmitStarted` 在首个 `DeltaArrived` 或 `Completed` 的效果列表里、排在 `EmitDelta` / `EmitComplete` 之前发出。
//! 于是 `Running` 期目标失败可 `FallbackChosen` 回到 `Submitted` 重提 Job，业务尚未收到任何内容；`Streaming` 期无回退边，
//! 目标失败即请求失败（partial 随 Failed 返回）。取消在任何非终态都可达 `Cancelled`，回执统一为 `Failed(Fatal(Cancelled))`。
//!
//! 轨迹样例：`Accepted --Routed--> Submitted @0ms`、`Submitted --Started--> Running @1ms`、`Running --FallbackChosen--> Submitted @2ms`、
//! `Submitted --Started--> Running @3ms`、`Running --DeltaArrived--> Streaming @4ms`、`Streaming --Completed--> Completed @5ms`。

use madinfer_core_types::{FatalKind, InferError, Source};
use madinfer_fsm::{Fsm, FsmBuilder, FsmEvent};

/// 请求状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RequestState {
    /// 已解析校验，待路由。
    Accepted,
    /// 升云前等待一次性授权。
    AwaitingConsent,
    /// Job 已提交调度器，等 worker 开始。
    Submitted,
    /// worker 在执行，尚无 Delta；Started 未发出（延迟到首个 Delta / Complete 前）。
    Running,
    /// 已有 Delta（Started 已发出）。
    Streaming,
    /// 终态。
    Completed,
    /// 终态。
    Failed,
    /// 终态。
    Cancelled,
}

impl RequestState {
    /// 是否终态。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// 请求事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestEvent {
    /// 路由得到目标链（当前目标无需授权）。
    Routed,
    /// 路由失败。
    RouteFailed {
        /// 错误。
        error: InferError,
    },
    /// 路由得到目标链，但当前目标升云需一次性授权。
    ConsentRequired,
    /// 用户允许。
    ConsentGranted,
    /// 用户拒绝。
    ConsentDenied,
    /// 目标失败且链已推进（Submitted / Running 期）。
    FallbackChosen,
    /// worker 开始执行（source 由 owner 记在附属字段，EmitStarted 时补）。
    Started {
        /// 实现来源。
        source: Source,
    },
    /// 一帧到达。
    DeltaArrived,
    /// 完成。
    Completed,
    /// 失败。
    Failed {
        /// 错误。
        error: InferError,
    },
    /// 调用方取消。
    CancelRequested,
}

/// 请求事件种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RequestEventKind {
    /// 见 [`RequestEvent::Routed`]。
    Routed,
    /// 见 [`RequestEvent::RouteFailed`]。
    RouteFailed,
    /// 见 [`RequestEvent::ConsentRequired`]。
    ConsentRequired,
    /// 见 [`RequestEvent::ConsentGranted`]。
    ConsentGranted,
    /// 见 [`RequestEvent::ConsentDenied`]。
    ConsentDenied,
    /// 见 [`RequestEvent::FallbackChosen`]。
    FallbackChosen,
    /// 见 [`RequestEvent::Started`]。
    Started,
    /// 见 [`RequestEvent::DeltaArrived`]。
    DeltaArrived,
    /// 见 [`RequestEvent::Completed`]。
    Completed,
    /// 见 [`RequestEvent::Failed`]。
    Failed,
    /// 见 [`RequestEvent::CancelRequested`]。
    CancelRequested,
}

impl FsmEvent for RequestEvent {
    type Kind = RequestEventKind;

    fn kind(&self) -> RequestEventKind {
        match self {
            RequestEvent::Routed => RequestEventKind::Routed,
            RequestEvent::RouteFailed { .. } => RequestEventKind::RouteFailed,
            RequestEvent::ConsentRequired => RequestEventKind::ConsentRequired,
            RequestEvent::ConsentGranted => RequestEventKind::ConsentGranted,
            RequestEvent::ConsentDenied => RequestEventKind::ConsentDenied,
            RequestEvent::FallbackChosen => RequestEventKind::FallbackChosen,
            RequestEvent::Started { .. } => RequestEventKind::Started,
            RequestEvent::DeltaArrived => RequestEventKind::DeltaArrived,
            RequestEvent::Completed => RequestEventKind::Completed,
            RequestEvent::Failed { .. } => RequestEventKind::Failed,
            RequestEvent::CancelRequested => RequestEventKind::CancelRequested,
        }
    }
}

/// 请求效果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestEffect {
    /// 向调度器提交（当前链游标处的目标）。
    SubmitJob,
    /// 向调度器取消。
    CancelJob,
    /// 向宿主 Authorizer 发起一次性授权。
    RequestConsent,
    /// 投递 Started（source 由 owner 从附属字段补）。
    EmitStarted,
    /// 投递一帧（合帧器已产出）。
    EmitDelta,
    /// 投递 Complete。
    EmitComplete,
    /// 投递 Failed。
    EmitFailed {
        /// 错误。
        error: InferError,
    },
    /// 结束合帧器条目（强制放行 pending）。
    FinishFramer,
}

const CANCELLED: InferError = InferError::Fatal {
    kind: FatalKind::Cancelled,
    detail: 0,
};

const CONSENT_DENIED: InferError = InferError::Fatal {
    kind: FatalKind::ConsentDenied,
    detail: 0,
};

fn none(_: &RequestEvent) -> Vec<RequestEffect> {
    Vec::new()
}

fn submit(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::SubmitJob]
}

fn request_consent(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::RequestConsent]
}

fn started_then_delta(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::EmitStarted, RequestEffect::EmitDelta]
}

fn started_then_complete(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![
        RequestEffect::EmitStarted,
        RequestEffect::FinishFramer,
        RequestEffect::EmitComplete,
    ]
}

fn delta(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::EmitDelta]
}

fn complete(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::FinishFramer, RequestEffect::EmitComplete]
}

fn failed(e: &RequestEvent) -> Vec<RequestEffect> {
    let error = match e {
        RequestEvent::RouteFailed { error } | RequestEvent::Failed { error } => *error,
        _ => InferError::Fatal {
            kind: FatalKind::Internal,
            detail: 1,
        },
    };
    vec![
        RequestEffect::FinishFramer,
        RequestEffect::EmitFailed { error },
    ]
}

fn consent_denied(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::EmitFailed {
        error: CONSENT_DENIED,
    }]
}

fn cancelled_before_job(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![RequestEffect::EmitFailed { error: CANCELLED }]
}

fn cancelled_before_delta(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![
        RequestEffect::CancelJob,
        RequestEffect::EmitFailed { error: CANCELLED },
    ]
}

fn cancelled_with_job(_: &RequestEvent) -> Vec<RequestEffect> {
    vec![
        RequestEffect::CancelJob,
        RequestEffect::FinishFramer,
        RequestEffect::EmitFailed { error: CANCELLED },
    ]
}

/// 迁移表条目数。
pub const REQUEST_FSM_TABLE_SIZE: usize = 20;

/// 建一台请求 FSM（初始 Accepted）。
pub fn build_request_fsm() -> Fsm<RequestState, RequestEvent, RequestEffect> {
    use RequestEventKind as K;
    use RequestState as S;
    let table = Ok(FsmBuilder::<S, RequestEvent, RequestEffect>::new(
        S::Accepted,
    ))
    // Accepted
    .and_then(|b| b.on_with(S::Accepted, K::Routed, S::Submitted, submit))
    .and_then(|b| b.on_with(S::Accepted, K::RouteFailed, S::Failed, failed))
    .and_then(|b| {
        b.on_with(
            S::Accepted,
            K::ConsentRequired,
            S::AwaitingConsent,
            request_consent,
        )
    })
    .and_then(|b| {
        b.on_with(
            S::Accepted,
            K::CancelRequested,
            S::Cancelled,
            cancelled_before_job,
        )
    })
    // AwaitingConsent
    .and_then(|b| b.on_with(S::AwaitingConsent, K::ConsentGranted, S::Submitted, submit))
    .and_then(|b| {
        b.on_with(
            S::AwaitingConsent,
            K::ConsentDenied,
            S::Failed,
            consent_denied,
        )
    })
    .and_then(|b| {
        b.on_with(
            S::AwaitingConsent,
            K::CancelRequested,
            S::Cancelled,
            cancelled_before_job,
        )
    })
    // Submitted
    .and_then(|b| b.on_with(S::Submitted, K::FallbackChosen, S::Submitted, submit))
    .and_then(|b| b.on_with(S::Submitted, K::Started, S::Running, none))
    .and_then(|b| b.on_with(S::Submitted, K::Failed, S::Failed, failed))
    .and_then(|b| {
        b.on_with(
            S::Submitted,
            K::CancelRequested,
            S::Cancelled,
            cancelled_with_job,
        )
    })
    // Running（Started 未发出）
    .and_then(|b| {
        b.on_with(
            S::Running,
            K::DeltaArrived,
            S::Streaming,
            started_then_delta,
        )
    })
    .and_then(|b| {
        b.on_with(
            S::Running,
            K::Completed,
            S::Completed,
            started_then_complete,
        )
    })
    .and_then(|b| b.on_with(S::Running, K::Failed, S::Failed, failed))
    .and_then(|b| b.on_with(S::Running, K::FallbackChosen, S::Submitted, submit))
    .and_then(|b| {
        b.on_with(
            S::Running,
            K::CancelRequested,
            S::Cancelled,
            cancelled_before_delta,
        )
    })
    // Streaming（Started 已发出，无回退边）
    .and_then(|b| b.on_with(S::Streaming, K::DeltaArrived, S::Streaming, delta))
    .and_then(|b| b.on_with(S::Streaming, K::Completed, S::Completed, complete))
    .and_then(|b| b.on_with(S::Streaming, K::Failed, S::Failed, failed))
    .and_then(|b| {
        b.on_with(
            S::Streaming,
            K::CancelRequested,
            S::Cancelled,
            cancelled_with_job,
        )
    });
    match table {
        Ok(builder) => builder.build(),
        Err(_) => FsmBuilder::new(S::Accepted).build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use madinfer_core_types::DegradeKind;

    fn count(effects: &[RequestEffect]) -> (usize, usize) {
        let started = effects
            .iter()
            .filter(|e| matches!(e, RequestEffect::EmitStarted))
            .count();
        let terminals = effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    RequestEffect::EmitComplete | RequestEffect::EmitFailed { .. }
                )
            })
            .count();
        (started, terminals)
    }

    #[test]
    fn table_is_locked() {
        assert_eq!(build_request_fsm().table_size(), REQUEST_FSM_TABLE_SIZE);
    }

    #[test]
    fn started_is_deferred_until_first_delta_and_emitted_before_it() {
        let mut fsm = build_request_fsm();
        assert_eq!(
            fsm.fire(&RequestEvent::Routed, 0).effects,
            vec![RequestEffect::SubmitJob]
        );
        let t = fsm.fire(
            &RequestEvent::Started {
                source: Source::Bundled,
            },
            1,
        );
        assert!(t.accepted);
        assert!(t.effects.is_empty(), "Started 只推状态，不发事件");
        assert_eq!(fsm.state(), RequestState::Running);
        let t = fsm.fire(&RequestEvent::DeltaArrived, 2);
        assert_eq!(
            t.effects,
            vec![RequestEffect::EmitStarted, RequestEffect::EmitDelta],
            "首个 Delta 前一刻发 Started"
        );
        assert_eq!(fsm.state(), RequestState::Streaming);
        let t = fsm.fire(&RequestEvent::DeltaArrived, 3);
        assert_eq!(t.effects, vec![RequestEffect::EmitDelta]);
        let t = fsm.fire(&RequestEvent::Completed, 4);
        assert_eq!(
            t.effects,
            vec![RequestEffect::FinishFramer, RequestEffect::EmitComplete]
        );

        // 无 Delta 直接完成：Started 紧贴 Complete 之前。
        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::Routed, 0);
        fsm.fire(
            &RequestEvent::Started {
                source: Source::SystemModel,
            },
            1,
        );
        let t = fsm.fire(&RequestEvent::Completed, 2);
        assert_eq!(
            t.effects,
            vec![
                RequestEffect::EmitStarted,
                RequestEffect::FinishFramer,
                RequestEffect::EmitComplete
            ]
        );
    }

    #[test]
    fn fallback_is_allowed_in_running_but_not_in_streaming_and_started_stays_once() {
        let mut fsm = build_request_fsm();
        let mut started = 0;
        let mut terminals = 0;
        let script = [
            RequestEvent::Routed,
            RequestEvent::FallbackChosen, // Submitted 期回退：重提
            RequestEvent::Started {
                source: Source::SystemModel,
            },
            RequestEvent::FallbackChosen, // Running 期回退：业务尚未收到任何内容，允许
            RequestEvent::Started {
                source: Source::Bundled,
            },
            RequestEvent::Started {
                source: Source::Cloud,
            }, // 非法：Running 再 Started
            RequestEvent::DeltaArrived,
            RequestEvent::FallbackChosen, // 非法：Streaming 期不再推进
            RequestEvent::DeltaArrived,
            RequestEvent::Completed,
            RequestEvent::Completed,    // 非法：终态后无事件
            RequestEvent::DeltaArrived, // 非法
        ];
        let mut submits = 0;
        for (i, e) in script.iter().enumerate() {
            let t = fsm.fire(e, i as u64);
            let (s, term) = count(&t.effects);
            started += s;
            terminals += term;
            submits += t
                .effects
                .iter()
                .filter(|e| matches!(e, RequestEffect::SubmitJob))
                .count();
        }
        assert_eq!(started, 1, "Started 至多一次");
        assert_eq!(terminals, 1, "终态恰一次");
        assert_eq!(submits, 3, "首提 + 两次回退重提");
        assert_eq!(fsm.state(), RequestState::Completed);
        assert_eq!(fsm.unexpected_count(), 4);
        assert_eq!(fsm.fired_count(), 8);
        let trace: Vec<String> = fsm.trace().map(|r| r.describe()).collect();
        assert_eq!(trace[2], "Submitted --Started--> Running @2ms");
        assert_eq!(trace[3], "Running --FallbackChosen--> Submitted @3ms");
    }

    #[test]
    fn failure_after_streaming_finishes_framer_before_failed_without_second_started() {
        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::Routed, 0);
        fsm.fire(
            &RequestEvent::Started {
                source: Source::SystemModel,
            },
            1,
        );
        fsm.fire(&RequestEvent::DeltaArrived, 2);
        let t = fsm.fire(
            &RequestEvent::Failed {
                error: InferError::Degradable {
                    kind: DegradeKind::Deadline,
                },
            },
            3,
        );
        assert_eq!(
            t.effects,
            vec![
                RequestEffect::FinishFramer,
                RequestEffect::EmitFailed {
                    error: InferError::Degradable {
                        kind: DegradeKind::Deadline
                    }
                }
            ]
        );
        // Running 期失败（无回退可选时 owner 直接投 Failed）：没有 Started，业务只看到 Failed。
        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::Routed, 0);
        fsm.fire(
            &RequestEvent::Started {
                source: Source::Bundled,
            },
            1,
        );
        let t = fsm.fire(
            &RequestEvent::Failed {
                error: InferError::Fatal {
                    kind: FatalKind::AssetCorrupt,
                    detail: 0,
                },
            },
            2,
        );
        assert_eq!(count(&t.effects), (0, 1), "Running 期失败无 Started");
    }

    #[test]
    fn awaiting_consent_has_three_exits() {
        let mut fsm = build_request_fsm();
        let t = fsm.fire(&RequestEvent::ConsentRequired, 0);
        assert_eq!(t.effects, vec![RequestEffect::RequestConsent]);
        assert_eq!(fsm.state(), RequestState::AwaitingConsent);
        let t = fsm.fire(&RequestEvent::ConsentGranted, 1);
        assert_eq!(t.effects, vec![RequestEffect::SubmitJob]);
        assert_eq!(fsm.state(), RequestState::Submitted);

        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::ConsentRequired, 0);
        let t = fsm.fire(&RequestEvent::ConsentDenied, 1);
        assert!(
            matches!(t.effects.as_slice(), [RequestEffect::EmitFailed { error }] if error.code() == 3007)
        );
        assert_eq!(fsm.state(), RequestState::Failed);

        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::ConsentRequired, 0);
        let t = fsm.fire(&RequestEvent::CancelRequested, 1);
        assert!(
            matches!(t.effects.as_slice(), [RequestEffect::EmitFailed { error }] if error.code() == 3008),
            "授权期取消：无 Job 可取消"
        );
        assert_eq!(fsm.state(), RequestState::Cancelled);
        assert!(
            !fsm.fire(&RequestEvent::ConsentGranted, 2).accepted,
            "终态后授权回执哑火"
        );
    }

    #[test]
    fn cancel_effects_depend_on_phase() {
        let mut fsm = build_request_fsm();
        let t = fsm.fire(&RequestEvent::CancelRequested, 0);
        assert!(!t.effects.contains(&RequestEffect::CancelJob));
        assert!(
            matches!(t.effects[0], RequestEffect::EmitFailed { error } if error.code() == 3008)
        );

        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::Routed, 0);
        let t = fsm.fire(&RequestEvent::CancelRequested, 1);
        assert_eq!(
            t.effects[0],
            RequestEffect::CancelJob,
            "Submitted 期有 Job 可取消"
        );

        let mut fsm = build_request_fsm();
        fsm.fire(&RequestEvent::Routed, 0);
        fsm.fire(
            &RequestEvent::Started {
                source: Source::Bundled,
            },
            1,
        );
        let t = fsm.fire(&RequestEvent::CancelRequested, 2);
        assert_eq!(
            t.effects,
            vec![
                RequestEffect::CancelJob,
                RequestEffect::EmitFailed { error: CANCELLED }
            ],
            "Running 期取消：无 Started、无 Delta，无需结束合帧器"
        );
    }
}
