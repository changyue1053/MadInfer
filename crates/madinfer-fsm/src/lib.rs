//! madinfer-fsm —— 表驱动 FSM 内核。范式同源 mad `capability-im-engine` 的 `fsm/Fsm.kt` 与 imlink `core/fsm/fsm.h`：
//! 查表迁移、记录轨迹、拒绝非法事件，只做这三件事。全 SDK 的状态机（调度 Job、模型驻留、算法包安装、配置应用、
//! 请求生命周期）都用它，不各写一套 `match`。
//!
//! 五条规约在 Rust 里的落点：
//! 1. **扁平枚举**——状态 `S: Copy + Eq + Hash`，嵌套层级在类型上不可表达；进度、原因、计数放 owner 的附属字段。
//! 2. **表驱动 + 门票**——迁移表只能经 [`FsmBuilder`] 在构造期注册，[`FsmBuilder::build`] 之后表就是只读的：
//!    Kotlin 版用"首次 fire 后禁止改表"的运行期检查，这里由类型保证（builder 被消费，`Fsm` 没有注册方法）。
//!    [`Fsm::fire`] 返回的 [`Transition::accepted`] 是门票：竞态下后到的一方拿不到门票，副作用天然只执行一次。
//!    不在表中的 (状态, 事件) 组合计入 [`Fsm::unexpected_count`]，状态不变、**绝不 panic**——线上乱序是常态不是异常。
//! 3. **sans-io 纯函数**——效果构造器是 `fn(&E) -> Vec<F>` 函数指针（不能捕获环境，纯度由类型保证）；
//!    `fire` 本身零副作用，效果作为数据返回，由 owner 在自己的线程执行。
//! 4. **64 条环形轨迹**——每次迁移记 (from, 事件种类, to, 注入时钟毫秒)；[`Fsm::trace`] 按时间序导出，
//!    FSM 变更的 PR 必须附 [`TransitionRecord::describe`] 的样例。
//! 5. **`table_size` 锁拓扑**——单测断言 [`Fsm::table_size`]，改图必改测试。
//!
//! 事件按"种类"查表：迁移键是 (状态, [`FsmEvent::Kind`])。事件是携带负载的枚举，`kind()` 把它投影成无负载的
//! 种类枚举——同一种类在同一状态下恰有一条出边；负载经效果构造器进入效果，不参与查表。
//!
//! 执行顺序契约（与 Kotlin / imlink 一致）：`fire` **先改状态**、再记轨迹、再算效果、再返回；owner 拿到 [`Transition`]
//! 后可继续 fire（链式编排），看到的是新状态。
//!
//! 线程契约：非线程安全；约定收口到单一 actor 线程（调度 control 线程 / 装配线程），跨线程事件先投递再 fire。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::Hash;

/// 环形轨迹容量（规约④定值 64，与 Kotlin `Fsm.TRACE_CAPACITY` 同）。
pub const TRACE_CAPACITY: usize = 64;

/// 事件：携负载的枚举，投影到无负载的种类枚举供查表。
pub trait FsmEvent {
    /// 事件种类（与事件枚举的变体一一对应，无负载）。
    type Kind: Copy + Eq + Hash + fmt::Debug;

    /// 本事件的种类。
    fn kind(&self) -> Self::Kind;
}

/// 效果构造器：事件 → 效果列表的纯函数（函数指针，不能捕获环境）。
pub type EffectBuilder<E, F> = fn(&E) -> Vec<F>;

struct Registered<S, E, F> {
    to: S,
    build: EffectBuilder<E, F>,
}

/// 迁移表构造错误：同一 (from, 种类) 重复注册——一个组合恰有一条出边（编程错误，构造期快速失败）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateTransition<S: fmt::Debug, K: fmt::Debug> {
    /// 起点状态。
    pub from: S,
    /// 事件种类。
    pub kind: K,
}

impl<S: fmt::Debug, K: fmt::Debug> fmt::Display for DuplicateTransition<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "重复注册迁移：{:?} × {:?} 已有出边",
            self.from, self.kind
        )
    }
}

impl<S: fmt::Debug, K: fmt::Debug> std::error::Error for DuplicateTransition<S, K> {}

/// 构造期：注册迁移表；[`FsmBuilder::build`] 后表只读。
pub struct FsmBuilder<S, E: FsmEvent, F> {
    initial: S,
    table: HashMap<(S, E::Kind), Registered<S, E, F>>,
}

impl<S, E, F> FsmBuilder<S, E, F>
where
    S: Copy + Eq + Hash + fmt::Debug,
    E: FsmEvent,
{
    /// 从初始状态开始建表。
    pub fn new(initial: S) -> Self {
        Self {
            initial,
            table: HashMap::new(),
        }
    }

    /// 注册一条迁移：(from, kind) → to，无效果。
    pub fn on(
        self,
        from: S,
        kind: E::Kind,
        to: S,
    ) -> Result<Self, DuplicateTransition<S, E::Kind>> {
        self.on_with(from, kind, to, |_| Vec::new())
    }

    /// 注册一条携效果构造器的迁移。
    pub fn on_with(
        mut self,
        from: S,
        kind: E::Kind,
        to: S,
        build: EffectBuilder<E, F>,
    ) -> Result<Self, DuplicateTransition<S, E::Kind>> {
        let key = (from, kind);
        if self.table.contains_key(&key) {
            return Err(DuplicateTransition { from, kind });
        }
        self.table.insert(key, Registered { to, build });
        Ok(self)
    }

    /// 封表；之后没有任何注册入口。
    pub fn build(self) -> Fsm<S, E, F> {
        Fsm {
            state: self.initial,
            table: self.table,
            trace: VecDeque::with_capacity(TRACE_CAPACITY),
            unexpected_count: 0,
            fired_count: 0,
        }
    }
}

/// 表驱动 FSM。
pub struct Fsm<S, E: FsmEvent, F> {
    state: S,
    table: HashMap<(S, E::Kind), Registered<S, E, F>>,
    trace: VecDeque<TransitionRecord<S, E::Kind>>,
    unexpected_count: u64,
    fired_count: u64,
}

/// [`Fsm::fire`] 的返回：`accepted` 是门票（false = 被表拒绝，状态未变，效果恒空）。
#[derive(Debug)]
pub struct Transition<S, F> {
    /// 门票。
    pub accepted: bool,
    /// 迁移前状态（被拒绝时等于当前状态）。
    pub from: S,
    /// 迁移后状态（被拒绝时等于当前状态）。
    pub to: S,
    /// 本次迁移输出的效果（数据不是动作，由 owner 执行）。
    pub effects: Vec<F>,
}

impl<S, E, F> Fsm<S, E, F>
where
    S: Copy + Eq + Hash + fmt::Debug,
    E: FsmEvent,
{
    /// 当前状态。
    pub fn state(&self) -> S {
        self.state
    }

    /// 迁移表条目数；单测断言它锁死拓扑（规约⑤）。
    pub fn table_size(&self) -> usize {
        self.table.len()
    }

    /// 不在表中的 (状态, 事件) 组合累计数；监控基线，暴涨即协议或竞态出了新花样。
    pub fn unexpected_count(&self) -> u64 {
        self.unexpected_count
    }

    /// 已接受的迁移数。
    pub fn fired_count(&self) -> u64 {
        self.fired_count
    }

    /// 当前状态下某种类事件是否有出边（owner 用于"能不能"的预判，不改状态）。
    pub fn accepts(&self, kind: E::Kind) -> bool {
        self.table.contains_key(&(self.state, kind))
    }

    /// 投递事件：命中则迁移并返回 (accepted, 效果)；不在表中则 unexpected +1、状态不变、效果为空。
    pub fn fire(&mut self, event: &E, now_ms: u64) -> Transition<S, F> {
        let kind = event.kind();
        let Some(registered) = self.table.get(&(self.state, kind)) else {
            self.unexpected_count += 1;
            return Transition {
                accepted: false,
                from: self.state,
                to: self.state,
                effects: Vec::new(),
            };
        };
        let from = self.state;
        let to = registered.to;
        let build = registered.build;
        self.state = to; // 先改状态：链式编排看到的是新状态
        self.fired_count += 1;
        if self.trace.len() == TRACE_CAPACITY {
            self.trace.pop_front();
        }
        self.trace.push_back(TransitionRecord {
            from,
            kind,
            to,
            at_ms: now_ms,
        });
        let effects = build(event); // 纯函数，副作用零发生
        Transition {
            accepted: true,
            from,
            to,
            effects,
        }
    }

    /// 环形轨迹（最旧 → 最新，至多 [`TRACE_CAPACITY`] 条）。
    pub fn trace(&self) -> impl Iterator<Item = &TransitionRecord<S, E::Kind>> {
        self.trace.iter()
    }
}

impl<S, E, F> fmt::Debug for Fsm<S, E, F>
where
    S: fmt::Debug,
    E: FsmEvent,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fsm")
            .field("state", &self.state)
            .field("table_size", &self.table.len())
            .field("unexpected", &self.unexpected_count)
            .finish()
    }
}

/// 环形轨迹条目：一次已发生的迁移事实。线上排障第一问"这个实体经历了什么"由它回答，不靠翻日志拼时序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionRecord<S, K> {
    /// 起点。
    pub from: S,
    /// 事件种类。
    pub kind: K,
    /// 终点。
    pub to: S,
    /// 注入时钟毫秒。
    pub at_ms: u64,
}

impl<S: fmt::Debug, K: fmt::Debug> TransitionRecord<S, K> {
    /// 转移日志样例格式：`Queued --Admitted--> Running @120ms`（FSM 变更 PR 附带的 DoD 条款用它导出）。
    pub fn describe(&self) -> String {
        format!(
            "{:?} --{:?}--> {:?} @{}ms",
            self.from, self.kind, self.to, self.at_ms
        )
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum Light {
        Red,
        Green,
        Yellow,
    }

    enum Ev {
        TimerElapsed { seq: u32 },
        EmergencyPressed,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum EvKind {
        TimerElapsed,
        EmergencyPressed,
    }

    impl FsmEvent for Ev {
        type Kind = EvKind;
        fn kind(&self) -> EvKind {
            match self {
                Ev::TimerElapsed { .. } => EvKind::TimerElapsed,
                Ev::EmergencyPressed => EvKind::EmergencyPressed,
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Fx {
        ArmTimer(u32),
        Honk,
    }

    fn arm(e: &Ev) -> Vec<Fx> {
        match e {
            Ev::TimerElapsed { seq } => vec![Fx::ArmTimer(*seq + 1)],
            Ev::EmergencyPressed => vec![Fx::Honk],
        }
    }

    fn build() -> Fsm<Light, Ev, Fx> {
        FsmBuilder::new(Light::Red)
            .on_with(Light::Red, EvKind::TimerElapsed, Light::Green, arm)
            .expect("table")
            .on_with(Light::Green, EvKind::TimerElapsed, Light::Yellow, arm)
            .expect("table")
            .on_with(Light::Yellow, EvKind::TimerElapsed, Light::Red, arm)
            .expect("table")
            .on_with(Light::Green, EvKind::EmergencyPressed, Light::Red, arm)
            .expect("table")
            .build()
    }

    #[test]
    fn table_size_locks_topology() {
        assert_eq!(build().table_size(), 4);
    }

    #[test]
    fn fire_changes_state_first_then_returns_effects() {
        let mut fsm = build();
        let t = fsm.fire(&Ev::TimerElapsed { seq: 7 }, 100);
        assert!(t.accepted);
        assert_eq!((t.from, t.to), (Light::Red, Light::Green));
        assert_eq!(t.effects, vec![Fx::ArmTimer(8)]);
        assert_eq!(fsm.state(), Light::Green);
        assert!(fsm.accepts(EvKind::EmergencyPressed));
    }

    #[test]
    fn unexpected_event_is_counted_not_panicked() {
        let mut fsm = build();
        let t = fsm.fire(&Ev::EmergencyPressed, 5); // Red 下无出边
        assert!(!t.accepted);
        assert!(t.effects.is_empty());
        assert_eq!(fsm.state(), Light::Red);
        assert_eq!(fsm.unexpected_count(), 1);
        assert_eq!(fsm.fired_count(), 0);
    }

    #[test]
    fn duplicate_registration_fails_at_build_time() {
        let err = FsmBuilder::<Light, Ev, Fx>::new(Light::Red)
            .on(Light::Red, EvKind::TimerElapsed, Light::Green)
            .expect("first")
            .on(Light::Red, EvKind::TimerElapsed, Light::Yellow)
            .err()
            .expect("duplicate");
        assert_eq!(err.from, Light::Red);
        assert_eq!(err.kind, EvKind::TimerElapsed);
    }

    #[test]
    fn trace_is_a_64_entry_ring() {
        let mut fsm = build();
        for i in 0..70u64 {
            fsm.fire(&Ev::TimerElapsed { seq: 0 }, i);
        }
        let trace: Vec<_> = fsm.trace().collect();
        assert_eq!(trace.len(), TRACE_CAPACITY);
        assert_eq!(trace[0].at_ms, 6, "最旧的 6 条被挤掉");
        assert_eq!(trace[63].at_ms, 69);
        assert_eq!(trace[0].describe(), "Red --TimerElapsed--> Green @6ms");
    }
}
