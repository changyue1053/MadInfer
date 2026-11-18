//! madinfer-runtime —— L4 运行时（设计文档 C5）。
//!
//! - [`residency`]：模型驻留 FSM（分析篇图 6）：Loading → Mapped → Warm → Serving ⇄ Cooling → Unloading → Unloaded；引用归零进 Cooling
//!   不立即卸载（到期票带代数）；加载中取用返回 `Acquire::Loading` 等待；12 条迁移。
//! - [`registry`]：模型注册表——`begin_load` / `on_loaded` / `on_load_failed` 加载协议、引用计数、最近使用、footprint、
//!   一基座规则（同 backend 的基座只允许一个 Serving）、压力逐出顺序（空闲上下文 KV → Cooling 适配器 → Cooling 模型 → Critical 时丢全部 KV）。
//! - [`context`]：上下文池——亲和 worker、KV 预算、LRU 回收（生成上下文 = KV 缓存 + 适配器亲和；词表提案 `Context`，与 IM 的 `Conversation` 区分）。
//!
//! 全部 sans-io：加载 / 卸载 / 取消后台 Job 以效果返回，由 worker 池执行；时间由调用方传入（注入时钟）。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod context;
pub mod registry;
pub mod residency;

pub use context::{ContextPool, ContextPoolStats, KvBudget, Reserve};
pub use registry::{Acquire, EvictReport, ModelKey, ModelRegistry, RuntimeEffect, RuntimeStats};
pub use residency::{
    build_residency_fsm, LifeEffect, LifeEvent, LifeEventKind, LifeState, RESIDENCY_FSM_TABLE_SIZE,
};
