//! 能力索引（设计文档 C2 关键机制 ②）：聚合资产就绪、后端 availability、开关、档位，任一变化重算并 bump version 后整份广播。
//!
//! 纯计算：输入是各子系统的不可变快照（[`IndexInputs`]），输出是 capabilities.v1 的 Rust 镜像（[`CapabilitySnapshot`]）；
//! 只有内容真变才 bump version（与 KMP 侧 `CapabilitySnapshot.version` 单调一致）。

use std::collections::{BTreeMap, BTreeSet};

use madinfer_core_types::{BackendId, CapabilityId, PackageRef, Source, Tier};
use madinfer_router::{MappingTable, Target};

/// 能力状态（capabilities.v1 entries[].state）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapState {
    /// 可用。
    Available,
    /// 需下载（预计字节）。
    NeedsDownload {
        /// 字节。
        bytes: u64,
    },
    /// 下载中（千分比）。
    Downloading {
        /// 0～1000。
        permille: u16,
    },
    /// 此设备不支持。
    Unsupported,
    /// 被开关关闭。
    Disabled,
}

impl CapState {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NeedsDownload { .. } => "needs_download",
            Self::Downloading { .. } => "downloading",
            Self::Unsupported => "unsupported",
            Self::Disabled => "disabled",
        }
    }
}

/// 延迟档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyTier {
    /// < 300 ms。
    Fast,
    /// < 2 s。
    Normal,
    /// 其余。
    Slow,
}

/// 不可用原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// 档位不够。
    TierTooLow,
    /// 系统模型未就绪。
    SystemModelMissing,
    /// 开关。
    Switch,
    /// 后端隔离。
    Quarantined,
}

/// 一条能力状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityStatus {
    /// 能力。
    pub id: CapabilityId,
    /// 状态。
    pub state: CapState,
    /// 实现来源（Available 时）。
    pub source: Option<Source>,
    /// 延迟档。
    pub latency: LatencyTier,
    /// 原因。
    pub reason: Option<UnavailableReason>,
}

/// 快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySnapshot {
    /// 单调版本。
    pub version: u64,
    /// 档位。
    pub device_tier: Tier,
    /// 按能力名排序的条目。
    pub entries: Vec<CapabilityStatus>,
}

/// 算法包就绪度（来自 madinfer-assets）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageReadiness {
    /// 已安装激活。
    Installed,
    /// 需下载。
    NeedsDownload {
        /// 字节。
        bytes: u64,
    },
    /// 下载中。
    Downloading {
        /// 千分比。
        permille: u16,
    },
}

/// 索引输入（各子系统快照）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexInputs {
    /// 映射表（注册的能力与候选）。
    pub mapping: MappingTable,
    /// 档位。
    pub tier: Tier,
    /// 当前 OS。
    pub os: Option<madinfer_core_types::Os>,
    /// 可用的后端（系统模型 available / 内联引擎已注册）。
    pub available_backends: BTreeSet<BackendId>,
    /// 隔离的后端。
    pub quarantined: BTreeSet<BackendId>,
    /// 算法包就绪度。
    pub packages: BTreeMap<PackageRef, PackageReadiness>,
    /// 关闭的能力。
    pub disabled: BTreeSet<CapabilityId>,
    /// 全关。
    pub all_off: bool,
    /// 云端可达。
    pub cloud_reachable: bool,
}

/// 能力索引。
#[derive(Debug)]
pub struct CapabilityIndex {
    snapshot: CapabilitySnapshot,
}

impl Default for CapabilityIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityIndex {
    /// 空索引（version 0）。
    pub fn new() -> Self {
        Self {
            snapshot: CapabilitySnapshot {
                version: 0,
                device_tier: Tier::Unknown,
                entries: Vec::new(),
            },
        }
    }

    /// 当前快照。
    pub fn snapshot(&self) -> &CapabilitySnapshot {
        &self.snapshot
    }

    /// 重算；内容有变则 bump version 并返回新快照（供整份广播），无变返回 None。
    pub fn update(&mut self, inputs: &IndexInputs) -> Option<CapabilitySnapshot> {
        let entries = compute(inputs);
        if entries == self.snapshot.entries
            && inputs.tier == self.snapshot.device_tier
            && self.snapshot.version > 0
        {
            return None;
        }
        self.snapshot = CapabilitySnapshot {
            version: self.snapshot.version + 1,
            device_tier: inputs.tier,
            entries,
        };
        Some(self.snapshot.clone())
    }
}

/// 纯计算：每条注册能力的状态。
///
/// 判定顺序（取"最好"的候选）：任一候选 Available → Available（来源取该候选）；否则有下载中 → Downloading；有需下载 → NeedsDownload；
/// 开关关 → Disabled；其余 Unsupported（原因取最先命中的门）。
pub fn compute(inputs: &IndexInputs) -> Vec<CapabilityStatus> {
    let mut out: Vec<CapabilityStatus> = inputs
        .mapping
        .entries
        .iter()
        .map(|entry| {
            if inputs.all_off || inputs.disabled.contains(&entry.capability) {
                return CapabilityStatus {
                    id: entry.capability.clone(),
                    state: CapState::Disabled,
                    source: None,
                    latency: LatencyTier::Slow,
                    reason: Some(UnavailableReason::Switch),
                };
            }
            let mut best: Option<(Source, LatencyTier)> = None;
            let mut downloading: Option<u16> = None;
            let mut needs_download: Option<u64> = None;
            let mut reason: Option<UnavailableReason> = None;
            let mut candidates: Vec<_> = entry.candidates.iter().collect();
            candidates.sort_by_key(|c| c.priority);
            for c in candidates {
                if c.when.os.is_some_and(|os| Some(os) != inputs.os) {
                    continue;
                }
                match &c.target {
                    Target::System { backend } => {
                        if inputs.quarantined.contains(backend) {
                            reason.get_or_insert(UnavailableReason::Quarantined);
                        } else if inputs.available_backends.contains(backend) {
                            best.get_or_insert((Source::SystemModel, LatencyTier::Fast));
                        } else {
                            reason.get_or_insert(UnavailableReason::SystemModelMissing);
                        }
                    }
                    Target::Bundled { backend, package } => {
                        // 与 madinfer-router 同一口径（madai-w08 G11）：候选写了 tier_min 以其为准；未写按 Tier::allowed_sources
                        // 缺省表——Unknown / Low 不许内联包（此前只排除 Unknown，Low 档探测给 Available 与路由不一致）。
                        let tier_ok = match c.when.tier_min {
                            Some(min) => inputs.tier != Tier::Unknown && inputs.tier >= min,
                            None => inputs.tier.allows_bundled_by_default(),
                        };
                        if !tier_ok {
                            reason.get_or_insert(UnavailableReason::TierTooLow);
                            continue;
                        }
                        if inputs.quarantined.contains(backend)
                            || !inputs.available_backends.contains(backend)
                        {
                            reason.get_or_insert(UnavailableReason::Quarantined);
                            continue;
                        }
                        match inputs.packages.get(package) {
                            Some(PackageReadiness::Installed) => {
                                let latency = if inputs.tier == Tier::Flagship {
                                    LatencyTier::Fast
                                } else {
                                    LatencyTier::Normal
                                };
                                best.get_or_insert((Source::Bundled, latency));
                            }
                            Some(PackageReadiness::Downloading { permille }) => {
                                downloading =
                                    Some(downloading.map_or(*permille, |p| p.max(*permille)));
                            }
                            Some(PackageReadiness::NeedsDownload { bytes }) => {
                                needs_download =
                                    Some(needs_download.map_or(*bytes, |b| b.min(*bytes)));
                            }
                            None => {
                                needs_download.get_or_insert(0);
                            }
                        }
                    }
                    Target::Cloud { .. } => {
                        if inputs.cloud_reachable {
                            best.get_or_insert((Source::Cloud, LatencyTier::Normal));
                        }
                    }
                }
            }
            match (best, downloading, needs_download) {
                (Some((source, latency)), _, _) => CapabilityStatus {
                    id: entry.capability.clone(),
                    state: CapState::Available,
                    source: Some(source),
                    latency,
                    reason: None,
                },
                (None, Some(permille), _) => CapabilityStatus {
                    id: entry.capability.clone(),
                    state: CapState::Downloading { permille },
                    source: None,
                    latency: LatencyTier::Normal,
                    reason: None,
                },
                (None, None, Some(bytes)) => CapabilityStatus {
                    id: entry.capability.clone(),
                    state: CapState::NeedsDownload { bytes },
                    source: None,
                    latency: LatencyTier::Normal,
                    reason: None,
                },
                (None, None, None) => CapabilityStatus {
                    id: entry.capability.clone(),
                    state: CapState::Unsupported,
                    source: None,
                    latency: LatencyTier::Slow,
                    reason: reason.or(Some(UnavailableReason::TierTooLow)),
                },
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use madinfer_core_types::Os;
    use madinfer_router::{Candidate, Conditions, EndpointKind, MappingEntry};

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::new(s).expect("id")
    }

    fn inputs() -> IndexInputs {
        let mapping = MappingTable {
            entries: vec![
                MappingEntry {
                    capability: cap("summarize"),
                    candidates: vec![
                        Candidate {
                            priority: 0,
                            target: Target::System {
                                backend: BackendId("ios.foundation_models".into()),
                            },
                            when: Conditions {
                                os: Some(Os::Ios),
                                ..Conditions::default()
                            },
                        },
                        Candidate {
                            priority: 1,
                            target: Target::Bundled {
                                backend: BackendId("llama.cpp".into()),
                                package: PackageRef::parse("catchup-summary@1.4.2").expect("pkg"),
                            },
                            when: Conditions {
                                tier_min: Some(Tier::Mid),
                                ..Conditions::default()
                            },
                        },
                        Candidate {
                            priority: 2,
                            target: Target::Cloud {
                                endpoint: "cloud.enterprise".into(),
                                kind: EndpointKind::Enterprise,
                            },
                            when: Conditions::default(),
                        },
                    ],
                },
                MappingEntry {
                    capability: cap("embed.text"),
                    candidates: vec![Candidate {
                        priority: 0,
                        target: Target::Bundled {
                            backend: BackendId("llama.cpp".into()),
                            package: PackageRef::parse("embed-bge-s@2.0.0").expect("pkg"),
                        },
                        when: Conditions {
                            tier_min: Some(Tier::Low),
                            ..Conditions::default()
                        },
                    }],
                },
            ],
        };
        let mut i = IndexInputs {
            mapping,
            tier: Tier::Mid,
            os: Some(Os::Ios),
            ..IndexInputs::default()
        };
        i.available_backends.insert(BackendId("llama.cpp".into()));
        i
    }

    #[test]
    fn version_bumps_only_on_change_and_entries_are_sorted() {
        let mut index = CapabilityIndex::new();
        let mut i = inputs();
        let first = index.update(&i).expect("first snapshot");
        assert_eq!(first.version, 1);
        assert_eq!(first.entries[0].id, cap("embed.text"), "按能力名排序");
        assert_eq!(
            first.entries[0].state,
            CapState::NeedsDownload { bytes: 0 },
            "包未登记 = 需下载"
        );
        assert_eq!(
            first.entries[1].state,
            CapState::NeedsDownload { bytes: 0 },
            "系统模型缺、包未登记：可下载优先于 Unsupported"
        );
        assert!(index.update(&i).is_none(), "无变化不 bump");

        let mut low = inputs();
        low.tier = Tier::Low;
        let low_entries = compute(&low);
        assert_eq!(
            low_entries[1].state,
            CapState::Unsupported,
            "低端机连下载资格都没有"
        );
        assert_eq!(
            low_entries[1].reason,
            Some(UnavailableReason::SystemModelMissing),
            "原因取最先命中的门"
        );

        i.packages.insert(
            PackageRef::parse("catchup-summary@1.4.2").expect("pkg"),
            PackageReadiness::Downloading { permille: 640 },
        );
        let second = index.update(&i).expect("changed");
        assert_eq!(second.version, 2);
        assert_eq!(
            second.entries[1].state,
            CapState::Downloading { permille: 640 }
        );

        i.packages.insert(
            PackageRef::parse("catchup-summary@1.4.2").expect("pkg"),
            PackageReadiness::Installed,
        );
        let third = index.update(&i).expect("changed");
        assert_eq!(third.entries[1].state, CapState::Available);
        assert_eq!(third.entries[1].source, Some(Source::Bundled));
        assert_eq!(
            third.entries[1].latency,
            LatencyTier::Normal,
            "中端机 Normal"
        );

        i.available_backends
            .insert(BackendId("ios.foundation_models".into()));
        let fourth = index.update(&i).expect("changed");
        assert_eq!(
            fourth.entries[1].source,
            Some(Source::SystemModel),
            "priority 0 的系统模型优先"
        );
        assert_eq!(fourth.entries[1].latency, LatencyTier::Fast);
    }

    #[test]
    fn switches_and_cloud_and_tier() {
        let mut i = inputs();
        i.disabled.insert(cap("summarize"));
        let entries = compute(&i);
        assert_eq!(entries[1].state, CapState::Disabled);
        assert_eq!(entries[1].reason, Some(UnavailableReason::Switch));

        let mut i = inputs();
        i.cloud_reachable = true;
        let entries = compute(&i);
        assert_eq!(entries[1].state, CapState::Available);
        assert_eq!(
            entries[1].source,
            Some(Source::Cloud),
            "端侧不就绪时云端兜底"
        );

        let mut i = inputs();
        i.tier = Tier::Low;
        i.packages.insert(
            PackageRef::parse("catchup-summary@1.4.2").expect("pkg"),
            PackageReadiness::Installed,
        );
        let entries = compute(&i);
        assert_eq!(
            entries[1].state,
            CapState::Unsupported,
            "低端机不满足 tier_min=mid，即使包已装"
        );
        assert_eq!(
            entries[0].state,
            CapState::NeedsDownload { bytes: 0 },
            "embed.text 的 tier_min=low 仍可下载"
        );

        // madai-w08 G11：候选未写 tier_min → 按 Tier::allowed_sources 缺省口径，Low 档内联包 Unsupported（与路由同口径）；
        // Mid 起放行。ffi 演示映射正是 tier_min = None 的形态（I-H-04 反馈 ②）。
        let mut unstated = inputs();
        unstated.tier = Tier::Low;
        unstated.packages.insert(
            PackageRef::parse("catchup-summary@1.4.2").expect("pkg"),
            PackageReadiness::Installed,
        );
        for c in &mut unstated.mapping.entries[0].candidates {
            if matches!(c.target, Target::Bundled { .. }) {
                c.when.tier_min = None;
            }
        }
        assert_eq!(
            compute(&unstated)[1].state,
            CapState::Unsupported,
            "Low 档 + 候选未写 tier_min → 缺省口径不许内联包（系统模型缺、云端未接时 Unsupported）"
        );
        unstated.tier = Tier::Mid;
        assert_eq!(
            compute(&unstated)[1].source,
            Some(Source::Bundled),
            "Mid 档缺省口径三源全开，已装包即端侧"
        );
    }
}
