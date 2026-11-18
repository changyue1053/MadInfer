//! 业务配额（设计文档 C4 关键机制 ③）：成本 = 运行 ms × 峰值 MB，按业务滑动窗口累计；超额降一档，再超额 Reject(QuotaExceeded)。

use std::collections::{HashMap, VecDeque};

use madinfer_core_types::{BusinessId, QoS};

/// 一条配额：窗口与预算（config.v1 thresholds.quotas）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    /// 窗口毫秒。
    pub window_ms: u64,
    /// 窗口内成本预算（ms × MB）。
    pub budget_cost: u64,
}

/// 配额表：按业务查，缺省项兜底；无缺省且未登记 = 不限。
#[derive(Clone, Debug, Default)]
pub struct QuotaTable {
    per_business: HashMap<BusinessId, Quota>,
    default: Option<Quota>,
}

impl QuotaTable {
    /// 建表。
    pub fn new(
        per_business: impl IntoIterator<Item = (BusinessId, Quota)>,
        default: Option<Quota>,
    ) -> Self {
        Self {
            per_business: per_business.into_iter().collect(),
            default,
        }
    }

    /// 查配额。
    pub fn quota_for(&self, business: BusinessId) -> Option<Quota> {
        self.per_business.get(&business).copied().or(self.default)
    }
}

/// 配额裁决。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaVerdict {
    /// 预算内。
    Within,
    /// 本次会越过预算：降一档执行。
    Downgrade,
    /// 已超额：拒绝。
    Exceeded,
}

/// 滑动窗口台账。
#[derive(Clone, Debug, Default)]
pub struct QuotaLedger {
    table: QuotaTable,
    charges: HashMap<BusinessId, VecDeque<(u64, u64)>>,
    /// 降档次数（观测）。
    pub downgrade_count: u64,
    /// 拒绝次数（观测）。
    pub reject_count: u64,
}

impl QuotaLedger {
    /// 以配额表建账。
    pub fn new(table: QuotaTable) -> Self {
        Self {
            table,
            ..Self::default()
        }
    }

    /// 换配额表（配置热更）；历史记账保留。
    pub fn replace_table(&mut self, table: QuotaTable) {
        self.table = table;
    }

    /// 记账（Job 完成时按实际 run_ms × peak_memory_mb）。
    pub fn charge(&mut self, business: BusinessId, cost: u64, now_ms: u64) {
        self.charges
            .entry(business)
            .or_default()
            .push_back((now_ms, cost));
        self.prune(business, now_ms);
    }

    /// 窗口内已用成本。
    pub fn usage(&mut self, business: BusinessId, now_ms: u64) -> u64 {
        self.prune(business, now_ms);
        self.charges
            .get(&business)
            .map_or(0, |q| q.iter().map(|(_, c)| *c).sum())
    }

    /// 裁决：未登记且无缺省 = Within。
    pub fn verdict(
        &mut self,
        business: BusinessId,
        estimate_cost: u64,
        now_ms: u64,
    ) -> QuotaVerdict {
        let Some(quota) = self.table.quota_for(business) else {
            return QuotaVerdict::Within;
        };
        let used = self.usage(business, now_ms);
        if used > quota.budget_cost {
            self.reject_count += 1;
            QuotaVerdict::Exceeded
        } else if used.saturating_add(estimate_cost) > quota.budget_cost {
            self.downgrade_count += 1;
            QuotaVerdict::Downgrade
        } else {
            QuotaVerdict::Within
        }
    }

    fn prune(&mut self, business: BusinessId, now_ms: u64) {
        let Some(quota) = self.table.quota_for(business) else {
            return;
        };
        if let Some(q) = self.charges.get_mut(&business) {
            let floor = now_ms.saturating_sub(quota.window_ms);
            while q.front().is_some_and(|(at, _)| *at < floor) {
                q.pop_front();
            }
        }
    }
}

/// 降一档；后台档无处可降返回 None（调用方转 Exceeded）。
pub const fn downgrade(qos: QoS) -> Option<QoS> {
    match qos {
        QoS::Interactive => Some(QoS::Async),
        QoS::Async => Some(QoS::Background),
        QoS::Background => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn b(n: u16) -> BusinessId {
        BusinessId::new(n).expect("valid")
    }

    #[test]
    fn sliding_window_forgets_old_charges() {
        let mut ledger = QuotaLedger::new(QuotaTable::new(
            [(
                b(1),
                Quota {
                    window_ms: 1_000,
                    budget_cost: 100,
                },
            )],
            None,
        ));
        ledger.charge(b(1), 60, 0);
        assert_eq!(ledger.verdict(b(1), 30, 10), QuotaVerdict::Within);
        assert_eq!(
            ledger.verdict(b(1), 50, 10),
            QuotaVerdict::Downgrade,
            "60 + 50 > 100"
        );
        ledger.charge(b(1), 50, 20);
        assert_eq!(
            ledger.verdict(b(1), 1, 30),
            QuotaVerdict::Exceeded,
            "110 > 100"
        );
        assert_eq!(
            ledger.verdict(b(1), 1, 1_100),
            QuotaVerdict::Within,
            "窗口滑过，旧账消失"
        );
        assert_eq!(ledger.downgrade_count, 1);
        assert_eq!(ledger.reject_count, 1);
    }

    #[test]
    fn unregistered_business_without_default_is_unlimited() {
        let mut ledger = QuotaLedger::new(QuotaTable::default());
        ledger.charge(b(9), u64::MAX / 2, 0);
        assert_eq!(ledger.verdict(b(9), u64::MAX / 2, 0), QuotaVerdict::Within);
    }

    #[test]
    fn downgrade_ladder_ends_at_background() {
        assert_eq!(downgrade(QoS::Interactive), Some(QoS::Async));
        assert_eq!(downgrade(QoS::Async), Some(QoS::Background));
        assert_eq!(downgrade(QoS::Background), None);
    }
}
