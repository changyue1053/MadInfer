//! 后端替身：[`NoopBackend`]（一切成功、零输出）与 [`FakeBackend`]（脚本化 token 流，可注入延迟 / 错误 / 崩溃 / 可用性变化，记调用台账）。
//!
//! 两者都遵守后端六条：取消后至多再产出 0 个 token 即返回 `Cancelled`；并发上限按 `caps.concurrency` 返回 `Busy`；
//! `caps.constraint == None` 时传入约束返回 `Unsupported`。夹具允许 `RwLock`（imcore 规则第 7 条豁免形态四）。

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use madinfer_backend::{
    Availability, Backend, BackendCaps, BackendError, CancelToken, Constraint, ConstraintSupport,
    Context, ContextOptions, FinishReason, GenerateOptions, GenerateStats, LoadOpts,
    MemoryFootprint, Model, ModelFormat, ModelMeta, Quant, Role, SinkControl, TokenPiece,
    TokenSink, Turn,
};
use madinfer_core_types::BackendId;

/// 替身缺省能力：CPU 槽、并发 1、可中止、无约束、4 K 上下文。
pub const fn default_caps() -> BackendCaps {
    BackendCaps {
        slot: madinfer_core_types::SlotClass::Cpu,
        max_context_tokens: 4_096,
        concurrency: 1,
        supports_abort: true,
        constraint: ConstraintSupport::None,
        tools: false,
        image_input: false,
        audio_input: false,
    }
}

/// 并发席位：打开上下文时占一席，Drop 归还；超出 `concurrency` 返回 `Busy`。
struct ContextSlot {
    live: Arc<AtomicU8>,
}

impl ContextSlot {
    fn acquire(live: &Arc<AtomicU8>, limit: u8) -> Result<Self, BackendError> {
        let mut current = live.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return Err(BackendError::Busy);
            }
            match live.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(Self {
                        live: Arc::clone(live),
                    })
                }
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for ContextSlot {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------
// NoopBackend
// ---------------------------------------------------------------------------

/// 一切成功、零输出的后端：`generate` 不调 sink，立即返回空统计。
pub struct NoopBackend {
    id: BackendId,
    caps: BackendCaps,
}

impl NoopBackend {
    /// 缺省能力。
    pub fn new(id: &str) -> Self {
        Self::with_caps(id, default_caps())
    }

    /// 指定能力。
    pub fn with_caps(id: &str, caps: BackendCaps) -> Self {
        Self {
            id: BackendId(id.to_owned()),
            caps,
        }
    }
}

impl Backend for NoopBackend {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn probe(&self) -> BackendCaps {
        self.caps
    }

    fn availability(&self) -> Availability {
        Availability::Available
    }

    fn load(&self, _opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError> {
        Ok(Box::new(NoopModel {
            caps: self.caps,
            live: Arc::new(AtomicU8::new(0)),
        }))
    }
}

struct NoopModel {
    caps: BackendCaps,
    live: Arc<AtomicU8>,
}

impl Model for NoopModel {
    fn meta(&self) -> ModelMeta {
        ModelMeta {
            params_m: 0,
            max_context_tokens: self.caps.max_context_tokens,
            format: ModelFormat::SystemManaged,
            quant: Quant::Unquantized,
        }
    }

    fn footprint(&self) -> MemoryFootprint {
        MemoryFootprint::default()
    }

    fn open_context(&self, _options: &ContextOptions) -> Result<Box<dyn Context>, BackendError> {
        let slot = ContextSlot::acquire(&self.live, self.caps.concurrency)?;
        Ok(Box::new(NoopContext { _slot: slot }))
    }

    fn unload(self: Box<Self>) {}
}

struct NoopContext {
    _slot: ContextSlot,
}

impl Context for NoopContext {
    fn generate(
        &mut self,
        _turn: &Turn,
        _options: &GenerateOptions,
        _constraint: Option<&Constraint>,
        _sink: &mut dyn TokenSink,
        cancel: &CancelToken,
    ) -> Result<GenerateStats, BackendError> {
        if cancel.is_cancelled() {
            return Err(BackendError::Cancelled);
        }
        Ok(GenerateStats {
            prompt_tokens: 0,
            gen_tokens: 0,
            finish: FinishReason::Stop,
        })
    }
}

// ---------------------------------------------------------------------------
// FakeBackend
// ---------------------------------------------------------------------------

/// 脚本：token 序列、在第 N 片前报错 / 崩溃、片间延迟、占用与可用性。
///
/// `fail_at` / `crash_at` 的下标 = 出错前已产出的 token 数（`Some((0, e))` = 一个 token 都不出就失败；
/// 等于 `tokens.len()` = 全部产出后再失败）。
///
/// `echo_prompt`（madai-w10 I-C-09 J5-a，ffi 调试建站选项 `debug_script_backend` 的脚本源）：置位时忽略 `tokens`，
/// 每次 `generate` 以**最后一条 user 消息原文**（无 user 消息则最后一条消息）为输出——按空白切词元、空白归前一词元
/// （`split_inclusive(char::is_whitespace)`），拼接逐字节等于原文；紧凑 JSON 即单词元。调用方靠 prompt 决定后端产出，
/// 例如给结构化请求送"合法 JSON 但缺 required 字段"的 prompt 即可确定性造出终态校验不过（2003 携 partial）。
/// `fail_at` / `crash_at` / `token_delay_ms` / `max_tokens` 截断照常按词元下标生效。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeScript {
    /// 逐片文本（`echo_prompt` 置位时忽略）。
    pub tokens: Vec<String>,
    /// 以最后一条 user 消息为输出（见类型注）。
    pub echo_prompt: bool,
    /// 在第 N 片前返回该错误。
    pub fail_at: Option<(usize, BackendError)>,
    /// 在第 N 片前返回 `Crashed`。
    pub crash_at: Option<usize>,
    /// 片间延迟（worker 线程 `thread::sleep`，不读墙钟；madai-w12 I-C-11 起分 ≤ 5 ms 切片等待、取消旗翻起即停等，
    /// 并可经 [`FakeBackend::set_token_delay_ms`] 在线改——ffi 调试入口 `debug_set_token_delay_ms` 的落点）。
    pub token_delay_ms: u64,
    /// `footprint()` 报告值。
    pub footprint: MemoryFootprint,
    /// `availability()` 报告值。
    pub availability: Availability,
    /// 统计里报告的 prompt token 数。
    pub prompt_tokens: u32,
    /// `Backend::context_kv_mb` / `Model::context_kv_mb` 报告值（madai-w16 I-C-14；缺省 0 = 不申报，池只记亲和与命中）。
    pub kv_per_context_mb: u32,
}

impl Default for FakeScript {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            echo_prompt: false,
            fail_at: None,
            crash_at: None,
            token_delay_ms: 0,
            footprint: MemoryFootprint {
                resident_mb: 120,
                mapped_mb: 1_843,
            },
            availability: Availability::Available,
            prompt_tokens: 0,
            kv_per_context_mb: 0,
        }
    }
}

impl FakeScript {
    /// 只给 token 序列，其余缺省。
    pub fn tokens<S: AsRef<str>>(tokens: &[S]) -> Self {
        Self {
            tokens: tokens.iter().map(|t| t.as_ref().to_owned()).collect(),
            ..Self::default()
        }
    }

    /// 以最后一条 user 消息原文为输出（词元切分口径见类型注），其余缺省。
    pub fn echo_prompt() -> Self {
        Self {
            echo_prompt: true,
            ..Self::default()
        }
    }

    /// 本次 `generate` 的词元序列：`echo_prompt` 置位时由 `turn` 切出，否则用 `tokens`。
    fn tokens_for(&self, turn: &Turn) -> Vec<String> {
        if !self.echo_prompt {
            return self.tokens.clone();
        }
        turn.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .or(turn.messages.last())
            .map(|m| {
                m.content
                    .split_inclusive(char::is_whitespace)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 在第 N 片前报错。
    pub fn failing_at(mut self, index: usize, error: BackendError) -> Self {
        self.fail_at = Some((index, error));
        self
    }

    /// 在第 N 片前崩溃。
    pub fn crashing_at(mut self, index: usize) -> Self {
        self.crash_at = Some(index);
        self
    }

    /// 片间延迟。
    pub fn with_token_delay_ms(mut self, ms: u64) -> Self {
        self.token_delay_ms = ms;
        self
    }

    /// 占用。
    pub fn with_footprint(mut self, footprint: MemoryFootprint) -> Self {
        self.footprint = footprint;
        self
    }

    /// 可用性。
    pub fn with_availability(mut self, availability: Availability) -> Self {
        self.availability = availability;
        self
    }

    /// 每上下文 KV 读数（madai-w16 I-C-14）。
    pub fn with_kv_per_context_mb(mut self, mb: u32) -> Self {
        self.kv_per_context_mb = mb;
        self
    }
}

/// 调用台账（原子计数；跨线程可读）。
#[derive(Debug, Default)]
pub struct FakeLedger {
    /// `probe` 次数。
    pub probe_calls: AtomicU64,
    /// `load` 次数。
    pub load_calls: AtomicU64,
    /// `open_context` 次数（含被 Busy 拒绝的）。
    pub open_context_calls: AtomicU64,
    /// 成功打开的上下文数（madai-w16 I-C-14；与 [`Self::contexts_dropped`] 之差 = 此刻活着的上下文句柄数）。
    pub contexts_opened: AtomicU64,
    /// 已被 Drop 的上下文句柄数（madai-w16 I-C-14：worker 驻留 / 真释放的后端侧读数——句柄 Drop 即 KV 与席位归还）。
    pub contexts_dropped: AtomicU64,
    /// `generate` 次数。
    pub generate_calls: AtomicU64,
    /// 因取消返回 `Cancelled` 的次数。
    pub cancelled_count: AtomicU64,
    /// 产出的 token 片总数。
    pub tokens_emitted: AtomicU64,
    /// `unload` 次数。
    pub unload_calls: AtomicU64,
}

impl FakeLedger {
    /// 此刻活着的上下文句柄数（成功打开 − 已 Drop）。
    pub fn contexts_live(&self) -> u64 {
        self.contexts_opened
            .load(Ordering::SeqCst)
            .saturating_sub(self.contexts_dropped.load(Ordering::SeqCst))
    }
}

/// 脚本化后端。共享脚本：`set_script` / `set_availability` 对之后的调用生效（含已加载的模型）。
pub struct FakeBackend {
    id: BackendId,
    caps: BackendCaps,
    script: Arc<RwLock<FakeScript>>,
    ledger: Arc<FakeLedger>,
}

impl FakeBackend {
    /// 缺省能力 + 脚本。
    pub fn new(id: &str, script: FakeScript) -> Self {
        Self {
            id: BackendId(id.to_owned()),
            caps: default_caps(),
            script: Arc::new(RwLock::new(script)),
            ledger: Arc::new(FakeLedger::default()),
        }
    }

    /// 改能力声明（并发数、约束支持…）。
    pub fn with_caps(mut self, caps: BackendCaps) -> Self {
        self.caps = caps;
        self
    }

    /// 台账句柄（后端进注册表后仍可断言）。
    pub fn ledger(&self) -> Arc<FakeLedger> {
        Arc::clone(&self.ledger)
    }

    /// `generate` 次数。
    pub fn generate_calls(&self) -> u64 {
        self.ledger.generate_calls.load(Ordering::SeqCst)
    }

    /// 因取消返回的次数。
    pub fn cancelled_count(&self) -> u64 {
        self.ledger.cancelled_count.load(Ordering::SeqCst)
    }

    /// 注入可用性变化。
    pub fn set_availability(&self, availability: Availability) {
        if let Ok(mut s) = self.script.write() {
            s.availability = availability;
        }
    }

    /// 换脚本。
    pub fn set_script(&self, script: FakeScript) {
        if let Ok(mut s) = self.script.write() {
            *s = script;
        }
    }

    /// 只改片间延迟（madai-w12 I-C-11：ffi 调试入口 `debug_set_token_delay_ms` 的落点）：对之后开始的 `generate` 生效，
    /// 进行中的一轮在下一片前读到新值；脚本其余字段不动。
    pub fn set_token_delay_ms(&self, ms: u64) {
        if let Ok(mut s) = self.script.write() {
            s.token_delay_ms = ms;
        }
    }

    /// 当前片间延迟。
    pub fn token_delay_ms(&self) -> u64 {
        self.script().token_delay_ms
    }

    fn script(&self) -> FakeScript {
        self.script.read().map(|s| s.clone()).unwrap_or_default()
    }
}

/// 片间延迟的等待切片：每 `TOKEN_DELAY_SLICE_MS` 查一次取消旗，翻旗即停等（不读墙钟，只数切片）。
const TOKEN_DELAY_SLICE_MS: u64 = 5;

/// 片间延迟（madai-w12 I-C-11 起可被取消打断）：`ms` 毫秒分成 ≤ 5 ms 的切片逐段 `thread::sleep`，每段前查
/// `cancel`——翻旗即返回，调用方随后按六条之①以 `Cancelled` 收口，取消响应不再受大延迟拖长。
fn wait_between_pieces(ms: u64, cancel: &CancelToken) {
    let mut remaining = ms;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return;
        }
        let step = remaining.min(TOKEN_DELAY_SLICE_MS);
        thread::sleep(Duration::from_millis(step));
        remaining -= step;
    }
}

impl Backend for FakeBackend {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn probe(&self) -> BackendCaps {
        self.ledger.probe_calls.fetch_add(1, Ordering::SeqCst);
        self.caps
    }

    fn availability(&self) -> Availability {
        self.script().availability
    }

    fn load(&self, _opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError> {
        self.ledger.load_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeModel {
            caps: self.caps,
            script: Arc::clone(&self.script),
            ledger: Arc::clone(&self.ledger),
            live: Arc::new(AtomicU8::new(0)),
        }))
    }

    fn context_kv_mb(&self) -> u32 {
        self.script().kv_per_context_mb
    }
}

struct FakeModel {
    caps: BackendCaps,
    script: Arc<RwLock<FakeScript>>,
    ledger: Arc<FakeLedger>,
    live: Arc<AtomicU8>,
}

impl Model for FakeModel {
    fn meta(&self) -> ModelMeta {
        ModelMeta {
            params_m: 500,
            max_context_tokens: self.caps.max_context_tokens,
            format: ModelFormat::Gguf,
            quant: Quant::Q4Km,
        }
    }

    fn footprint(&self) -> MemoryFootprint {
        self.script.read().map(|s| s.footprint).unwrap_or_default()
    }

    fn open_context(&self, _options: &ContextOptions) -> Result<Box<dyn Context>, BackendError> {
        self.ledger
            .open_context_calls
            .fetch_add(1, Ordering::SeqCst);
        let slot = ContextSlot::acquire(&self.live, self.caps.concurrency)?;
        self.ledger.contexts_opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeContext {
            caps: self.caps,
            script: Arc::clone(&self.script),
            ledger: Arc::clone(&self.ledger),
            _slot: slot,
        }))
    }

    fn unload(self: Box<Self>) {
        self.ledger.unload_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn context_kv_mb(&self) -> u32 {
        self.script
            .read()
            .map(|s| s.kv_per_context_mb)
            .unwrap_or_default()
    }
}

struct FakeContext {
    caps: BackendCaps,
    script: Arc<RwLock<FakeScript>>,
    ledger: Arc<FakeLedger>,
    _slot: ContextSlot,
}

impl Drop for FakeContext {
    fn drop(&mut self) {
        // 句柄 Drop = 后端 KV 与席位归还（madai-w16 I-C-14 台账：worker 是否真释放据此断言）。
        self.ledger.contexts_dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl Context for FakeContext {
    fn generate(
        &mut self,
        turn: &Turn,
        options: &GenerateOptions,
        constraint: Option<&Constraint>,
        sink: &mut dyn TokenSink,
        cancel: &CancelToken,
    ) -> Result<GenerateStats, BackendError> {
        self.ledger.generate_calls.fetch_add(1, Ordering::SeqCst);
        let script = self.script.read().map(|s| s.clone()).unwrap_or_default();
        if constraint.is_some() && self.caps.constraint == ConstraintSupport::None {
            return Err(BackendError::Unsupported);
        }
        if turn.messages.is_empty() {
            return Err(BackendError::InvalidInput);
        }
        let tokens = script.tokens_for(turn);
        let limit = tokens
            .len()
            .min(usize::try_from(options.max_tokens).unwrap_or(usize::MAX));
        let mut emitted = 0u32;
        for (i, text) in tokens.iter().take(limit).enumerate() {
            if i > 0 {
                // 片间延迟每片重读（set_token_delay_ms 对进行中的一轮在下一片前生效），等待可被取消打断。
                let delay = self
                    .script
                    .read()
                    .map(|s| s.token_delay_ms)
                    .unwrap_or(script.token_delay_ms);
                if delay > 0 {
                    wait_between_pieces(delay, cancel);
                }
            }
            // 六条之①：翻旗后不再产出任何 token。
            if cancel.is_cancelled() {
                self.ledger.cancelled_count.fetch_add(1, Ordering::SeqCst);
                return Err(BackendError::Cancelled);
            }
            if script.crash_at == Some(i) {
                return Err(BackendError::Crashed);
            }
            if let Some((at, error)) = script.fail_at {
                if at == i {
                    return Err(error);
                }
            }
            emitted += 1;
            self.ledger.tokens_emitted.fetch_add(1, Ordering::SeqCst);
            let control = sink.on_token(TokenPiece {
                text: text.clone(),
                tokens: 1,
                is_last: i + 1 == limit,
            });
            if control == SinkControl::Stop {
                return Ok(GenerateStats {
                    prompt_tokens: script.prompt_tokens,
                    gen_tokens: emitted,
                    finish: FinishReason::SinkStopped,
                });
            }
        }
        if cancel.is_cancelled() && limit < tokens.len() {
            self.ledger.cancelled_count.fetch_add(1, Ordering::SeqCst);
            return Err(BackendError::Cancelled);
        }
        if script.crash_at == Some(limit) {
            return Err(BackendError::Crashed);
        }
        if let Some((at, error)) = script.fail_at {
            if at == limit {
                return Err(error);
            }
        }
        Ok(GenerateStats {
            prompt_tokens: script.prompt_tokens,
            gen_tokens: emitted,
            finish: if limit < tokens.len() {
                FinishReason::Length
            } else {
                FinishReason::Stop
            },
        })
    }
}

/// 收集式 sink：拼接文本、记片数；可在第 N 片翻取消旗或回 `Stop`。
#[derive(Debug, Default)]
pub struct CollectingSink {
    /// 收到的片。
    pub pieces: Vec<TokenPiece>,
    /// 在收到第 N 片时（1 起）对该令牌翻旗。
    pub cancel_on_piece: Option<(usize, CancelToken)>,
    /// 在收到第 N 片时（1 起）回 `Stop`。
    pub stop_on_piece: Option<usize>,
}

impl CollectingSink {
    /// 空 sink。
    pub fn new() -> Self {
        Self::default()
    }

    /// 拼接文本。
    pub fn text(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }
}

impl TokenSink for CollectingSink {
    fn on_token(&mut self, piece: TokenPiece) -> SinkControl {
        self.pieces.push(piece);
        let n = self.pieces.len();
        if let Some((at, token)) = &self.cancel_on_piece {
            if *at == n {
                token.cancel(madinfer_backend::CancelReason::Caller);
            }
        }
        if self.stop_on_piece == Some(n) {
            SinkControl::Stop
        } else {
            SinkControl::Continue
        }
    }
}
