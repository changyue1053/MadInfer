//! 门控：资源信号聚合（[`Gates`]）与准入策略（[`GatePolicy`]；缺省表 [`DefaultGatePolicy`]，设计文档 C4 关键机制 ①）。

use madinfer_core_types::{QoS, RetryKind};
use madinfer_host::{AppState, MemoryPressure, OsSignal, ThermalState};

/// 宿主主链路忙信号的有效期：5 s 无更新自动解除。
pub const MAIN_LINK_BUSY_TTL_MS: u64 = 5_000;

/// 资源信号聚合（过去式事实的最新读数）。缺省（`Default`）全部"不可知"：后台档三门永不齐开。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Gates {
    /// 热状态；None = 未知。
    pub thermal: Option<ThermalState>,
    /// 充电；None = 未知。
    pub charging: Option<bool>,
    /// 电量百分比；None = 未知（宿主读不到）。未知不参与电量门控——策略不得以满电 / 空电代入，最新一条信号报未知即回 None（不沿用旧读数）。
    pub battery_pct: Option<u8>,
    /// 低电量模式。
    pub low_power_mode: bool,
    /// 内存压力；None = 未知。
    pub memory: Option<MemoryPressure>,
    /// 前后台；None = 未知。
    pub app: Option<AppState>,
    /// 用户空闲；None = 未知。
    pub idle: Option<bool>,
    /// 主链路忙到何时（注入时钟毫秒）。
    pub main_link_busy_until_ms: Option<u64>,
}

impl Gates {
    /// 吸收一条信号。
    pub fn apply(&mut self, signal: OsSignal, now_ms: u64) {
        match signal {
            OsSignal::Thermal(state) => self.thermal = Some(state),
            OsSignal::Battery {
                level_pct,
                charging,
                low_power_mode,
            } => {
                // 未知原样记 None（madai-w09 I-C-08：不再由入口面以 100 代入）。
                self.battery_pct = level_pct;
                self.charging = Some(charging);
                self.low_power_mode = low_power_mode;
            }
            OsSignal::Memory(level) => self.memory = Some(level),
            OsSignal::App { state, .. } => self.app = Some(state),
            OsSignal::Network(_) => {}
            OsSignal::Idle(idle) => self.idle = Some(idle),
            OsSignal::MainLinkBusy(busy) => {
                self.main_link_busy_until_ms = if busy {
                    Some(now_ms.saturating_add(MAIN_LINK_BUSY_TTL_MS))
                } else {
                    None
                };
            }
        }
    }

    /// 主链路此刻是否忙。
    pub fn main_link_busy(&self, now_ms: u64) -> bool {
        self.main_link_busy_until_ms
            .is_some_and(|until| now_ms < until)
    }

    /// 热状态是否 ≥ 某档（未知按 Nominal）。
    pub fn thermal_at_least(&self, level: ThermalState) -> bool {
        self.thermal.unwrap_or(ThermalState::Nominal) >= level
    }

    /// 内存压力是否 ≥ 某档（未知按 Normal）。
    pub fn memory_at_least(&self, level: MemoryPressure) -> bool {
        self.memory.unwrap_or(MemoryPressure::Normal) >= level
    }
}

/// 哪扇门拦的（进 telemetry sched_wait.gate）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GateName {
    /// 热。
    Thermal,
    /// 内存。
    Memory,
    /// 充电。
    Charging,
    /// 空闲。
    Idle,
    /// 前后台窗口。
    AppWindow,
    /// 主链路忙。
    HostBusy,
    /// 低电量模式。
    LowPower,
}

/// 准入结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// 放行。
    Run,
    /// 推迟。
    Defer {
        /// 推迟毫秒。
        ms: u64,
        /// 门。
        gate: GateName,
    },
    /// 拒绝（可重试类）。
    Reject {
        /// 原因。
        kind: RetryKind,
        /// 门。
        gate: GateName,
    },
}

/// 准入策略（可由配置替换缺省表）。
pub trait GatePolicy: Send {
    /// 判定。
    fn admit(&self, qos: QoS, gates: &Gates, now_ms: u64) -> Admission;
}

/// 缺省准入表（设计文档 C4 ①）：
///
/// | 档 | 规则 |
/// |---|---|
/// | 交互 | 热 Critical → Reject(ThermalThrottled)；内存 Critical → Reject(MemoryPressure)；其余 Run |
/// | 异步 | 热 ≥ Serious → Defer(2 s)；主链路忙 → Defer(剩余忙时长，≤ 2 s)；内存 Critical → Defer(2 s)；其余 Run |
/// | 后台 | 要求 充电 ∧ 空闲 ∧ 热 ≤ Fair ∧ (前台 ∨ 后台任务窗口) ∧ 内存 Normal ∧ 主链路不忙 ∧ 非低电量，否则 Defer(60 s) |
#[derive(Clone, Debug, Default)]
pub struct DefaultGatePolicy;

impl DefaultGatePolicy {
    /// 异步档推迟量。
    pub const ASYNC_DEFER_MS: u64 = 2_000;
    /// 后台档推迟量。
    pub const BACKGROUND_DEFER_MS: u64 = 60_000;
}

impl GatePolicy for DefaultGatePolicy {
    fn admit(&self, qos: QoS, gates: &Gates, now_ms: u64) -> Admission {
        match qos {
            QoS::Interactive => {
                if gates.thermal_at_least(ThermalState::Critical) {
                    Admission::Reject {
                        kind: RetryKind::ThermalThrottled,
                        gate: GateName::Thermal,
                    }
                } else if gates.memory_at_least(MemoryPressure::Critical) {
                    Admission::Reject {
                        kind: RetryKind::MemoryPressure,
                        gate: GateName::Memory,
                    }
                } else {
                    Admission::Run
                }
            }
            QoS::Async => {
                if gates.thermal_at_least(ThermalState::Serious) {
                    Admission::Defer {
                        ms: Self::ASYNC_DEFER_MS,
                        gate: GateName::Thermal,
                    }
                } else if gates.main_link_busy(now_ms) {
                    let remaining = gates
                        .main_link_busy_until_ms
                        .map_or(0, |u| u.saturating_sub(now_ms));
                    Admission::Defer {
                        ms: remaining.clamp(1, Self::ASYNC_DEFER_MS),
                        gate: GateName::HostBusy,
                    }
                } else if gates.memory_at_least(MemoryPressure::Critical) {
                    Admission::Defer {
                        ms: Self::ASYNC_DEFER_MS,
                        gate: GateName::Memory,
                    }
                } else {
                    Admission::Run
                }
            }
            QoS::Background => {
                let defer = |gate| Admission::Defer {
                    ms: Self::BACKGROUND_DEFER_MS,
                    gate,
                };
                if gates.charging != Some(true) {
                    return defer(GateName::Charging);
                }
                if gates.idle != Some(true) {
                    return defer(GateName::Idle);
                }
                if gates.thermal_at_least(ThermalState::Serious) {
                    return defer(GateName::Thermal);
                }
                if !matches!(
                    gates.app,
                    Some(AppState::Foreground | AppState::BackgroundTask)
                ) {
                    return defer(GateName::AppWindow);
                }
                if gates.memory_at_least(MemoryPressure::Warning) {
                    return defer(GateName::Memory);
                }
                if gates.main_link_busy(now_ms) {
                    return defer(GateName::HostBusy);
                }
                if gates.low_power_mode {
                    return defer(GateName::LowPower);
                }
                Admission::Run
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_green(now: u64) -> Gates {
        let mut g = Gates::default();
        g.apply(OsSignal::Thermal(ThermalState::Nominal), now);
        g.apply(
            OsSignal::Battery {
                level_pct: Some(80),
                charging: true,
                low_power_mode: false,
            },
            now,
        );
        g.apply(OsSignal::Memory(MemoryPressure::Normal), now);
        g.apply(
            OsSignal::App {
                state: AppState::Foreground,
                budget_ms: None,
            },
            now,
        );
        g.apply(OsSignal::Idle(true), now);
        g
    }

    #[test]
    fn unknown_gates_run_interactive_defer_background() {
        let p = DefaultGatePolicy;
        let g = Gates::default();
        assert_eq!(p.admit(QoS::Interactive, &g, 0), Admission::Run);
        assert_eq!(p.admit(QoS::Async, &g, 0), Admission::Run);
        assert_eq!(
            p.admit(QoS::Background, &g, 0),
            Admission::Defer {
                ms: 60_000,
                gate: GateName::Charging
            },
            "后台档缺省不可知 → 三门永不齐开"
        );
    }

    #[test]
    fn interactive_rejects_only_on_critical() {
        let p = DefaultGatePolicy;
        let mut g = all_green(0);
        g.apply(OsSignal::Thermal(ThermalState::Serious), 0);
        assert_eq!(p.admit(QoS::Interactive, &g, 0), Admission::Run);
        g.apply(OsSignal::Thermal(ThermalState::Critical), 0);
        assert_eq!(
            p.admit(QoS::Interactive, &g, 0),
            Admission::Reject {
                kind: RetryKind::ThermalThrottled,
                gate: GateName::Thermal
            }
        );
    }

    #[test]
    fn async_defers_while_host_busy_and_recovers_after_ttl() {
        let p = DefaultGatePolicy;
        let mut g = all_green(1_000);
        g.apply(OsSignal::MainLinkBusy(true), 1_000);
        assert_eq!(
            p.admit(QoS::Async, &g, 1_500),
            Admission::Defer {
                ms: 2_000,
                gate: GateName::HostBusy
            }
        );
        assert_eq!(
            p.admit(QoS::Async, &g, 5_500),
            Admission::Defer {
                ms: 500,
                gate: GateName::HostBusy
            },
            "剩余忙时长 < 2 s 时按剩余"
        );
        assert_eq!(
            p.admit(QoS::Async, &g, 6_000),
            Admission::Run,
            "5 s 无更新自动解除"
        );
    }

    /// 电量未知（None）原样记 None，不以满电代入；最新一条信号为准（已知 → 未知 → 已知）。
    #[test]
    fn battery_unknown_is_recorded_as_unknown_not_full() {
        let mut g = Gates::default();
        assert_eq!(g.battery_pct, None, "缺省不可知");
        g.apply(
            OsSignal::Battery {
                level_pct: None,
                charging: true,
                low_power_mode: false,
            },
            0,
        );
        assert_eq!(g.battery_pct, None, "未知不得记成 Some(100)");
        assert_eq!(g.charging, Some(true), "电量未知时充电读数仍按实记");
        g.apply(
            OsSignal::Battery {
                level_pct: Some(15),
                charging: false,
                low_power_mode: true,
            },
            0,
        );
        assert_eq!(g.battery_pct, Some(15));
        g.apply(
            OsSignal::Battery {
                level_pct: None,
                charging: false,
                low_power_mode: true,
            },
            0,
        );
        assert_eq!(g.battery_pct, None, "读数丢失后不沿用旧值 15");
        assert!(g.low_power_mode, "低电量模式独立于电量读数");
    }

    /// None 不降级：缺省表下电量未知与满电的准入结论逐档相同（电量未知不参与门控）。
    #[test]
    fn battery_unknown_does_not_degrade_any_qos() {
        let p = DefaultGatePolicy;
        let known = all_green(0);
        let mut unknown = all_green(0);
        unknown.apply(
            OsSignal::Battery {
                level_pct: None,
                charging: true,
                low_power_mode: false,
            },
            0,
        );
        assert_eq!(unknown.battery_pct, None);
        for qos in [QoS::Interactive, QoS::Async, QoS::Background] {
            assert_eq!(
                p.admit(qos, &unknown, 0),
                p.admit(qos, &known, 0),
                "{qos:?}：电量未知与已知 80% 同结论"
            );
            assert_eq!(p.admit(qos, &unknown, 0), Admission::Run);
        }
    }

    /// 电量阈值策略（`SchedCore::with_policy` 可注入的 `GatePolicy`）看到的是真值：Some(低) 降级、Some(高) 与 None 都放行。
    /// 缺省表 [`DefaultGatePolicy`] 今日无电量阈值门（后台档的"非低电量"是 low_power_mode 开关）；本用例用测试内阈值策略
    /// 钉住 `Gates.battery_pct` 对策略的契约——若入口面仍把 None 代入 100，"未知"与"满电"在此不可区分。
    #[test]
    fn battery_level_policy_degrades_only_on_known_low_level() {
        struct LevelPolicy {
            threshold_pct: u8,
        }
        impl GatePolicy for LevelPolicy {
            fn admit(&self, qos: QoS, gates: &Gates, now_ms: u64) -> Admission {
                let low = gates
                    .battery_pct
                    .is_some_and(|pct| pct <= self.threshold_pct);
                if qos == QoS::Async && low && gates.charging != Some(true) {
                    return Admission::Defer {
                        ms: DefaultGatePolicy::ASYNC_DEFER_MS,
                        gate: GateName::LowPower,
                    };
                }
                DefaultGatePolicy.admit(qos, gates, now_ms)
            }
        }
        let p = LevelPolicy { threshold_pct: 20 };
        let battery = |level_pct| OsSignal::Battery {
            level_pct,
            charging: false,
            low_power_mode: false,
        };
        let mut g = all_green(0);
        g.apply(battery(Some(10)), 0);
        assert_eq!(
            p.admit(QoS::Async, &g, 0),
            Admission::Defer {
                ms: 2_000,
                gate: GateName::LowPower
            },
            "Some(低) 降级"
        );
        g.apply(battery(Some(20)), 0);
        assert!(
            matches!(p.admit(QoS::Async, &g, 0), Admission::Defer { .. }),
            "阈值含等号"
        );
        g.apply(battery(Some(21)), 0);
        assert_eq!(p.admit(QoS::Async, &g, 0), Admission::Run, "Some(高) 放行");
        g.apply(battery(None), 0);
        assert_eq!(g.battery_pct, None);
        assert_eq!(
            p.admit(QoS::Async, &g, 0),
            Admission::Run,
            "None 不降级：未知不参与电量门控"
        );
        assert_eq!(
            p.admit(QoS::Interactive, &g, 0),
            Admission::Run,
            "交互档不受电量阈值影响"
        );
    }

    #[test]
    fn background_requires_all_gates() {
        let p = DefaultGatePolicy;
        let g = all_green(0);
        assert_eq!(p.admit(QoS::Background, &g, 0), Admission::Run);
        let mut g2 = g.clone();
        g2.apply(OsSignal::Idle(false), 0);
        assert!(matches!(
            p.admit(QoS::Background, &g2, 0),
            Admission::Defer {
                gate: GateName::Idle,
                ..
            }
        ));
        let mut g3 = g.clone();
        g3.apply(
            OsSignal::App {
                state: AppState::Background,
                budget_ms: None,
            },
            0,
        );
        assert!(matches!(
            p.admit(QoS::Background, &g3, 0),
            Admission::Defer {
                gate: GateName::AppWindow,
                ..
            }
        ));
    }
}
