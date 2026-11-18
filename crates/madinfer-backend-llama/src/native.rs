//! 真实引擎路径（cargo feature `llama-sys`）：[`NativeModel`] / [`NativeContext`] 用 [`crate::sys`] 的两个 RAII 句柄
//! 实现 `Model` / `Context`，走六条契约的同一条链：加载 → 词元化 → prefill → greedy 逐词元解码 → 逐片回调 → 取消 → 释放。
//!
//! spike 口径（madai-w16 I-C-15，报告 §1）：
//! - 采样恒 greedy（argmax）：同模型 + 同 prompt → 同词元序列；`temperature_x100` / `top_p_bp` / `seed` 只过形式校验不生效
//!   （真采样链、GBNF 采样器归 I4，故 `probe().constraint == None`，传入约束返回 `Unsupported`）；
//! - prompt = 各消息 `content` 以 `\n` 拼接（无角色标记、无对话模板——模板归 I4）；
//! - 每轮清 KV 重新 prefill（不复用前缀；驻留复用归 I-C-14 的 `ContextKey` 线）；
//! - 片发射延后一词元：只有看见下一词元（或终止）才知道前一片是否最后一片，`is_last` 因此如实；
//!   取消在每词元前查旗——翻旗后 0 片再出、返回 `Cancelled`（六条之①②）；
//! - 只发射完整 UTF-8 码点：词元字节先入缓冲，取最长合法前缀；结尾残字节按 lossy 收尾（不丢词元计数）；
//! - 占用：驻留 = 张量字节 + 每上下文 KV × 活跃上下文；映射 = 文件字节（mmap 时）+ 同 KV，恒 ≥ 驻留；
//!   KV 每上下文 = n_ctx × n_layer × 2 × (n_embd / n_head × n_head_kv) × 2 B（f16 K/V，引擎缺省类型）；
//!   `Model::context_kv_mb` 回同一算式在后端窗口上限（`n_ctx_for(max_context_tokens)`）下的值（madai-w17 I-C-16 接线），
//!   与 `footprint` 的每上下文 KV 项同源——owner 池记账自此按真模型参数而非 `LlamaOptions.kv_per_context_mb` 常数。

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use madinfer_backend::{
    BackendError, CancelToken, Constraint, Context, ContextOptions, FinishReason, GenerateOptions,
    MemoryFootprint, Model, ModelFormat, ModelMeta, Quant, SinkControl, TokenPiece, TokenSink,
    Turn,
};

use crate::emulation::{validate, ContextSlot};
use crate::sys::{ContextHandle, DecodeFailure, LoadFailure, ModelHandle};
use crate::LlamaOptions;

const MIB: u64 = 1024 * 1024;
/// 单次 decode 的批上限（prefill 分块）。
const N_BATCH: u32 = 512;

fn ceil_mib(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(MIB)).unwrap_or(u32::MAX)
}

/// 加载真实模型；失败按文件 / 引擎两分映射。
pub(crate) fn load(
    path: &Path,
    options: LlamaOptions,
    mmap: bool,
) -> Result<Box<dyn Model>, BackendError> {
    let handle = ModelHandle::load(path, mmap).map_err(|failure| match failure {
        LoadFailure::Missing => BackendError::ModelMissing,
        LoadFailure::Corrupt => BackendError::ModelCorrupt,
    })?;
    let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(Box::new(NativeModel::new(
        handle, options, mmap, file_bytes,
    )))
}

/// 已加载的真实模型：句柄 + 占用口径 + 并发席位。
pub(crate) struct NativeModel {
    handle: Arc<ModelHandle>,
    options: LlamaOptions,
    mmap: bool,
    file_bytes: u64,
    live: Arc<AtomicU8>,
}

impl NativeModel {
    fn new(handle: ModelHandle, options: LlamaOptions, mmap: bool, file_bytes: u64) -> Self {
        Self {
            handle: Arc::new(handle),
            options,
            mmap,
            file_bytes,
            live: Arc::new(AtomicU8::new(0)),
        }
    }

    /// 本后端对上下文窗口的取值：请求值夹在 [1, 后端上限]，再夹到训练长度。
    fn n_ctx_for(&self, requested: u32) -> u32 {
        let cap = self.options.max_context_tokens.max(1);
        let train = self.handle.n_ctx_train().max(1);
        requested.clamp(1, cap).min(train)
    }

    /// 每上下文 KV 估算（MB）：n_ctx × n_layer × 2 × head_dim × n_head_kv × 2 B。
    pub(crate) fn kv_per_context_mb(&self, n_ctx: u32) -> u32 {
        let n_head = u64::from(self.handle.n_head().max(1));
        let head_dim = u64::from(self.handle.n_embd()) / n_head;
        let per_token = u64::from(self.handle.n_layer())
            * 2
            * head_dim
            * u64::from(self.handle.n_head_kv())
            * 2;
        ceil_mib(per_token.saturating_mul(u64::from(n_ctx)))
    }

    /// 量化档：由引擎描述串判（`Quant` 闭合枚举无 Q4_0 等档位时落 `Unquantized`——上游反馈，报告 §7）。
    fn quant(&self) -> Quant {
        let desc = self.handle.desc();
        if desc.contains("Q4_K") {
            Quant::Q4Km
        } else if desc.contains("Q8_0") {
            Quant::Q80
        } else if desc.contains("F16") || desc.contains("BF16") {
            Quant::Fp16
        } else {
            Quant::Unquantized
        }
    }
}

impl Model for NativeModel {
    fn meta(&self) -> ModelMeta {
        ModelMeta {
            params_m: u32::try_from(self.handle.n_params() / 1_000_000).unwrap_or(u32::MAX),
            max_context_tokens: self.handle.n_ctx_train(),
            format: ModelFormat::Gguf,
            quant: self.quant(),
        }
    }

    fn footprint(&self) -> MemoryFootprint {
        let live = u32::from(self.live.load(Ordering::Acquire));
        let kv = Model::context_kv_mb(self).saturating_mul(live);
        let tensors_mb = ceil_mib(self.handle.size_bytes());
        let mapped_base = if self.mmap {
            ceil_mib(self.file_bytes).max(tensors_mb)
        } else {
            tensors_mb
        };
        MemoryFootprint {
            resident_mb: tensors_mb.saturating_add(kv),
            mapped_mb: mapped_base.saturating_add(kv),
        }
    }

    fn open_context(&self, options: &ContextOptions) -> Result<Box<dyn Context>, BackendError> {
        let slot = ContextSlot::acquire(&self.live, self.options.concurrency)?;
        let n_ctx = self.n_ctx_for(options.max_context_tokens);
        let handle = ContextHandle::new(&self.handle, n_ctx, N_BATCH.min(n_ctx), options.threads)
            .ok_or(BackendError::OutOfMemory)?;
        Ok(Box::new(NativeContext {
            _slot: slot,
            model: Arc::clone(&self.handle),
            handle,
        }))
    }

    fn unload(self: Box<Self>) {}

    fn context_kv_mb(&self) -> u32 {
        // 加载后真值：按引擎读出的模型参数在后端窗口上限下算；`open_context` 请求更小窗口时实际 KV ≤ 此值（记账取上界）。
        self.kv_per_context_mb(self.n_ctx_for(self.options.max_context_tokens))
    }
}

/// 真实生成上下文：席位 + 引擎句柄；`Send` 不 `Sync`。
pub(crate) struct NativeContext {
    _slot: ContextSlot,
    model: Arc<ModelHandle>,
    handle: ContextHandle,
}

/// prompt 渲染：各消息 `content` 以 `\n` 拼接（spike 口径，见模块注）。
fn render_prompt(turn: &Turn) -> String {
    let mut prompt = String::new();
    for (i, message) in turn.messages.iter().enumerate() {
        if i > 0 {
            prompt.push('\n');
        }
        prompt.push_str(&message.content);
    }
    prompt
}

/// 从字节缓冲取出最长合法 UTF-8 前缀（残尾留在缓冲里等下一词元）。
fn take_utf8_prefix(buf: &mut Vec<u8>) -> String {
    match std::str::from_utf8(buf) {
        Ok(_) => String::from_utf8(std::mem::take(buf)).unwrap_or_default(),
        Err(error) => {
            let valid = error.valid_up_to();
            let rest = buf.split_off(valid);
            let head = std::mem::replace(buf, rest);
            String::from_utf8(head).unwrap_or_default()
        }
    }
}

fn map_decode(failure: DecodeFailure, cancel: &CancelToken) -> BackendError {
    match failure {
        DecodeFailure::Aborted if cancel.is_cancelled() => BackendError::Cancelled,
        DecodeFailure::Aborted | DecodeFailure::Fatal => BackendError::Crashed,
        DecodeFailure::NoKvSlot => BackendError::OutOfMemory,
        DecodeFailure::InvalidBatch => BackendError::InvalidInput,
    }
}

/// 发射一片；`Stop` 时返回 `Err(SinkStopped)` 让循环收尾。
struct Emitter<'a> {
    sink: &'a mut dyn TokenSink,
    emitted_tokens: u32,
}

impl Emitter<'_> {
    fn emit(&mut self, text: String, tokens: u32, is_last: bool) -> Result<(), FinishReason> {
        self.emitted_tokens = self.emitted_tokens.saturating_add(tokens);
        match self.sink.on_token(TokenPiece {
            text,
            tokens,
            is_last,
        }) {
            SinkControl::Continue => Ok(()),
            SinkControl::Stop => Err(FinishReason::SinkStopped),
        }
    }
}

impl Context for NativeContext {
    fn generate(
        &mut self,
        turn: &Turn,
        options: &GenerateOptions,
        constraint: Option<&Constraint>,
        sink: &mut dyn TokenSink,
        cancel: &CancelToken,
    ) -> Result<madinfer_backend::GenerateStats, BackendError> {
        validate(turn, options)?;
        if constraint.is_some() {
            return Err(BackendError::Unsupported);
        }
        if cancel.is_cancelled() {
            return Err(BackendError::Cancelled);
        }

        let mut prompt_tokens = self.model.tokenize(&render_prompt(turn), true);
        if prompt_tokens.is_empty() {
            return Err(BackendError::InvalidInput);
        }
        let n_ctx = self.handle.n_ctx();
        let prompt_len = u32::try_from(prompt_tokens.len()).unwrap_or(u32::MAX);
        if prompt_len >= n_ctx {
            return Err(BackendError::InvalidInput);
        }

        // prefill：清 KV，按 N_BATCH 分块解码；每块前查旗。
        self.handle.clear();
        for chunk in prompt_tokens.chunks_mut(N_BATCH as usize) {
            if cancel.is_cancelled() {
                return Err(BackendError::Cancelled);
            }
            self.handle
                .decode(chunk)
                .map_err(|failure| map_decode(failure, cancel))?;
        }

        let max_new = options.max_tokens.min(n_ctx - prompt_len);
        let mut emitter = Emitter {
            sink,
            emitted_tokens: 0,
        };
        let mut generated: u32 = 0;
        let mut utf8_buf: Vec<u8> = Vec::new();
        let mut pending_tokens: u32 = 0;
        let mut pending: Option<(String, u32)> = None;
        let mut joined = String::new();
        let mut finish = FinishReason::Stop;

        loop {
            if generated >= max_new {
                finish = FinishReason::Length;
                break;
            }
            if cancel.is_cancelled() {
                return Err(BackendError::Cancelled);
            }
            let token = self.handle.sample_greedy();
            if self.model.is_eog(token) {
                break;
            }
            generated += 1;
            pending_tokens = pending_tokens.saturating_add(1);
            utf8_buf.extend_from_slice(&self.model.token_to_piece(token));
            let text = take_utf8_prefix(&mut utf8_buf);
            if !text.is_empty() {
                joined.push_str(&text);
                if options
                    .stop
                    .iter()
                    .any(|s| !s.is_empty() && joined.contains(s))
                {
                    // 停止串：首次含停止串的那一片整片不发、不计入发射数（与仿真核同口径）。
                    pending_tokens = 0;
                    break;
                }
                let previous = pending.replace((text, pending_tokens));
                pending_tokens = 0;
                if let Some((prev_text, prev_tokens)) = previous {
                    if let Err(reason) = emitter.emit(prev_text, prev_tokens, false) {
                        return Ok(stats(prompt_len, emitter.emitted_tokens, reason));
                    }
                }
            }
            let mut next = [token];
            self.handle
                .decode(&mut next)
                .map_err(|failure| map_decode(failure, cancel))?;
        }

        if cancel.is_cancelled() && finish == FinishReason::Length {
            return Err(BackendError::Cancelled);
        }
        // 收尾：残字节 lossy 并进最后一片、未归片的词元计数一并归入；最后一片 is_last = true。
        if !utf8_buf.is_empty() || pending_tokens > 0 {
            let tail = String::from_utf8_lossy(&utf8_buf).into_owned();
            match pending.as_mut() {
                Some((text, tokens)) => {
                    text.push_str(&tail);
                    *tokens = tokens.saturating_add(pending_tokens);
                }
                None => pending = Some((tail, pending_tokens)),
            }
        }
        if let Some((text, tokens)) = pending {
            if let Err(reason) = emitter.emit(text, tokens, true) {
                return Ok(stats(prompt_len, emitter.emitted_tokens, reason));
            }
        }
        Ok(stats(prompt_len, emitter.emitted_tokens, finish))
    }
}

fn stats(
    prompt_tokens: u32,
    gen_tokens: u32,
    finish: FinishReason,
) -> madinfer_backend::GenerateStats {
    madinfer_backend::GenerateStats {
        prompt_tokens,
        gen_tokens,
        finish,
    }
}

/// 真实引擎的 greedy 轨迹（金样与体积 / 速度读数用；不经 `Context` trait——词元 id 在 trait 面上不可见）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeTrace {
    /// 引擎描述串（架构 / 规模 / 量化）。
    pub model_desc: String,
    /// 参数量。
    pub n_params: u64,
    /// 张量字节数。
    pub tensor_bytes: u64,
    /// 训练上下文长度。
    pub n_ctx_train: u32,
    /// 词表大小。
    pub n_vocab: u32,
    /// 每词元 KV 字节（全部层，f16 K/V）。
    pub kv_bytes_per_token: u64,
    /// prompt 词元 id（含 BOS）。
    pub prompt_tokens: Vec<i32>,
    /// 生成词元 id（greedy；到 EOG 或 `max_tokens` 止，EOG 不计）。
    pub tokens: Vec<i32>,
    /// 生成文本（lossy 拼接）。
    pub text: String,
}

/// 加载 `model_path`，对 `prompt` 做 greedy 续写 `max_tokens` 个词元，返回轨迹。`threads` 为解码线程数。
pub fn greedy_trace(
    model_path: &Path,
    prompt: &str,
    max_tokens: u32,
    threads: u8,
) -> Result<NativeTrace, BackendError> {
    let handle = Arc::new(ModelHandle::load(model_path, true).map_err(
        |failure| match failure {
            LoadFailure::Missing => BackendError::ModelMissing,
            LoadFailure::Corrupt => BackendError::ModelCorrupt,
        },
    )?);
    let mut prompt_tokens = handle.tokenize(prompt, true);
    if prompt_tokens.is_empty() {
        return Err(BackendError::InvalidInput);
    }
    let prompt_len = u32::try_from(prompt_tokens.len()).unwrap_or(u32::MAX);
    let n_ctx = prompt_len
        .saturating_add(max_tokens)
        .saturating_add(1)
        .min(handle.n_ctx_train().max(1));
    if prompt_len >= n_ctx {
        return Err(BackendError::InvalidInput);
    }
    let mut ctx = ContextHandle::new(&handle, n_ctx, N_BATCH.min(n_ctx), threads)
        .ok_or(BackendError::OutOfMemory)?;
    for chunk in prompt_tokens.chunks_mut(N_BATCH as usize) {
        ctx.decode(chunk).map_err(|_| BackendError::Crashed)?;
    }
    let mut tokens = Vec::new();
    let mut bytes = Vec::new();
    while tokens.len() < max_tokens as usize
        && prompt_len + u32::try_from(tokens.len()).unwrap_or(0) < n_ctx
    {
        let token = ctx.sample_greedy();
        if handle.is_eog(token) {
            break;
        }
        tokens.push(token);
        bytes.extend_from_slice(&handle.token_to_piece(token));
        let mut next = [token];
        ctx.decode(&mut next).map_err(|_| BackendError::Crashed)?;
    }
    let n_head = u64::from(handle.n_head().max(1));
    let head_dim = u64::from(handle.n_embd()) / n_head;
    Ok(NativeTrace {
        model_desc: handle.desc(),
        n_params: handle.n_params(),
        tensor_bytes: handle.size_bytes(),
        n_ctx_train: handle.n_ctx_train(),
        n_vocab: handle.n_vocab(),
        kv_bytes_per_token: u64::from(handle.n_layer())
            * 2
            * head_dim
            * u64::from(handle.n_head_kv())
            * 2,
        prompt_tokens,
        tokens,
        text: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_prefix_keeps_partial_code_point_in_buffer() {
        // "端" = E7 AB AF；先来两字节，再来一字节。
        let mut buf = vec![0xE7, 0xAB];
        assert_eq!(take_utf8_prefix(&mut buf), "");
        assert_eq!(buf, vec![0xE7, 0xAB]);
        buf.push(0xAF);
        buf.extend_from_slice(b"ok");
        assert_eq!(take_utf8_prefix(&mut buf), "端ok");
        assert!(buf.is_empty());
    }

    #[test]
    fn prompt_joins_message_contents_with_newline() {
        let turn = Turn {
            messages: vec![
                madinfer_backend::Message {
                    role: madinfer_backend::Role::System,
                    content: "a".to_owned(),
                },
                madinfer_backend::Message {
                    role: madinfer_backend::Role::User,
                    content: "b".to_owned(),
                },
            ],
        };
        assert_eq!(render_prompt(&turn), "a\nb");
        assert_eq!(render_prompt(&Turn::user("solo")), "solo");
    }

    #[test]
    fn ceil_mib_rounds_up() {
        assert_eq!(ceil_mib(0), 0);
        assert_eq!(ceil_mib(1), 1);
        assert_eq!(ceil_mib(MIB), 1);
        assert_eq!(ceil_mib(MIB + 1), 2);
    }
}
