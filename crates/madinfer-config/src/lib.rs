//! madinfer-config —— 横切 · 配置（设计文档 C14）。
//!
//! 应用条件（P4）：验签通过 ∧ schema / 解析通过 ∧ version 大于当前 ∧ min_sdk 满足 ∧ 设备桶在 rollout 内 ∧ 语义校验通过
//! （能力、后端、端点均已注册；阈值范围合法）。任一不满足整份忽略并给出 `config_rejected{reason}`。
//! 应用是原子的：先构造新文档并全量校验，再一次换指针；`previous` 保留供回滚。
//!
//! 每份到达的文档跑一台管线 FSM（[`apply::build_apply_fsm`]，7 条迁移）：Received → Verified → Parsed → Validated → Applied，
//! 任一阶段失败 → Rejected；轨迹即 `config_apply` 观测事件的来源。sans-io：持久化与广播以 [`apply::ConfigEffect`] 返回给 owner。
//!
//! 禁区：没有任何字段能改变隐私门——`when` 里出现 `privacy` 直接判语义错误。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod apply;
pub mod doc;
pub mod verify;

pub use apply::{
    build_apply_fsm, ApplyOutcome, ApplyStage, ConfigEffect, ConfigStats, ConfigStore, Registry,
    RejectReason, APPLY_FSM_TABLE_SIZE,
};
pub use doc::{
    CandidateDoc, ConfigDoc, MappingEntryDoc, QuotaDoc, Signature, Switches, TargetDoc, Thresholds,
    WhenDoc,
};
pub use verify::{SignatureVerifier, TrustAllVerifier, VerifyOutcome};
