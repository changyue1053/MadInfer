//! SoC / NPU 静态识别表：编译内数据 + 查表纯函数。
//!
//! IO 事实（厂商 / 家族串）来自宿主的 [`madinfer_host::DeviceFacts`]；本表只做归一化后的精确匹配，
//! 给出该 SoC 家族的静态规格（内存带宽估值、NPU 形态）。查表未中不是错误——画像走 `Tier::Unknown` 判定路径。
//! 带宽是**静态估值**（按公开内存规格换算），微基准实测落地后由实测覆盖（见 `DeviceProfile::from_facts_with_bandwidth`）。

use madinfer_host::Soc;

/// NPU 接入 API（与 device.v1 `npu.api` 同词：core_ai / qnn / aicore / hiai / neuron）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NpuApi {
    /// Apple 神经引擎（Core ML 栈）。
    CoreAi,
    /// 高通 QNN。
    Qnn,
    /// Google AICore。
    Aicore,
    /// 华为 HiAI。
    Hiai,
    /// 联发科 NeuroPilot。
    Neuron,
}

impl NpuApi {
    /// 协议字符串（device.v1 `npu.api`）。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::CoreAi => "core_ai",
            Self::Qnn => "qnn",
            Self::Aicore => "aicore",
            Self::Hiai => "hiai",
            Self::Neuron => "neuron",
        }
    }
}

/// 识别表一行：一个 SoC 家族的静态规格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocSpec {
    /// 厂商（归一化形态：小写、空格折叠为 `-`）。
    pub vendor: &'static str,
    /// 家族（归一化形态同上）。
    pub family: &'static str,
    /// 内存带宽静态估值（GB/s × 10）。
    pub memory_bandwidth_gbps_x10: u32,
    /// NPU 接入 API（None = 无 NPU 或本 SDK 未接入）。
    pub npu_api: Option<NpuApi>,
    /// NPU int8 算力估值（TOPS × 10；无 NPU 为 0）。
    pub npu_int8_tops_x10: u32,
}

/// 静态识别表（按厂商分组；家族键为归一化形态）。
pub const SOC_SPECS: &[SocSpec] = &[
    // 高通
    entry("qualcomm", "sm8450", 512, Some(NpuApi::Qnn), 260),
    entry("qualcomm", "sm8550", 672, Some(NpuApi::Qnn), 340),
    entry("qualcomm", "sm8650", 768, Some(NpuApi::Qnn), 450),
    entry("qualcomm", "sm8750", 853, Some(NpuApi::Qnn), 550),
    entry("qualcomm", "sm7675", 512, Some(NpuApi::Qnn), 200),
    entry("qualcomm", "sm6450", 256, Some(NpuApi::Qnn), 120),
    // 联发科
    entry("mediatek", "mt6985", 640, Some(NpuApi::Neuron), 300),
    entry("mediatek", "mt6989", 768, Some(NpuApi::Neuron), 460),
    entry("mediatek", "mt6991", 853, Some(NpuApi::Neuron), 600),
    // Apple
    entry("apple", "a16", 512, Some(NpuApi::CoreAi), 170),
    entry("apple", "a17", 512, Some(NpuApi::CoreAi), 350),
    entry("apple", "a18", 600, Some(NpuApi::CoreAi), 380),
    entry("apple", "m2", 1_024, Some(NpuApi::CoreAi), 158),
    // 海思
    entry("hisilicon", "kirin9000s", 460, Some(NpuApi::Hiai), 160),
    entry("hisilicon", "kirin9010", 512, Some(NpuApi::Hiai), 200),
    // Google
    entry("google", "tensor-g3", 512, Some(NpuApi::Aicore), 200),
    entry("google", "tensor-g4", 683, Some(NpuApi::Aicore), 320),
    // 三星（NPU 本 SDK 未接入）
    entry("samsung", "exynos2400", 683, None, 0),
];

const fn entry(
    vendor: &'static str,
    family: &'static str,
    memory_bandwidth_gbps_x10: u32,
    npu_api: Option<NpuApi>,
    npu_int8_tops_x10: u32,
) -> SocSpec {
    SocSpec {
        vendor,
        family,
        memory_bandwidth_gbps_x10,
        npu_api,
        npu_int8_tops_x10,
    }
}

/// 归一化：去首尾空白、ASCII 小写、内部空白折叠为单个 `-`（"Tensor G4" 与 "tensor-g4" 同键）。
fn normalize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_gap = false;
    for c in value.trim().chars() {
        if c.is_whitespace() {
            pending_gap = true;
            continue;
        }
        if pending_gap && !out.is_empty() {
            out.push('-');
        }
        pending_gap = false;
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// 查表纯函数：归一化后按 vendor + family 精确匹配；未中返回 `None`。
pub fn identify_soc(soc: &Soc) -> Option<&'static SocSpec> {
    let vendor = normalize(&soc.vendor);
    let family = normalize(&soc.family);
    SOC_SPECS
        .iter()
        .find(|spec| spec.vendor == vendor && spec.family == family)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soc(vendor: &str, family: &str) -> Soc {
        Soc {
            vendor: vendor.to_owned(),
            family: family.to_owned(),
        }
    }

    #[test]
    fn lookup_is_case_and_whitespace_insensitive() {
        let hit = identify_soc(&soc("Qualcomm", " SM8650 "));
        assert_eq!(hit.map(|s| s.memory_bandwidth_gbps_x10), Some(768));
        assert_eq!(hit.and_then(|s| s.npu_api), Some(NpuApi::Qnn));
        let tensor = identify_soc(&soc("GOOGLE", "Tensor G4"));
        assert_eq!(tensor.map(|s| s.family), Some("tensor-g4"));
    }

    #[test]
    fn unknown_soc_returns_none() {
        assert!(identify_soc(&soc("qualcomm", "sm9999")).is_none());
        assert!(identify_soc(&soc("", "")).is_none());
        assert!(identify_soc(&Soc::default()).is_none());
    }

    #[test]
    fn table_is_normalized_and_free_of_duplicates() {
        for spec in SOC_SPECS {
            assert_eq!(spec.vendor, normalize(spec.vendor), "厂商键须已归一化");
            assert_eq!(spec.family, normalize(spec.family), "家族键须已归一化");
            assert!(spec.memory_bandwidth_gbps_x10 > 0);
            if spec.npu_api.is_none() {
                assert_eq!(spec.npu_int8_tops_x10, 0, "无 NPU 不得报算力");
            }
        }
        let mut keys: Vec<(&str, &str)> = SOC_SPECS.iter().map(|s| (s.vendor, s.family)).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), SOC_SPECS.len(), "识别表键不得重复");
    }

    #[test]
    fn npu_api_wire_strings_match_device_v1() {
        let wires: Vec<&str> = [
            NpuApi::CoreAi,
            NpuApi::Qnn,
            NpuApi::Aicore,
            NpuApi::Hiai,
            NpuApi::Neuron,
        ]
        .iter()
        .map(|api| api.wire())
        .collect();
        assert_eq!(wires, vec!["core_ai", "qnn", "aicore", "hiai", "neuron"]);
    }
}
