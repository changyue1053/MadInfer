//! madinfer-observe —— 横切 · 观测（设计文档 C13 / §5.5 P5）。
//!
//! 三道门把用户内容挡在观测之外：
//! 1. [`TelemetryEvent`] 是闭合枚举，字段只有整数、枚举、[`RegisteredId`]、trace id；没有 `String` 字段。
//! 2. [`RegisteredId`] 只能由 [`IdRegistry`] 构造（能力名、后端名、包名、端点名先登记再用），编译期阻断自由字符串。
//! 3. 序列化出的键集合 ⊆ `schemas/telemetry.v1.json` 的白名单——单测读 schema 文件逐键核对（telemetry 文档 `additionalProperties: false`）。
//!
//! 批量：环形缓冲 + 定时刷出（5 s 或 50 条，先到先刷），sans-io——[`Telemetry::offer`] / [`Telemetry::flush_due`] 返回批次，
//! owner 在投递线程调 `TelemetrySink::emit`。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::{BTreeSet, VecDeque};

use madinfer_core_types::{Os, PrivacyTag, QoS, Source, Tier};
use serde_json::{json, Map, Value};

/// 注册制 id：只能经 [`IdRegistry::register`] 得到。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegisteredId(String);

impl RegisteredId {
    /// 字符串视图。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// id 注册表：能力 / 后端 / 包 / 端点名先登记；未登记的字符串进不了事件。
#[derive(Debug, Default)]
pub struct IdRegistry {
    known: BTreeSet<String>,
}

impl IdRegistry {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记（长度 ≤ 64，只准 `[A-Za-z0-9._@:-]`）；非法返回 None。
    pub fn register(&mut self, id: &str) -> Option<RegisteredId> {
        let ok = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b':' | b'-')
            });
        if !ok {
            return None;
        }
        self.known.insert(id.to_owned());
        Some(RegisteredId(id.to_owned()))
    }

    /// 已登记则取。
    pub fn get(&self, id: &str) -> Option<RegisteredId> {
        self.known.contains(id).then(|| RegisteredId(id.to_owned()))
    }
}

/// 热状态（与 madinfer-host 同名枚举的观测投影，避免 host 类型进协议层）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thermal {
    /// nominal。
    Nominal,
    /// fair。
    Fair,
    /// serious。
    Serious,
    /// critical。
    Critical,
}

/// 内存等级。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLevel {
    /// normal。
    Normal,
    /// warning。
    Warning,
    /// critical。
    Critical,
}

/// 请求结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// complete。
    Complete,
    /// failed。
    Failed,
    /// cancelled。
    Cancelled,
}

/// 准入结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionKind {
    /// run。
    Run,
    /// defer。
    Defer,
    /// reject。
    Reject,
}

/// 门。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// charging。
    Charging,
    /// idle。
    Idle,
    /// thermal。
    Thermal,
    /// quota。
    Quota,
    /// memory。
    Memory,
}

/// 加载 / 安装阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// map。
    Map,
    /// load。
    Load,
    /// warm。
    Warm,
    /// verified。
    Verified,
    /// installed。
    Installed,
    /// activated。
    Activated,
}

/// 约束模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    /// native。
    Native,
    /// emulated。
    Emulated,
    /// none。
    None,
}

/// 授权结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// allow。
    Allow,
    /// allow_remember。
    AllowRemember,
    /// deny。
    Deny,
}

/// 可用性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// available。
    Available,
    /// downloading。
    Downloading,
    /// unavailable。
    Unavailable,
}

/// 观测事件（闭合；字段名与 telemetry.v1 一致）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TelemetryEvent {
    /// 请求开始。
    RequestStart {
        /// trace id（32 hex）。
        trace: [u8; 16],
        /// 能力。
        capability: RegisteredId,
        /// 业务。
        business: u16,
        /// QoS。
        qos: QoS,
        /// 隐私。
        privacy: PrivacyTag,
    },
    /// 请求结束。
    RequestEnd {
        /// trace。
        trace: [u8; 16],
        /// 能力。
        capability: RegisteredId,
        /// 业务。
        business: u16,
        /// QoS。
        qos: QoS,
        /// 来源。
        source: Source,
        /// 后端（路由前失败时缺席；madai-w05 D1，I-C-03 U-9）。
        backend: Option<RegisteredId>,
        /// 包。
        package: Option<RegisteredId>,
        /// 结局。
        outcome: Outcome,
        /// 错误码（0 = 无）。
        code: u16,
        /// 排队。
        queued_ms: u32,
        /// 首 token。
        ttft_ms: u32,
        /// 总时长。
        total_ms: u32,
        /// prompt token。
        prompt_tokens: u32,
        /// 生成 token。
        gen_tokens: u32,
        /// tps × 100。
        tps_x100: u32,
        /// 峰值内存。
        peak_memory_mb: u32,
        /// 热。
        thermal: Thermal,
        /// 回退步数。
        fallback_steps: u8,
        /// 前缀命中。
        prefix_hit: bool,
        /// 冷页入。
        cold_page_in: bool,
        /// 约束模式。
        constraint: Constraint,
        /// 采纳（业务回填）。
        adopted: Option<bool>,
    },
    /// 路由决策。
    RouteDecision {
        /// 能力。
        capability: RegisteredId,
        /// 隐私。
        privacy: PrivacyTag,
        /// 原因码。
        reason: &'static str,
        /// 链长。
        chain_len: u8,
        /// 选中来源。
        chosen: Source,
    },
    /// 调度等待。
    SchedWait {
        /// QoS。
        qos: QoS,
        /// 等待。
        wait_ms: u32,
        /// 准入。
        admission: AdmissionKind,
        /// 门。
        gate: Gate,
    },
    /// 抢占。
    Preempt {
        /// 被抢占者 QoS。
        qos: QoS,
        /// 已运行。
        ms: u32,
    },
    /// 模型加载。
    ModelLoad {
        /// 后端。
        backend: RegisteredId,
        /// 包（系统模型无本地包时缺席；madai-w05 D1，I-C-03 U-9）。
        package: Option<RegisteredId>,
        /// 阶段。
        phase: Phase,
        /// 耗时。
        ms: u32,
        /// AOT 命中。
        aot_hit: bool,
        /// 常驻。
        resident_mb: u32,
        /// 映射。
        mapped_mb: u32,
    },
    /// 模型卸载。
    ModelUnload {
        /// 后端。
        backend: RegisteredId,
        /// 包。
        package: RegisteredId,
        /// 常驻释放。
        resident_mb: u32,
    },
    /// 适配器切换。
    AdapterSwitch {
        /// 后端。
        backend: RegisteredId,
        /// 包。
        package: RegisteredId,
        /// 耗时。
        ms: u32,
    },
    /// 内存压力。
    MemoryPressure {
        /// 等级。
        level: MemoryLevel,
        /// 驱逐 KV。
        evicted_kv_mb: u32,
        /// 卸载模型数。
        unloaded_models: u32,
    },
    /// 热变化。
    ThermalChange {
        /// 热。
        thermal: Thermal,
    },
    /// 资产安装。
    AssetInstall {
        /// 包。
        package: RegisteredId,
        /// 阶段。
        phase: Phase,
        /// 字节。
        bytes: u64,
        /// 耗时。
        ms: u32,
    },
    /// 配置应用。
    ConfigApply {
        /// 版本。
        version: u64,
        /// 结局。
        outcome: Outcome,
    },
    /// 配置拒绝。
    ConfigRejected {
        /// 版本（解析失败为 0）。
        version: u64,
        /// 原因码。
        reason: &'static str,
    },
    /// 可用性变化。
    AvailabilityChange {
        /// 后端。
        backend: RegisteredId,
        /// 可用性。
        availability: Availability,
    },
    /// 授权。
    Consent {
        /// 能力。
        capability: RegisteredId,
        /// 结论。
        decision: Decision,
    },
    /// 错误。
    Error {
        /// 码。
        code: u16,
        /// kind 字符串（注册制 wire()）。
        kind: &'static str,
        /// 后端。
        backend: Option<RegisteredId>,
        /// 包。
        package: Option<RegisteredId>,
    },
    /// 慢 sink（回调阻塞）。
    SlowSink {
        /// 阻塞。
        ms: u32,
    },
    /// 后端隔离。
    Quarantine {
        /// 后端。
        backend: RegisteredId,
        /// 原因码。
        reason: &'static str,
    },
}

impl TelemetryEvent {
    /// 协议 `type`。
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::RequestStart { .. } => "request_start",
            Self::RequestEnd { .. } => "request_end",
            Self::RouteDecision { .. } => "route_decision",
            Self::SchedWait { .. } => "sched_wait",
            Self::Preempt { .. } => "preempt",
            Self::ModelLoad { .. } => "model_load",
            Self::ModelUnload { .. } => "model_unload",
            Self::AdapterSwitch { .. } => "adapter_switch",
            Self::MemoryPressure { .. } => "memory_pressure",
            Self::ThermalChange { .. } => "thermal_change",
            Self::AssetInstall { .. } => "asset_install",
            Self::ConfigApply { .. } => "config_apply",
            Self::ConfigRejected { .. } => "config_rejected",
            Self::AvailabilityChange { .. } => "availability_change",
            Self::Consent { .. } => "consent",
            Self::Error { .. } => "error",
            Self::SlowSink { .. } => "slow_sink",
            Self::Quarantine { .. } => "quarantine",
        }
    }

    /// 序列化为 telemetry.v1 events[] 的一项。
    pub fn to_json(&self, t_ms: u64) -> Value {
        let mut m = Map::new();
        m.insert("t".into(), json!(t_ms));
        m.insert("type".into(), json!(self.type_name()));
        let put = |m: &mut Map<String, Value>, k: &str, v: Value| {
            m.insert(k.to_owned(), v);
        };
        match self {
            Self::RequestStart {
                trace,
                capability,
                business,
                qos,
                privacy,
            } => {
                put(&mut m, "trace", json!(hex(trace)));
                put(&mut m, "capability", json!(capability.as_str()));
                put(&mut m, "business", json!(business));
                put(&mut m, "qos", json!(qos.wire()));
                put(&mut m, "privacy", json!(privacy.wire()));
            }
            Self::RequestEnd {
                trace,
                capability,
                business,
                qos,
                source,
                backend,
                package,
                outcome,
                code,
                queued_ms,
                ttft_ms,
                total_ms,
                prompt_tokens,
                gen_tokens,
                tps_x100,
                peak_memory_mb,
                thermal,
                fallback_steps,
                prefix_hit,
                cold_page_in,
                constraint,
                adopted,
            } => {
                put(&mut m, "trace", json!(hex(trace)));
                put(&mut m, "capability", json!(capability.as_str()));
                put(&mut m, "business", json!(business));
                put(&mut m, "qos", json!(qos.wire()));
                put(&mut m, "source", json!(source.wire()));
                if let Some(b) = backend {
                    put(&mut m, "backend", json!(b.as_str()));
                }
                if let Some(p) = package {
                    put(&mut m, "package", json!(p.as_str()));
                }
                put(&mut m, "outcome", json!(outcome_wire(*outcome)));
                put(&mut m, "code", json!(code));
                put(&mut m, "queued_ms", json!(queued_ms));
                put(&mut m, "ttft_ms", json!(ttft_ms));
                put(&mut m, "total_ms", json!(total_ms));
                put(&mut m, "prompt_tokens", json!(prompt_tokens));
                put(&mut m, "gen_tokens", json!(gen_tokens));
                put(&mut m, "tps_x100", json!(tps_x100));
                put(&mut m, "peak_memory_mb", json!(peak_memory_mb));
                put(&mut m, "thermal", json!(thermal_wire(*thermal)));
                put(&mut m, "fallback_steps", json!(fallback_steps));
                put(&mut m, "prefix_hit", json!(prefix_hit));
                put(&mut m, "cold_page_in", json!(cold_page_in));
                put(&mut m, "constraint", json!(constraint_wire(*constraint)));
                put(&mut m, "adopted", json!(adopted));
            }
            Self::RouteDecision {
                capability,
                privacy,
                reason,
                chain_len,
                chosen,
            } => {
                put(&mut m, "capability", json!(capability.as_str()));
                put(&mut m, "privacy", json!(privacy.wire()));
                put(&mut m, "reason", json!(reason));
                put(&mut m, "chain_len", json!(chain_len));
                put(&mut m, "chosen", json!(chosen.wire()));
            }
            Self::SchedWait {
                qos,
                wait_ms,
                admission,
                gate,
            } => {
                put(&mut m, "qos", json!(qos.wire()));
                put(&mut m, "wait_ms", json!(wait_ms));
                put(
                    &mut m,
                    "admission",
                    json!(match admission {
                        AdmissionKind::Run => "run",
                        AdmissionKind::Defer => "defer",
                        AdmissionKind::Reject => "reject",
                    }),
                );
                put(
                    &mut m,
                    "gate",
                    json!(match gate {
                        Gate::Charging => "charging",
                        Gate::Idle => "idle",
                        Gate::Thermal => "thermal",
                        Gate::Quota => "quota",
                        Gate::Memory => "memory",
                    }),
                );
            }
            Self::Preempt { qos, ms } => {
                put(&mut m, "qos", json!(qos.wire()));
                put(&mut m, "ms", json!(ms));
            }
            Self::ModelLoad {
                backend,
                package,
                phase,
                ms,
                aot_hit,
                resident_mb,
                mapped_mb,
            } => {
                put(&mut m, "backend", json!(backend.as_str()));
                if let Some(p) = package {
                    put(&mut m, "package", json!(p.as_str()));
                }
                put(&mut m, "phase", json!(phase_wire(*phase)));
                put(&mut m, "ms", json!(ms));
                put(&mut m, "aot_hit", json!(aot_hit));
                put(&mut m, "resident_mb", json!(resident_mb));
                put(&mut m, "mapped_mb", json!(mapped_mb));
            }
            Self::ModelUnload {
                backend,
                package,
                resident_mb,
            } => {
                put(&mut m, "backend", json!(backend.as_str()));
                put(&mut m, "package", json!(package.as_str()));
                put(&mut m, "resident_mb", json!(resident_mb));
            }
            Self::AdapterSwitch {
                backend,
                package,
                ms,
            } => {
                put(&mut m, "backend", json!(backend.as_str()));
                put(&mut m, "package", json!(package.as_str()));
                put(&mut m, "ms", json!(ms));
            }
            Self::MemoryPressure {
                level,
                evicted_kv_mb,
                unloaded_models,
            } => {
                put(
                    &mut m,
                    "level",
                    json!(match level {
                        MemoryLevel::Normal => "normal",
                        MemoryLevel::Warning => "warning",
                        MemoryLevel::Critical => "critical",
                    }),
                );
                put(&mut m, "evicted_kv_mb", json!(evicted_kv_mb));
                put(&mut m, "unloaded_models", json!(unloaded_models));
            }
            Self::ThermalChange { thermal } => {
                put(&mut m, "thermal", json!(thermal_wire(*thermal)))
            }
            Self::AssetInstall {
                package,
                phase,
                bytes,
                ms,
            } => {
                put(&mut m, "package", json!(package.as_str()));
                put(&mut m, "phase", json!(phase_wire(*phase)));
                put(&mut m, "bytes", json!(bytes));
                put(&mut m, "ms", json!(ms));
            }
            Self::ConfigApply { version, outcome } => {
                put(&mut m, "version", json!(version));
                put(
                    &mut m,
                    "outcome",
                    json!(match outcome {
                        Outcome::Complete => "applied",
                        Outcome::Failed | Outcome::Cancelled => "rejected",
                    }),
                );
            }
            Self::ConfigRejected { version, reason } => {
                put(&mut m, "version", json!(version));
                put(&mut m, "reason", json!(reason));
            }
            Self::AvailabilityChange {
                backend,
                availability,
            } => {
                put(&mut m, "backend", json!(backend.as_str()));
                put(
                    &mut m,
                    "availability",
                    json!(match availability {
                        Availability::Available => "available",
                        Availability::Downloading => "downloading",
                        Availability::Unavailable => "unavailable",
                    }),
                );
            }
            Self::Consent {
                capability,
                decision,
            } => {
                put(&mut m, "capability", json!(capability.as_str()));
                put(
                    &mut m,
                    "decision",
                    json!(match decision {
                        Decision::Allow => "allow",
                        Decision::AllowRemember => "allow_remember",
                        Decision::Deny => "deny",
                    }),
                );
            }
            Self::Error {
                code,
                kind,
                backend,
                package,
            } => {
                put(&mut m, "code", json!(code));
                put(&mut m, "kind", json!(kind));
                if let Some(b) = backend {
                    put(&mut m, "backend", json!(b.as_str()));
                }
                if let Some(p) = package {
                    put(&mut m, "package", json!(p.as_str()));
                }
            }
            Self::SlowSink { ms } => put(&mut m, "ms", json!(ms)),
            Self::Quarantine { backend, reason } => {
                put(&mut m, "backend", json!(backend.as_str()));
                put(&mut m, "reason", json!(reason));
            }
        }
        Value::Object(m)
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

const fn outcome_wire(o: Outcome) -> &'static str {
    match o {
        Outcome::Complete => "complete",
        Outcome::Failed => "failed",
        Outcome::Cancelled => "cancelled",
    }
}

const fn thermal_wire(t: Thermal) -> &'static str {
    match t {
        Thermal::Nominal => "nominal",
        Thermal::Fair => "fair",
        Thermal::Serious => "serious",
        Thermal::Critical => "critical",
    }
}

const fn constraint_wire(c: Constraint) -> &'static str {
    match c {
        Constraint::Native => "native",
        Constraint::Emulated => "emulated",
        Constraint::None => "none",
    }
}

const fn phase_wire(p: Phase) -> &'static str {
    match p {
        Phase::Map => "map",
        Phase::Load => "load",
        Phase::Warm => "warm",
        Phase::Verified => "verified",
        Phase::Installed => "installed",
        Phase::Activated => "activated",
    }
}

/// 批次头（telemetry.v1 顶层字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchMeta {
    /// 实例名（≤ 64）。
    pub instance: String,
    /// SDK 版本。
    pub sdk: String,
    /// 档位。
    pub tier: Tier,
    /// OS。
    pub os: Os,
    /// OS 版本（≤ 32）。
    pub os_version: String,
}

/// 一批事件（已带时间戳）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    /// (时刻, 事件)。
    pub events: Vec<(u64, TelemetryEvent)>,
}

impl Batch {
    /// 序列化为 telemetry.v1 文档。
    pub fn to_json(&self, meta: &BatchMeta) -> String {
        let doc = json!({
            "schema": 1,
            "instance": meta.instance,
            "sdk": meta.sdk,
            "tier": meta.tier.wire(),
            "os": meta.os.wire(),
            "os_version": meta.os_version,
            "events": self.events.iter().map(|(t, e)| e.to_json(*t)).collect::<Vec<_>>(),
        });
        doc.to_string()
    }
}

/// 采样 + 批量（sans-io）。
#[derive(Debug)]
pub struct Telemetry {
    buffer: VecDeque<(u64, TelemetryEvent)>,
    sampling_bp: u16,
    flush_interval_ms: u64,
    max_batch: usize,
    last_flush_ms: u64,
    counter: u64,
    /// 被采样丢弃的事件数。
    pub sampled_out: u64,
    /// 缓冲溢出丢弃数（超过 20 × max_batch 仍未刷出）。
    pub dropped: u64,
}

impl Telemetry {
    /// 缺省：5 s 或 50 条。
    pub fn new(sampling_bp: u16, now_ms: u64) -> Self {
        Self::with_policy(sampling_bp, 5_000, 50, now_ms)
    }

    /// 自定义策略。
    pub fn with_policy(
        sampling_bp: u16,
        flush_interval_ms: u64,
        max_batch: usize,
        now_ms: u64,
    ) -> Self {
        Self {
            buffer: VecDeque::new(),
            sampling_bp: sampling_bp.min(10_000),
            flush_interval_ms,
            max_batch: max_batch.max(1),
            last_flush_ms: now_ms,
            counter: 0,
            sampled_out: 0,
            dropped: 0,
        }
    }

    /// 缓冲中的条数。
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// 下次定时刷出时刻。
    pub fn next_flush_ms(&self) -> u64 {
        self.last_flush_ms.saturating_add(self.flush_interval_ms)
    }

    /// 收一条：采样（确定性、均匀：每 10 000 条留 `sampling_bp` 条，按计数等距选取）；满 max_batch 立即返回批次。
    /// 错误 / 隔离 / 配置事件不采样（低频高价值）。
    pub fn offer(&mut self, event: TelemetryEvent, now_ms: u64) -> Option<Batch> {
        self.counter += 1;
        let always = matches!(
            event,
            TelemetryEvent::Error { .. }
                | TelemetryEvent::Quarantine { .. }
                | TelemetryEvent::ConfigApply { .. }
                | TelemetryEvent::ConfigRejected { .. }
        );
        let bp = u64::from(self.sampling_bp);
        let keep = bp > 0 && (self.counter.wrapping_mul(bp) % 10_000) < bp;
        if !always && !keep {
            self.sampled_out += 1;
            return None;
        }
        if self.buffer.len() >= self.max_batch * 20 {
            self.buffer.pop_front();
            self.dropped += 1;
        }
        self.buffer.push_back((now_ms, event));
        if self.buffer.len() >= self.max_batch {
            return Some(self.take(now_ms));
        }
        None
    }

    /// 定时刷出（到期且有内容）。
    pub fn flush_due(&mut self, now_ms: u64) -> Option<Batch> {
        if now_ms < self.next_flush_ms() || self.buffer.is_empty() {
            return None;
        }
        Some(self.take(now_ms))
    }

    /// 关停排水：全部刷出。
    pub fn drain(&mut self, now_ms: u64) -> Option<Batch> {
        if self.buffer.is_empty() {
            return None;
        }
        Some(self.take(now_ms))
    }

    fn take(&mut self, now_ms: u64) -> Batch {
        self.last_flush_ms = now_ms;
        let n = self.buffer.len().min(self.max_batch);
        Batch {
            events: self.buffer.drain(..n).collect(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const SCHEMA: &str = include_str!("../../../schemas/telemetry.v1.json");

    fn registry() -> IdRegistry {
        IdRegistry::new()
    }

    fn all_events(r: &mut IdRegistry) -> Vec<TelemetryEvent> {
        let cap = r.register("summarize").expect("id");
        let backend = r.register("llama.cpp").expect("id");
        let package = r.register("catchup-summary@1.4.2").expect("id");
        vec![
            TelemetryEvent::RequestStart {
                trace: [0xab; 16],
                capability: cap.clone(),
                business: 1,
                qos: QoS::Interactive,
                privacy: PrivacyTag::E2eePlaintext,
            },
            TelemetryEvent::RequestEnd {
                trace: [0xab; 16],
                capability: cap.clone(),
                business: 1,
                qos: QoS::Interactive,
                source: Source::Bundled,
                backend: Some(backend.clone()),
                package: Some(package.clone()),
                outcome: Outcome::Complete,
                code: 0,
                queued_ms: 12,
                ttft_ms: 410,
                total_ms: 6650,
                prompt_tokens: 1830,
                gen_tokens: 152,
                tps_x100: 2450,
                peak_memory_mb: 2210,
                thermal: Thermal::Fair,
                fallback_steps: 0,
                prefix_hit: true,
                cold_page_in: false,
                constraint: Constraint::Native,
                adopted: None,
            },
            TelemetryEvent::RouteDecision {
                capability: cap.clone(),
                privacy: PrivacyTag::E2eePlaintext,
                reason: "privacy_forced_local",
                chain_len: 1,
                chosen: Source::Bundled,
            },
            TelemetryEvent::SchedWait {
                qos: QoS::Background,
                wait_ms: 61_000,
                admission: AdmissionKind::Defer,
                gate: Gate::Charging,
            },
            TelemetryEvent::Preempt {
                qos: QoS::Background,
                ms: 300,
            },
            TelemetryEvent::ModelLoad {
                backend: backend.clone(),
                package: None,
                phase: Phase::Map,
                ms: 180,
                aot_hit: true,
                resident_mb: 120,
                mapped_mb: 1843,
            },
            TelemetryEvent::ModelUnload {
                backend: backend.clone(),
                package: package.clone(),
                resident_mb: 120,
            },
            TelemetryEvent::AdapterSwitch {
                backend: backend.clone(),
                package: package.clone(),
                ms: 40,
            },
            TelemetryEvent::MemoryPressure {
                level: MemoryLevel::Warning,
                evicted_kv_mb: 96,
                unloaded_models: 0,
            },
            TelemetryEvent::ThermalChange {
                thermal: Thermal::Serious,
            },
            TelemetryEvent::AssetInstall {
                package: package.clone(),
                phase: Phase::Verified,
                bytes: 31_457_280,
                ms: 5400,
            },
            TelemetryEvent::ConfigApply {
                version: 20260904003,
                outcome: Outcome::Complete,
            },
            TelemetryEvent::ConfigRejected {
                version: 20260904004,
                reason: "signature_invalid",
            },
            TelemetryEvent::AvailabilityChange {
                backend: backend.clone(),
                availability: Availability::Available,
            },
            TelemetryEvent::Consent {
                capability: cap,
                decision: Decision::AllowRemember,
            },
            TelemetryEvent::Error {
                code: 3005,
                kind: "asset_corrupt",
                backend: Some(backend.clone()),
                package: Some(package),
            },
            TelemetryEvent::SlowSink { ms: 120 },
            TelemetryEvent::Quarantine {
                backend,
                reason: "crash_x3",
            },
        ]
    }

    #[test]
    fn every_event_type_serializes_within_the_schema_whitelist() {
        let schema: Value = serde_json::from_str(SCHEMA).expect("schema json");
        let item = &schema["properties"]["events"]["items"];
        assert_eq!(
            item["additionalProperties"],
            Value::Bool(false),
            "白名单型文档"
        );
        let allowed: BTreeSet<String> = item["properties"]
            .as_object()
            .expect("props")
            .keys()
            .cloned()
            .collect();
        let types: BTreeSet<String> = item["properties"]["type"]["enum"]
            .as_array()
            .expect("enum")
            .iter()
            .map(|v| v.as_str().expect("str").to_owned())
            .collect();
        let mut r = registry();
        let events = all_events(&mut r);
        assert_eq!(events.len(), 18, "18 种事件类型全覆盖");
        let mut seen = BTreeSet::new();
        for e in &events {
            let v = e.to_json(1_725_417_600_123);
            let obj = v.as_object().expect("object");
            for k in obj.keys() {
                assert!(
                    allowed.contains(k),
                    "{} 序列化出白名单外的键 {k}",
                    e.type_name()
                );
            }
            assert!(
                types.contains(e.type_name()),
                "type {} 不在闭合枚举",
                e.type_name()
            );
            seen.insert(e.type_name().to_owned());
            // 值只准是整数 / 布尔 / null / 注册制字符串 / hex：不允许出现空白（用户内容的最简特征）。
            for (k, val) in obj {
                if let Some(s) = val.as_str() {
                    assert!(!s.contains(char::is_whitespace), "{k} 含空白：{s}");
                }
            }
        }
        assert_eq!(seen, types, "枚举与 schema 的 type 集合一一对应");
    }

    #[test]
    fn registry_rejects_free_text() {
        let mut r = registry();
        assert!(r.register("summarize").is_some());
        assert!(r.register("用户说了什么").is_none(), "非 ASCII 词表字符");
        assert!(r.register("has space").is_none());
        assert!(r.register(&"x".repeat(65)).is_none());
        assert!(r.get("summarize").is_some());
        assert!(r.get("not-registered").is_none());
    }

    #[test]
    fn batching_by_count_or_time_and_sampling() {
        let mut t = Telemetry::with_policy(10_000, 5_000, 3, 0);
        let mut r = registry();
        let events = all_events(&mut r);
        assert!(t.offer(events[3].clone(), 10).is_none());
        assert!(t.offer(events[4].clone(), 20).is_none());
        let batch = t.offer(events[9].clone(), 30).expect("满 3 条即刷");
        assert_eq!(batch.events.len(), 3);
        assert!(t.offer(events[3].clone(), 40).is_none());
        assert!(t.flush_due(4_000).is_none(), "未到 5 s");
        let batch = t.flush_due(5_040).expect("到期刷");
        assert_eq!(batch.events.len(), 1);
        let json = batch.to_json(&BatchMeta {
            instance: "im-main".into(),
            sdk: "0.2.0".into(),
            tier: Tier::Flagship,
            os: Os::Ios,
            os_version: "27.0".into(),
        });
        let doc: Value = serde_json::from_str(&json).expect("json");
        assert_eq!(doc["schema"], 1);
        assert_eq!(doc["events"][0]["type"], "sched_wait");
        assert_eq!(doc["events"][0]["gate"], "charging");

        // 采样 1%：普通事件大多被丢，Error 永远保留。
        let mut t = Telemetry::with_policy(100, 5_000, 1_000, 0);
        for _ in 0..1_000 {
            t.offer(events[4].clone(), 0);
        }
        assert_eq!(t.pending(), 10, "1% 采样：1000 条留 10");
        assert_eq!(t.sampled_out, 990);
        t.offer(events[15].clone(), 0);
        assert_eq!(t.pending(), 11, "错误事件不采样");
        assert!(t.drain(1).is_some());
        assert_eq!(t.pending(), 0);
    }
}
