//! madinfer-api —— L1 能力面（设计文档 C2）。
//!
//! madai-w02 落三块与线程 / IO 无关的纯逻辑；madai-w04（I2b）落核 actor：[`core`] 的 `CoreActor`（control 线程独占调度核、
//! 每请求一台请求 FSM、路由决策、合帧器、能力索引、配置、观测、后端注册表、模型注册表）+ [`workers`] 的 `WorkerPool`
//! + [`pipeline`] 的请求 / 事件类型与目标映射。`Infer` 门面（FFI 形状由 C ABI 与 UniFFI 倒推）随 I3 落地，`Core::builder()` 是它的雏形。
//!
//! - [`framer`]：token 合帧（缺省 80 ms），范式同 `capability-im-engine` 的 `TransferThrottle`——窗口内折叠、到期票带代数、
//!   终态强制放行、计数台账。
//! - [`request`]：请求生命周期 FSM（20 条迁移），把"Started 至多一次且先于任何 Delta / Complete → 终态恰一次"的顺序保证做成表；
//!   Started 延迟到首个 Delta / Complete 前一刻发出，降级链只在首个 Delta 前推进（已出 Delta 后目标失败 = 请求失败，partial 随 Failed 返回）；
//!   升云前的一次性授权是 `AwaitingConsent` 态。
//! - [`capability_index`]：能力索引，聚合资产就绪、后端 availability、开关、档位，重算 capabilities.v1 快照并单调 bump version。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod capability_index;
pub mod core;
pub mod framer;
pub mod pipeline;
pub mod request;
pub mod structured;
pub mod workers;

pub use capability_index::{
    CapState, CapabilityIndex, CapabilitySnapshot, CapabilityStatus, IndexInputs, LatencyTier,
    UnavailableReason,
};
pub use core::{
    ApplyReport, Core, CoreBuilder, CoreCommand, CoreHandle, CoreStats, DEFAULT_MAILBOX_CAPACITY,
};
pub use framer::{Delta, Framer, FramerEffect, FramerStats, TokenPiece, DEFAULT_FRAME_MS};
pub use pipeline::{Event, EventSink, GenRequest, GenStats, Output, OutputSpec};
pub use request::{
    build_request_fsm, RequestEffect, RequestEvent, RequestEventKind, RequestState,
    REQUEST_FSM_TABLE_SIZE,
};
pub use structured::{complete_prefix, StructuredError, StructuredTracker};
pub use workers::{ContextReservation, WorkOrder, WorkOutcome, WorkerIndex, WorkerPool};
