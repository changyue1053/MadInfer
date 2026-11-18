//! 后端注册表：注册时 `probe()` 一次并缓存能力；连续崩溃计数 → quarantine；导出不可变快照给 worker 线程。
//!
//! 无锁：注册表归属核 actor 线程独占（`&mut self`），跨线程只传 [`RegistrySnapshot`]（`Arc` 共享、只读）。
//! 设计文档 C6 写的 `RwLock<BTreeMap<…>>` 在本仓被 clippy.toml 的"禁共享锁"口径替换为快照。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use madinfer_core_types::BackendId;
use madinfer_host::{LogLevel, Logger, NoopLogger};

use crate::types::{Availability, BackendCaps};
use crate::Backend;

/// 连续崩溃多少次进入 quarantine（设计文档 C6 ④：直到进程重启）。
pub const QUARANTINE_CRASH_THRESHOLD: u8 = 3;

/// 注册失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// 同 id 已注册（注册后不移除，重复即编程错误）。
    DuplicateId(BackendId),
}

impl core::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "backend {} already registered", id.0),
        }
    }
}

impl std::error::Error for RegisterError {}

/// 台账。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegistryStats {
    /// 注册成功。
    pub registered: u64,
    /// 重复 id 被拒。
    pub rejected_duplicates: u64,
    /// 崩溃计数（累计）。
    pub crashes: u64,
    /// 进入 quarantine 的次数。
    pub quarantined: u64,
}

/// 后端注册表（核 actor 线程独占）。
pub struct BackendRegistry {
    backends: BTreeMap<BackendId, Arc<dyn Backend>>,
    caps: BTreeMap<BackendId, BackendCaps>,
    crash_streak: BTreeMap<BackendId, u8>,
    quarantined: BTreeSet<BackendId>,
    logger: Arc<dyn Logger>,
    stats: RegistryStats,
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendRegistry {
    /// 空表，无留痕。
    pub fn new() -> Self {
        Self::with_logger(Arc::new(NoopLogger))
    }

    /// 空表，带留痕（`key=value`：后端 id、计数；无用户内容）。
    pub fn with_logger(logger: Arc<dyn Logger>) -> Self {
        Self {
            backends: BTreeMap::new(),
            caps: BTreeMap::new(),
            crash_streak: BTreeMap::new(),
            quarantined: BTreeSet::new(),
            logger,
            stats: RegistryStats::default(),
        }
    }

    /// 台账。
    pub fn stats(&self) -> RegistryStats {
        self.stats
    }

    /// 已注册数。
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    /// 注册：`probe()` 恰调用一次，结果缓存；重复 id 拒绝。
    pub fn register(&mut self, backend: Arc<dyn Backend>) -> Result<(), RegisterError> {
        let id = backend.id().clone();
        if self.backends.contains_key(&id) {
            self.stats.rejected_duplicates += 1;
            return Err(RegisterError::DuplicateId(id));
        }
        let caps = backend.probe();
        self.log(format!(
            "registry=register backend={} concurrency={} supports_abort={} constraint={:?}",
            id.0, caps.concurrency, caps.supports_abort, caps.constraint
        ));
        self.caps.insert(id.clone(), caps);
        self.backends.insert(id, backend);
        self.stats.registered += 1;
        Ok(())
    }

    /// 取后端（quarantine 的仍可取——隔离只是不进决策链）。
    pub fn get(&self, id: &BackendId) -> Option<Arc<dyn Backend>> {
        self.backends.get(id).cloned()
    }

    /// 缓存的能力声明。
    pub fn caps(&self, id: &BackendId) -> Option<&BackendCaps> {
        self.caps.get(id)
    }

    /// 全部已注册 id（有序）。
    pub fn ids(&self) -> impl Iterator<Item = &BackendId> {
        self.backends.keys()
    }

    /// 记一次崩溃；连续达到 [`QUARANTINE_CRASH_THRESHOLD`] 进入 quarantine。返回本次是否触发隔离。
    pub fn record_crash(&mut self, id: &BackendId) -> bool {
        if !self.backends.contains_key(id) {
            return false;
        }
        self.stats.crashes += 1;
        let streak = self.crash_streak.entry(id.clone()).or_insert(0);
        *streak = streak.saturating_add(1);
        let streak = *streak;
        self.log(format!("registry=crash backend={} streak={streak}", id.0));
        if streak >= QUARANTINE_CRASH_THRESHOLD && self.quarantined.insert(id.clone()) {
            self.stats.quarantined += 1;
            self.logger.log(
                LogLevel::Warn,
                "madinfer-backend",
                &format!(
                    "registry=quarantine backend={} reason=crash_x{streak}",
                    id.0
                ),
            );
            return true;
        }
        false
    }

    /// 记一次成功执行：连续崩溃计数归零（"连续"的定义）。
    pub fn record_success(&mut self, id: &BackendId) {
        self.crash_streak.remove(id);
    }

    /// 隔离中的后端集合。
    pub fn quarantined(&self) -> &BTreeSet<BackendId> {
        &self.quarantined
    }

    /// 是否隔离中。
    pub fn is_quarantined(&self, id: &BackendId) -> bool {
        self.quarantined.contains(id)
    }

    /// 此刻可进决策链的后端：已注册 ∧ 未隔离 ∧ `availability() == Available`。
    pub fn available_backends(&self) -> BTreeSet<BackendId> {
        self.backends
            .iter()
            .filter(|(id, _)| !self.quarantined.contains(*id))
            .filter(|(_, b)| b.availability() == Availability::Available)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// 不变量：`supports_abort = false` 的后端不接交互档 Job；隔离中的也不接。
    pub fn accepts_interactive(&self, id: &BackendId) -> bool {
        !self.quarantined.contains(id) && self.caps.get(id).is_some_and(|c| c.supports_abort)
    }

    /// 不可变快照（交给 worker 线程；之后的注册 / 隔离不反映到已发出的快照）。
    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            backends: self.backends.clone(),
            caps: self.caps.clone(),
            quarantined: self.quarantined.clone(),
        }
    }

    fn log(&self, facts: String) {
        self.logger.log(LogLevel::Info, "madinfer-backend", &facts);
    }
}

/// 注册表快照：只读，`Send + Sync`，以 `Arc` 在线程间共享。
#[derive(Clone)]
pub struct RegistrySnapshot {
    backends: BTreeMap<BackendId, Arc<dyn Backend>>,
    caps: BTreeMap<BackendId, BackendCaps>,
    quarantined: BTreeSet<BackendId>,
}

impl RegistrySnapshot {
    /// 取后端。
    pub fn get(&self, id: &BackendId) -> Option<&Arc<dyn Backend>> {
        self.backends.get(id)
    }

    /// 能力声明。
    pub fn caps(&self, id: &BackendId) -> Option<&BackendCaps> {
        self.caps.get(id)
    }

    /// 是否隔离中（快照时刻）。
    pub fn is_quarantined(&self, id: &BackendId) -> bool {
        self.quarantined.contains(id)
    }

    /// 全部 id。
    pub fn ids(&self) -> impl Iterator<Item = &BackendId> {
        self.backends.keys()
    }

    /// 后端数。
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

impl core::fmt::Debug for RegistrySnapshot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegistrySnapshot")
            .field("backends", &self.backends.keys().collect::<Vec<_>>())
            .field("quarantined", &self.quarantined)
            .finish()
    }
}
