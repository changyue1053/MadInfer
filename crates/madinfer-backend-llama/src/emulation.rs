//! 确定性仿真核：seed + 输入词元 → 确定性 token 流（回显式），无真实引擎。
//!
//! 生成计划在 `generate` 开头一次算好（词元回显 + seed 决定的起始旋转 + 括标首尾词元；带约束时改为按 GBNF 走语法
//! 产出——[`crate::grammar`]，madai-w08 G4 Emulated 约束模式），先按 `max_tokens` 与停止串截断（停止串按"拼接文本首次
//! 包含停止串的那一词元整个不发"，跨词元匹配，llama 语义的保守近似；`max_tokens` 截断报 `Length`），再把截断后的全文
//! **按输出字节数确定性切片**（[`EMULATED_PIECE_BYTES`]，madai-w12 I-C-11 / R-w11-5）逐片发射：
//! 每片发射前查一次 [`CancelToken`]，翻旗后不再发射任何片即返回 `Cancelled`（六条之①的 0 片形态）；
//! [`GenerateStats`] 按实际发射如实报告——`gen_tokens` 仍是词元数（词元计数随片携带，不随切片变）。
//! 占用模型：驻留 = 权重驻留 + 每上下文 KV × 活跃上下文数；mmap 关闭时映射 = 驻留（映射恒 ≥ 驻留）。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use madinfer_backend::{
    BackendError, CancelToken, Constraint, Context, ContextOptions, FinishReason, GenerateOptions,
    GenerateStats, MemoryFootprint, Model, ModelFormat, ModelMeta, Quant, SinkControl, TokenPiece,
    TokenSink, Turn,
};

use crate::grammar::Grammar;
use crate::LlamaOptions;

/// 仿真元数据：报告为 0.5B GGUF Q4_K_M（与设计文档 C7"0.5B 测试模型跑金样"的口径同型）。
const EMULATED_PARAMS_M: u32 = 500;

/// 仿真核切片字节数 N（madai-w12 I-C-11，R-w11-5"仿真后端切帧随调度时序变"）：整轮输出文本（已按 `max_tokens` /
/// 停止串截断）按输出字节数确定性切片——每片 ≤ N 字节、只在完整 UTF-8 码点边界切（ASCII 恰 N；多字节码点不拆，故可短于 N）；
/// 一片的 `tokens` = 起点落在本片内的词元数，全轮之和 = 词元数（`gen_tokens` 不随切片变）。片序列是输出文本的纯函数，
/// 与 worker / control 线程的调度时序无关。核内 80 ms 合帧（madinfer-api `Framer`）仍在：输出 ≤ 2N 字节（≤ 2 片）时 delta
/// 帧数 = 片数（首片立即放行、第二片或折叠进终态强制放行或窗口过后放行，两路都恰 1 帧），逐帧文本可钉；更长输出首帧 = 首片、
/// 帧数 ∈ [2, 片数] 随窗口——宿主断言帧数只准在 ≤ 2N 字节的输出上做（`madinfer.h` §5b"仿真核切片"条同文）。
/// 取值论证：≥ 4 才能让任一码点独占一片；32 字节 ≈ 8 个 ASCII 词元 / 10 个 CJK 字——三端自检 `self-check` × max_tokens 8
/// 的输出「self-check」16 字节恰 1 片 1 帧，ffi 端到端「总结一下今天的群消息」36 字节恰 2 片 2 帧。改值只改本常量与用例。
pub const EMULATED_PIECE_BYTES: usize = 32;

// 编译期钉住下界：任一 UTF-8 码点（≤ 4 字节）都能独占一片，切片器不会被迫拆码点。
const _: () = assert!(EMULATED_PIECE_BYTES >= 4);

/// 并发席位：打开上下文占一席，Drop 归还；超出上限返回 `Busy`（仿真核与真实引擎路径共用）。
pub(crate) struct ContextSlot {
    live: Arc<AtomicU8>,
}

impl ContextSlot {
    pub(crate) fn acquire(live: &Arc<AtomicU8>, limit: u8) -> Result<Self, BackendError> {
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

/// 仿真模型：占用模型 + 并发席位；无真实权重。
pub(crate) struct EmulatedModel {
    options: LlamaOptions,
    /// 加载参数里 mmap 的取舍（决定映射口径）。
    mmap: bool,
    live: Arc<AtomicU8>,
}

impl EmulatedModel {
    pub(crate) fn new(options: LlamaOptions, mmap: bool) -> Self {
        Self {
            options,
            mmap,
            live: Arc::new(AtomicU8::new(0)),
        }
    }
}

impl Model for EmulatedModel {
    fn meta(&self) -> ModelMeta {
        ModelMeta {
            params_m: EMULATED_PARAMS_M,
            max_context_tokens: self.options.max_context_tokens,
            format: ModelFormat::Gguf,
            quant: Quant::Q4Km,
        }
    }

    fn footprint(&self) -> MemoryFootprint {
        let live = u32::from(self.live.load(Ordering::Acquire));
        let kv = self.options.kv_per_context_mb.saturating_mul(live);
        let resident_base = self.options.weights_resident_mb;
        let mapped_base = if self.mmap {
            self.options.weights_mapped_mb.max(resident_base)
        } else {
            resident_base
        };
        MemoryFootprint {
            resident_mb: resident_base.saturating_add(kv),
            mapped_mb: mapped_base.saturating_add(kv),
        }
    }

    fn open_context(&self, _options: &ContextOptions) -> Result<Box<dyn Context>, BackendError> {
        let slot = ContextSlot::acquire(&self.live, self.options.concurrency)?;
        Ok(Box::new(EmulatedContext { _slot: slot }))
    }

    fn unload(self: Box<Self>) {}

    fn context_kv_mb(&self) -> u32 {
        // 与 footprint 的 KV 项同一常数（madai-w16 I-C-14）：owner 按它记账，池"已用"= 本值 × 驻留上下文数，与占用模型一致。
        self.options.kv_per_context_mb
    }
}

pub(crate) struct EmulatedContext {
    _slot: ContextSlot,
}

impl Context for EmulatedContext {
    fn generate(
        &mut self,
        turn: &Turn,
        options: &GenerateOptions,
        constraint: Option<&Constraint>,
        sink: &mut dyn TokenSink,
        cancel: &CancelToken,
    ) -> Result<GenerateStats, BackendError> {
        validate(turn, options)?;
        // 声明与行为一致：caps.constraint == Emulated——有约束按语法产出（madai-w08 G4），无约束回显。
        let plan = match constraint {
            Some(constraint) => EmulatedPlan::build_constrained(turn, options, constraint)?,
            None => EmulatedPlan::build(turn, options),
        };
        // 词元序列 → 按字节数切片（madai-w12 I-C-11）：片边界只由输出文本决定，词元计数随片携带。
        let pieces = split_by_bytes(&plan.pieces, EMULATED_PIECE_BYTES);
        let total = pieces.len();
        let mut emitted: u32 = 0;
        for (i, (text, tokens)) in pieces.into_iter().enumerate() {
            // 每片发射前查一次取消旗：翻旗后 0 片再出，直接返回 Cancelled（六条之①）。
            if cancel.is_cancelled() {
                return Err(BackendError::Cancelled);
            }
            emitted = emitted.saturating_add(tokens);
            let control = sink.on_token(TokenPiece {
                text,
                tokens,
                is_last: i + 1 == total,
            });
            if control == SinkControl::Stop {
                return Ok(GenerateStats {
                    prompt_tokens: plan.prompt_tokens,
                    gen_tokens: emitted,
                    finish: FinishReason::SinkStopped,
                });
            }
        }
        // 计划因 max_tokens 截断且此刻已翻旗：仍有未出内容，按取消收尾（与 testkit FakeBackend 同口径）。
        if cancel.is_cancelled() && plan.finish == FinishReason::Length {
            return Err(BackendError::Cancelled);
        }
        Ok(GenerateStats {
            prompt_tokens: plan.prompt_tokens,
            gen_tokens: emitted,
            finish: plan.finish,
        })
    }
}

/// 形式校验：空消息、越界采样参数、停止串超额都是 `InvalidInput`（§7.3 表 3009 行；两条路径共用）。
pub(crate) fn validate(turn: &Turn, options: &GenerateOptions) -> Result<(), BackendError> {
    if turn.messages.is_empty() {
        return Err(BackendError::InvalidInput);
    }
    if options.temperature_x100 > 200 || options.top_p_bp > 10_000 || options.stop.len() > 8 {
        return Err(BackendError::InvalidInput);
    }
    Ok(())
}

/// 一轮生成的确定性计划：先算全量片序列，再按 max_tokens 与停止串截断。
struct EmulatedPlan {
    pieces: Vec<String>,
    prompt_tokens: u32,
    finish: FinishReason,
}

impl EmulatedPlan {
    fn build(turn: &Turn, options: &GenerateOptions) -> Self {
        let words = echo_words(turn);
        let prompt_tokens = words_len_u32(words.len()) + words_len_u32(turn.messages.len());

        // seed + 输入词元 → 确定性起始旋转（同输入同 seed 同输出；seed 缺省 0）。
        let seed = options.seed.unwrap_or(0);
        let state = mix64(seed ^ fold_words(&words));
        let mut pieces: Vec<String> = Vec::with_capacity(words.len() + 2);
        pieces.push("「".to_owned());
        if !words.is_empty() {
            let rotation = usize::try_from(state % words_len_u64(words.len())).unwrap_or(0);
            for k in 0..words.len() {
                let word = words[(rotation + k) % words.len()];
                if k == 0 {
                    pieces.push(word.to_owned());
                } else {
                    pieces.push(format!(" {word}"));
                }
            }
        }
        pieces.push("」".to_owned());
        Self::truncate(pieces, prompt_tokens, options)
    }

    /// Emulated 约束模式（madai-w08 G4）：按 GBNF 走出一份必然被语法接受的文本（schema2gbnf 的语法即合法 JSON），
    /// 分支 / 重复由 seed + 输入词元决定，字符串内容优先取回显词元；语法编不过报 `InvalidInput`。
    /// 之后与回显同样过 max_tokens 与停止串截断（截断后的文本可能不再合法——由核侧 schema 校验兜住）。
    fn build_constrained(
        turn: &Turn,
        options: &GenerateOptions,
        constraint: &Constraint,
    ) -> Result<Self, BackendError> {
        let grammar =
            Grammar::parse(&constraint.grammar).map_err(|_| BackendError::InvalidInput)?;
        let words = echo_words(turn);
        let prompt_tokens = words_len_u32(words.len()) + words_len_u32(turn.messages.len());
        let seed = options.seed.unwrap_or(0);
        let state = mix64(seed ^ fold_words(&words));
        let pieces = grammar
            .walk(state, &words)
            .map_err(|_| BackendError::InvalidInput)?;
        Ok(Self::truncate(pieces, prompt_tokens, options))
    }

    fn truncate(mut pieces: Vec<String>, prompt_tokens: u32, options: &GenerateOptions) -> Self {
        // max_tokens 截断。
        let cap = usize::try_from(options.max_tokens).unwrap_or(usize::MAX);
        let mut finish = FinishReason::Stop;
        if pieces.len() > cap {
            pieces.truncate(cap);
            finish = FinishReason::Length;
        }

        // 停止串：拼接文本首次包含任一停止串的那一片整片不发（跨片匹配；空停止串忽略）。
        if !options.stop.is_empty() {
            let mut joined = String::new();
            let mut cut = None;
            for (i, piece) in pieces.iter().enumerate() {
                joined.push_str(piece);
                if options
                    .stop
                    .iter()
                    .any(|s| !s.is_empty() && joined.contains(s))
                {
                    cut = Some(i);
                    break;
                }
            }
            if let Some(i) = cut {
                pieces.truncate(i);
                finish = FinishReason::Stop;
            }
        }

        Self {
            pieces,
            prompt_tokens,
            finish,
        }
    }
}

/// 词元序列按输出字节数切片（[`EMULATED_PIECE_BYTES`] 口径）：返回 `(片文本, 起点落在片内的词元数)`。
/// 每片 ≤ `limit` 字节且只在完整码点边界切；单个码点宽于 `limit` 时独占一片（`limit` ≥ 4 时不会发生）。
/// 空词元（当前两条计划都不产）计入所在位置的片；`pieces` 为空 → 空序列。各片 `tokens` 之和恒 = `pieces.len()`。
fn split_by_bytes(pieces: &[String], limit: usize) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut text = String::new();
    let mut tokens: u32 = 0;
    for piece in pieces {
        let mut starts_here = true;
        for ch in piece.chars() {
            if !text.is_empty() && text.len() + ch.len_utf8() > limit {
                out.push((std::mem::take(&mut text), tokens));
                tokens = 0;
            }
            if starts_here {
                tokens = tokens.saturating_add(1);
                starts_here = false;
            }
            text.push(ch);
        }
        if starts_here {
            // 空词元：起点落在当前片。
            tokens = tokens.saturating_add(1);
        }
    }
    if !text.is_empty() {
        out.push((text, tokens));
    } else if tokens > 0 {
        // 只剩空词元的计数：并进上一片；没有上一片则单独一片空文本（与旧口径"空片也是一片"同）。
        match out.last_mut() {
            Some(last) => last.1 = last.1.saturating_add(tokens),
            None => out.push((String::new(), tokens)),
        }
    }
    out
}

/// 回显词汇：全部消息按空白切词（系统 / 用户 / 助手同池）。
fn echo_words(turn: &Turn) -> Vec<&str> {
    turn.messages
        .iter()
        .flat_map(|m| m.content.split_whitespace())
        .collect()
}

fn words_len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn words_len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX).max(1)
}

/// splitmix64 终混（确定性、平台无关）。
fn mix64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x
}

/// 输入词元折叠为 64 位（顺序敏感；仿真核内部混合，不承诺跨版本稳定，金样锁行为）。
fn fold_words(words: &[&str]) -> u64 {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
    for word in words {
        for b in word.as_bytes() {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
        h = h.wrapping_add(0x9E37_79B9_7F4A_7C15);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use madinfer_backend::{Message, Role};

    fn turn(content: &str) -> Turn {
        Turn::user(content)
    }

    #[test]
    fn plan_is_deterministic_for_same_seed_and_input() {
        let options = GenerateOptions {
            seed: Some(7),
            ..GenerateOptions::default()
        };
        let a = EmulatedPlan::build(&turn("alpha beta gamma"), &options);
        let b = EmulatedPlan::build(&turn("alpha beta gamma"), &options);
        assert_eq!(a.pieces, b.pieces);
        assert_eq!(a.prompt_tokens, 4, "3 词元 + 1 条消息");
        assert_eq!(a.finish, FinishReason::Stop);
    }

    #[test]
    fn whitespace_only_content_yields_bracket_pair() {
        let plan = EmulatedPlan::build(&turn("   "), &GenerateOptions::default());
        assert_eq!(plan.pieces, vec!["「".to_owned(), "」".to_owned()]);
        assert_eq!(plan.prompt_tokens, 1, "0 词元 + 1 条消息");
    }

    #[test]
    fn system_words_join_the_echo_vocabulary() {
        let mixed = Turn {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "sys".to_owned(),
                },
                Message {
                    role: Role::User,
                    content: "ask".to_owned(),
                },
            ],
        };
        let plan = EmulatedPlan::build(&mixed, &GenerateOptions::default());
        assert_eq!(plan.pieces.len(), 4, "「 + 2 词元 + 」");
        assert_eq!(plan.prompt_tokens, 4, "2 词元 + 2 条消息");
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn split_by_bytes_cuts_ascii_at_exactly_n_and_keeps_token_counts() {
        // 4 词元 25 字节，N = 8 → 8 / 8 / 8 / 1；词元起点 0 / 6 / 11 / 17 → 片内词元数 2 / 1 / 1 / 0，和 = 4。
        let pieces = words(&["alpha ", "beta ", "gamma ", "deltaxxx"]);
        let split = split_by_bytes(&pieces, 8);
        let texts: Vec<&str> = split.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, vec!["alpha be", "ta gamma", " deltaxx", "x"]);
        let tokens: Vec<u32> = split.iter().map(|(_, n)| *n).collect();
        assert_eq!(tokens, vec![2, 1, 1, 0]);
        assert_eq!(tokens.iter().sum::<u32>(), 4);
        assert_eq!(
            split.iter().map(|(t, _)| t.as_str()).collect::<String>(),
            pieces.concat()
        );
    }

    #[test]
    fn split_by_bytes_never_splits_a_code_point_and_can_be_shorter_than_n() {
        // 「 + 10 个 CJK 字 + 」 = 36 字节，N = 32 → 30（10 个 3 字节码点）+ 6；词元 3 个：起点 0 / 3 落首片，33 落次片。
        let pieces = words(&["「", "总结一下今天的群消息", "」"]);
        let split = split_by_bytes(&pieces, 32);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].0, "「总结一下今天的群消");
        assert_eq!(split[0].0.len(), 30, "多字节码点不拆：短于 N");
        assert_eq!(split[1].0, "息」");
        assert_eq!((split[0].1, split[1].1), (2, 1));
        // 单个码点宽于 N（N = 2）：独占一片，不 panic、不拆字节。
        let wide = split_by_bytes(&words(&["端"]), 2);
        assert_eq!(wide, vec![("端".to_owned(), 1)]);
        assert!(split_by_bytes(&[], 32).is_empty());
    }

    #[test]
    fn split_by_bytes_is_a_pure_function_of_the_text() {
        // 同一文本、不同词元切分 → 同一片边界，只有 tokens 的归属不同；和恒为词元数。
        let a = split_by_bytes(&words(&["ab", "cd", "ef", "gh"]), 3);
        let b = split_by_bytes(&words(&["abcdefgh"]), 3);
        let texts = |s: &[(String, u32)]| s.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&a), texts(&b));
        assert_eq!(texts(&a), vec!["abc", "def", "gh"]);
        assert_eq!(a.iter().map(|(_, n)| *n).sum::<u32>(), 4);
        assert_eq!(b.iter().map(|(_, n)| *n).sum::<u32>(), 1);
    }
}
