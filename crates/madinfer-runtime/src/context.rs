//! 上下文池（生成上下文 = KV 缓存 + 适配器亲和，词表提案 Context，与 IM 的 Conversation 区分）：亲和 worker、KV 预算、LRU 回收。上下文只在其亲和 worker 上被调用；KV 已用不超过预算（不变量）。
//!
//! 按前缀丢弃（madai-w14 I-C-12，设计稿 R-8 / T6）：账户关闭时宿主以 `acct:<id>/` 前缀调 [`ContextPool::drop_prefix`]，命中的键立即离池——
//! 之后同键 `reserve` 必是 [`Reserve::Created`]、`hits` 不增；在飞条目的 KV 记账延迟到其 `release`（任务终态）才结清，且不写回。
//! madai-w15 I-C-13：[`ContextPool::drop_prefix_keys`] 回被丢的键给 owner（worker 释放后端 KV）；单键 [`ContextPool::drop_context`]
//! 的在飞口径与前缀口径统一（延迟结清）。owner（madinfer-api 核 actor）把 `reserve` / `release` 挂在 Job 生命周期上。

use std::collections::HashMap;

use madinfer_core_types::ContextKey;

/// KV 预算（config.v1 thresholds.kv_budget_mb[tier]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvBudget {
    /// 上限 MB（0 = 本档不准留 KV，每次请求重算）。
    pub limit_mb: u32,
    /// 已用 MB。
    pub used_mb: u32,
}

impl KvBudget {
    /// 空账。
    pub const fn new(limit_mb: u32) -> Self {
        Self {
            limit_mb,
            used_mb: 0,
        }
    }

    /// 剩余。
    pub const fn free_mb(&self) -> u32 {
        self.limit_mb.saturating_sub(self.used_mb)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    worker: u8,
    kv_mb: u32,
    last_used_ms: u64,
    in_use: bool,
}

/// 台账。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextPoolStats {
    /// 命中（同键复用）。
    pub hits: u64,
    /// 新建。
    pub misses: u64,
    /// 为腾预算回收的上下文数。
    pub evicted_for_budget: u64,
    /// 压力回收的上下文数。
    pub evicted_for_pressure: u64,
    /// 因预算不足被拒的预留。
    pub rejected: u64,
    /// 业务按前缀丢弃的上下文数（[`ContextPool::drop_prefix`]；含在飞条目——其 KV 记账延迟到 `release` 结清）。
    pub dropped: u64,
}

/// 在飞条目被 [`ContextPool::drop_prefix`] 命中后的 KV 记账：键已离池（不可复用），预算到 `release`（任务终态）才结清。
#[derive(Clone, Debug, PartialEq, Eq)]
struct DeferredKv {
    key: ContextKey,
    kv_mb: u32,
}

/// 上下文池。
#[derive(Debug)]
pub struct ContextPool {
    budget: KvBudget,
    entries: HashMap<ContextKey, Entry>,
    deferred: Vec<DeferredKv>,
    stats: ContextPoolStats,
}

/// 预留结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reserve {
    /// 复用已有上下文（亲和 worker）。
    Reused {
        /// worker。
        worker: u8,
    },
    /// 新建（已回收的上下文键随附，owner 释放其 KV）。
    Created {
        /// worker。
        worker: u8,
        /// 为腾预算回收的上下文。
        evicted: Vec<ContextKey>,
    },
    /// 预算不足且无可回收（→ 1003 MemoryPressure 或按无 KV 模式跑）。
    OverBudget,
}

impl ContextPool {
    /// 建池。
    pub fn new(budget: KvBudget) -> Self {
        Self {
            budget,
            entries: HashMap::new(),
            deferred: Vec::new(),
            stats: ContextPoolStats::default(),
        }
    }

    /// 预算。
    pub fn budget(&self) -> KvBudget {
        self.budget
    }

    /// 台账。
    pub fn stats(&self) -> ContextPoolStats {
        self.stats
    }

    /// 上下文数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 被 [`Self::drop_prefix`] 在飞命中、尚未 `release` 结清 KV 记账的条目数（键已不可复用，预算仍计已用）。
    pub fn deferred_len(&self) -> usize {
        self.deferred.len()
    }

    /// 换预算（配置热更 / 档位变化）；超出部分由下一次 `evict_idle` 回收。
    pub fn set_limit(&mut self, limit_mb: u32) {
        self.budget.limit_mb = limit_mb;
    }

    /// 预留一个上下文：同键复用；否则在预算内新建，预算不足先 LRU 回收空闲上下文。`pick_worker` 由调用方给（按槽 / 亲和策略）。
    pub fn reserve(
        &mut self,
        key: &ContextKey,
        kv_mb: u32,
        pick_worker: u8,
        now_ms: u64,
    ) -> Reserve {
        if let Some(e) = self.entries.get_mut(key) {
            e.last_used_ms = now_ms;
            e.in_use = true;
            self.stats.hits += 1;
            return Reserve::Reused { worker: e.worker };
        }
        if kv_mb > self.budget.limit_mb {
            self.stats.rejected += 1;
            return Reserve::OverBudget;
        }
        let mut evicted = Vec::new();
        while self.budget.free_mb() < kv_mb {
            let Some(victim) = self.lru_idle() else {
                self.stats.rejected += 1;
                // 回收到一半也不够：把已回收的还回去是不可能的（KV 已释放），如实返回 OverBudget，owner 走无 KV 模式。
                return Reserve::OverBudget;
            };
            if let Some(e) = self.entries.remove(&victim) {
                self.budget.used_mb = self.budget.used_mb.saturating_sub(e.kv_mb);
            }
            self.stats.evicted_for_budget += 1;
            evicted.push(victim);
        }
        self.budget.used_mb += kv_mb;
        self.entries.insert(
            key.clone(),
            Entry {
                worker: pick_worker,
                kv_mb,
                last_used_ms: now_ms,
                in_use: true,
            },
        );
        self.stats.misses += 1;
        Reserve::Created {
            worker: pick_worker,
            evicted,
        }
    }

    /// 一次生成结束：上下文回到空闲（KV 保留，可被回收）。键若已被 [`Self::drop_prefix`] 在飞命中，则在此结清其 KV 记账、
    /// 不写回（同键多条延迟记账按先后各结一条；延迟期间同键被重新预留的新条目由后到的 `release` 置闲——两次 `release`
    /// 都到齐后台账与实际一致，中间只会把新条目多标一段 `in_use`，偏保守：不会被误回收）。
    pub fn release(&mut self, key: &ContextKey, now_ms: u64) {
        if let Some(i) = self.deferred.iter().position(|d| d.key == *key) {
            let settled = self.deferred.remove(i);
            self.budget.used_mb = self.budget.used_mb.saturating_sub(settled.kv_mb);
            return;
        }
        if let Some(e) = self.entries.get_mut(key) {
            e.in_use = false;
            e.last_used_ms = now_ms;
        }
    }

    /// 业务按前缀丢弃上下文（账户关闭：前缀 `acct:<id>/`，设计稿 R-8；madai-w14 I-C-12）：键 `starts_with(prefix)` 的全部
    /// 条目立即离池——之后同键 [`Self::reserve`] 必是 [`Reserve::Created`]、`hits` 不增（T6 核侧半边）。空闲条目的 KV 立即
    /// 结清；在飞条目（`in_use`）的 KV 记账延迟到其 [`Self::release`]（任务终态）才结清——worker 仍持有该后端上下文，预算
    /// 提前放会让"已用 ≤ 预算"在实际内存上失真；`release` 对已离池的键不写回。返回丢弃条数（含在飞）。
    /// 空前缀不含账户段（`ContextKey` 禁无账户段），按 0 处理、不动池；不存在的前缀回 0。
    /// 只要条数的调用方用本方法；要拿回被丢的键（让 worker 释放后端 KV）用 [`Self::drop_prefix_keys`]，两者同一实现。
    pub fn drop_prefix(&mut self, prefix: &str) -> u32 {
        u32::try_from(self.drop_prefix_keys(prefix).len()).unwrap_or(u32::MAX)
    }

    /// 同 [`Self::drop_prefix`]，返回被丢弃的键（含在飞；madai-w15 I-C-13，I-C-12 §7 提案）：owner 据此让 worker 释放
    /// 后端 KV（在飞条目的键也在其中——worker 邮箱 FIFO 使释放自然排在当前生成之后）。键按字典序返回（`HashMap` 无序，
    /// 给 owner 与用例一个确定的序）。空前缀 / 无匹配回空。
    pub fn drop_prefix_keys(&mut self, prefix: &str) -> Vec<ContextKey> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let mut victims: Vec<ContextKey> = self
            .entries
            .keys()
            .filter(|k| k.as_str().starts_with(prefix))
            .cloned()
            .collect();
        victims.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut dropped = Vec::with_capacity(victims.len());
        for key in victims {
            if self.detach(&key) {
                dropped.push(key);
            }
        }
        self.stats.dropped += u64::try_from(dropped.len()).unwrap_or(u64::MAX);
        dropped
    }

    /// 业务显式丢弃单个上下文：键立即离池；在飞条目的 KV 记账同 [`Self::drop_prefix`] 延迟到 `release` 结清
    /// （madai-w15 I-C-13 统一口径，I-C-12 §6 残项 2——此前单键口径立即结清，与前缀口径不一）。不计 `dropped`（那是按前缀丢弃的台账）。
    pub fn drop_context(&mut self, key: &ContextKey) -> bool {
        self.detach(key)
    }

    /// 让一个键离池：空闲条目 KV 立即结清；在飞条目转入延迟记账（到 `release` 结清、不写回）。键不存在回 false。
    fn detach(&mut self, key: &ContextKey) -> bool {
        let Some(e) = self.entries.remove(key) else {
            return false;
        };
        if e.in_use {
            self.deferred.push(DeferredKv {
                key: key.clone(),
                kv_mb: e.kv_mb,
            });
        } else {
            self.budget.used_mb = self.budget.used_mb.saturating_sub(e.kv_mb);
        }
        true
    }

    /// 压力回收：按 LRU 回收空闲上下文直到释放 ≥ `target_mb`（或无可回收）；返回被回收的键与实际释放量。
    pub fn evict_idle(&mut self, target_mb: u32) -> (Vec<ContextKey>, u32) {
        let mut freed = 0u32;
        let mut keys = Vec::new();
        while freed < target_mb {
            let Some(victim) = self.lru_idle() else {
                break;
            };
            if let Some(e) = self.entries.remove(&victim) {
                freed += e.kv_mb;
                self.budget.used_mb = self.budget.used_mb.saturating_sub(e.kv_mb);
                self.stats.evicted_for_pressure += 1;
                keys.push(victim);
            }
        }
        (keys, freed)
    }

    /// 丢全部空闲上下文（Critical）。
    pub fn drop_all_idle(&mut self) -> Vec<ContextKey> {
        self.evict_idle(u32::MAX).0
    }

    /// 某上下文的亲和 worker。
    pub fn worker_of(&self, key: &ContextKey) -> Option<u8> {
        self.entries.get(key).map(|e| e.worker)
    }

    fn lru_idle(&self) -> Option<ContextKey> {
        self.entries
            .iter()
            .filter(|(_, e)| !e.in_use)
            .min_by_key(|(k, e)| (e.last_used_ms, k.as_str().to_owned()))
            .map(|(k, _)| k.clone())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn key(s: &str) -> ContextKey {
        ContextKey::new(s).expect("key")
    }

    #[test]
    fn reuse_hits_and_budget_evicts_lru_idle() {
        let mut pool = ContextPool::new(KvBudget::new(256));
        assert!(matches!(
            pool.reserve(&key("conv:1"), 100, 0, 10),
            Reserve::Created { worker: 0, .. }
        ));
        assert!(matches!(
            pool.reserve(&key("conv:2"), 100, 1, 20),
            Reserve::Created { worker: 1, .. }
        ));
        pool.release(&key("conv:1"), 30);
        pool.release(&key("conv:2"), 40);
        assert_eq!(
            pool.reserve(&key("conv:1"), 100, 3, 50),
            Reserve::Reused { worker: 0 },
            "同键复用且亲和 worker 不变"
        );
        // 已用 200，新上下文 100 → 需回收：conv:2 空闲且 LRU（conv:1 在用）。
        match pool.reserve(&key("conv:3"), 100, 2, 60) {
            Reserve::Created { evicted, .. } => assert_eq!(evicted, vec![key("conv:2")]),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(pool.budget().used_mb, 200);
        assert_eq!(pool.stats().hits, 1);
        assert_eq!(pool.stats().evicted_for_budget, 1);
        // 再来一个 100：只剩 conv:1（在用）与 conv:3（在用）→ OverBudget。
        assert_eq!(
            pool.reserve(&key("conv:4"), 100, 0, 70),
            Reserve::OverBudget
        );
        assert_eq!(pool.stats().rejected, 1);
        assert!(
            pool.budget().used_mb <= pool.budget().limit_mb,
            "不变量：已用 ≤ 预算"
        );
    }

    #[test]
    fn pressure_eviction_frees_idle_only() {
        let mut pool = ContextPool::new(KvBudget::new(512));
        pool.reserve(&key("a"), 100, 0, 1);
        pool.reserve(&key("b"), 100, 0, 2);
        pool.reserve(&key("c"), 100, 0, 3);
        pool.release(&key("a"), 4);
        pool.release(&key("c"), 5);
        let (keys, freed) = pool.evict_idle(150);
        assert_eq!(keys, vec![key("a"), key("c")], "按 LRU 回收空闲直到 ≥ 150");
        assert_eq!(freed, 200);
        assert_eq!(pool.len(), 1, "在用的 b 不动");
        assert_eq!(pool.worker_of(&key("b")), Some(0));
        // 单键丢弃在飞条目：键立即离池，KV 记账延迟到 release（madai-w15 I-C-13 与前缀口径统一）。
        assert!(pool.drop_context(&key("b")));
        assert!(pool.is_empty());
        assert_eq!(pool.deferred_len(), 1);
        assert_eq!(pool.budget().used_mb, 100, "在飞：终态前预算不放");
        assert!(!pool.drop_context(&key("b")), "已离池：false");
        pool.release(&key("b"), 6);
        assert_eq!(pool.deferred_len(), 0);
        assert_eq!(pool.budget().used_mb, 0, "终态结清、不写回");
        assert_eq!(pool.stats().dropped, 0, "单键丢弃不计按前缀丢弃台账");
    }

    #[test]
    fn drop_prefix_keys_returns_sorted_victims_including_in_flight_and_counts_once() {
        let mut pool = ContextPool::new(KvBudget::new(512));
        pool.reserve(&key("acct:a/conv:2"), 100, 0, 1); // 在飞
        pool.reserve(&key("acct:a/conv:1"), 100, 1, 2);
        pool.reserve(&key("acct:b/conv:1"), 100, 2, 3);
        pool.release(&key("acct:a/conv:1"), 4);
        assert_eq!(
            pool.drop_prefix_keys("acct:a/"),
            vec![key("acct:a/conv:1"), key("acct:a/conv:2")],
            "字典序、含在飞"
        );
        assert_eq!(pool.stats().dropped, 2);
        assert_eq!(pool.deferred_len(), 1);
        assert_eq!(pool.budget().used_mb, 200, "空闲 100 结清、在飞 100 待终态");
        assert!(pool.drop_prefix_keys("acct:a/").is_empty(), "重复丢弃：空");
        assert!(pool.drop_prefix_keys("").is_empty(), "空前缀：空");
        assert_eq!(pool.stats().dropped, 2, "空结果不计数");
        assert_eq!(pool.drop_prefix("acct:b/"), 1, "条数口径同一实现");
        assert_eq!(pool.stats().dropped, 3);
        assert!(pool.is_empty());
    }

    #[test]
    fn drop_prefix_makes_same_key_reserve_created_and_hits_flat() {
        // T6 核侧半边：A 关户后 B 以同一会话 id 进来，不得命中 A 的上下文。
        let mut pool = ContextPool::new(KvBudget::new(512));
        pool.reserve(&key("acct:a/conv:1"), 100, 0, 1);
        pool.reserve(&key("acct:a/conv:2"), 100, 1, 2);
        pool.reserve(&key("acct:b/conv:1"), 100, 2, 3);
        for k in ["acct:a/conv:1", "acct:a/conv:2", "acct:b/conv:1"] {
            pool.release(&key(k), 4);
        }
        assert_eq!(
            pool.reserve(&key("acct:a/conv:1"), 100, 3, 5),
            Reserve::Reused { worker: 0 }
        );
        pool.release(&key("acct:a/conv:1"), 6);
        assert_eq!(pool.stats().hits, 1);
        assert_eq!(pool.budget().used_mb, 300);

        assert_eq!(pool.drop_prefix("acct:a/"), 2, "A 的两个上下文都离池");
        assert_eq!(pool.stats().dropped, 2);
        assert_eq!(pool.len(), 1, "只剩 B");
        assert_eq!(pool.deferred_len(), 0, "都是空闲条目：KV 立即结清");
        assert_eq!(pool.budget().used_mb, 100);
        assert_eq!(pool.worker_of(&key("acct:a/conv:1")), None);

        assert!(
            matches!(
                pool.reserve(&key("acct:a/conv:1"), 100, 5, 7),
                Reserve::Created { worker: 5, .. }
            ),
            "drop 后同键预留必是新建"
        );
        assert_eq!(pool.stats().hits, 1, "hits 不增");
        assert_eq!(pool.stats().misses, 4);
        assert_eq!(
            pool.reserve(&key("acct:b/conv:1"), 100, 9, 8),
            Reserve::Reused { worker: 2 },
            "别的账户不受影响"
        );
        assert_eq!(pool.stats().hits, 2);
    }

    #[test]
    fn drop_prefix_defers_in_flight_kv_until_release_and_never_writes_back() {
        let mut pool = ContextPool::new(KvBudget::new(512));
        pool.reserve(&key("acct:a/conv:1"), 100, 0, 1); // 在飞
        pool.reserve(&key("acct:a/conv:2"), 50, 0, 2);
        pool.release(&key("acct:a/conv:2"), 3); // 空闲
        assert_eq!(pool.drop_prefix("acct:a/"), 2, "在飞与空闲都计入");
        assert_eq!(pool.len(), 0, "两键都已离池");
        assert_eq!(pool.deferred_len(), 1, "在飞条目延迟结清");
        assert_eq!(
            pool.budget().used_mb,
            100,
            "空闲 50 结清、在飞 100 仍计已用"
        );
        assert!(
            pool.evict_idle(u32::MAX).0.is_empty(),
            "延迟条目不是空闲上下文，压力回收不碰"
        );

        // 延迟期间同键被新账户重新预留：新建，且不复用旧 KV。
        assert!(matches!(
            pool.reserve(&key("acct:a/conv:1"), 100, 3, 4),
            Reserve::Created { worker: 3, .. }
        ));
        assert_eq!(pool.stats().hits, 0);
        assert_eq!(pool.budget().used_mb, 200);

        // 旧任务终态：结清延迟记账、不写回；新条目仍在用（保守：不会被回收）。
        pool.release(&key("acct:a/conv:1"), 5);
        assert_eq!(pool.deferred_len(), 0);
        assert_eq!(pool.budget().used_mb, 100);
        assert_eq!(pool.len(), 1);
        assert!(pool.evict_idle(u32::MAX).0.is_empty(), "新条目仍标在用");
        // 新任务终态：新条目置闲，可被回收。
        pool.release(&key("acct:a/conv:1"), 6);
        assert_eq!(pool.evict_idle(u32::MAX).0, vec![key("acct:a/conv:1")]);
        assert_eq!(pool.budget().used_mb, 0, "不变量：全部结清后已用归零");
        assert_eq!(pool.stats().dropped, 2);
    }

    #[test]
    fn drop_prefix_in_flight_without_re_reserve_settles_on_release_only() {
        let mut pool = ContextPool::new(KvBudget::new(256));
        pool.reserve(&key("acct:a/conv:1"), 100, 0, 1);
        assert_eq!(pool.drop_prefix("acct:a/"), 1);
        assert_eq!(pool.budget().used_mb, 100, "终态前预算不放");
        assert_eq!(pool.drop_prefix("acct:a/"), 0, "重复 drop 不重复计数");
        pool.release(&key("acct:a/conv:1"), 2);
        assert!(pool.is_empty(), "终态不写回");
        assert_eq!(pool.deferred_len(), 0);
        assert_eq!(pool.budget().used_mb, 0);
        assert_eq!(pool.stats().dropped, 1);
    }

    #[test]
    fn drop_prefix_ignores_empty_and_unknown_prefixes() {
        let mut pool = ContextPool::new(KvBudget::new(512));
        pool.reserve(&key("acct:a/conv:1"), 100, 0, 1);
        pool.reserve(&key("acct:b/conv:1"), 100, 1, 2);
        assert_eq!(pool.drop_prefix(""), 0, "空前缀不含账户段：不动池");
        assert_eq!(pool.drop_prefix("acct:zz/"), 0, "不存在的前缀");
        assert_eq!(
            pool.drop_prefix("acct:a/conv:10"),
            0,
            "比键更长的前缀不匹配"
        );
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.budget().used_mb, 200);
        assert_eq!(pool.stats().dropped, 0);
    }
}
