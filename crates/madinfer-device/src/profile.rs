//! 设备画像组装：宿主事实 + 识别表 → 档位与来源门。纯函数；画像不含任何设备标识符（device.v1 不变量）。

use madinfer_core_types::{Source, Tier};
use madinfer_host::DeviceFacts;

use crate::soc::{identify_soc, SocSpec};
use crate::tier::{allowed_sources, judge_tier, TierLines};

/// 设备画像（C11 产物）：档位、识别出的 SoC 规格、判定所用带宽。
///
/// 带宽本批取识别表静态估值；微基准实测落地后经 [`DeviceProfile::from_facts_with_bandwidth`] 覆盖
/// （基准本身是 IO，归宿主执行窗口，此处只消费数值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    /// 档位。
    pub tier: Tier,
    /// 识别出的 SoC 规格（查表未中 = None）。
    pub soc: Option<&'static SocSpec>,
    /// 判定所用内存带宽（GB/s × 10；None = 未知，此时档位必为 Unknown）。
    pub memory_bandwidth_gbps_x10: Option<u32>,
}

impl DeviceProfile {
    /// 以缺省档位线（编译内值）判档。
    pub fn from_facts(facts: &DeviceFacts) -> Self {
        Self::from_facts_with_lines(facts, &TierLines::default())
    }

    /// 档位线由配置传入（config.v1 `thresholds.tier_lines`）。
    pub fn from_facts_with_lines(facts: &DeviceFacts, lines: &TierLines) -> Self {
        let soc = identify_soc(&facts.soc);
        let bandwidth = soc.map(|spec| spec.memory_bandwidth_gbps_x10);
        Self {
            tier: judge_tier(facts.ram_mb, bandwidth, lines),
            soc,
            memory_bandwidth_gbps_x10: bandwidth,
        }
    }

    /// 以实测带宽（微基准结果）代替表估值判档；SoC 查表结果仍随画像携带。
    pub fn from_facts_with_bandwidth(
        facts: &DeviceFacts,
        measured_gbps_x10: u32,
        lines: &TierLines,
    ) -> Self {
        Self {
            tier: judge_tier(facts.ram_mb, Some(measured_gbps_x10), lines),
            soc: identify_soc(&facts.soc),
            memory_bandwidth_gbps_x10: Some(measured_gbps_x10),
        }
    }

    /// 本档允许的实现来源。
    pub const fn allowed_sources(&self) -> &'static [Source] {
        allowed_sources(self.tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use madinfer_core_types::Os;
    use madinfer_host::{Npu, Soc};

    fn facts(vendor: &str, family: &str, ram_mb: u32) -> DeviceFacts {
        DeviceFacts {
            os: Os::Android,
            os_version: "16".to_owned(),
            model_class: format!("{family}-class"),
            soc: Soc {
                vendor: vendor.to_owned(),
                family: family.to_owned(),
            },
            ram_mb,
            npu: Some(Npu {
                vendor: vendor.to_owned(),
                api: "qnn".to_owned(),
                api_version: "2.28".to_owned(),
            }),
        }
    }

    #[test]
    fn identified_soc_with_ample_ram_is_flagship() {
        let profile = DeviceProfile::from_facts(&facts("qualcomm", "sm8650", 12_288));
        assert_eq!(profile.tier, Tier::Flagship);
        assert_eq!(profile.memory_bandwidth_gbps_x10, Some(768));
        assert_eq!(
            profile.allowed_sources(),
            &[Source::SystemModel, Source::Bundled, Source::Cloud]
        );
    }

    #[test]
    fn bandwidth_estimate_drags_a17_class_to_mid() {
        let profile = DeviceProfile::from_facts(&facts("apple", "a17", 8_192));
        assert_eq!(profile.tier, Tier::Mid);
        assert_eq!(profile.memory_bandwidth_gbps_x10, Some(512));
    }

    #[test]
    fn unidentified_soc_yields_unknown_and_gates_bundled() {
        let profile = DeviceProfile::from_facts(&facts("acme", "x1", 16_384));
        assert_eq!(profile.tier, Tier::Unknown);
        assert!(profile.soc.is_none());
        assert_eq!(
            profile.allowed_sources(),
            &[Source::SystemModel, Source::Cloud],
            "Unknown 档只许系统模型与云端"
        );
    }

    #[test]
    fn measured_bandwidth_overrides_table_estimate() {
        let lines = TierLines::default();
        let base = facts("apple", "a17", 12_288);
        assert_eq!(DeviceProfile::from_facts(&base).tier, Tier::Mid);
        let measured = DeviceProfile::from_facts_with_bandwidth(&base, 640, &lines);
        assert_eq!(measured.tier, Tier::Flagship, "实测带宽越线后升档");
        assert_eq!(measured.memory_bandwidth_gbps_x10, Some(640));
        assert!(measured.soc.is_some(), "查表结果仍随画像携带");
    }
}
