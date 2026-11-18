//! 后端契约的数据类型：能力声明、模型元数据、内存占用、一轮输入、采样参数、约束、可用性、加载 / 上下文参数、token 片与统计。
//!
//! 数值一律整数或定点（`_x100` / `_bp`），与 schemas 的口径一致；用户内容（消息文本、token 文本、语法文本）是**载荷**，
//! 只经这些类型搬运，不进日志与观测。

use std::path::PathBuf;

use madinfer_core_types::{AssetId, PackageRef, SlotClass};

/// 后端产出的一片 token（已在完整 UTF-8 码点边界）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPiece {
    /// 文本（载荷）。
    pub text: String,
    /// 本片包含的 token 数。
    pub tokens: u32,
    /// 是否最后一片。
    pub is_last: bool,
}

/// sink 对后端的回话：继续或停止（业务侧停止条件已满足）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkControl {
    /// 继续生成。
    Continue,
    /// 到此为止（后端在下一 token 前返回 Ok）。
    Stop,
}

/// 约束解码支持度。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConstraintSupport {
    /// 引擎原生（GBNF / 系统模型 schema）。
    Native,
    /// 核内 FSM 过滤模拟。
    Emulated,
    /// 不支持：传入约束返回 `Unsupported` 或忽略（契约测试两种都接受，见 testkit）。
    None,
}

/// 后端在此设备上的能力声明（`probe()` 结果，注册时缓存进 [`crate::BackendRegistry`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackendCaps {
    /// 执行槽类别（调度按此排队；探测时声明，注册后不变。madai-w05 D5，I-C-03 U-8）。
    pub slot: SlotClass,
    /// 上下文窗口上限（token）。
    pub max_context_tokens: u32,
    /// 同一 Model 同时打开的上下文数上限（NPU 与系统模型通常 1）。
    pub concurrency: u8,
    /// 运行中能否中止；false 则不接交互档 Job（设计文档 R10）。
    pub supports_abort: bool,
    /// 约束解码。
    pub constraint: ConstraintSupport,
    /// 工具调用。
    pub tools: bool,
    /// 图像输入。
    pub image_input: bool,
    /// 音频输入。
    pub audio_input: bool,
}

/// 权重格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelFormat {
    /// llama.cpp GGUF。
    Gguf,
    /// ExecuTorch PTE。
    Pte,
    /// LiteRT-LM。
    LiteRtLm,
    /// MindSpore。
    Ms,
    /// 系统模型（宿主托管，无本地权重）。
    SystemManaged,
}

/// 量化。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quant {
    /// Q4_K_M。
    Q4Km,
    /// Q8_0。
    Q80,
    /// W8A8。
    W8A8,
    /// FP16。
    Fp16,
    /// 未量化 / 未知。
    Unquantized,
}

/// 模型元数据（`Model::meta()`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelMeta {
    /// 参数量（百万）。
    pub params_m: u32,
    /// 训练 / 支持的上下文上限。
    pub max_context_tokens: u32,
    /// 格式。
    pub format: ModelFormat,
    /// 量化。
    pub quant: Quant,
}

/// 内存占用；`load` 返回后即准确（设计文档 §4.2 六条之④）：mmap 的 `mapped_mb` 与实际驻留 `resident_mb` 分开报。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MemoryFootprint {
    /// 实际驻留 MB。
    pub resident_mb: u32,
    /// 映射 MB（≥ resident）。
    pub mapped_mb: u32,
}

/// 消息角色（request.v1 message.role）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// system。
    System,
    /// user。
    User,
    /// assistant。
    Assistant,
    /// tool。
    Tool,
}

/// 一条消息（载荷）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// 角色。
    pub role: Role,
    /// 内容。
    pub content: String,
}

/// 一轮生成的输入：完整消息历史（后端自行决定复用多少 KV）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {
    /// 消息。
    pub messages: Vec<Message>,
}

impl Turn {
    /// 单条 user 消息。
    pub fn user(content: &str) -> Self {
        Self {
            messages: vec![Message {
                role: Role::User,
                content: content.to_owned(),
            }],
        }
    }
}

/// 采样参数（request.v1 options 的定点形式：temperature × 100、top_p 万分比）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GenerateOptions {
    /// temperature × 100（0～200）。
    pub temperature_x100: u16,
    /// top_p 万分比（0～10000）。
    pub top_p_bp: u16,
    /// 生成上限。
    pub max_tokens: u32,
    /// 停止串（≤ 8）。
    pub stop: Vec<String>,
    /// 随机种子。
    pub seed: Option<u64>,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            temperature_x100: 70,
            top_p_bp: 9_500,
            max_tokens: 512,
            stop: Vec::new(),
            seed: None,
        }
    }
}

/// 约束解码的语法（C12 由 JSON Schema 编译得到的 GBNF 文本；载荷）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Constraint {
    /// 语法文本。
    pub grammar: String,
}

/// 不可用原因（系统模型）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnavailableReason {
    /// 系统模型未安装 / 未下载。
    NotInstalled,
    /// OS 版本不满足。
    OsTooOld,
    /// 地区不支持。
    Region,
    /// 机型不支持。
    DeviceUnsupported,
    /// 被用户 / 企业策略关闭。
    Disabled,
}

/// 可用性（内联后端恒 `Available`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    /// 可用。
    Available,
    /// 系统模型下载中。
    Downloading {
        /// 0～1000。
        permille: u16,
    },
    /// 不可用。
    Unavailable {
        /// 原因。
        reason: UnavailableReason,
    },
}

/// 加载参数。权重已由资产层验签；系统模型 / 云端后端忽略 `package` / `weights` / `weights_path`。
///
/// `weights` 是内容寻址 id（记账 / 观测键），`weights_path` 是该 blob **已验签落盘后的文件路径**（madai-w17 I-C-16 只加带缺省）：
/// 需要读文件的真实引擎（llama.cpp）在 `load` 时取它；`None` = owner 未给路径，后端按自身形态处置（仿真核不读文件、
/// 真实引擎回 `ModelMissing`）。资产层接入后由它把 `weights` 解析成 `weights_path`；此前由装配方（madinfer-ffi）经
/// `CoreBuilder::weights_path` 直给。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadOpts {
    /// 算法包。
    pub package: Option<PackageRef>,
    /// 权重 blob。
    pub weights: Option<AssetId>,
    /// 权重文件路径（已验签、已落盘；真实引擎读它）。
    pub weights_path: Option<PathBuf>,
    /// 线程数建议。
    pub threads: u8,
    /// 是否 mmap。
    pub mmap: bool,
}

impl Default for LoadOpts {
    fn default() -> Self {
        Self {
            package: None,
            weights: None,
            weights_path: None,
            threads: 1,
            mmap: true,
        }
    }
}

/// 上下文参数（打开生成上下文时给）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContextOptions {
    /// 上下文窗口。
    pub max_context_tokens: u32,
    /// 线程数（来自 JobCtx）。
    pub threads: u8,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            max_context_tokens: 4_096,
            threads: 1,
        }
    }
}

/// 结束原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FinishReason {
    /// 自然结束（EOS / 停止串）。
    Stop,
    /// 到 `max_tokens`。
    Length,
    /// sink 返回 `Stop`。
    SinkStopped,
}

/// 一轮生成的统计（时间由 worker 按注入时钟测，不在此处）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GenerateStats {
    /// prompt token 数。
    pub prompt_tokens: u32,
    /// 生成 token 数。
    pub gen_tokens: u32,
    /// 结束原因。
    pub finish: FinishReason,
}
