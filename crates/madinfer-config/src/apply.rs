//! 应用管线：每份到达的文档跑一台 FSM；`ConfigStore` 持活动 / 上一版，sans-io 输出效果。

use std::collections::BTreeSet;
use std::sync::Arc;

use madinfer_core_types::{BackendId, CapabilityId};
use madinfer_fsm::{Fsm, FsmBuilder, FsmEvent};
use madinfer_router::MappingTable;
use semver::{Version, VersionReq};

use crate::doc::{ConfigDoc, SemanticError};
use crate::verify::{SignatureVerifier, VerifyOutcome};

/// 管线阶段（状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApplyStage {
    /// 字节到达。
    Received,
    /// 验签通过。
    Verified,
    /// 解析通过。
    Parsed,
    /// 语义 / 版本 / 分桶通过。
    Validated,
    /// 已切换为活动配置。
    Applied,
    /// 被拒绝。
    Rejected,
}

/// 拒绝原因（telemetry config_rejected.reason）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// 未知密钥。
    UnknownKey(String),
    /// 签名不符；`timed_out` 为真 = 期限内没等到验签回执（madai-w11 I-C-10，[`crate::VerifyOutcome::TimedOut`]），
    /// 拒因同码、只在 detail 留痕 `verify=timeout`。
    SignatureInvalid {
        /// 验签超时（而非验假）。
        timed_out: bool,
    },
    /// JSON / schema 解析失败。
    SchemaInvalid(String),
    /// 版本不大于当前。
    VersionNotNewer {
        /// 收到。
        received: u64,
        /// 当前。
        active: u64,
    },
    /// min_sdk 不满足。
    MinSdkUnsatisfied(String),
    /// 设备桶不在灰度区间。
    OutOfRollout {
        /// 设备桶。
        bucket: u16,
    },
    /// 语义错误。
    Semantic(SemanticError),
}

impl RejectReason {
    /// 观测用原因码（注册制字符串，不含自由文本）。
    pub const fn wire(&self) -> &'static str {
        match self {
            Self::UnknownKey(_) => "unknown_key",
            Self::SignatureInvalid { .. } => "signature_invalid",
            Self::SchemaInvalid(_) => "schema_invalid",
            Self::VersionNotNewer { .. } => "version_not_newer",
            Self::MinSdkUnsatisfied(_) => "min_sdk_unsatisfied",
            Self::OutOfRollout { .. } => "out_of_rollout",
            Self::Semantic(_) => "semantic_invalid",
        }
    }
}

/// 管线事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyEvent {
    /// 验签通过。
    SignatureAccepted,
    /// 验签失败。
    SignatureRejected(RejectReason),
    /// 解析通过。
    ParseSucceeded,
    /// 解析失败。
    ParseFailed(RejectReason),
    /// 校验通过。
    ValidationPassed,
    /// 校验失败。
    ValidationFailed(RejectReason),
    /// 指针已切换。
    Switched {
        /// 新版本。
        version: u64,
    },
}

/// 管线事件种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApplyEventKind {
    /// 见 [`ApplyEvent::SignatureAccepted`]。
    SignatureAccepted,
    /// 见 [`ApplyEvent::SignatureRejected`]。
    SignatureRejected,
    /// 见 [`ApplyEvent::ParseSucceeded`]。
    ParseSucceeded,
    /// 见 [`ApplyEvent::ParseFailed`]。
    ParseFailed,
    /// 见 [`ApplyEvent::ValidationPassed`]。
    ValidationPassed,
    /// 见 [`ApplyEvent::ValidationFailed`]。
    ValidationFailed,
    /// 见 [`ApplyEvent::Switched`]。
    Switched,
}

impl FsmEvent for ApplyEvent {
    type Kind = ApplyEventKind;

    fn kind(&self) -> ApplyEventKind {
        match self {
            ApplyEvent::SignatureAccepted => ApplyEventKind::SignatureAccepted,
            ApplyEvent::SignatureRejected(_) => ApplyEventKind::SignatureRejected,
            ApplyEvent::ParseSucceeded => ApplyEventKind::ParseSucceeded,
            ApplyEvent::ParseFailed(_) => ApplyEventKind::ParseFailed,
            ApplyEvent::ValidationPassed => ApplyEventKind::ValidationPassed,
            ApplyEvent::ValidationFailed(_) => ApplyEventKind::ValidationFailed,
            ApplyEvent::Switched { .. } => ApplyEventKind::Switched,
        }
    }
}

/// 管线效果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyEffect {
    /// 记 config_rejected。
    RecordRejected(RejectReason),
    /// 记 config_apply 并广播。
    RecordApplied {
        /// 版本。
        version: u64,
    },
}

fn rejected(e: &ApplyEvent) -> Vec<ApplyEffect> {
    match e {
        ApplyEvent::SignatureRejected(r)
        | ApplyEvent::ParseFailed(r)
        | ApplyEvent::ValidationFailed(r) => {
            vec![ApplyEffect::RecordRejected(r.clone())]
        }
        _ => Vec::new(),
    }
}

fn applied(e: &ApplyEvent) -> Vec<ApplyEffect> {
    match e {
        ApplyEvent::Switched { version } => vec![ApplyEffect::RecordApplied { version: *version }],
        _ => Vec::new(),
    }
}

/// 迁移表条目数。
pub const APPLY_FSM_TABLE_SIZE: usize = 7;

/// 建一台应用管线 FSM。
pub fn build_apply_fsm() -> Fsm<ApplyStage, ApplyEvent, ApplyEffect> {
    use ApplyEventKind as K;
    use ApplyStage as S;
    let table = Ok(FsmBuilder::<S, ApplyEvent, ApplyEffect>::new(S::Received))
        .and_then(|b| b.on(S::Received, K::SignatureAccepted, S::Verified))
        .and_then(|b| b.on_with(S::Received, K::SignatureRejected, S::Rejected, rejected))
        .and_then(|b| b.on(S::Verified, K::ParseSucceeded, S::Parsed))
        .and_then(|b| b.on_with(S::Verified, K::ParseFailed, S::Rejected, rejected))
        .and_then(|b| b.on(S::Parsed, K::ValidationPassed, S::Validated))
        .and_then(|b| b.on_with(S::Parsed, K::ValidationFailed, S::Rejected, rejected))
        .and_then(|b| b.on_with(S::Validated, K::Switched, S::Applied, applied));
    match table {
        Ok(builder) => builder.build(),
        Err(_) => FsmBuilder::new(S::Received).build(),
    }
}

/// 注册表视图（语义校验用；空集 = 跳过该项检查）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    /// 已注册能力。
    pub capabilities: BTreeSet<CapabilityId>,
    /// 已注册后端。
    pub backends: BTreeSet<BackendId>,
    /// 已注册云端端点。
    pub endpoints: BTreeSet<String>,
}

/// 应用结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// 已应用。
    Applied {
        /// 版本。
        version: u64,
    },
    /// 已拒绝。
    Rejected {
        /// 原因。
        reason: RejectReason,
        /// 在哪个阶段被拒。
        stage: ApplyStage,
    },
}

/// 效果（owner 执行：落盘、广播、观测）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigEffect {
    /// 写 config/active.json。
    PersistActive(Vec<u8>),
    /// 写 config/previous.json。
    PersistPrevious(Vec<u8>),
    /// 广播新配置（路由 / 调度 / 运行时 / 能力索引各自 validate 已在切换前完成）。
    Broadcast {
        /// 版本。
        version: u64,
    },
    /// 观测：config_apply。
    RecordApplied {
        /// 版本。
        version: u64,
    },
    /// 观测：config_rejected。
    RecordRejected {
        /// 收到的版本（解析失败时 None）。
        version: Option<u64>,
        /// 原因码。
        reason: &'static str,
    },
}

/// 台账。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfigStats {
    /// 收到。
    pub received: u64,
    /// 应用。
    pub applied: u64,
    /// 拒绝。
    pub rejected: u64,
    /// 回滚。
    pub rolled_back: u64,
}

/// 配置管理器。
pub struct ConfigStore {
    active: Arc<ConfigDoc>,
    previous: Option<Arc<ConfigDoc>>,
    active_table: MappingTable,
    bucket: u16,
    sdk: Version,
    verifier: Box<dyn SignatureVerifier>,
    registry: Registry,
    stats: ConfigStats,
    last_trace: Vec<String>,
}

impl ConfigStore {
    /// 以编译期缺省配置起步。`bucket` = 设备灰度桶（0～999，由 install_id hash 得出，宿主 KvStore 持久化）。
    pub fn new(
        bucket: u16,
        sdk: Version,
        verifier: Box<dyn SignatureVerifier>,
        registry: Registry,
    ) -> Self {
        let active = Arc::new(ConfigDoc::builtin_default());
        Self {
            active_table: MappingTable::default(),
            active,
            previous: None,
            bucket: bucket.min(999),
            sdk,
            verifier,
            registry,
            stats: ConfigStats::default(),
            last_trace: Vec::new(),
        }
    }

    /// 活动配置。
    pub fn active(&self) -> &Arc<ConfigDoc> {
        &self.active
    }

    /// 活动映射表（已转换、已校验）。
    pub fn active_table(&self) -> &MappingTable {
        &self.active_table
    }

    /// 上一版。
    pub fn previous(&self) -> Option<&Arc<ConfigDoc>> {
        self.previous.as_ref()
    }

    /// 台账。
    pub fn stats(&self) -> ConfigStats {
        self.stats
    }

    /// 最近一次应用的迁移轨迹（`describe` 格式）。
    pub fn last_trace(&self) -> &[String] {
        &self.last_trace
    }

    /// 应用一份到达的文档。
    pub fn apply(&mut self, bytes: &[u8], now_ms: u64) -> (ApplyOutcome, Vec<ConfigEffect>) {
        self.stats.received += 1;
        let mut fsm = build_apply_fsm();
        let mut effects = Vec::new();

        // ① 验签需要先解析出 signature（正文形式校验放 ②，让"解析失败"落在验签阶段——没有可验的签名）。
        let doc = serde_json::from_slice::<ConfigDoc>(bytes);
        let sig_event = match &doc {
            Ok(d) if !self.verifier.knows_key(&d.signature.key_id) => {
                ApplyEvent::SignatureRejected(RejectReason::UnknownKey(d.signature.key_id.clone()))
            }
            Ok(d) => match self.verifier.verify(&d.signing_payload(), &d.signature) {
                VerifyOutcome::Verified => ApplyEvent::SignatureAccepted,
                VerifyOutcome::Rejected => {
                    ApplyEvent::SignatureRejected(RejectReason::SignatureInvalid {
                        timed_out: false,
                    })
                }
                // 没等到宿主验签回执：拒因表中语义最近者仍是 signature_invalid，detail 留痕 verify=timeout（madai-w11 I-C-10）。
                VerifyOutcome::TimedOut => {
                    ApplyEvent::SignatureRejected(RejectReason::SignatureInvalid {
                        timed_out: true,
                    })
                }
            },
            Err(e) => ApplyEvent::SignatureRejected(RejectReason::SchemaInvalid(e.to_string())),
        };
        if let Some(reason) = rejected_reason(&fsm.fire(&sig_event, now_ms).effects) {
            return self.finish_rejected(&fsm, None, effects, reason);
        }
        let Ok(doc) = doc else {
            return self.finish_rejected(
                &fsm,
                None,
                effects,
                RejectReason::SignatureInvalid { timed_out: false },
            );
        };

        // ② schema 形式校验（范围 / 常量）
        let parse_event = match doc.validate_thresholds() {
            Ok(()) => ApplyEvent::ParseSucceeded,
            Err(e) => ApplyEvent::ParseFailed(RejectReason::Semantic(e)),
        };
        if let Some(reason) = rejected_reason(&fsm.fire(&parse_event, now_ms).effects) {
            return self.finish_rejected(&fsm, Some(doc.version), effects, reason);
        }

        // ③ 版本 / min_sdk / 分桶 / 语义（含映射表转换）
        let (validation_event, table) = match self.validate(&doc) {
            Ok(table) => (ApplyEvent::ValidationPassed, Some(table)),
            Err(reason) => (ApplyEvent::ValidationFailed(reason), None),
        };
        if let Some(reason) = rejected_reason(&fsm.fire(&validation_event, now_ms).effects) {
            return self.finish_rejected(&fsm, Some(doc.version), effects, reason);
        }

        // ④ 原子切换：新文档与新表一起换；旧的进 previous。
        let version = doc.version;
        let previous = std::mem::replace(&mut self.active, Arc::new(doc));
        if let Some(table) = table {
            self.active_table = table;
        }
        effects.push(ConfigEffect::PersistPrevious(
            serde_json::to_vec(&*previous).unwrap_or_default(),
        ));
        effects.push(ConfigEffect::PersistActive(
            serde_json::to_vec(&*self.active).unwrap_or_default(),
        ));
        self.previous = Some(previous);
        fsm.fire(&ApplyEvent::Switched { version }, now_ms);
        effects.push(ConfigEffect::Broadcast { version });
        effects.push(ConfigEffect::RecordApplied { version });
        self.stats.applied += 1;
        self.last_trace = fsm.trace().map(|r| r.describe()).collect();
        (ApplyOutcome::Applied { version }, effects)
    }

    /// 回滚到上一版（仅调试构建对宿主开放；云端回滚是下发更大版本号的旧内容）。
    pub fn rollback(&mut self) -> Option<Vec<ConfigEffect>> {
        let previous = self.previous.take()?;
        let table = previous
            .mapping_table(
                &self.registry.capabilities,
                &self.registry.backends,
                &self.registry.endpoints,
            )
            .unwrap_or_default();
        let demoted = std::mem::replace(&mut self.active, previous);
        self.active_table = table;
        self.stats.rolled_back += 1;
        let version = self.active.version;
        Some(vec![
            ConfigEffect::PersistPrevious(serde_json::to_vec(&*demoted).unwrap_or_default()),
            ConfigEffect::PersistActive(serde_json::to_vec(&*self.active).unwrap_or_default()),
            ConfigEffect::Broadcast { version },
        ])
    }

    fn validate(&self, doc: &ConfigDoc) -> Result<MappingTable, RejectReason> {
        if doc.version <= self.active.version {
            return Err(RejectReason::VersionNotNewer {
                received: doc.version,
                active: self.active.version,
            });
        }
        let req = VersionReq::parse(&doc.min_sdk)
            .map_err(|_| RejectReason::MinSdkUnsatisfied(doc.min_sdk.clone()))?;
        if !req.matches(&self.sdk) {
            return Err(RejectReason::MinSdkUnsatisfied(doc.min_sdk.clone()));
        }
        if !doc.rollout.contains(self.bucket) {
            return Err(RejectReason::OutOfRollout {
                bucket: self.bucket,
            });
        }
        doc.mapping_table(
            &self.registry.capabilities,
            &self.registry.backends,
            &self.registry.endpoints,
        )
        .map_err(RejectReason::Semantic)
    }

    fn finish_rejected(
        &mut self,
        fsm: &Fsm<ApplyStage, ApplyEvent, ApplyEffect>,
        version: Option<u64>,
        mut effects: Vec<ConfigEffect>,
        reason: RejectReason,
    ) -> (ApplyOutcome, Vec<ConfigEffect>) {
        self.stats.rejected += 1;
        self.last_trace = fsm.trace().map(|r| r.describe()).collect();
        let stage = fsm.trace().last().map_or(ApplyStage::Received, |r| r.from);
        effects.push(ConfigEffect::RecordRejected {
            version,
            reason: reason.wire(),
        });
        (ApplyOutcome::Rejected { reason, stage }, effects)
    }
}

fn rejected_reason(effects: &[ApplyEffect]) -> Option<RejectReason> {
    effects.iter().find_map(|e| match e {
        ApplyEffect::RecordRejected(r) => Some(r.clone()),
        ApplyEffect::RecordApplied { .. } => None,
    })
}

impl ConfigStore {
    /// 桶：install_id 的 xxh3-64 % 1000（协议算法，madai-w06 首日 E2 落定）。
    /// 实现取 xxhash-rust（vendor 收录、deny 白名单在册）——与云端侧同一规范实现对齐，两侧分桶一致；
    /// W05 前的 FNV-1a 占位桶随本批作废（I3 前无跨端比对面，无迁移负担；金样测试钉住 xxh3 读数防回退）。
    pub fn bucket_from_install_id(install_id: &[u8]) -> u16 {
        u16::try_from(xxhash_rust::xxh3::xxh3_64(install_id) % 1000).unwrap_or(0)
    }
}
