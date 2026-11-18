//! 真实 llama.cpp 引擎绑定（cargo feature `llama-sys`，缺省关；madai-w16 I-C-15 宿主 spike）。
//!
//! 本 crate 唯一的 unsafe 所在：`extern "C"` 声明与 `csrc/madinfer_llama_bridge.h` 逐条对应——只有标量与不透明指针跨边界，
//! llama.h 的 by-value 结构（model / context / batch / sampler 参数）全部在 C 桥里消化，升 tag 时错位是编译错不是内存错。
//! 两个 RAII 句柄：[`ModelHandle`]（`llama_model_free`）、[`ContextHandle`]（`llama_free` + 采样链释放，持模型 `Arc`
//! 保证模型晚于上下文释放）。每处 unsafe 带 SAFETY 注释；`unsafe_op_in_unsafe_fn` 判红。
//!
//! 本批不接：abort_callback（decode 内中止，取消目前在词元间查旗）、GBNF 采样器、LoRA、对话模板、增量 detokenize 之外的
//! 特殊词元渲染——归 I4（报告 §6）。

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_char, c_int, CString};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::{Arc, Once};

/// 钉版 build number（`third_party/PINS.toml` `[llama_cpp] tag = "b<n>"` 的 n；`csrc/CMakeLists.txt` 同值编进桥）。
pub(crate) const PINNED_BUILD_NUMBER: i32 = 10_883;

/// 不透明：`struct llama_model`。
#[repr(C)]
pub(crate) struct RawModel {
    _opaque: [u8; 0],
}

/// 不透明：桥侧 `madinfer_llama_ctx`（llama_context + greedy 采样链）。
#[repr(C)]
pub(crate) struct RawContext {
    _opaque: [u8; 0],
}

extern "C" {
    fn madinfer_llama_build_number() -> i32;
    fn madinfer_llama_backend_init();
    fn madinfer_llama_model_load(path: *const c_char, use_mmap: c_int) -> *mut RawModel;
    fn madinfer_llama_model_free(model: *mut RawModel);
    fn madinfer_llama_model_n_ctx_train(model: *const RawModel) -> i32;
    fn madinfer_llama_model_n_vocab(model: *const RawModel) -> i32;
    fn madinfer_llama_model_n_layer(model: *const RawModel) -> i32;
    fn madinfer_llama_model_n_embd(model: *const RawModel) -> i32;
    fn madinfer_llama_model_n_head(model: *const RawModel) -> i32;
    fn madinfer_llama_model_n_head_kv(model: *const RawModel) -> i32;
    fn madinfer_llama_model_n_params(model: *const RawModel) -> u64;
    fn madinfer_llama_model_size(model: *const RawModel) -> u64;
    fn madinfer_llama_model_desc(model: *const RawModel, buf: *mut c_char, cap: i32) -> i32;
    fn madinfer_llama_tokenize(
        model: *const RawModel,
        text: *const c_char,
        text_len: i32,
        out: *mut i32,
        out_cap: i32,
        add_special: c_int,
    ) -> i32;
    fn madinfer_llama_token_to_piece(
        model: *const RawModel,
        token: i32,
        buf: *mut c_char,
        cap: i32,
    ) -> i32;
    fn madinfer_llama_token_is_eog(model: *const RawModel, token: i32) -> c_int;
    fn madinfer_llama_ctx_new(
        model: *const RawModel,
        n_ctx: u32,
        n_batch: u32,
        n_threads: i32,
    ) -> *mut RawContext;
    fn madinfer_llama_ctx_free(ctx: *mut RawContext);
    fn madinfer_llama_ctx_n_ctx(ctx: *const RawContext) -> u32;
    fn madinfer_llama_ctx_clear(ctx: *mut RawContext);
    fn madinfer_llama_decode(ctx: *mut RawContext, tokens: *mut i32, n_tokens: i32) -> i32;
    fn madinfer_llama_sample_greedy(ctx: *mut RawContext) -> i32;
}

static BACKEND_INIT: Once = Once::new();

/// 进程级一次初始化（llama_backend_init + 日志回调置空）。不配对 free：进程生命周期内保持。
fn ensure_backend() {
    BACKEND_INIT.call_once(|| {
        // SAFETY: 无参、可重入的进程级初始化；Once 保证只跑一次。
        unsafe { madinfer_llama_backend_init() };
    });
}

/// 链接进来的 llama.cpp build number（桥编译期常量）。
pub(crate) fn linked_build_number() -> i32 {
    // SAFETY: 纯函数、无参、无副作用。
    unsafe { madinfer_llama_build_number() }
}

/// 链接自检：桥在场且 build number 与钉版一致。
pub(crate) fn native_engine_available() -> bool {
    linked_build_number() == PINNED_BUILD_NUMBER
}

/// 加载失败的两分：文件不可读（`ModelMissing`）/ 引擎拒载（`ModelCorrupt`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadFailure {
    Missing,
    Corrupt,
}

/// `llama_decode` 返回码的分档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeFailure {
    /// 1：KV 无槽（上下文满）。
    NoKvSlot,
    /// 2：被 abort 回调中止。
    Aborted,
    /// -1：批非法。
    InvalidBatch,
    /// < -1：引擎致命错误。
    Fatal,
}

fn non_negative(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// 已加载模型的 RAII 句柄。
pub(crate) struct ModelHandle {
    raw: NonNull<RawModel>,
}

// SAFETY: llama_model 加载后只读；llama.cpp 允许多个线程对同一 model 并发词元化 / 创建 context（上游并行示例形态）。
unsafe impl Send for ModelHandle {}
// SAFETY: 同上——本句柄不暴露任何改写模型的入口。
unsafe impl Sync for ModelHandle {}

impl ModelHandle {
    /// 按路径加载；`mmap` 决定 `LLAMA_LOAD_MODE_MMAP` / `NONE`。
    pub(crate) fn load(path: &Path, mmap: bool) -> Result<Self, LoadFailure> {
        ensure_backend();
        if !path.is_file() {
            return Err(LoadFailure::Missing);
        }
        let c_path =
            CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| LoadFailure::Missing)?;
        // SAFETY: c_path 以 NUL 结尾且在调用期间存活；桥对 NULL 返回值有定义。
        let raw = unsafe { madinfer_llama_model_load(c_path.as_ptr(), c_int::from(mmap)) };
        NonNull::new(raw)
            .map(|raw| Self { raw })
            .ok_or(LoadFailure::Corrupt)
    }

    fn ptr(&self) -> *const RawModel {
        self.raw.as_ptr()
    }

    /// 训练上下文长度。
    pub(crate) fn n_ctx_train(&self) -> u32 {
        // SAFETY: 句柄有效（RAII）；只读查询。
        non_negative(unsafe { madinfer_llama_model_n_ctx_train(self.ptr()) })
    }

    /// 词表大小。
    pub(crate) fn n_vocab(&self) -> u32 {
        // SAFETY: 同上。
        non_negative(unsafe { madinfer_llama_model_n_vocab(self.ptr()) })
    }

    /// 层数。
    pub(crate) fn n_layer(&self) -> u32 {
        // SAFETY: 同上。
        non_negative(unsafe { madinfer_llama_model_n_layer(self.ptr()) })
    }

    /// 隐层宽。
    pub(crate) fn n_embd(&self) -> u32 {
        // SAFETY: 同上。
        non_negative(unsafe { madinfer_llama_model_n_embd(self.ptr()) })
    }

    /// 注意力头数。
    pub(crate) fn n_head(&self) -> u32 {
        // SAFETY: 同上。
        non_negative(unsafe { madinfer_llama_model_n_head(self.ptr()) })
    }

    /// KV 头数（GQA）。
    pub(crate) fn n_head_kv(&self) -> u32 {
        // SAFETY: 同上。
        non_negative(unsafe { madinfer_llama_model_n_head_kv(self.ptr()) })
    }

    /// 参数量。
    pub(crate) fn n_params(&self) -> u64 {
        // SAFETY: 同上。
        unsafe { madinfer_llama_model_n_params(self.ptr()) }
    }

    /// 张量字节数。
    pub(crate) fn size_bytes(&self) -> u64 {
        // SAFETY: 同上。
        unsafe { madinfer_llama_model_size(self.ptr()) }
    }

    /// 模型描述串（架构 / 规模 / 量化，如 "llama 135M Q4_K - Medium"）。
    pub(crate) fn desc(&self) -> String {
        let mut buf = vec![0u8; 256];
        // SAFETY: buf 可写且长度如实传入；桥不写 NUL 之外的越界字节。
        let written =
            unsafe { madinfer_llama_model_desc(self.ptr(), buf.as_mut_ptr().cast(), 256) };
        let len = usize::try_from(written).unwrap_or(0).min(buf.len());
        buf.truncate(len);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// 词元化；`add_special` 按模型配置加 BOS。空文本 + 不加特殊词元 → 空序列。
    pub(crate) fn tokenize(&self, text: &str, add_special: bool) -> Vec<i32> {
        let text_len = i32::try_from(text.len()).unwrap_or(i32::MAX);
        let mut out: Vec<i32> = vec![0; text.len() + 8];
        loop {
            let cap = i32::try_from(out.len()).unwrap_or(i32::MAX);
            // SAFETY: text 指针 + 长度如实；out 可写且容量如实；模型句柄有效。
            let n = unsafe {
                madinfer_llama_tokenize(
                    self.ptr(),
                    text.as_ptr().cast(),
                    text_len,
                    out.as_mut_ptr(),
                    cap,
                    c_int::from(add_special),
                )
            };
            if n >= 0 {
                out.truncate(non_negative(n) as usize);
                return out;
            }
            // 负数 = 所需容量；INT32_MIN 表示溢出（不会发生在受控输入上），按空处理。
            let Some(needed) = n.checked_neg() else {
                return Vec::new();
            };
            out.resize(non_negative(needed) as usize, 0);
        }
    }

    /// 词元 → 原始字节片（可能不是完整 UTF-8 码点边界，调用方拼接后再切）。
    pub(crate) fn token_to_piece(&self, token: i32) -> Vec<u8> {
        let mut buf = vec![0u8; 64];
        loop {
            let cap = i32::try_from(buf.len()).unwrap_or(i32::MAX);
            // SAFETY: buf 可写且容量如实；模型句柄有效。
            let n = unsafe {
                madinfer_llama_token_to_piece(self.ptr(), token, buf.as_mut_ptr().cast(), cap)
            };
            if n >= 0 {
                buf.truncate(non_negative(n) as usize);
                return buf;
            }
            let Some(needed) = n.checked_neg() else {
                return Vec::new();
            };
            buf.resize(non_negative(needed) as usize, 0);
        }
    }

    /// 生成结束词元（EOS / EOT 族）。
    pub(crate) fn is_eog(&self, token: i32) -> bool {
        // SAFETY: 只读查询。
        unsafe { madinfer_llama_token_is_eog(self.ptr(), token) != 0 }
    }
}

impl Drop for ModelHandle {
    fn drop(&mut self) {
        // SAFETY: 指针由 load 取得且只释放一次；持本句柄 Arc 的上下文都已析构（ContextHandle 持 Arc）。
        unsafe { madinfer_llama_model_free(self.raw.as_ptr()) };
    }
}

/// 生成上下文的 RAII 句柄（KV 缓存 + greedy 采样链）。`Send` 不 `Sync`：只在打开它的 worker 上用。
pub(crate) struct ContextHandle {
    raw: NonNull<RawContext>,
    /// 模型晚于上下文释放。
    _model: Arc<ModelHandle>,
}

// SAFETY: llama_context 无线程亲和，指针可随 worker 搬运；同一时刻只有持有者一方调用（&mut self）。
unsafe impl Send for ContextHandle {}

impl ContextHandle {
    /// 打开上下文；`None` = 引擎分配失败（KV 内存）。
    pub(crate) fn new(
        model: &Arc<ModelHandle>,
        n_ctx: u32,
        n_batch: u32,
        n_threads: u8,
    ) -> Option<Self> {
        // Scheduler thread counts are hints, not a guarantee of host CPU capacity.
        // In particular, a four-thread spinning GGML pool on a two-CPU runner can
        // starve its own decode workers. Respect affinity/quota where reported by
        // Rust; conservatively use one thread when capacity cannot be queried.
        let available = std::thread::available_parallelism().map_or(1, usize::from);
        let n_threads = bounded_threads(n_threads, available);
        // SAFETY: 模型句柄有效；桥对 NULL 有定义。
        let raw =
            unsafe { madinfer_llama_ctx_new(model.ptr(), n_ctx, n_batch, i32::from(n_threads)) };
        NonNull::new(raw).map(|raw| Self {
            raw,
            _model: Arc::clone(model),
        })
    }

    /// 实际窗口（引擎可能对齐 / 截到训练长度）。
    pub(crate) fn n_ctx(&self) -> u32 {
        // SAFETY: 句柄有效；只读。
        unsafe { madinfer_llama_ctx_n_ctx(self.raw.as_ptr()) }
    }

    /// 清 KV 与采样链（一轮开始）。
    pub(crate) fn clear(&mut self) {
        // SAFETY: 句柄有效；&mut self 保证独占。
        unsafe { madinfer_llama_ctx_clear(self.raw.as_ptr()) };
    }

    /// 解码一批（≤ n_batch）；位置由引擎跟踪。
    pub(crate) fn decode(&mut self, tokens: &mut [i32]) -> Result<(), DecodeFailure> {
        let n = i32::try_from(tokens.len()).map_err(|_| DecodeFailure::InvalidBatch)?;
        if n == 0 {
            return Err(DecodeFailure::InvalidBatch);
        }
        // SAFETY: tokens 可写且长度如实（llama_batch 形态要求非 const 指针，引擎只读）；句柄有效。
        let code = unsafe { madinfer_llama_decode(self.raw.as_ptr(), tokens.as_mut_ptr(), n) };
        match code {
            0 => Ok(()),
            1 => Err(DecodeFailure::NoKvSlot),
            2 => Err(DecodeFailure::Aborted),
            -1 => Err(DecodeFailure::InvalidBatch),
            _ => Err(DecodeFailure::Fatal),
        }
    }

    /// 对最近一次 decode 的 logits 取 argmax。
    pub(crate) fn sample_greedy(&mut self) -> i32 {
        // SAFETY: 句柄有效；前一次 decode 已产出 logits（调用方保证顺序）。
        unsafe { madinfer_llama_sample_greedy(self.raw.as_ptr()) }
    }
}

impl Drop for ContextHandle {
    fn drop(&mut self) {
        // SAFETY: 指针由 new 取得且只释放一次。
        unsafe { madinfer_llama_ctx_free(self.raw.as_ptr()) };
    }
}

fn bounded_threads(requested: u8, available: usize) -> u8 {
    usize::from(requested.max(1)).min(available.max(1)) as u8
}

#[cfg(test)]
mod thread_budget_tests {
    use super::bounded_threads;

    #[test]
    fn thread_hint_never_exceeds_available_cpus() {
        for requested in 0..=u8::MAX {
            for available in [0, 1, 2, 4, 8, 256, usize::MAX] {
                let actual = usize::from(bounded_threads(requested, available));
                assert!((1..=usize::from(requested.max(1))).contains(&actual));
                assert!(actual <= available.max(1));
            }
        }
        assert_eq!(bounded_threads(4, 2), 2);
        assert_eq!(bounded_threads(4, 8), 4);
    }
}
