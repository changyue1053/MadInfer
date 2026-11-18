//! config.v1 文档类型（schemas/config.v1.json 的 Rust 镜像；未知字段忽略，R5）。
//!
//! ## 签名载荷规范形（[`ConfigDoc::signing_payload`]；madai-w10 I-C-09 J2，I-H-05 反馈 ③ / I-S-05 U-7）
//!
//! 载荷 = **结构体重序列化**，不是原文件字节。步骤与口径（云端签发侧与三端壳 fixture 必须按同一口径规范化，否则真核对
//! 云端文档 100% `signature_invalid`）：
//! 1. 文档反序列化进 [`ConfigDoc`]：未知键在顶层与固定结构层（`thresholds` / `switches` / `target` / `keys[]` …）丢弃；
//!    `when` 内未知键进 [`WhenDoc::extra`] 保留。缺省字段补齐——`thresholds` / `switches` / `keys` 的全部字段都出现
//!    （`deadline_ms` 三档、`frame_ms` 80、`gc_keep_days` 7、空表 `{}` / 空数组 `[]`）；`when` 的 None 字段不出现。
//! 2. `serde_json::to_value`：对象键按 UTF-8 字节序（serde_json 缺省 `BTreeMap`；本仓未启用 `preserve_order`），数组保持文档顺序。
//! 3. 顶层删去 `signature`。
//! 4. `serde_json::to_vec` 紧凑序列化：无空白；整数十进制；字串按 serde_json 转义（`"` `\` 与控制字符转义，`/` 与非 ASCII
//!    原样 UTF-8）；`DeadlineMs::async_` 键名 `async`、`KeyDoc::public_key` 键名 `pub`。
//!
//! 金样：仓根 `fixtures/signing_payload.golden.json`（madai-w11 I-C-10 起是事实源；`tests/signing_payload.rs` 读它断言）——
//! 三份文档：`non_canonical_shuffled`（813 字节、sha256 `795de2eb…7814d9`，即 madai-w10 金样）/ `minimal_defaults`（505 字节，
//! 缺省补齐）/ `escapes_and_unicode`（1027 字节，转义与非 ASCII）。三端壳与云端签发侧读同一份文件断言（消费法见 README），
//! 不各自手写规范形。I3 换 JCS（RFC 8785）时本文件与金样同批改；任何让金样变红的改动都要在报告 §7 通知三端。

use std::collections::{BTreeMap, BTreeSet};

use madinfer_core_types::{BackendId, CapabilityId, Os, PackageRef, Tier};
use madinfer_router::{Candidate, Conditions, EndpointKind, MappingEntry, MappingTable, Target};
use serde::{Deserialize, Serialize};

/// 签名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// 算法（ed25519）。
    pub alg: String,
    /// 密钥 id。
    pub key_id: String,
    /// base64 签名。
    pub sig: String,
}

/// 灰度桶区间（闭区间，1000 桶）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollout {
    /// [from, to]。
    pub buckets: [u16; 2],
}

impl Rollout {
    /// 桶是否在区间内。
    pub const fn contains(&self, bucket: u16) -> bool {
        self.buckets[0] <= bucket && bucket <= self.buckets[1]
    }
}

/// 路由目标（按 kind 区分）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetDoc {
    /// 系统模型。
    System {
        /// 后端。
        backend: String,
    },
    /// 内联引擎 + 包。
    Bundled {
        /// 后端。
        backend: String,
        /// `name@version`。
        package: String,
    },
    /// 云端。
    Cloud {
        /// 端点。
        endpoint: String,
    },
}

/// `when` 条件。`privacy` 字段若出现会被反序列化进 `extra` 并在语义校验判红。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WhenDoc {
    /// OS。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// availability = available。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    /// 最低档位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_min: Option<String>,
    /// 其余字段（含被禁的 privacy）。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 候选。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateDoc {
    /// 优先级。
    pub priority: u8,
    /// 目标。
    pub target: TargetDoc,
    /// 条件。
    #[serde(default)]
    pub when: WhenDoc,
}

/// 一条能力映射。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingEntryDoc {
    /// 能力。
    pub capability: String,
    /// 候选。
    pub candidates: Vec<CandidateDoc>,
}

/// 配额。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaDoc {
    /// 窗口秒。
    pub window_s: u32,
    /// 成本预算（ms × MB）。
    pub cost_ms_mb: u64,
}

/// 三档截止上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlineMs {
    /// 交互。
    pub interactive: u32,
    /// 异步。
    #[serde(rename = "async")]
    pub async_: u32,
    /// 后台。
    pub background: u32,
}

impl Default for DeadlineMs {
    fn default() -> Self {
        Self {
            interactive: 3_000,
            async_: 20_000,
            background: 120_000,
        }
    }
}

/// 档位线。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierLine {
    /// RAM MB。
    pub ram_mb: u32,
    /// 内存带宽 GB/s × 10。
    pub memory_bandwidth_gbps_x10: u32,
}

/// 阈值。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    /// 档位线。
    #[serde(default)]
    pub tier_lines: BTreeMap<String, TierLine>,
    /// 置信度下限（万分比），按能力。
    #[serde(default)]
    pub confidence_min_bp: BTreeMap<String, u16>,
    /// 截止上限。
    #[serde(default)]
    pub deadline_ms: DeadlineMs,
    /// 合帧窗口。
    #[serde(default = "default_frame_ms")]
    pub frame_ms: u32,
    /// KV 预算 MB，按档位。
    #[serde(default)]
    pub kv_budget_mb: BTreeMap<String, u32>,
    /// 配额，按业务 id（字符串键）。
    #[serde(default)]
    pub quotas: BTreeMap<String, QuotaDoc>,
    /// GC 保留天数。
    #[serde(default = "default_gc_keep_days")]
    pub gc_keep_days: u32,
}

fn default_frame_ms() -> u32 {
    80
}

fn default_gc_keep_days() -> u32 {
    7
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            tier_lines: BTreeMap::new(),
            confidence_min_bp: BTreeMap::new(),
            deadline_ms: DeadlineMs::default(),
            frame_ms: default_frame_ms(),
            kv_budget_mb: BTreeMap::new(),
            quotas: BTreeMap::new(),
            gc_keep_days: default_gc_keep_days(),
        }
    }
}

/// 开关。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Switches {
    /// 能力开关（未列出 = 开）。
    #[serde(default)]
    pub capabilities: BTreeMap<String, bool>,
    /// 只走云。
    #[serde(default)]
    pub cloud_only: bool,
    /// 全关。
    #[serde(default)]
    pub all_off: bool,
}

/// 密钥轮换条目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyDoc {
    /// 密钥 id。
    pub key_id: String,
    /// 算法。
    pub alg: String,
    /// 公钥 base64。
    #[serde(rename = "pub")]
    pub public_key: String,
    /// 生效时刻。
    pub not_before: String,
}

/// config.v1 文档。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDoc {
    /// 恒 1。
    pub schema: u32,
    /// 单调版本。
    pub version: u64,
    /// 签发时刻（RFC 3339）。
    pub issued_at: String,
    /// 最低 SDK 版本要求（semver req）。
    pub min_sdk: String,
    /// 灰度。
    pub rollout: Rollout,
    /// 映射表。
    pub mapping: Vec<MappingEntryDoc>,
    /// 阈值。
    #[serde(default)]
    pub thresholds: Thresholds,
    /// 开关。
    #[serde(default)]
    pub switches: Switches,
    /// 密钥。
    #[serde(default)]
    pub keys: Vec<KeyDoc>,
    /// 签名。
    pub signature: Signature,
}

/// 语义错误（校验阶段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticError {
    /// schema 字段不是 1。
    SchemaVersion(u32),
    /// 能力名非法或未注册。
    UnknownCapability(String),
    /// 后端未注册。
    UnknownBackend(String),
    /// 端点未注册。
    UnknownEndpoint(String),
    /// 包引用格式错。
    BadPackageRef(String),
    /// 枚举字符串非法。
    BadEnum {
        /// 字段。
        field: &'static str,
        /// 值。
        value: String,
    },
    /// `when` 含 privacy（隐私门禁区）。
    PrivacyInWhen(String),
    /// 阈值越界。
    ThresholdOutOfRange(&'static str),
    /// 桶区间非法。
    BadRollout,
}

impl ConfigDoc {
    /// 编译期缺省配置（version 0，空映射，缺省阈值）：任一时刻至少有它兜底。
    pub fn builtin_default() -> Self {
        Self {
            schema: 1,
            version: 0,
            issued_at: "1970-01-01T00:00:00Z".to_owned(),
            min_sdk: ">=0.0.0".to_owned(),
            rollout: Rollout { buckets: [0, 999] },
            mapping: Vec::new(),
            thresholds: Thresholds::default(),
            switches: Switches::default(),
            keys: Vec::new(),
            signature: Signature {
                alg: "none".to_owned(),
                key_id: "builtin".to_owned(),
                sig: String::new(),
            },
        }
    }

    /// 转成路由映射表（同时做语义校验；`known_*` 为空集时跳过对应的注册检查）。
    pub fn mapping_table(
        &self,
        known_capabilities: &BTreeSet<CapabilityId>,
        known_backends: &BTreeSet<BackendId>,
        known_endpoints: &BTreeSet<String>,
    ) -> Result<MappingTable, SemanticError> {
        let mut entries = Vec::with_capacity(self.mapping.len());
        for m in &self.mapping {
            let capability = CapabilityId::new(&m.capability)
                .ok_or_else(|| SemanticError::UnknownCapability(m.capability.clone()))?;
            if !known_capabilities.is_empty() && !known_capabilities.contains(&capability) {
                return Err(SemanticError::UnknownCapability(m.capability.clone()));
            }
            let mut candidates = Vec::with_capacity(m.candidates.len());
            for c in &m.candidates {
                if let Some(bad) = c.when.extra.keys().find(|k| k.as_str() == "privacy") {
                    return Err(SemanticError::PrivacyInWhen(bad.clone()));
                }
                let target = match &c.target {
                    TargetDoc::System { backend } => {
                        check_backend(backend, known_backends)?;
                        Target::System {
                            backend: BackendId(backend.clone()),
                        }
                    }
                    TargetDoc::Bundled { backend, package } => {
                        check_backend(backend, known_backends)?;
                        Target::Bundled {
                            backend: BackendId(backend.clone()),
                            package: PackageRef::parse(package)
                                .ok_or_else(|| SemanticError::BadPackageRef(package.clone()))?,
                        }
                    }
                    TargetDoc::Cloud { endpoint } => {
                        if !known_endpoints.is_empty() && !known_endpoints.contains(endpoint) {
                            return Err(SemanticError::UnknownEndpoint(endpoint.clone()));
                        }
                        Target::Cloud {
                            endpoint: endpoint.clone(),
                            kind: if endpoint.contains("enterprise") {
                                EndpointKind::Enterprise
                            } else {
                                EndpointKind::ThirdParty
                            },
                        }
                    }
                };
                let os = match c.when.os.as_deref() {
                    None => None,
                    Some("ios") => Some(Os::Ios),
                    Some("android") => Some(Os::Android),
                    Some("ohos") => Some(Os::Ohos),
                    Some("jvm") => Some(Os::Jvm),
                    Some(other) => {
                        return Err(SemanticError::BadEnum {
                            field: "when.os",
                            value: other.to_owned(),
                        })
                    }
                };
                let tier_min = match c.when.tier_min.as_deref() {
                    None => None,
                    Some("low") => Some(Tier::Low),
                    Some("mid") => Some(Tier::Mid),
                    Some("flagship") => Some(Tier::Flagship),
                    Some(other) => {
                        return Err(SemanticError::BadEnum {
                            field: "when.tier_min",
                            value: other.to_owned(),
                        })
                    }
                };
                let requires_available = match c.when.availability.as_deref() {
                    None => false,
                    Some("available") => true,
                    Some(other) => {
                        return Err(SemanticError::BadEnum {
                            field: "when.availability",
                            value: other.to_owned(),
                        })
                    }
                };
                candidates.push(Candidate {
                    priority: c.priority,
                    target,
                    when: Conditions {
                        os,
                        tier_min,
                        requires_available,
                    },
                });
            }
            entries.push(MappingEntry {
                capability,
                candidates,
            });
        }
        Ok(MappingTable { entries })
    }

    /// 阈值与桶的范围校验。
    pub fn validate_thresholds(&self) -> Result<(), SemanticError> {
        if self.schema != 1 {
            return Err(SemanticError::SchemaVersion(self.schema));
        }
        if self.rollout.buckets[0] > self.rollout.buckets[1] || self.rollout.buckets[1] > 999 {
            return Err(SemanticError::BadRollout);
        }
        let t = &self.thresholds;
        if !(16..=500).contains(&t.frame_ms) {
            return Err(SemanticError::ThresholdOutOfRange("frame_ms"));
        }
        for (name, v) in [
            ("deadline_ms.interactive", t.deadline_ms.interactive),
            ("deadline_ms.async", t.deadline_ms.async_),
            ("deadline_ms.background", t.deadline_ms.background),
        ] {
            if !(100..=600_000).contains(&v) {
                return Err(SemanticError::ThresholdOutOfRange(name));
            }
        }
        if t.confidence_min_bp.values().any(|bp| *bp > 10_000) {
            return Err(SemanticError::ThresholdOutOfRange("confidence_min_bp"));
        }
        if t.quotas.values().any(|q| q.window_s == 0) {
            return Err(SemanticError::ThresholdOutOfRange("quotas.window_s"));
        }
        Ok(())
    }

    /// 路由开关快照。
    pub fn router_switches(&self) -> madinfer_router::Switches {
        madinfer_router::Switches {
            disabled_capabilities: self
                .switches
                .capabilities
                .iter()
                .filter(|(_, on)| !**on)
                .filter_map(|(name, _)| CapabilityId::new(name))
                .collect(),
            cloud_only: self.switches.cloud_only,
            all_off: self.switches.all_off,
        }
    }

    /// 序列化去掉 signature 的文档（验签载荷；I3 换 JCS 规范化，此处为 serde_json 紧凑序列化 + 键排序——BTreeMap 已保证）。
    /// 规范形四步口径与金样见模块头注（madai-w10 I-C-09 J2）。
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature = Signature {
            alg: String::new(),
            key_id: String::new(),
            sig: String::new(),
        };
        let mut value = serde_json::to_value(&unsigned).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = &mut value {
            map.remove("signature");
        }
        serde_json::to_vec(&value).unwrap_or_default()
    }
}

fn check_backend(backend: &str, known: &BTreeSet<BackendId>) -> Result<(), SemanticError> {
    if backend.is_empty() || (!known.is_empty() && !known.contains(&BackendId(backend.to_owned())))
    {
        return Err(SemanticError::UnknownBackend(backend.to_owned()));
    }
    Ok(())
}
