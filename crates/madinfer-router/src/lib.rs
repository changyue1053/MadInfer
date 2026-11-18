//! madinfer-router —— L2 路由（设计文档 C3）。
//!
//! 三级顺序固定：`privacy_gate` → 静态过滤（`when` 条件：os、档位（显式 tier_min 优先，未写按 `Tier::allowed_sources` 缺省口径：
//! Unknown / Low 不许内联包）、availability、switch）→ 按 priority 排序。
//! 全部纯函数（sans-io，同 im-core `link_auth.rs` 口径）：映射表与可用性视图由 madinfer-config / madinfer-api 以不可变快照传入。
//!
//! 不变量（单测穷举断言，不是配置）：E2EE / Regulated 的决策链中不存在 `Target::Cloud`；配置的 `when` 里没有 privacy 字段，
//! 隐私门不受任何配置影响。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::BTreeSet;

use madinfer_core_types::{
    BackendId, CapabilityId, FatalKind, InferError, Os, PackageRef, PrivacyTag, QoS, Source, Tier,
};

/// 云端端点类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndpointKind {
    /// 企业自有云（Internal 标签可去，需授权）。
    Enterprise,
    /// 第三方（只有 Public 可去）。
    ThirdParty,
}

/// 路由目标。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// 系统模型（外挂后端）。
    System {
        /// 后端。
        backend: BackendId,
    },
    /// 内联引擎 + 算法包。
    Bundled {
        /// 后端。
        backend: BackendId,
        /// 算法包。
        package: PackageRef,
    },
    /// 云端。
    Cloud {
        /// 端点 id。
        endpoint: String,
        /// 端点类别。
        kind: EndpointKind,
    },
}

impl Target {
    /// 是否云端。
    pub const fn is_cloud(&self) -> bool {
        matches!(self, Self::Cloud { .. })
    }
}

/// `when` 条件（config.v1 mapping.candidates[].when；不含 privacy——schema 里 `not: required: [privacy]`）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Conditions {
    /// 限定 OS。
    pub os: Option<Os>,
    /// 最低档位。**未写**时按档位缺省口径 [`Tier::allowed_sources`] 判（Unknown / Low 不许内联包；madai-w08 G11）；
    /// 写了即配置作者对该候选的明示，优先于缺省表（Unknown 档仍一律不满足任何 `tier_min`）。
    pub tier_min: Option<Tier>,
    /// 要求后端 / 包此刻可用。
    pub requires_available: bool,
}

/// 候选。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// 优先级（越小越先）。
    pub priority: u8,
    /// 目标。
    pub target: Target,
    /// 条件。
    pub when: Conditions,
}

/// 一条能力的映射。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingEntry {
    /// 能力。
    pub capability: CapabilityId,
    /// 候选。
    pub candidates: Vec<Candidate>,
}

/// 映射表（配置快照）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MappingTable {
    /// 条目。
    pub entries: Vec<MappingEntry>,
}

impl MappingTable {
    /// 查能力。
    pub fn entry(&self, capability: &CapabilityId) -> Option<&MappingEntry> {
        self.entries.iter().find(|e| &e.capability == capability)
    }
}

/// 开关（config.v1 switches）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Switches {
    /// 关闭的能力（未列出 = 开）。
    pub disabled_capabilities: BTreeSet<CapabilityId>,
    /// 只走云。
    pub cloud_only: bool,
    /// 全关。
    pub all_off: bool,
}

/// 可用性视图（能力索引聚合后的快照）。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AvailabilityView {
    /// 此刻可用的后端（系统模型 availability = available；内联引擎已注册）。
    pub available_backends: BTreeSet<BackendId>,
    /// 已安装并激活的算法包。
    pub installed_packages: BTreeSet<PackageRef>,
    /// 隔离中的后端（不在任何决策链里）。
    pub quarantined: BTreeSet<BackendId>,
    /// 可达的云端端点。
    pub reachable_endpoints: BTreeSet<String>,
    /// 开关。
    pub switches: Switches,
}

/// 路由输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteInput {
    /// 能力。
    pub capability: CapabilityId,
    /// 隐私标签。
    pub privacy: PrivacyTag,
    /// QoS。
    pub qos: QoS,
    /// 档位。
    pub tier: Tier,
    /// OS。
    pub os: Os,
}

/// 路由原因码（Started.route_reason / telemetry route_decision.reason）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RouteReason {
    /// 隐私标签强制端侧。
    PrivacyForcedLocal,
    /// 档位静态过滤后的结果。
    StaticTier,
    /// 可用性过滤后的结果。
    Availability,
    /// 开关（cloud_only）。
    Switch,
    /// 映射表缺省顺序。
    Default,
    /// 降级链推进后的目标。
    Fallback,
}

impl RouteReason {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::PrivacyForcedLocal => "privacy_forced_local",
            Self::StaticTier => "static_tier",
            Self::Availability => "availability",
            Self::Switch => "switch",
            Self::Default => "default",
            Self::Fallback => "fallback",
        }
    }
}

/// 链中一个已定目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutedTarget {
    /// 目标。
    pub target: Target,
    /// 升云前需一次性授权（Internal 标签 → 企业云）。
    pub requires_consent: bool,
}

/// 路由决策：有序目标链 + 游标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDecision {
    /// 链。
    pub chain: Vec<RoutedTarget>,
    /// 原因。
    pub reason: RouteReason,
    /// 当前游标。
    pub cursor: usize,
}

impl RouteDecision {
    /// 当前目标。
    pub fn current(&self) -> Option<&RoutedTarget> {
        self.chain.get(self.cursor)
    }

    /// 剩余可回退步数。
    pub fn remaining(&self) -> usize {
        self.chain.len().saturating_sub(self.cursor + 1)
    }
}

/// 降级推进结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Advance {
    /// Retryable：不推进，原地重试由调度器管。
    RetryInPlace,
    /// 推进到下一个目标。
    Next(RoutedTarget),
    /// 链耗尽（→ 2005 ChainExhausted）。
    Exhausted,
    /// 不可推进的 Fatal（隐私 / 取消 / 参数错等），终止。
    Stop,
}

/// 隐私门（纯函数）：标签 vs 目标。
///
/// | 标签 | System | Bundled | Cloud(Enterprise) | Cloud(ThirdParty) |
/// |---|---|---|---|---|
/// | Public | ✓ | ✓ | ✓ | ✓ |
/// | Internal | ✓ | ✓ | ✓（需授权） | ✗ |
/// | E2eePlaintext / Regulated | ✓ | ✓ | ✗ | ✗ |
pub const fn privacy_gate(privacy: PrivacyTag, target: &Target) -> bool {
    match target {
        Target::System { .. } | Target::Bundled { .. } => true,
        Target::Cloud { kind, .. } => match privacy {
            PrivacyTag::Public => true,
            PrivacyTag::Internal => matches!(kind, EndpointKind::Enterprise),
            PrivacyTag::E2eePlaintext | PrivacyTag::Regulated => false,
        },
    }
}

fn target_available(target: &Target, view: &AvailabilityView) -> bool {
    match target {
        Target::System { backend } => {
            view.available_backends.contains(backend) && !view.quarantined.contains(backend)
        }
        Target::Bundled { backend, package } => {
            view.available_backends.contains(backend)
                && !view.quarantined.contains(backend)
                && view.installed_packages.contains(package)
        }
        Target::Cloud { endpoint, .. } => view.reachable_endpoints.contains(endpoint),
    }
}

/// 目标对应的实现来源（档位缺省口径按来源判）。
const fn source_of(target: &Target) -> Source {
    match target {
        Target::System { .. } => Source::SystemModel,
        Target::Bundled { .. } => Source::Bundled,
        Target::Cloud { .. } => Source::Cloud,
    }
}

fn backend_of(target: &Target) -> Option<&BackendId> {
    match target {
        Target::System { backend } | Target::Bundled { backend, .. } => Some(backend),
        Target::Cloud { .. } => None,
    }
}

/// 求解目标链。
///
/// 失败：能力未登记 → `CapabilityUnknown`；all_off 或能力开关关 → `CapabilityDisabled`；
/// 隐私门后无候选 → `PrivacyDenied`；静态过滤后无候选 → `UnsupportedDevice`。
pub fn resolve(
    input: &RouteInput,
    table: &MappingTable,
    view: &AvailabilityView,
) -> Result<RouteDecision, InferError> {
    let fatal = |kind: FatalKind| InferError::Fatal { kind, detail: 0 };
    let entry = table
        .entry(&input.capability)
        .ok_or(fatal(FatalKind::CapabilityUnknown))?;
    if view.switches.all_off
        || view
            .switches
            .disabled_capabilities
            .contains(&input.capability)
    {
        return Err(fatal(FatalKind::CapabilityDisabled));
    }

    // ① 隐私门
    let after_privacy: Vec<&Candidate> = entry
        .candidates
        .iter()
        .filter(|c| privacy_gate(input.privacy, &c.target))
        .collect();
    if after_privacy.is_empty() {
        return Err(fatal(FatalKind::PrivacyDenied));
    }
    let privacy_forced_local =
        after_privacy.len() < entry.candidates.len() && input.privacy.local_only();

    // ② 静态过滤：cloud_only 开关、OS、档位（Unknown 档只允许系统模型与云端）、可用性、隔离
    let mut reason = if privacy_forced_local {
        RouteReason::PrivacyForcedLocal
    } else {
        RouteReason::Default
    };
    let mut filtered: Vec<&Candidate> = Vec::new();
    for c in after_privacy {
        if view.switches.cloud_only && !c.target.is_cloud() {
            reason = RouteReason::Switch;
            continue;
        }
        if c.when.os.is_some_and(|os| os != input.os) {
            continue;
        }
        // 档位：候选写了 tier_min 以其为准（Unknown 不满足任何 tier_min）；未写则按档位缺省口径 Tier::allowed_sources
        // （Unknown / Low 只许系统模型与云端——madai-w08 G11，此前只有 Unknown 排除内联包，Low 档端侧包照放行）。
        let tier_ok = match c.when.tier_min {
            Some(min) => input.tier != Tier::Unknown && input.tier >= min,
            None => input.tier.allowed_sources().contains(&source_of(&c.target)),
        };
        if !tier_ok {
            if reason == RouteReason::Default {
                reason = RouteReason::StaticTier;
            }
            continue;
        }
        if let Some(b) = backend_of(&c.target) {
            if view.quarantined.contains(b) {
                if reason == RouteReason::Default {
                    reason = RouteReason::Availability;
                }
                continue;
            }
        }
        if c.when.requires_available && !target_available(&c.target, view) {
            if reason == RouteReason::Default {
                reason = RouteReason::Availability;
            }
            continue;
        }
        filtered.push(c);
    }
    if filtered.is_empty() {
        return Err(fatal(FatalKind::UnsupportedDevice));
    }

    // ③ 按 priority 排序（稳定：同 priority 保持配置顺序）
    filtered.sort_by_key(|c| c.priority);
    let chain = filtered
        .into_iter()
        .map(|c| RoutedTarget {
            requires_consent: c.target.is_cloud() && input.privacy == PrivacyTag::Internal,
            target: c.target.clone(),
        })
        .collect();
    Ok(RouteDecision {
        chain,
        reason,
        cursor: 0,
    })
}

/// 降级推进（设计文档 §7 规则）：Retryable 不推进；Degradable 推进；Fatal 中 AssetMissing / AssetCorrupt / UnsupportedDevice 推进，
/// 其余（隐私、取消、参数、授权拒绝…）终止。
pub fn advance(decision: &mut RouteDecision, error: &InferError) -> Advance {
    let may_advance = match error {
        InferError::Retryable { .. } => return Advance::RetryInPlace,
        InferError::Degradable { .. } => true,
        InferError::Fatal { kind, .. } => matches!(
            kind,
            FatalKind::AssetMissing | FatalKind::AssetCorrupt | FatalKind::UnsupportedDevice
        ),
    };
    if !may_advance {
        return Advance::Stop;
    }
    if decision.cursor + 1 >= decision.chain.len() {
        return Advance::Exhausted;
    }
    decision.cursor += 1;
    decision.reason = RouteReason::Fallback;
    decision
        .current()
        .cloned()
        .map_or(Advance::Exhausted, Advance::Next)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use madinfer_core_types::DegradeKind;

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::new(s).expect("id")
    }

    fn backend(s: &str) -> BackendId {
        BackendId(s.to_owned())
    }

    fn pkg(s: &str) -> PackageRef {
        PackageRef::parse(s).expect("pkg")
    }

    fn table() -> MappingTable {
        MappingTable {
            entries: vec![MappingEntry {
                capability: cap("summarize"),
                candidates: vec![
                    Candidate {
                        priority: 0,
                        target: Target::System {
                            backend: backend("ios.foundation_models"),
                        },
                        when: Conditions {
                            os: Some(Os::Ios),
                            tier_min: None,
                            requires_available: true,
                        },
                    },
                    Candidate {
                        priority: 0,
                        target: Target::System {
                            backend: backend("android.aicore"),
                        },
                        when: Conditions {
                            os: Some(Os::Android),
                            tier_min: None,
                            requires_available: true,
                        },
                    },
                    Candidate {
                        priority: 1,
                        target: Target::Bundled {
                            backend: backend("llama.cpp"),
                            package: pkg("catchup-summary@1.4.2"),
                        },
                        when: Conditions {
                            os: None,
                            tier_min: Some(Tier::Mid),
                            requires_available: true,
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
                    Candidate {
                        priority: 3,
                        target: Target::Cloud {
                            endpoint: "cloud.public".into(),
                            kind: EndpointKind::ThirdParty,
                        },
                        when: Conditions::default(),
                    },
                ],
            }],
        }
    }

    fn view(fm_available: bool, package_installed: bool) -> AvailabilityView {
        let mut v = AvailabilityView::default();
        if fm_available {
            v.available_backends
                .insert(backend("ios.foundation_models"));
        }
        v.available_backends.insert(backend("llama.cpp"));
        if package_installed {
            v.installed_packages.insert(pkg("catchup-summary@1.4.2"));
        }
        v.reachable_endpoints.insert("cloud.enterprise".into());
        v.reachable_endpoints.insert("cloud.public".into());
        v
    }

    fn input(privacy: PrivacyTag, tier: Tier, os: Os) -> RouteInput {
        RouteInput {
            capability: cap("summarize"),
            privacy,
            qos: QoS::Interactive,
            tier,
            os,
        }
    }

    #[test]
    fn e2ee_never_routes_to_cloud_under_any_view_or_tier() {
        // 穷举：两种 local_only 标签 × 三个 OS × 四档 × 可用性四组合 × 开关三态。
        for privacy in [PrivacyTag::E2eePlaintext, PrivacyTag::Regulated] {
            for os in [Os::Ios, Os::Android, Os::Ohos] {
                for tier in [Tier::Low, Tier::Mid, Tier::Flagship, Tier::Unknown] {
                    for fm in [false, true] {
                        for installed in [false, true] {
                            for cloud_only in [false, true] {
                                let mut v = view(fm, installed);
                                v.switches.cloud_only = cloud_only;
                                match resolve(&input(privacy, tier, os), &table(), &v) {
                                    Ok(d) => assert!(
                                        d.chain.iter().all(|t| !t.target.is_cloud()),
                                        "{privacy:?} {os:?} {tier:?} fm={fm} pkg={installed} cloud_only={cloud_only}: {d:?}"
                                    ),
                                    Err(e) => assert!(
                                        matches!(
                                            e,
                                            InferError::Fatal {
                                                kind: FatalKind::UnsupportedDevice | FatalKind::PrivacyDenied,
                                                ..
                                            }
                                        ),
                                        "{e:?}"
                                    ),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn internal_marks_enterprise_cloud_consent_and_drops_third_party() {
        let d = resolve(
            &input(PrivacyTag::Internal, Tier::Flagship, Os::Ios),
            &table(),
            &view(true, true),
        )
        .expect("ok");
        let targets: Vec<&Target> = d.chain.iter().map(|t| &t.target).collect();
        assert_eq!(
            targets.len(),
            3,
            "FM → bundled → enterprise；第三方被隐私门剔除"
        );
        assert!(matches!(targets[0], Target::System { .. }));
        assert!(matches!(targets[1], Target::Bundled { .. }));
        assert!(matches!(
            targets[2],
            Target::Cloud {
                kind: EndpointKind::Enterprise,
                ..
            }
        ));
        assert!(d.chain[2].requires_consent);
        assert!(!d.chain[0].requires_consent);
        assert_eq!(d.reason, RouteReason::Default);
    }

    #[test]
    fn privacy_forced_local_reason_and_availability_filtering() {
        let d = resolve(
            &input(PrivacyTag::E2eePlaintext, Tier::Flagship, Os::Ios),
            &table(),
            &view(false, true),
        )
        .expect("ok");
        assert_eq!(d.reason, RouteReason::PrivacyForcedLocal);
        assert_eq!(d.chain.len(), 1, "FM 不可用被过滤，只剩 bundled");
        assert!(matches!(d.chain[0].target, Target::Bundled { .. }));

        let err = resolve(
            &input(PrivacyTag::E2eePlaintext, Tier::Low, Os::Ios),
            &table(),
            &view(false, true),
        )
        .expect_err("low tier");
        assert_eq!(
            err.code(),
            3001,
            "低端机 + 无系统模型 + 隐私禁云 → UnsupportedDevice"
        );
    }

    #[test]
    fn low_tier_excludes_bundled_unless_candidate_states_tier_min() {
        // madai-w08 G11（I-H-04 反馈 ②）：候选未写 tier_min 时按 Tier::allowed_sources 缺省口径——Low 档不许内联包；
        // 候选显式 tier_min = Low 则是配置作者明示，放行；Mid / Flagship 缺省三源全开。
        fn with_bundled_tier_min(tier_min: Option<Tier>) -> MappingTable {
            let mut t = table();
            for c in &mut t.entries[0].candidates {
                if matches!(c.target, Target::Bundled { .. }) {
                    c.when.tier_min = tier_min;
                }
            }
            t
        }
        let has_bundled = |d: &RouteDecision| {
            d.chain
                .iter()
                .any(|s| matches!(s.target, Target::Bundled { .. }))
        };

        let d = resolve(
            &input(PrivacyTag::Public, Tier::Low, Os::Ios),
            &with_bundled_tier_min(None),
            &view(true, true),
        )
        .expect("ok");
        assert!(
            !has_bundled(&d),
            "Low 档 + 候选未写 tier_min → 内联包出链：{d:?}"
        );
        assert_eq!(d.reason, RouteReason::StaticTier);

        let d = resolve(
            &input(PrivacyTag::Public, Tier::Low, Os::Ios),
            &with_bundled_tier_min(Some(Tier::Low)),
            &view(true, true),
        )
        .expect("ok");
        assert!(
            has_bundled(&d),
            "候选显式 tier_min = Low → 内联包在链：{d:?}"
        );

        for tier in [Tier::Mid, Tier::Flagship] {
            let d = resolve(
                &input(PrivacyTag::Public, tier, Os::Ios),
                &with_bundled_tier_min(None),
                &view(true, true),
            )
            .expect("ok");
            assert!(has_bundled(&d), "{tier:?} 缺省口径应含内联包：{d:?}");
        }
    }

    #[test]
    fn unknown_tier_only_allows_system_and_cloud() {
        let d = resolve(
            &input(PrivacyTag::Public, Tier::Unknown, Os::Ios),
            &table(),
            &view(true, true),
        )
        .expect("ok");
        assert!(d
            .chain
            .iter()
            .all(|t| !matches!(t.target, Target::Bundled { .. })));
        assert_eq!(d.chain.len(), 3);
    }

    #[test]
    fn switches_disable_and_cloud_only() {
        let mut v = view(true, true);
        v.switches.disabled_capabilities.insert(cap("summarize"));
        assert_eq!(
            resolve(
                &input(PrivacyTag::Public, Tier::Flagship, Os::Ios),
                &table(),
                &v
            )
            .expect_err("off")
            .code(),
            3002
        );
        let mut v = view(true, true);
        v.switches.cloud_only = true;
        let d = resolve(
            &input(PrivacyTag::Public, Tier::Flagship, Os::Ios),
            &table(),
            &v,
        )
        .expect("ok");
        assert!(d.chain.iter().all(|t| t.target.is_cloud()));
        assert_eq!(d.reason, RouteReason::Switch);
        // 云端候选 when = {}（不看可用性），Public 下空视图仍有云端链；Regulated 下端侧全不可用 → 3001。
        assert_eq!(
            resolve(
                &input(PrivacyTag::Regulated, Tier::Flagship, Os::Ios),
                &table(),
                &AvailabilityView::default()
            )
            .expect_err("nothing available")
            .code(),
            3001
        );
        assert_eq!(
            resolve(
                &RouteInput {
                    capability: cap("translate"),
                    ..input(PrivacyTag::Public, Tier::Flagship, Os::Ios)
                },
                &table(),
                &view(true, true)
            )
            .expect_err("unknown")
            .code(),
            3003
        );
    }

    #[test]
    fn advance_follows_section_7_rules() {
        let mut d = resolve(
            &input(PrivacyTag::Public, Tier::Flagship, Os::Ios),
            &table(),
            &view(true, true),
        )
        .expect("ok");
        assert_eq!(d.chain.len(), 4);
        assert_eq!(
            advance(
                &mut d,
                &InferError::Retryable {
                    kind: madinfer_core_types::RetryKind::Busy,
                    retry_after_ms: 1
                }
            ),
            Advance::RetryInPlace
        );
        assert_eq!(d.cursor, 0);
        assert!(matches!(
            advance(
                &mut d,
                &InferError::Degradable {
                    kind: DegradeKind::LowConfidence
                }
            ),
            Advance::Next(_)
        ));
        assert_eq!((d.cursor, d.reason), (1, RouteReason::Fallback));
        assert!(matches!(
            advance(
                &mut d,
                &InferError::Fatal {
                    kind: FatalKind::AssetMissing,
                    detail: 0
                }
            ),
            Advance::Next(_)
        ));
        assert_eq!(
            advance(
                &mut d,
                &InferError::Fatal {
                    kind: FatalKind::ConsentDenied,
                    detail: 0
                }
            ),
            Advance::Stop
        );
        assert!(matches!(
            advance(
                &mut d,
                &InferError::Degradable {
                    kind: DegradeKind::Deadline
                }
            ),
            Advance::Next(_)
        ));
        assert_eq!(
            advance(
                &mut d,
                &InferError::Degradable {
                    kind: DegradeKind::Deadline
                }
            ),
            Advance::Exhausted
        );
        assert_eq!(d.remaining(), 0);
    }
}
