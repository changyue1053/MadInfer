//! 模型驻留 FSM（12 条迁移）。引用计数、footprint、最近使用是 owner 附属字段，不进状态。
//!
//! ```text
//!  Loading ──Loaded──▶ Mapped ──Warmed──▶ Warm ──Acquired──▶ Serving ──LastReleased──▶ Cooling ──CoolElapsed──▶ Unloading ──UnloadFinished──▶ Unloaded
//!    │  │                │                 │                    ▲                          │                        ▲
//!    │  └──LoadFailed──▶ Unloaded          │                    │                          │                        │
//!    └──EvictRequested───┴──EvictRequested─┴──EvictRequested────┼──────Acquired───────────┘（回到 Serving，取消到期票）
//!                                                               └──────EvictRequested（Cooling → Unloading）──────┘
//! ```
//!
//! madai-w04 加初始态 `Loading`（9 → 12）：worker 在加载期间的取用返回 `Acquire::Loading` 等待；加载失败直接 Unloaded（终态即移除）；
//! 加载途中压力逐出走 Unloading（取消加载）。轨迹样例：`Loading --Loaded--> Mapped @3ms`、`Mapped --Warmed--> Warm @4ms`、
//! `Warm --Acquired--> Serving @4ms`；失败路径 `Loading --LoadFailed--> Unloaded @9ms`。

use madinfer_fsm::{Fsm, FsmBuilder, FsmEvent};

/// 驻留状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LifeState {
    /// 后端加载中（已登记，权重未就绪）。
    Loading,
    /// 已 mmap，权重未触页。
    Mapped,
    /// 预热完成（首次 prefill 或触页），无引用。
    Warm,
    /// 有引用，正在服务。
    Serving,
    /// 引用归零，等冷却到期。
    Cooling,
    /// 卸载中。
    Unloading,
    /// 终态。
    Unloaded,
}

impl LifeState {
    /// 是否占内存（Unloaded 之外都占）。
    pub const fn resident(self) -> bool {
        !matches!(self, Self::Unloaded)
    }
}

/// 驻留事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifeEvent {
    /// 后端加载完成（mmap 就位）。
    Loaded,
    /// 后端加载失败。
    LoadFailed,
    /// 预热完成。
    Warmed,
    /// 有人取用（refs 0 → 1）。
    Acquired,
    /// 最后一个引用释放。
    LastReleased {
        /// 冷却时长。
        cool_ms: u64,
    },
    /// 冷却到期票。
    CoolElapsed,
    /// 压力逐出。
    EvictRequested,
    /// 后端已释放。
    UnloadFinished,
}

/// 驻留事件种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LifeEventKind {
    /// 见 [`LifeEvent::Loaded`]。
    Loaded,
    /// 见 [`LifeEvent::LoadFailed`]。
    LoadFailed,
    /// 见 [`LifeEvent::Warmed`]。
    Warmed,
    /// 见 [`LifeEvent::Acquired`]。
    Acquired,
    /// 见 [`LifeEvent::LastReleased`]。
    LastReleased,
    /// 见 [`LifeEvent::CoolElapsed`]。
    CoolElapsed,
    /// 见 [`LifeEvent::EvictRequested`]。
    EvictRequested,
    /// 见 [`LifeEvent::UnloadFinished`]。
    UnloadFinished,
}

impl FsmEvent for LifeEvent {
    type Kind = LifeEventKind;

    fn kind(&self) -> LifeEventKind {
        match self {
            LifeEvent::Loaded => LifeEventKind::Loaded,
            LifeEvent::LoadFailed => LifeEventKind::LoadFailed,
            LifeEvent::Warmed => LifeEventKind::Warmed,
            LifeEvent::Acquired => LifeEventKind::Acquired,
            LifeEvent::LastReleased { .. } => LifeEventKind::LastReleased,
            LifeEvent::CoolElapsed => LifeEventKind::CoolElapsed,
            LifeEvent::EvictRequested => LifeEventKind::EvictRequested,
            LifeEvent::UnloadFinished => LifeEventKind::UnloadFinished,
        }
    }
}

/// 驻留效果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifeEffect {
    /// 登记冷却到期票。
    ArmCoolTimer {
        /// 冷却时长。
        cool_ms: u64,
    },
    /// 让在途到期票哑火。
    CancelCoolTimer,
    /// 调后端卸载（释放 mmap / context）。
    Unload,
}

fn arm_cool(e: &LifeEvent) -> Vec<LifeEffect> {
    match e {
        LifeEvent::LastReleased { cool_ms } => vec![LifeEffect::ArmCoolTimer { cool_ms: *cool_ms }],
        _ => Vec::new(),
    }
}

fn cancel_cool(_: &LifeEvent) -> Vec<LifeEffect> {
    vec![LifeEffect::CancelCoolTimer]
}

fn unload(_: &LifeEvent) -> Vec<LifeEffect> {
    vec![LifeEffect::Unload]
}

/// 迁移表条目数。
pub const RESIDENCY_FSM_TABLE_SIZE: usize = 12;

/// 建一台驻留 FSM（初始 Loading）。
pub fn build_residency_fsm() -> Fsm<LifeState, LifeEvent, LifeEffect> {
    use LifeEventKind as K;
    use LifeState as S;
    let table = Ok(FsmBuilder::<S, LifeEvent, LifeEffect>::new(S::Loading))
        .and_then(|b| b.on(S::Loading, K::Loaded, S::Mapped))
        .and_then(|b| b.on(S::Loading, K::LoadFailed, S::Unloaded))
        .and_then(|b| b.on_with(S::Loading, K::EvictRequested, S::Unloading, unload))
        .and_then(|b| b.on(S::Mapped, K::Warmed, S::Warm))
        .and_then(|b| b.on_with(S::Mapped, K::EvictRequested, S::Unloading, unload))
        .and_then(|b| b.on(S::Warm, K::Acquired, S::Serving))
        .and_then(|b| b.on_with(S::Warm, K::EvictRequested, S::Unloading, unload))
        .and_then(|b| b.on_with(S::Serving, K::LastReleased, S::Cooling, arm_cool))
        .and_then(|b| b.on_with(S::Cooling, K::Acquired, S::Serving, cancel_cool))
        .and_then(|b| b.on_with(S::Cooling, K::CoolElapsed, S::Unloading, unload))
        .and_then(|b| b.on_with(S::Cooling, K::EvictRequested, S::Unloading, unload))
        .and_then(|b| b.on(S::Unloading, K::UnloadFinished, S::Unloaded));
    match table {
        Ok(builder) => builder.build(),
        Err(_) => FsmBuilder::new(S::Loading).build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_locked() {
        assert_eq!(build_residency_fsm().table_size(), RESIDENCY_FSM_TABLE_SIZE);
    }

    #[test]
    fn loading_has_three_exits() {
        let mut fsm = build_residency_fsm();
        assert_eq!(fsm.state(), LifeState::Loading);
        assert!(
            !fsm.fire(&LifeEvent::Acquired, 1).accepted,
            "加载中不可取用（owner 返回 Acquire::Loading 等待）"
        );
        assert!(fsm.fire(&LifeEvent::Loaded, 3).accepted);
        assert_eq!(fsm.state(), LifeState::Mapped);

        let mut fsm = build_residency_fsm();
        assert!(fsm.fire(&LifeEvent::LoadFailed, 9).accepted);
        assert_eq!(fsm.state(), LifeState::Unloaded);
        assert_eq!(
            fsm.trace().map(|r| r.describe()).collect::<Vec<_>>(),
            vec!["Loading --LoadFailed--> Unloaded @9ms"]
        );

        let mut fsm = build_residency_fsm();
        let t = fsm.fire(&LifeEvent::EvictRequested, 2);
        assert_eq!(
            t.effects,
            vec![LifeEffect::Unload],
            "加载途中逐出 = 取消加载"
        );
        assert_eq!(fsm.state(), LifeState::Unloading);
    }

    #[test]
    fn serving_cools_then_unloads_unless_reacquired() {
        let mut fsm = build_residency_fsm();
        fsm.fire(&LifeEvent::Loaded, 0);
        fsm.fire(&LifeEvent::Warmed, 0);
        fsm.fire(&LifeEvent::Acquired, 1);
        let t = fsm.fire(&LifeEvent::LastReleased { cool_ms: 30_000 }, 2);
        assert_eq!(
            t.effects,
            vec![LifeEffect::ArmCoolTimer { cool_ms: 30_000 }]
        );
        let t = fsm.fire(&LifeEvent::Acquired, 3);
        assert_eq!(t.effects, vec![LifeEffect::CancelCoolTimer]);
        assert_eq!(fsm.state(), LifeState::Serving);
        fsm.fire(&LifeEvent::LastReleased { cool_ms: 30_000 }, 4);
        let t = fsm.fire(&LifeEvent::CoolElapsed, 30_004);
        assert_eq!(t.effects, vec![LifeEffect::Unload]);
        fsm.fire(&LifeEvent::UnloadFinished, 30_010);
        assert_eq!(fsm.state(), LifeState::Unloaded);
        assert!(
            !fsm.fire(&LifeEvent::Acquired, 30_011).accepted,
            "卸载后不可取用，owner 需重新加载"
        );
    }

    #[test]
    fn serving_model_cannot_be_evicted() {
        let mut fsm = build_residency_fsm();
        fsm.fire(&LifeEvent::Loaded, 0);
        fsm.fire(&LifeEvent::Warmed, 0);
        fsm.fire(&LifeEvent::Acquired, 1);
        assert!(
            !fsm.fire(&LifeEvent::EvictRequested, 2).accepted,
            "Serving 的 refs > 0，逐出无出边（不变量）"
        );
        assert_eq!(fsm.unexpected_count(), 1);
    }
}
