//! Job、槽与资源估算。

use std::collections::HashMap;

use madinfer_core_types::{BusinessId, CapabilityId, QoS, RequestId, SlotClass};

/// 调度器分配的 Job 标识（进程内单调）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// Job 类型：Task 型抢占立即让出；Generate 型（有状态生成）在下一 token 边界让出并可 snapshot 后重排。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// 单次前向。
    Task,
    /// 有状态生成（KV、流式、约束解码）。
    Generate,
}

/// 资源估算（来自算法包 compat.memory_mb 与历史 tps；配额成本 = estimated_ms × peak_memory_mb）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceEstimate {
    /// 峰值内存 MB。
    pub peak_memory_mb: u32,
    /// 预计运行毫秒。
    pub estimated_ms: u32,
}

impl ResourceEstimate {
    /// 配额成本（ms × MB）。
    pub const fn cost(&self) -> u64 {
        self.estimated_ms as u64 * self.peak_memory_mb as u64
    }
}

/// 提交给调度器的 Job。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// 所属请求。
    pub request: RequestId,
    /// 能力（观测归因）。
    pub capability: CapabilityId,
    /// 业务（配额归因）。
    pub business: BusinessId,
    /// QoS 档（配额超额时可被降一档）。
    pub qos: QoS,
    /// 类型。
    pub kind: JobKind,
    /// 目标槽。
    pub slot: SlotClass,
    /// 截止时刻（注入时钟毫秒）。
    pub deadline_at_ms: u64,
    /// 同档内优先级（越小越先；NPU 串行队列按它排）。
    pub priority: u8,
    /// 资源估算。
    pub estimate: ResourceEstimate,
    /// 零拷贝直通（Task 型交互档）：跳过排队但仍占槽计数。
    pub passthrough: bool,
}

/// 交给 worker 的执行上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobCtx {
    /// Job。
    pub job: JobId,
    /// 请求。
    pub request: RequestId,
    /// 实际执行档（可能已被配额降档）。
    pub qos: QoS,
    /// 槽。
    pub slot: SlotClass,
    /// 截止时刻。
    pub deadline_at_ms: u64,
    /// 线程数建议：交互 = 大核数，异步 = 大核数 / 2，后台 = 2。
    pub threads: u8,
    /// 排队时长（进 Started.queued_ms）。
    pub queued_ms: u64,
}

/// 槽表：每类容量与占用者。不变量：任一时刻每个槽最多一个运行中 Job（容量即并发上限）。
#[derive(Clone, Debug, Default)]
pub struct SlotTable {
    capacity: HashMap<SlotClass, usize>,
    occupants: HashMap<SlotClass, Vec<JobId>>,
}

impl SlotTable {
    /// 建表。
    pub fn new(capacity: impl IntoIterator<Item = (SlotClass, usize)>) -> Self {
        Self {
            capacity: capacity.into_iter().collect(),
            occupants: HashMap::new(),
        }
    }

    /// 容量（未登记的类别为 0 = 不可用）。
    pub fn capacity(&self, slot: SlotClass) -> usize {
        self.capacity.get(&slot).copied().unwrap_or(0)
    }

    /// 已登记容量的槽类别（确定性顺序：Cpu、Gpu、Npu、External 按编号）。worker 池按它起线程。
    pub fn classes(&self) -> Vec<SlotClass> {
        let mut classes: Vec<SlotClass> = self
            .capacity
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(c, _)| *c)
            .collect();
        classes.sort_by_key(|c| match c {
            SlotClass::Cpu => (0u8, 0u8),
            SlotClass::Gpu => (1, 0),
            SlotClass::Npu => (2, 0),
            SlotClass::External(n) => (3, *n),
        });
        classes
    }

    /// 是否有空位。
    pub fn has_free(&self, slot: SlotClass) -> bool {
        self.occupied(slot) < self.capacity(slot)
    }

    /// 占用数。
    pub fn occupied(&self, slot: SlotClass) -> usize {
        self.occupants.get(&slot).map_or(0, Vec::len)
    }

    /// 占用者。
    pub fn occupants(&self, slot: SlotClass) -> &[JobId] {
        self.occupants.get(&slot).map_or(&[], Vec::as_slice)
    }

    /// 占位；无空位返回 false。
    pub fn acquire(&mut self, slot: SlotClass, job: JobId) -> bool {
        if !self.has_free(slot) {
            return false;
        }
        self.occupants.entry(slot).or_default().push(job);
        true
    }

    /// 释放；不在位返回 false（重复释放是编程错误，计数不崩溃）。
    pub fn release(&mut self, slot: SlotClass, job: JobId) -> bool {
        let Some(list) = self.occupants.get_mut(&slot) else {
            return false;
        };
        let before = list.len();
        list.retain(|j| *j != job);
        before != list.len()
    }

    /// 全部占用数。
    pub fn total_occupied(&self) -> usize {
        self.occupants.values().map(Vec::len).sum()
    }
}

/// 线程数建议表。
pub const fn threads_for(qos: QoS, big_cores: u8) -> u8 {
    match qos {
        QoS::Interactive => {
            if big_cores == 0 {
                1
            } else {
                big_cores
            }
        }
        QoS::Async => {
            if big_cores <= 1 {
                1
            } else {
                big_cores / 2
            }
        }
        QoS::Background => 2,
    }
}
