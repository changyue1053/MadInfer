//! 档位判定：RAM 与内存带宽两维各判一档、取低档；任一维缺失即 `Tier::Unknown`。
//!
//! 档位线可由参数传入（config.v1 `thresholds.tier_lines`），缺省为编译内值（设计文档 §5.4 示例同值）。
//! Unknown / Low 档只允许系统模型与云端（[`allowed_sources`]）——内联模型不进决策链。

use madinfer_core_types::{Source, Tier};

/// 一档的准入线（两维都须达线）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TierLine {
    /// RAM 下限（MB）。
    pub ram_mb: u32,
    /// 内存带宽下限（GB/s × 10）。
    pub memory_bandwidth_gbps_x10: u32,
}

/// 档位线（Flagship / Mid 两条；线下即 Low）。约定 flagship 线各维 ≥ mid 线，配置语义校验在 C14。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TierLines {
    /// Flagship 准入线。
    pub flagship: TierLine,
    /// Mid 准入线。
    pub mid: TierLine,
}

impl Default for TierLines {
    /// 编译内缺省值（与 config.v1 `thresholds.tier_lines` 出厂示例同值）。
    fn default() -> Self {
        Self {
            flagship: TierLine {
                ram_mb: 11_000,
                memory_bandwidth_gbps_x10: 600,
            },
            mid: TierLine {
                ram_mb: 7_000,
                memory_bandwidth_gbps_x10: 350,
            },
        }
    }
}

const fn dimension_tier(value: u32, flagship_line: u32, mid_line: u32) -> Tier {
    if value >= flagship_line {
        Tier::Flagship
    } else if value >= mid_line {
        Tier::Mid
    } else {
        Tier::Low
    }
}

/// 档位判定纯函数：RAM 与带宽各判一档，取低档。
///
/// `ram_mb == 0`（宿主未报）或带宽未知（查表未中且无实测）→ `Tier::Unknown`——Unknown 单独判定，
/// 不参与两维比较（core-types `Tier` 的口径）。
pub fn judge_tier(ram_mb: u32, memory_bandwidth_gbps_x10: Option<u32>, lines: &TierLines) -> Tier {
    let Some(bandwidth) = memory_bandwidth_gbps_x10 else {
        return Tier::Unknown;
    };
    if ram_mb == 0 {
        return Tier::Unknown;
    }
    let by_ram = dimension_tier(ram_mb, lines.flagship.ram_mb, lines.mid.ram_mb);
    let by_bandwidth = dimension_tier(
        bandwidth,
        lines.flagship.memory_bandwidth_gbps_x10,
        lines.mid.memory_bandwidth_gbps_x10,
    );
    by_ram.min(by_bandwidth)
}

/// 本档缺省允许的实现来源（纯函数）：`Unknown` 与 `Low` 只许系统模型与云端，`Mid` / `Flagship` 三源全开。
/// madai-w08 G11：事实源上提到 `madinfer_core_types::Tier::allowed_sources`（路由与能力索引直接消费，候选显式
/// `tier_min` 优先于本表——I-H-04 复核发现 Low 档端侧包照放行的根因是本表此前只在 device 画像里，没进路由）；本函数保留为薄包装。
pub const fn allowed_sources(tier: Tier) -> &'static [Source] {
    tier.allowed_sources()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_takes_the_lower_of_two_dimensions() {
        let lines = TierLines::default();
        assert_eq!(judge_tier(12_288, Some(768), &lines), Tier::Flagship);
        assert_eq!(judge_tier(12_288, Some(512), &lines), Tier::Mid, "带宽拖低");
        assert_eq!(judge_tier(8_192, Some(853), &lines), Tier::Mid, "RAM 拖低");
        assert_eq!(judge_tier(4_096, Some(853), &lines), Tier::Low);
        assert_eq!(judge_tier(12_288, Some(100), &lines), Tier::Low);
    }

    #[test]
    fn lines_are_inclusive_at_the_boundary() {
        let lines = TierLines::default();
        assert_eq!(judge_tier(11_000, Some(600), &lines), Tier::Flagship);
        assert_eq!(judge_tier(10_999, Some(600), &lines), Tier::Mid);
        assert_eq!(judge_tier(7_000, Some(350), &lines), Tier::Mid);
        assert_eq!(judge_tier(7_000, Some(349), &lines), Tier::Low);
    }

    #[test]
    fn missing_dimension_yields_unknown() {
        let lines = TierLines::default();
        assert_eq!(judge_tier(12_288, None, &lines), Tier::Unknown);
        assert_eq!(judge_tier(0, Some(768), &lines), Tier::Unknown);
        assert_eq!(judge_tier(0, None, &lines), Tier::Unknown);
    }

    #[test]
    fn lines_are_a_parameter_not_a_constant() {
        let strict = TierLines {
            flagship: TierLine {
                ram_mb: 15_000,
                memory_bandwidth_gbps_x10: 900,
            },
            mid: TierLine {
                ram_mb: 11_000,
                memory_bandwidth_gbps_x10: 600,
            },
        };
        assert_eq!(judge_tier(12_288, Some(768), &strict), Tier::Mid);
        assert_eq!(
            judge_tier(12_288, Some(768), &TierLines::default()),
            Tier::Flagship
        );
    }

    #[test]
    fn unknown_and_low_allow_only_system_model_and_cloud() {
        assert_eq!(
            allowed_sources(Tier::Unknown),
            &[Source::SystemModel, Source::Cloud]
        );
        assert_eq!(
            allowed_sources(Tier::Low),
            &[Source::SystemModel, Source::Cloud]
        );
        // 薄包装与 core-types 事实源同表（G11）。
        for tier in [Tier::Low, Tier::Mid, Tier::Flagship, Tier::Unknown] {
            assert_eq!(allowed_sources(tier), tier.allowed_sources());
        }
        assert!(!allowed_sources(Tier::Unknown).contains(&Source::Bundled));
        assert!(!allowed_sources(Tier::Low).contains(&Source::Bundled));
        for tier in [Tier::Mid, Tier::Flagship] {
            assert_eq!(
                allowed_sources(tier),
                &[Source::SystemModel, Source::Bundled, Source::Cloud]
            );
        }
    }
}
