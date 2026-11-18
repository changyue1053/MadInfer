//! madinfer-backend-llama —— llama.cpp 后端适配器（设计文档 C7 / §4.2）。
//!
//! 缺省形态是**确定性仿真核**（无真实 llama.cpp）：按 seed + 输入词元产出确定性 token 流（回显式），
//! 支持每片查 [`madinfer_backend::CancelToken`]、停止串、`max_tokens`，[`madinfer_backend::GenerateStats`] 如实；
//! 能力声明与行为一致（CPU 槽、可中止、约束 **Emulated**——madai-w08 G4：带 GBNF 约束时按语法确定性产出，
//! 见 [`grammar`] 模块；不是真采样，语法编不过报 `InvalidInput`）。
//! 发射切片（madai-w12 I-C-11）：整轮输出按输出字节数切片，每片 ≤ [`EMULATED_PIECE_BYTES`] 字节、完整码点边界、
//! 词元计数随片携带——片边界是输出文本的纯函数，与调度时序无关（`gen_tokens` 仍是词元数）。
//! testkit 的 `backend_contract_tests!` 六条与固定 seed + greedy 金样在本 crate tests 里锁行为。
//!
//! 真实引擎经 cargo feature `llama-sys` 门控（缺省关；madai-w16 I-C-15 宿主 spike）：开启后 `build.rs` 以系统 cmake 编
//! `third_party/llama.cpp`（钉版见 `third_party/PINS.toml`）CPU 静态库并链进本 crate，[`LlamaBackend::native`] /
//! [`LlamaBackend::native_from_assets`] 构造的后端走真实路径（`sys` 绑定 + `native` 实现：加载 → 词元化 → greedy 逐词元解码 →
//! 逐片回调 → 取消 → 释放；采样恒 greedy、约束声明 `None`）；[`LlamaBackend::new`] 仍是仿真核，两条路径共用 `LlamaOptions` 与
//! 并发席位。权重路径（madai-w17 I-C-16）：装配形态由 owner 随每次 `load` 经 `LoadOpts::weights_path` 给（资产链已验签落盘的
//! 文件），`native_from_assets` 构造期不持路径；`native(options, path)` 是 spike / 用例形态，路径钉在构造期，`LoadOpts` 未给时用它。
//! 不带 feature 时本 crate 无 unsafe（`forbid`）；带 feature 时 unsafe 只在 `sys` 模块。

#![cfg_attr(not(feature = "llama-sys"), forbid(unsafe_code))]
#![deny(missing_docs)]

mod emulation;
pub mod grammar;
#[cfg(feature = "llama-sys")]
mod native;
#[cfg(feature = "llama-sys")]
mod sys;

#[cfg(feature = "llama-sys")]
use std::path::PathBuf;

use madinfer_backend::{
    Availability, Backend, BackendCaps, BackendError, ConstraintSupport, LoadOpts, Model,
};
use madinfer_core_types::{BackendId, SlotClass};

pub use emulation::EMULATED_PIECE_BYTES;
#[cfg(feature = "llama-sys")]
pub use native::{greedy_trace as native_greedy_trace, NativeTrace};

/// 注册制后端 id。
const LLAMA_BACKEND_ID: &str = "llama.cpp";

/// 构建参数：仿真核形态下决定能力声明与占用模型；真实引擎路径沿用 `max_context_tokens`（窗口上限）与 `concurrency`
/// （席位），占用改由引擎读数算（`weights_*` / `kv_per_context_mb` 不生效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LlamaOptions {
    /// 上下文窗口上限（token）。
    pub max_context_tokens: u32,
    /// 同一 Model 同时打开的上下文数上限（CPU 槽缺省 1，可调）。
    pub concurrency: u8,
    /// 权重驻留估值 MB（仿真占用模型）。
    pub weights_resident_mb: u32,
    /// mmap 映射估值 MB（≥ 驻留；`LoadOpts.mmap = false` 时映射按驻留报告）。
    pub weights_mapped_mb: u32,
    /// 每个打开的上下文的 KV 驻留 MB。
    pub kv_per_context_mb: u32,
}

impl Default for LlamaOptions {
    fn default() -> Self {
        Self {
            max_context_tokens: 4_096,
            concurrency: 1,
            weights_resident_mb: 96,
            weights_mapped_mb: 1_024,
            kv_per_context_mb: 8,
        }
    }
}

/// 引擎形态：仿真核 / 真实 llama.cpp（feature `llama-sys`）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Engine {
    Emulated,
    #[cfg(feature = "llama-sys")]
    Native {
        /// 构造期钉住的 GGUF 权重路径（spike / 用例形态；`tools/models/fetch.sh` 拉到 `~/.cache/madinfer/models/<id>.gguf`）。
        /// `None` = 装配形态，路径只在 `load` 时从 `LoadOpts::weights_path` 取。
        model_path: Option<PathBuf>,
    },
}

/// llama.cpp 后端（缺省仿真核；真实引擎经 feature `llama-sys` 的 [`LlamaBackend::native`] 接入）。
pub struct LlamaBackend {
    id: BackendId,
    options: LlamaOptions,
    engine: Engine,
}

impl LlamaBackend {
    /// 按参数构建仿真核。不做任何 IO。
    pub fn new(options: LlamaOptions) -> Self {
        Self {
            id: BackendId(LLAMA_BACKEND_ID.to_owned()),
            options,
            engine: Engine::Emulated,
        }
    }

    /// 按参数构建真实引擎后端，权重路径钉在构造期（spike / 用例形态，madai-w16 I-C-15）。`load` 时 `LoadOpts::weights_path`
    /// 若给则优先于它。构造不做 IO——文件缺失在 `load` 时报 `ModelMissing`。产品装配用 [`Self::native_from_assets`]。
    #[cfg(feature = "llama-sys")]
    pub fn native(options: LlamaOptions, model_path: impl Into<PathBuf>) -> Self {
        Self {
            id: BackendId(LLAMA_BACKEND_ID.to_owned()),
            options,
            engine: Engine::Native {
                model_path: Some(model_path.into()),
            },
        }
    }

    /// 按参数构建真实引擎后端，构造期不持路径（装配形态，madai-w17 I-C-16）：每次 `load` 从 `LoadOpts::weights_path`（owner 给的
    /// 已验签落盘文件）取权重；未给 → `ModelMissing`（3004，owner 走既有失败路径），不 panic、不读任何缺省位置。
    #[cfg(feature = "llama-sys")]
    pub fn native_from_assets(options: LlamaOptions) -> Self {
        Self {
            id: BackendId(LLAMA_BACKEND_ID.to_owned()),
            options,
            engine: Engine::Native { model_path: None },
        }
    }

    /// 是否真实引擎路径。
    pub fn is_native(&self) -> bool {
        self.engine != Engine::Emulated
    }
}

impl Backend for LlamaBackend {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn probe(&self) -> BackendCaps {
        // 不加载模型、不做 IO（§4.2 六条之③）；声明与各路径行为一致。
        BackendCaps {
            slot: SlotClass::Cpu,
            max_context_tokens: self.options.max_context_tokens,
            concurrency: self.options.concurrency,
            supports_abort: true,
            // 仿真核按 GBNF 走语法产出（madai-w08 G4）；真实路径本批未接 GBNF 采样器，声明 None、传约束回 Unsupported。
            constraint: if self.is_native() {
                ConstraintSupport::None
            } else {
                ConstraintSupport::Emulated
            },
            tools: false,
            image_input: false,
            audio_input: false,
        }
    }

    fn availability(&self) -> Availability {
        Availability::Available
    }

    fn load(&self, opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError> {
        match &self.engine {
            // 仿真核无真实权重：`package` / `weights` 不做存在性检查（资产接入随真实引擎落地）。
            Engine::Emulated => Ok(Box::new(emulation::EmulatedModel::new(
                self.options,
                opts.mmap,
            ))),
            #[cfg(feature = "llama-sys")]
            Engine::Native { model_path } => {
                // 装配形态：路径随 load 来（资产链）；spike 形态：LoadOpts 未给时退回构造期路径。两处都缺 → ModelMissing，不 panic。
                let path = opts
                    .weights_path
                    .as_deref()
                    .or(model_path.as_deref())
                    .ok_or(BackendError::ModelMissing)?;
                native::load(path, self.options, opts.mmap)
            }
        }
    }

    fn context_kv_mb(&self) -> u32 {
        // 加载前口径：配置常数（仿真核与 EmulatedModel::footprint 的每上下文 KV 项同一来源，缺省 8）。真实引擎的模型参数
        // （n_layer / n_head_kv / head_dim）只在加载后可知，加载前同样回这个常数作预留估值；加载后 owner 换用
        // Model::context_kv_mb 真值（NativeModel::kv_per_context_mb(n_ctx)，madai-w17 I-C-16）。
        self.options.kv_per_context_mb
    }
}

/// 真实引擎是否已链接且与钉版一致（feature `llama-sys`；桥的 build number == `third_party/PINS.toml` 的 tag 号）。
#[cfg(feature = "llama-sys")]
pub fn native_engine_available() -> bool {
    sys::native_engine_available()
}

/// 链接进来的 llama.cpp build number（feature `llama-sys`）。
#[cfg(feature = "llama-sys")]
pub fn native_engine_build_number() -> i32 {
    sys::linked_build_number()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use madinfer_backend::ContextOptions;

    use super::*;

    #[test]
    fn context_kv_mb_is_the_configured_constant_before_and_after_load_and_matches_footprint() {
        // madai-w16 I-C-14：加载前（Backend）与加载后（Model）两处读数同一常数；占用模型里每个打开的上下文加的就是这个数——
        // owner 按它记账时"池已用"与后端占用同口径。
        let default = LlamaBackend::new(LlamaOptions::default());
        assert_eq!(
            default.context_kv_mb(),
            8,
            "LlamaOptions 缺省 kv_per_context_mb"
        );
        let backend = LlamaBackend::new(LlamaOptions {
            kv_per_context_mb: 24,
            concurrency: 2,
            ..LlamaOptions::default()
        });
        assert_eq!(backend.context_kv_mb(), 24);
        let model = backend.load(&LoadOpts::default()).expect("load");
        assert_eq!(model.context_kv_mb(), 24, "加载后同一读数");
        let base = model.footprint().resident_mb;
        let held = model
            .open_context(&ContextOptions::default())
            .expect("open");
        assert_eq!(
            model.footprint().resident_mb,
            base + model.context_kv_mb(),
            "占用模型的每上下文 KV 项 = context_kv_mb"
        );
        drop(held);
        assert_eq!(model.footprint().resident_mb, base, "句柄 Drop 即归还");
        model.unload();
    }

    #[test]
    fn emulated_load_ignores_weights_path() {
        // madai-w17 I-C-16：仿真核不读文件——给了不存在的路径也照常加载（装配方对仿真核站也可能带 model_path）。
        let backend = LlamaBackend::new(LlamaOptions::default());
        let model = backend
            .load(&LoadOpts {
                weights_path: Some(std::path::PathBuf::from(
                    "/nonexistent/madinfer-i-c-16.gguf",
                )),
                ..LoadOpts::default()
            })
            .expect("仿真核忽略 weights_path");
        assert_eq!(model.context_kv_mb(), 8);
        model.unload();
    }

    /// 装配形态（`native_from_assets`）的缺路径处置：不 panic、回 `ModelMissing`（owner 走 3004 既有失败路径）。不需要模型文件。
    #[cfg(feature = "llama-sys")]
    #[test]
    fn native_from_assets_without_weights_path_is_model_missing_not_panic() {
        let backend = LlamaBackend::native_from_assets(LlamaOptions::default());
        assert!(backend.is_native());
        assert_eq!(
            backend.load(&LoadOpts::default()).err(),
            Some(BackendError::ModelMissing),
            "LoadOpts 未给路径"
        );
        assert_eq!(
            backend
                .load(&LoadOpts {
                    weights_path: Some(std::path::PathBuf::from(
                        "/nonexistent/madinfer-i-c-16.gguf"
                    )),
                    ..LoadOpts::default()
                })
                .err(),
            Some(BackendError::ModelMissing),
            "路径给了但文件不在"
        );
        assert_eq!(
            backend.context_kv_mb(),
            LlamaOptions::default().kv_per_context_mb,
            "加载前口径仍是配置常数"
        );
    }

    /// 真模型：`Model::context_kv_mb` 是引擎参数算出的真值（≠ 配置常数 8），且与占用模型的每上下文 KV 项同源。
    /// 模型经 `MADINFER_LLAMA_MODEL` 门控，缺席即测试失败。
    #[cfg(feature = "llama-sys")]
    #[test]
    fn native_context_kv_mb_is_engine_truth_after_load() {
        let Some(path) = std::env::var_os("MADINFER_LLAMA_MODEL").filter(|v| !v.is_empty()) else {
            panic!("MADINFER_LLAMA_MODEL required: native model coverage must not skip green");
        };
        let options = LlamaOptions {
            max_context_tokens: 512,
            ..LlamaOptions::default()
        };
        let backend = LlamaBackend::native_from_assets(options);
        let model = backend
            .load(&LoadOpts {
                weights_path: Some(std::path::PathBuf::from(path)),
                ..LoadOpts::default()
            })
            .expect("load via LoadOpts::weights_path");
        let kv = model.context_kv_mb();
        assert!(kv > 0, "真值 > 0");
        assert_ne!(kv, options.kv_per_context_mb, "不是配置常数");
        let base = model.footprint().resident_mb;
        let held = model
            .open_context(&ContextOptions {
                max_context_tokens: 512,
                threads: 1,
            })
            .expect("open");
        assert_eq!(
            model.footprint().resident_mb,
            base + kv,
            "占用的每上下文 KV 项 = context_kv_mb"
        );
        drop(held);
        model.unload();
    }
}
