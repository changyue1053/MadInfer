//! 模型注册表：每个已加载模型一台驻留 FSM + 附属字段（refs / footprint / last_used / 适配器）。
//! 一基座规则：同 backend 的基座权重只允许一个 Serving。压力逐出顺序：空闲上下文 KV → Cooling 适配器 → Cooling 模型 → Critical 时丢全部 KV。

use std::collections::HashMap;

use madinfer_core_types::{AssetId, BackendId};
use madinfer_host::MemoryPressure;

use crate::context::ContextPool;
use crate::residency::{build_residency_fsm, LifeEffect, LifeEvent, LifeState};

/// 模型键：后端 × 权重 blob（基座或适配器）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelKey {
    /// 后端。
    pub backend: BackendId,
    /// 权重。
    pub weights: AssetId,
    /// 是否基座（false = 适配器 / 小任务模型）。
    pub is_backbone: bool,
}

struct Loaded {
    fsm: madinfer_fsm::Fsm<LifeState, LifeEvent, LifeEffect>,
    refs: u32,
    footprint_mb: u32,
    last_used_ms: u64,
    cool_generation: u64,
}

/// 运行时效果（worker 池执行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeEffect {
    /// 调后端加载（完成后 owner 回投 `on_loaded` / `on_load_failed`）。
    Load {
        /// 模型。
        key: ModelKey,
    },
    /// 登记冷却到期票。
    ArmCoolTimer {
        /// 模型。
        key: ModelKey,
        /// 代数。
        generation: u64,
        /// 到期时刻。
        at_ms: u64,
    },
    /// 调后端卸载。
    Unload {
        /// 模型。
        key: ModelKey,
    },
    /// 释放这些上下文的 KV。
    DropContexts {
        /// 上下文键。
        keys: Vec<madinfer_core_types::ContextKey>,
    },
    /// 取消全部后台 Job（Critical）。
    CancelBackgroundJobs,
}

/// 逐出报告（telemetry memory_pressure）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// 释放的 KV MB。
    pub evicted_kv_mb: u32,
    /// 卸载的模型数。
    pub unloaded_models: u32,
    /// 卸载的适配器数。
    pub unloaded_adapters: u32,
}

/// 台账。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStats {
    /// 加载完成。
    pub loads: u64,
    /// 加载失败。
    pub load_failures: u64,
    /// 卸载。
    pub unloads: u64,
    /// 压力逐出次数。
    pub pressure_evictions: u64,
    /// 陈旧冷却票。
    pub stale_cool_timers: u64,
    /// 一基座规则触发（第二个基座等待）。
    pub backbone_conflicts: u64,
}

/// 取用结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acquire {
    /// 已持有引用。
    Held,
    /// 未加载：owner 先 `begin_load`（或 `register`）再取。
    NotLoaded,
    /// 加载中：调用方等待 `on_loaded` 后再取。
    Loading,
    /// 同 backend 已有另一基座 Serving：等待或让其 Cooling → Unloading。
    BackboneBusy {
        /// 占着的基座。
        serving: ModelKey,
    },
    /// 模型已卸载（终态），需重新加载。
    Unloaded,
}

/// 模型注册表。
pub struct ModelRegistry {
    models: HashMap<ModelKey, Loaded>,
    cool_ms: u64,
    stats: RuntimeStats,
}

impl ModelRegistry {
    /// 建表；`cool_ms` 是引用归零后的冷却时长。
    pub fn new(cool_ms: u64) -> Self {
        Self {
            models: HashMap::new(),
            cool_ms,
            stats: RuntimeStats::default(),
        }
    }

    /// 台账。
    pub fn stats(&self) -> RuntimeStats {
        self.stats
    }

    /// 某模型状态。
    pub fn state(&self, key: &ModelKey) -> Option<LifeState> {
        self.models.get(key).map(|m| m.fsm.state())
    }

    /// 某模型引用数。
    pub fn refs(&self, key: &ModelKey) -> u32 {
        self.models.get(key).map_or(0, |m| m.refs)
    }

    /// 常驻总量 MB（Unloaded 之外）。
    pub fn resident_mb(&self) -> u32 {
        self.models
            .values()
            .filter(|m| m.fsm.state().resident())
            .map(|m| m.footprint_mb)
            .sum()
    }

    /// 开始加载：登记为 Loading，返回 `Load` 效果交 worker 执行；已登记（任一状态）则无效果。
    pub fn begin_load(&mut self, key: ModelKey, now_ms: u64) -> Vec<RuntimeEffect> {
        if self.models.contains_key(&key) {
            return Vec::new();
        }
        self.models.insert(
            key.clone(),
            Loaded {
                fsm: build_residency_fsm(),
                refs: 0,
                footprint_mb: 0,
                last_used_ms: now_ms,
                cool_generation: 0,
            },
        );
        vec![RuntimeEffect::Load { key }]
    }

    /// 后端加载完成（mmap 就位）：Loading → Mapped，记 footprint。非 Loading 态的回执被 FSM 拒绝并计数。
    pub fn on_loaded(&mut self, key: &ModelKey, footprint_mb: u32, now_ms: u64) -> bool {
        let Some(m) = self.models.get_mut(key) else {
            return false;
        };
        if !m.fsm.fire(&LifeEvent::Loaded, now_ms).accepted {
            return false;
        }
        m.footprint_mb = footprint_mb;
        m.last_used_ms = now_ms;
        self.stats.loads += 1;
        true
    }

    /// 后端加载失败：Loading → Unloaded（终态即移除；再次需要时重新 `begin_load`）。
    pub fn on_load_failed(&mut self, key: &ModelKey, now_ms: u64) -> bool {
        let Some(m) = self.models.get_mut(key) else {
            return false;
        };
        if !m.fsm.fire(&LifeEvent::LoadFailed, now_ms).accepted {
            return false;
        }
        self.stats.load_failures += 1;
        self.models.remove(key);
        true
    }

    /// 取消加载（加载途中压力逐出）：Loading → Unloading [Unload]；worker 完成后回投 `on_unload_finished`。
    pub fn cancel_load(&mut self, key: &ModelKey, now_ms: u64) -> Vec<RuntimeEffect> {
        let Some(m) = self.models.get_mut(key) else {
            return Vec::new();
        };
        if m.fsm.state() != LifeState::Loading {
            return Vec::new();
        }
        let t = m.fsm.fire(&LifeEvent::EvictRequested, now_ms);
        Self::unload_effects(key, &t.effects)
    }

    /// 后端已同步加载完成（加载不经 worker 时）：等价 `begin_load` + `on_loaded`，直接登记为 Mapped。
    pub fn register(&mut self, key: ModelKey, footprint_mb: u32, now_ms: u64) {
        let _ = self.begin_load(key.clone(), now_ms);
        self.on_loaded(&key, footprint_mb, now_ms);
    }

    /// 预热完成。
    pub fn mark_warm(&mut self, key: &ModelKey, now_ms: u64) {
        if let Some(m) = self.models.get_mut(key) {
            m.fsm.fire(&LifeEvent::Warmed, now_ms);
        }
    }

    /// 取用。
    pub fn acquire(&mut self, key: &ModelKey, now_ms: u64) -> (Acquire, Vec<RuntimeEffect>) {
        let Some(state) = self.state(key) else {
            return (Acquire::NotLoaded, Vec::new());
        };
        if state == LifeState::Loading {
            return (Acquire::Loading, Vec::new());
        }
        if state == LifeState::Unloaded || state == LifeState::Unloading {
            return (Acquire::Unloaded, Vec::new());
        }
        if key.is_backbone {
            if let Some(serving) = self.serving_backbone_of(&key.backend, key) {
                self.stats.backbone_conflicts += 1;
                return (Acquire::BackboneBusy { serving }, Vec::new());
            }
        }
        if let Some(m) = self.models.get_mut(key) {
            if m.refs == 0 {
                // Mapped 直接取用：先视为 Warmed（首次 prefill 就是预热）。
                if m.fsm.state() == LifeState::Mapped {
                    m.fsm.fire(&LifeEvent::Warmed, now_ms);
                }
                let t = m.fsm.fire(&LifeEvent::Acquired, now_ms);
                if t.effects.contains(&LifeEffect::CancelCoolTimer) {
                    m.cool_generation += 1; // 在途到期票哑火
                }
            }
            m.refs += 1;
            m.last_used_ms = now_ms;
        }
        // 取用本身无 IO 效果（加载由 owner 在 register 前完成）；返回空效果列表保持签名与 release 对称。
        (Acquire::Held, Vec::new())
    }

    /// 释放；最后一个引用归零进 Cooling。
    pub fn release(&mut self, key: &ModelKey, now_ms: u64) -> Vec<RuntimeEffect> {
        let cool_ms = self.cool_ms;
        let Some(m) = self.models.get_mut(key) else {
            return Vec::new();
        };
        if m.refs == 0 {
            return Vec::new();
        }
        m.refs -= 1;
        m.last_used_ms = now_ms;
        if m.refs > 0 {
            return Vec::new();
        }
        let t = m.fsm.fire(&LifeEvent::LastReleased { cool_ms }, now_ms);
        let mut out = Vec::new();
        for fx in t.effects {
            if let LifeEffect::ArmCoolTimer { cool_ms } = fx {
                m.cool_generation += 1;
                out.push(RuntimeEffect::ArmCoolTimer {
                    key: key.clone(),
                    generation: m.cool_generation,
                    at_ms: now_ms.saturating_add(cool_ms),
                });
            }
        }
        out
    }

    /// 冷却到期票。
    pub fn on_cool_elapsed(
        &mut self,
        key: &ModelKey,
        generation: u64,
        now_ms: u64,
    ) -> Vec<RuntimeEffect> {
        let Some(m) = self.models.get_mut(key) else {
            self.stats.stale_cool_timers += 1;
            return Vec::new();
        };
        if m.cool_generation != generation || m.fsm.state() != LifeState::Cooling {
            self.stats.stale_cool_timers += 1;
            return Vec::new();
        }
        let t = m.fsm.fire(&LifeEvent::CoolElapsed, now_ms);
        Self::unload_effects(key, &t.effects)
    }

    /// 后端卸载完成。
    pub fn on_unload_finished(&mut self, key: &ModelKey, now_ms: u64) {
        if let Some(m) = self.models.get_mut(key) {
            if m.fsm.fire(&LifeEvent::UnloadFinished, now_ms).accepted {
                self.stats.unloads += 1;
            }
        }
        // 终态即移除；再次需要时重新 register。
        if self.state(key) == Some(LifeState::Unloaded) {
            self.models.remove(key);
        }
    }

    /// 压力逐出（设计文档 C5 ②）：空闲上下文 KV → Cooling 适配器 → Cooling 模型 → Critical 时取消后台 Job 并丢全部 KV。
    pub fn evict_for(
        &mut self,
        level: MemoryPressure,
        target_mb: u32,
        sessions: &mut ContextPool,
        now_ms: u64,
    ) -> (EvictReport, Vec<RuntimeEffect>) {
        self.stats.pressure_evictions += 1;
        let mut report = EvictReport::default();
        let mut effects = Vec::new();
        if level == MemoryPressure::Normal {
            return (report, effects);
        }
        // ① 空闲上下文 KV
        let (keys, freed) = sessions.evict_idle(target_mb);
        report.evicted_kv_mb += freed;
        if !keys.is_empty() {
            effects.push(RuntimeEffect::DropContexts { keys });
        }
        let mut remaining = target_mb.saturating_sub(freed);
        // ② Cooling 适配器 → ③ Cooling 模型（各自按 LRU）
        for want_base in [false, true] {
            if remaining == 0 && level != MemoryPressure::Critical {
                break;
            }
            let mut cooling: Vec<(u64, ModelKey, u32)> = self
                .models
                .iter()
                .filter(|(k, m)| k.is_backbone == want_base && m.fsm.state() == LifeState::Cooling)
                .map(|(k, m)| (m.last_used_ms, k.clone(), m.footprint_mb))
                .collect();
            // LRU：最久未用先卸；同时刻按权重 hash 稳定排序（确定性）。
            cooling.sort_by(|a, b| (a.0, a.1.weights.bytes()).cmp(&(b.0, b.1.weights.bytes())));
            for (_, key, footprint) in cooling {
                if remaining == 0 && level != MemoryPressure::Critical {
                    break;
                }
                if let Some(m) = self.models.get_mut(&key) {
                    let t = m.fsm.fire(&LifeEvent::EvictRequested, now_ms);
                    if t.accepted {
                        m.cool_generation += 1;
                        effects.extend(Self::unload_effects(&key, &t.effects));
                        if want_base {
                            report.unloaded_models += 1;
                        } else {
                            report.unloaded_adapters += 1;
                        }
                        remaining = remaining.saturating_sub(footprint);
                    }
                }
            }
        }
        // ④ Critical：取消后台 Job、丢全部空闲 KV（含刚才没够到的）
        if level == MemoryPressure::Critical {
            effects.push(RuntimeEffect::CancelBackgroundJobs);
            let keys = sessions.drop_all_idle();
            if !keys.is_empty() {
                effects.push(RuntimeEffect::DropContexts { keys });
            }
        }
        (report, effects)
    }

    fn serving_backbone_of(&self, backend: &BackendId, except: &ModelKey) -> Option<ModelKey> {
        self.models
            .iter()
            .find(|(k, m)| {
                k.is_backbone
                    && &k.backend == backend
                    && *k != except
                    && m.fsm.state() == LifeState::Serving
            })
            .map(|(k, _)| k.clone())
    }

    fn unload_effects(key: &ModelKey, effects: &[LifeEffect]) -> Vec<RuntimeEffect> {
        effects
            .iter()
            .filter_map(|fx| match fx {
                LifeEffect::Unload => Some(RuntimeEffect::Unload { key: key.clone() }),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::context::KvBudget;
    use madinfer_core_types::ContextKey;

    fn key(backend: &str, hex_byte: &str, is_backbone: bool) -> ModelKey {
        ModelKey {
            backend: BackendId(backend.to_owned()),
            weights: AssetId::parse(&format!("sha256:{}", hex_byte.repeat(32))).expect("id"),
            is_backbone,
        }
    }

    #[test]
    fn one_base_per_backend_and_cooling_with_generation() {
        let mut reg = ModelRegistry::new(30_000);
        let a = key("llama.cpp", "aa", true);
        let b = key("llama.cpp", "bb", true);
        reg.register(a.clone(), 1_900, 0);
        reg.register(b.clone(), 1_500, 0);
        assert_eq!(reg.acquire(&a, 1).0, Acquire::Held);
        assert_eq!(reg.acquire(&a, 2).0, Acquire::Held, "同基座第二个引用");
        assert_eq!(reg.refs(&a), 2);
        assert_eq!(
            reg.acquire(&b, 3).0,
            Acquire::BackboneBusy { serving: a.clone() },
            "一基座规则"
        );
        assert!(reg.release(&a, 4).is_empty(), "refs 2 → 1 无效果");
        let fx = reg.release(&a, 5);
        assert_eq!(
            fx,
            vec![RuntimeEffect::ArmCoolTimer {
                key: a.clone(),
                generation: 1,
                at_ms: 30_005
            }]
        );
        assert_eq!(reg.state(&a), Some(LifeState::Cooling));
        assert_eq!(
            reg.acquire(&b, 6).0,
            Acquire::Held,
            "a 进 Cooling 后 b 可 Serving"
        );
        // a 在 Cooling 期被重新取用：旧票哑火。
        assert_eq!(
            reg.acquire(&a, 7).0,
            Acquire::BackboneBusy { serving: b.clone() }
        );
        reg.release(&b, 8);
        assert_eq!(reg.acquire(&a, 9).0, Acquire::Held);
        assert!(
            reg.on_cool_elapsed(&a, 1, 30_005).is_empty(),
            "代数 1 的票已作废"
        );
        assert_eq!(reg.stats().stale_cool_timers, 1);
        reg.release(&a, 10);
        let fx = reg.on_cool_elapsed(&a, 3, 30_010);
        assert_eq!(fx, vec![RuntimeEffect::Unload { key: a.clone() }]);
        reg.on_unload_finished(&a, 30_020);
        assert_eq!(reg.state(&a), None, "终态即移除");
        assert_eq!(reg.stats().unloads, 1);
        assert_eq!(reg.resident_mb(), 1_500);
    }

    #[test]
    fn loading_defers_acquire_until_loaded_and_failure_removes_entry() {
        let mut reg = ModelRegistry::new(30_000);
        let a = key("llama.cpp", "aa", true);
        let fx = reg.begin_load(a.clone(), 1);
        assert_eq!(fx, vec![RuntimeEffect::Load { key: a.clone() }]);
        assert_eq!(reg.state(&a), Some(LifeState::Loading));
        assert!(
            reg.begin_load(a.clone(), 2).is_empty(),
            "重复 begin_load 无效果"
        );
        assert_eq!(
            reg.acquire(&a, 2).0,
            Acquire::Loading,
            "加载中取用 → 调用方等待"
        );
        assert_eq!(reg.resident_mb(), 0, "加载中 footprint 未知计 0");
        assert!(reg.on_loaded(&a, 1_900, 3));
        assert!(!reg.on_loaded(&a, 1_900, 3), "Mapped 再收 Loaded 被拒");
        assert_eq!(reg.state(&a), Some(LifeState::Mapped));
        assert_eq!(reg.resident_mb(), 1_900);
        assert_eq!(reg.acquire(&a, 4).0, Acquire::Held);
        assert_eq!(reg.stats().loads, 1);

        let b = key("llama.cpp", "bb", false);
        reg.begin_load(b.clone(), 5);
        assert!(reg.on_load_failed(&b, 9));
        assert_eq!(reg.state(&b), None, "失败即移除");
        assert_eq!(reg.acquire(&b, 10).0, Acquire::NotLoaded);
        assert_eq!(reg.stats().load_failures, 1);
        assert!(!reg.on_load_failed(&b, 11), "不存在的键忽略");

        let c = key("llama.cpp", "cc", false);
        reg.begin_load(c.clone(), 12);
        let fx = reg.cancel_load(&c, 13);
        assert_eq!(fx, vec![RuntimeEffect::Unload { key: c.clone() }]);
        assert_eq!(reg.state(&c), Some(LifeState::Unloading));
        assert_eq!(reg.acquire(&c, 14).0, Acquire::Unloaded);
        assert!(
            reg.cancel_load(&a, 15).is_empty(),
            "非 Loading 态不可取消加载"
        );
        reg.on_unload_finished(&c, 16);
        assert_eq!(reg.state(&c), None);
    }

    #[test]
    fn eviction_order_sessions_then_adapters_then_models_then_critical() {
        let mut reg = ModelRegistry::new(30_000);
        let base = key("llama.cpp", "aa", true);
        let adapter = key("llama.cpp", "cc", false);
        let serving = key("llama.cpp", "dd", false);
        reg.register(base.clone(), 1_900, 0);
        reg.register(adapter.clone(), 30, 0);
        reg.register(serving.clone(), 40, 0);
        for k in [&base, &adapter, &serving] {
            reg.acquire(k, 1);
        }
        reg.release(&base, 2);
        reg.release(&adapter, 3);
        let mut pool = ContextPool::new(KvBudget::new(512));
        pool.reserve(&ContextKey::new("conv:1").expect("k"), 96, 0, 1);
        pool.release(&ContextKey::new("conv:1").expect("k"), 2);
        pool.reserve(&ContextKey::new("conv:2").expect("k"), 64, 0, 3);

        let (report, fx) = reg.evict_for(MemoryPressure::Warning, 100, &mut pool, 10);
        assert_eq!(report.evicted_kv_mb, 96, "先回收空闲上下文 KV");
        assert_eq!(report.unloaded_adapters, 1, "不够再卸 Cooling 适配器");
        assert_eq!(report.unloaded_models, 0, "够了就不碰基座");
        assert_eq!(reg.state(&adapter), Some(LifeState::Unloading));
        assert_eq!(
            reg.state(&serving),
            Some(LifeState::Serving),
            "Serving 永不被逐出"
        );
        assert!(fx
            .iter()
            .any(|e| matches!(e, RuntimeEffect::Unload { key } if *key == adapter)));

        let (report, fx) = reg.evict_for(MemoryPressure::Critical, 0, &mut pool, 11);
        assert_eq!(report.unloaded_models, 1, "Critical：Cooling 基座也卸");
        assert!(fx.contains(&RuntimeEffect::CancelBackgroundJobs));
        assert_eq!(pool.len(), 1, "在用上下文 conv:2 保留");
        assert_eq!(reg.stats().pressure_evictions, 2);
    }
}
