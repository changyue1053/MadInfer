//! 预算与 QoS（request.v1 `budget`）。

/// QoS 三档。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum QoS {
    /// 交互：< 100 ms 首 token 目标，可抢占其他档。
    Interactive = 0,
    /// 异步：秒级。
    Async = 1,
    /// 后台批量：充电 ∧ 空闲 ∧ 温度正常三门齐开才跑。
    Background = 2,
}

impl QoS {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Async => "async",
            Self::Background => "background",
        }
    }

    /// 从协议字符串解析。
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "interactive" => Self::Interactive,
            "async" => Self::Async,
            "background" => Self::Background,
            _ => return None,
        })
    }
}

/// 预算：截止时间、QoS 档、内存上限（None = 用配置中心的档位预算）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Budget {
    /// 截止毫秒数，已截到 [`Budget::MIN_DEADLINE_MS`]～[`Budget::MAX_DEADLINE_MS`]。
    pub deadline_ms: u32,
    /// QoS 档。
    pub qos: QoS,
    /// 内存上限 MB。
    pub max_memory_mb: Option<u32>,
}

impl Budget {
    /// 协议下限 100 ms。
    pub const MIN_DEADLINE_MS: u32 = 100;
    /// 协议上限 600 s。
    pub const MAX_DEADLINE_MS: u32 = 600_000;

    /// 构造并把 deadline 截到协议区间（越界不是错误：配置上限再截一次由调度器做）。
    pub const fn new(deadline_ms: u32, qos: QoS) -> Self {
        let clamped = if deadline_ms < Self::MIN_DEADLINE_MS {
            Self::MIN_DEADLINE_MS
        } else if deadline_ms > Self::MAX_DEADLINE_MS {
            Self::MAX_DEADLINE_MS
        } else {
            deadline_ms
        };
        Self {
            deadline_ms: clamped,
            qos,
            max_memory_mb: None,
        }
    }

    /// 交互档缺省 2 s。
    pub const fn interactive() -> Self {
        Self::new(2_000, QoS::Interactive)
    }

    /// 异步档缺省 20 s。
    pub const fn async_default() -> Self {
        Self::new(20_000, QoS::Async)
    }

    /// 后台档缺省 120 s。
    pub const fn background() -> Self {
        Self::new(120_000, QoS::Background)
    }

    /// 带内存上限。
    pub const fn with_max_memory_mb(mut self, mb: u32) -> Self {
        self.max_memory_mb = Some(mb);
        self
    }
}
