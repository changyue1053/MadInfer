//! madinfer-preproc —— 预处理（设计文档 C12）：聊天模板装配、token 预算截断、JSON Schema → GBNF 编译。
//!
//! 三块全是纯函数（同输入同输出，无 IO、无时钟、无随机）：
//! - [`Template`]：把 `Turn` / 系统提示渲染成 prompt 文本（内置 `<|im_start|>` 标记式与朴素行式；算法包自带模板的沙箱渲染归后续批次）；
//! - [`Tokenizer`] + [`ByteTokenizer`] + [`fit_to_budget`]：token 粗估与超预算截断（最旧非 system 轮次先弃、system 保留）；
//! - [`SchemaCompiler`]：JSON Schema（draft 2020-12 子集）→ GBNF；子集之外返回 [`SchemaError::UnsupportedSchema`]，禁 panic。
//! - [`validate_instance`]：JSON 实例对同一 schema 子集的校验（madai-w08 G4：结构化输出终态前守门，不过即 2003）。
//! - 切块（map-reduce 计划）与增量 JSON 解析归后续批次。
//!
//! 消息文本与 schema 文本是**载荷**：本 crate 只装配与编译，不解读语义、不落日志。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod budget;
mod json;
mod schema;
mod template;
mod validate;

pub use budget::{
    fit_to_budget, BudgetOutcome, ByteTokenizer, Tokenizer, DEFAULT_BYTES_PER_TOKEN,
    MESSAGE_OVERHEAD_TOKENS,
};
pub use schema::{Grammar, SchemaCompiler, SchemaError, MAX_ITEM_BOUND};
pub use template::Template;
pub use validate::{validate_instance, ValidationError};
