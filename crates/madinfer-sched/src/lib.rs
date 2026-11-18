//! madinfer-sched —— L3 调度（设计文档 C4）。
//!
//! 结构沿 mad 的两条范式：
//! - **sans-io 核**（[`SchedCore`]）：所有状态（三档队列、槽表、配额台账、门控、定时器、每 Job 一台 FSM）归属一个对象，
//!   入口只有 [`SchedCore::handle`]（命令）与 [`SchedCore::tick`]（时间到），输出 [`SchedEffect`] 列表；不碰线程、不碰 IO、不读墙钟。
//!   测试用 `ManualClock` 步进，确定性回放。范式同 `capability-im-engine` 的 `MessageSender`（owner 持 FSM、执行效果）
//!   与 im-core `media.rs` 的"锁外 IO、锁内 apply_step"。
//! - **单消费者 actor**（[`SchedulerActor`]）：control 线程 + 有界邮箱（`std::sync::mpsc`），`recv_timeout` 等到下一个定时器
//!   到期或命令到达；不轮询、不共享锁（clippy.toml 禁 `Mutex`）。worker 完成后以 [`WorkerReport`] 回执进同一邮箱——
//!   API 调用与 worker 回执同序，与 `Engine.kt` 的邮箱泵一致。
//!
//! Job 生命周期是一台表驱动 FSM（[`fsm`] 模块，23 条迁移，`table_size` 锁拓扑）；进度 / 排队时长 / 抢占次数是 owner 的附属字段，
//! 不编码进状态。定时器带代数票：Deferred 的到期票与 Job 的 `defer_generation` 不符即哑火计数，零副作用。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod actor;
pub mod core;
pub mod fsm;
pub mod gates;
pub mod job;
pub mod quota;

pub use actor::{SchedulerActor, SchedulerHandle};
pub use core::{CancelReason, SchedCommand, SchedCore, SchedEffect, SchedStats, WorkerReport};
pub use fsm::{JobEffect, JobEvent, JobEventKind, JobState};
pub use gates::{Admission, DefaultGatePolicy, GateName, GatePolicy, Gates};
pub use job::{Job, JobCtx, JobId, JobKind, ResourceEstimate, SlotTable};
pub use quota::{Quota, QuotaLedger, QuotaTable, QuotaVerdict};
