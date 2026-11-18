//! 取消令牌：调度器 / 请求管线翻旗，引擎每 token 查一次（O(1)，无锁）。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// 取消原因（进 worker 回执与观测）。与 `madinfer_sched::CancelReason` 同构，由请求管线映射（Drain → Shutdown）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelReason {
    /// 调用方取消。
    Caller,
    /// 截止时刻到。
    Deadline,
    /// 被交互档抢占（Generate 型在下一 token 边界让出）。
    Preempt,
    /// 关停排水。
    Shutdown,
    /// 内存 / 热压力驱逐。
    Pressure,
}

impl CancelReason {
    /// 全部变体。
    pub const ALL: [Self; 5] = [
        Self::Caller,
        Self::Deadline,
        Self::Preempt,
        Self::Shutdown,
        Self::Pressure,
    ];

    /// 原子字节编码（0 保留为"未取消"）。
    const fn code(self) -> u8 {
        match self {
            Self::Caller => 1,
            Self::Deadline => 2,
            Self::Preempt => 3,
            Self::Shutdown => 4,
            Self::Pressure => 5,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Caller,
            2 => Self::Deadline,
            3 => Self::Preempt,
            4 => Self::Shutdown,
            5 => Self::Pressure,
            _ => return None,
        })
    }

    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Caller => "caller",
            Self::Deadline => "deadline",
            Self::Preempt => "preempt",
            Self::Shutdown => "shutdown",
            Self::Pressure => "pressure",
        }
    }
}

const LIVE: u8 = 0;

/// 取消令牌：`Arc` 共享的单字节原子——0 = 未取消，其余 = 原因编码。首个 `cancel` 生效，之后的原因被忽略（先到者定性）。
///
/// 后端契约（设计文档 §4.2 六条之①）：`generate` 在 `is_cancelled()` 为真后至多再产出一个 token 就返回
/// [`crate::BackendError::Cancelled`]；testkit 的 `backend_contract_tests!` 对每个实现验这一条。
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    state: Arc<AtomicU8>,
}

impl CancelToken {
    /// 未取消的新令牌。
    pub fn new() -> Self {
        Self::default()
    }

    /// 翻旗；重复调用无效果（保留首个原因）。返回本次是否生效。
    pub fn cancel(&self, reason: CancelReason) -> bool {
        self.state
            .compare_exchange(LIVE, reason.code(), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// 是否已取消。
    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) != LIVE
    }

    /// 取消原因（未取消为 None）。
    pub fn reason(&self) -> Option<CancelReason> {
        CancelReason::from_code(self.state.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_cancel_wins_and_clones_share_state() {
        let token = CancelToken::new();
        let shared = token.clone();
        assert!(!token.is_cancelled());
        assert_eq!(token.reason(), None);
        assert!(shared.cancel(CancelReason::Deadline));
        assert!(!token.cancel(CancelReason::Caller), "第二次取消被忽略");
        assert!(token.is_cancelled());
        assert_eq!(token.reason(), Some(CancelReason::Deadline));
        for reason in CancelReason::ALL {
            assert_eq!(CancelReason::from_code(reason.code()), Some(reason));
        }
    }
}
