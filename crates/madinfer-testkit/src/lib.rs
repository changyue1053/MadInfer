//! madinfer-testkit —— 替身与夹具（设计文档 C19）。形态沿 mad `capability-im-testing` 的 TestClock 与 im-core `testing::ManualClock`：
//! 手拨时钟、录音式端口、内存存储；[`backend`] 模块的 [`NoopBackend`] / [`FakeBackend`] 与 [`contract`] 模块的
//! [`backend_contract_tests!`] 契约宏随 madinfer-backend 落地。MemoryAssetStore 随 madinfer-assets 的 store trait 加入。
//!
//! 夹具允许用 `RwLock`（imcore 规则第 7 条豁免形态四：测试夹具），生产 crate 不得照抄。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod backend;
pub mod contract;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use madinfer_host::{
    Authorizer, Clock, ConsentContext, ConsentDecision, KvStore, LogLevel, Logger, SecureStore,
    TelemetrySink,
};

pub use backend::{default_caps, CollectingSink, FakeBackend, FakeLedger, FakeScript, NoopBackend};

/// 手拨时钟：`advance` / `set` 推进；`with_auto_step` 让每次读取自动前进（模拟"时间总在流动"的循环）。
pub struct ManualClock {
    now_ms: AtomicU64,
    auto_step_ms: AtomicU64,
}

impl ManualClock {
    /// 从某时刻起。
    pub fn at(start_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(start_ms),
            auto_step_ms: AtomicU64::new(0),
        }
    }

    /// 每次 `now_ms` 读取后自动前进 `step_ms`。
    pub fn with_auto_step(start_ms: u64, step_ms: u64) -> Self {
        let clock = Self::at(start_ms);
        clock.auto_step_ms.store(step_ms, Ordering::SeqCst);
        clock
    }

    /// 前进。
    pub fn advance(&self, ms: u64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }

    /// 设到某时刻（只准向前；向后设是测试写错，直接忽略并保持单调）。
    pub fn set(&self, ms: u64) {
        self.now_ms.fetch_max(ms, Ordering::SeqCst);
    }

    /// 不推进地读当前值（断言用）。
    pub fn peek(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::at(0)
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        let step = self.auto_step_ms.load(Ordering::SeqCst);
        if step == 0 {
            self.now_ms.load(Ordering::SeqCst)
        } else {
            self.now_ms.fetch_add(step, Ordering::SeqCst)
        }
    }
}

/// 一行留痕。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// 级别。
    pub level: LogLevel,
    /// 目标。
    pub target: &'static str,
    /// key=value 事实。
    pub facts: String,
}

/// 录音式日志：断言面 `lines()`；`facts_containing` 便于按 key 过滤。
#[derive(Default)]
pub struct RecordingLogger {
    lines: RwLock<Vec<LogLine>>,
}

impl RecordingLogger {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 全录。
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.read().map(|l| l.clone()).unwrap_or_default()
    }

    /// 含某片段的行。
    pub fn facts_containing(&self, needle: &str) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|l| l.facts.contains(needle))
            .map(|l| l.facts)
            .collect()
    }
}

impl Logger for RecordingLogger {
    fn log(&self, level: LogLevel, target: &'static str, facts: &str) {
        if let Ok(mut lines) = self.lines.write() {
            lines.push(LogLine {
                level,
                target,
                facts: facts.to_owned(),
            });
        }
    }
}

/// 内存键值（同时实现 KvStore 与 SecureStore）。
#[derive(Default)]
pub struct MemoryKv {
    map: RwLock<HashMap<String, Vec<u8>>>,
}

impl MemoryKv {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 键数。
    pub fn len(&self) -> usize {
        self.map.read().map(|m| m.len()).unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl KvStore for MemoryKv {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.map.read().ok().and_then(|m| m.get(key).cloned())
    }

    fn put(&self, key: &str, value: &[u8]) {
        if let Ok(mut m) = self.map.write() {
            m.insert(key.to_owned(), value.to_vec());
        }
    }

    fn delete(&self, key: &str) {
        if let Ok(mut m) = self.map.write() {
            m.remove(key);
        }
    }
}

impl SecureStore for MemoryKv {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        KvStore::get(self, key)
    }

    fn put(&self, key: &str, value: &[u8]) {
        KvStore::put(self, key, value)
    }

    fn delete(&self, key: &str) {
        KvStore::delete(self, key)
    }
}

/// 录音式观测出口。
#[derive(Default)]
pub struct RecordingTelemetrySink {
    batches: RwLock<Vec<String>>,
}

impl RecordingTelemetrySink {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 全录。
    pub fn batches(&self) -> Vec<String> {
        self.batches.read().map(|b| b.clone()).unwrap_or_default()
    }
}

impl TelemetrySink for RecordingTelemetrySink {
    fn emit(&self, batch_json: &str) {
        if let Ok(mut b) = self.batches.write() {
            b.push(batch_json.to_owned());
        }
    }
}

/// 脚本化授权：固定回答，记录请求。
pub struct ScriptedAuthorizer {
    decision: ConsentDecision,
    requests: RwLock<Vec<ConsentContext>>,
}

impl ScriptedAuthorizer {
    /// 固定回答。
    pub fn answering(decision: ConsentDecision) -> Self {
        Self {
            decision,
            requests: RwLock::new(Vec::new()),
        }
    }

    /// 收到的请求。
    pub fn requests(&self) -> Vec<ConsentContext> {
        self.requests.read().map(|r| r.clone()).unwrap_or_default()
    }
}

impl Authorizer for ScriptedAuthorizer {
    fn request(&self, context: ConsentContext, reply: Box<dyn FnOnce(ConsentDecision) + Send>) {
        if let Ok(mut r) = self.requests.write() {
            r.push(context);
        }
        reply(self.decision);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use madinfer_backend::{
        Availability, Backend, BackendCaps, BackendError, CancelToken, ConstraintSupport,
        ContextOptions, FinishReason, GenerateOptions, LoadOpts, Turn, UnavailableReason,
    };
    use std::sync::atomic::Ordering as AtomicOrdering;

    backend_contract_tests!(noop_backend => NoopBackend::new("noop"));
    backend_contract_tests!(fake_backend => FakeBackend::new(
        "fake",
        FakeScript::tokens(&["会", "议", "纪", "要", "：", "三", "件", "事", "。", "完"])
    ));
    backend_contract_tests!(fake_backend_native_constraint_concurrency_3 => FakeBackend::new(
        "fake.native",
        FakeScript::tokens(&["{", "\"ok\"", "}"])
    )
    .with_caps(BackendCaps {
        concurrency: 3,
        constraint: ConstraintSupport::Native,
        ..default_caps()
    }));
    backend_contract_tests!(boxed_dyn_backend => {
        let boxed: Box<dyn Backend> = Box::new(NoopBackend::new("boxed"));
        boxed
    });

    fn generate(
        backend: &FakeBackend,
        sink: &mut CollectingSink,
        cancel: &CancelToken,
    ) -> Result<madinfer_backend::GenerateStats, BackendError> {
        let model = backend.load(&LoadOpts::default()).expect("load");
        let mut context = model
            .open_context(&ContextOptions::default())
            .expect("open");
        context.generate(
            &Turn::user("hi"),
            &GenerateOptions::default(),
            None,
            sink,
            cancel,
        )
    }

    #[test]
    fn fake_script_emits_tokens_then_fails_or_crashes_at_index() {
        let fake = FakeBackend::new("fake", FakeScript::tokens(&["a", "b", "c"]));
        let mut sink = CollectingSink::new();
        let stats = generate(&fake, &mut sink, &CancelToken::new()).expect("ok");
        assert_eq!(sink.text(), "abc");
        assert_eq!(stats.gen_tokens, 3);
        assert_eq!(stats.finish, FinishReason::Stop);
        assert!(sink.pieces[2].is_last && !sink.pieces[1].is_last);

        fake.set_script(FakeScript::tokens(&["a", "b", "c"]).failing_at(1, BackendError::Refused));
        let mut sink = CollectingSink::new();
        assert_eq!(
            generate(&fake, &mut sink, &CancelToken::new()).err(),
            Some(BackendError::Refused)
        );
        assert_eq!(sink.text(), "a", "第 1 片前失败：只出 1 片");

        fake.set_script(FakeScript::tokens(&["a", "b", "c"]).crashing_at(3));
        let mut sink = CollectingSink::new();
        assert_eq!(
            generate(&fake, &mut sink, &CancelToken::new()).err(),
            Some(BackendError::Crashed)
        );
        assert_eq!(sink.text(), "abc", "下标 = len：全部产出后崩溃");
        assert_eq!(fake.generate_calls(), 3);
        assert_eq!(fake.ledger().tokens_emitted.load(AtomicOrdering::SeqCst), 7);
    }

    #[test]
    fn fake_sink_stop_and_max_tokens_and_cancel_ledger() {
        let fake = FakeBackend::new("fake", FakeScript::tokens(&["a", "b", "c", "d"]));
        let mut sink = CollectingSink {
            stop_on_piece: Some(2),
            ..CollectingSink::default()
        };
        let stats = generate(&fake, &mut sink, &CancelToken::new()).expect("ok");
        assert_eq!(
            (stats.gen_tokens, stats.finish),
            (2, FinishReason::SinkStopped)
        );

        let model = fake.load(&LoadOpts::default()).expect("load");
        let mut context = model
            .open_context(&ContextOptions::default())
            .expect("open");
        let mut sink = CollectingSink::new();
        let stats = context
            .generate(
                &Turn::user("hi"),
                &GenerateOptions {
                    max_tokens: 3,
                    ..GenerateOptions::default()
                },
                None,
                &mut sink,
                &CancelToken::new(),
            )
            .expect("ok");
        assert_eq!((stats.gen_tokens, stats.finish), (3, FinishReason::Length));

        let cancel = CancelToken::new();
        let mut sink = CollectingSink {
            cancel_on_piece: Some((1, cancel.clone())),
            ..CollectingSink::default()
        };
        assert_eq!(
            generate(&fake, &mut sink, &cancel).err(),
            Some(BackendError::Cancelled)
        );
        assert_eq!(sink.text(), "a", "翻旗后零片");
        assert_eq!(fake.cancelled_count(), 1);
    }

    #[test]
    fn fake_availability_can_be_injected() {
        let fake = FakeBackend::new("fake", FakeScript::default());
        assert_eq!(fake.availability(), Availability::Available);
        fake.set_availability(Availability::Downloading { permille: 300 });
        assert_eq!(
            fake.availability(),
            Availability::Downloading { permille: 300 }
        );
        fake.set_availability(Availability::Unavailable {
            reason: UnavailableReason::OsTooOld,
        });
        assert!(matches!(
            fake.availability(),
            Availability::Unavailable { .. }
        ));
        assert_eq!(fake.probe(), default_caps());
        assert_eq!(fake.ledger().probe_calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn manual_clock_advances_only_forward() {
        let clock = ManualClock::at(100);
        clock.advance(50);
        assert_eq!(clock.now_ms(), 150);
        clock.set(120);
        assert_eq!(clock.now_ms(), 150, "向后 set 被忽略");
        clock.set(400);
        assert_eq!(clock.now_ms(), 400);
    }

    #[test]
    fn auto_step_clock_moves_on_every_read() {
        let clock = ManualClock::with_auto_step(0, 10);
        assert_eq!(clock.now_ms(), 0);
        assert_eq!(clock.now_ms(), 10);
        assert_eq!(clock.peek(), 20);
    }

    #[test]
    fn recording_logger_filters_by_fact() {
        let logger = RecordingLogger::new();
        logger.log(LogLevel::Info, "sched", "job=1 state=Running");
        logger.log(LogLevel::Info, "sched", "job=2 state=Queued");
        assert_eq!(
            logger.facts_containing("Running"),
            vec!["job=1 state=Running".to_owned()]
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod echo_prompt_tests {
    use super::*;
    use madinfer_backend::{
        Backend, BackendError, CancelToken, ContextOptions, FinishReason, GenerateOptions,
        LoadOpts, Message, Role, Turn,
    };

    fn generate(
        turn: &Turn,
        options: &GenerateOptions,
    ) -> (
        String,
        Vec<String>,
        Result<madinfer_backend::GenerateStats, BackendError>,
    ) {
        let fake = FakeBackend::new("debug_script", FakeScript::echo_prompt());
        let model = fake.load(&LoadOpts::default()).expect("load");
        let mut context = model
            .open_context(&ContextOptions::default())
            .expect("open");
        let mut sink = CollectingSink::new();
        let stats = context.generate(turn, options, None, &mut sink, &CancelToken::new());
        let pieces = sink.pieces.iter().map(|p| p.text.clone()).collect();
        (sink.text(), pieces, stats)
    }

    #[test]
    fn echo_prompt_replays_last_user_message_split_on_whitespace_inclusive() {
        // madai-w10 I-C-09 J5-a：输出 = 最后一条 user 消息原文；按空白切词元、空白归前一词元，拼接逐字节等于原文。
        let turn = Turn {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "sys".to_owned(),
                },
                Message {
                    role: Role::User,
                    content: "first".to_owned(),
                },
                Message {
                    role: Role::Assistant,
                    content: "reply".to_owned(),
                },
                Message {
                    role: Role::User,
                    content: "{\"title\": \"x\",\n\"items\":[1]}".to_owned(),
                },
            ],
        };
        let (text, pieces, stats) = generate(&turn, &GenerateOptions::default());
        assert_eq!(text, "{\"title\": \"x\",\n\"items\":[1]}");
        assert_eq!(pieces, vec!["{\"title\": ", "\"x\",\n", "\"items\":[1]}"]);
        let stats = stats.expect("ok");
        assert_eq!((stats.gen_tokens, stats.finish), (3, FinishReason::Stop));
        // 紧凑 JSON 无空白 → 单词元。
        let (text, pieces, _) = generate(
            &Turn::user("{\"title\":\"x\"}"),
            &GenerateOptions::default(),
        );
        assert_eq!((text.as_str(), pieces.len()), ("{\"title\":\"x\"}", 1));
        // 没有 user 消息 → 取最后一条；空串 → 零词元正常完结。
        let turn = Turn {
            messages: vec![Message {
                role: Role::System,
                content: "only system".to_owned(),
            }],
        };
        assert_eq!(
            generate(&turn, &GenerateOptions::default()).0,
            "only system"
        );
        let (text, _, stats) = generate(&Turn::user(""), &GenerateOptions::default());
        assert_eq!((text.as_str(), stats.expect("ok").gen_tokens), ("", 0));
        // max_tokens 仍生效（截断走 Length）。
        let options = GenerateOptions {
            max_tokens: 1,
            ..GenerateOptions::default()
        };
        let (text, _, stats) = generate(&Turn::user("a b c"), &options);
        assert_eq!(
            (text.as_str(), stats.expect("ok").finish),
            ("a ", FinishReason::Length)
        );
    }
}
