//! token 预算：[`Tokenizer`] 计数 + 超预算截断。纯函数。
//!
//! 截断策略（C12）：超预算时从**最旧的非 system 消息**起逐条弃掉（user / assistant / tool 同待遇），
//! system 一律保留；只剩 system 仍超预算时不再截（`fits = false`，由调用方决定拒绝或换 map-reduce 切块——切块归后续批次）。

use madinfer_backend::{Message, Role, Turn};

/// token 计数器。真实 tokenizer 绑定（HF / GGUF 词表）归后续批次；内置 [`ByteTokenizer`] 粗估。
pub trait Tokenizer {
    /// 文本的 token 数估计（≥ 实际值的保守估计为佳）。
    fn count_tokens(&self, text: &str) -> u32;
}

/// 字节粗估：`ceil(UTF-8 字节数 / bytes_per_token)`，空文本为 0。
///
/// 缺省每 token 4 字节——BPE 词表下中英混排的保守均值（英文 ~4 字节/token，CJK ~3 字节/token，取偏小分母会高估，宁高勿低）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteTokenizer {
    bytes_per_token: u32,
}

/// 缺省粗估分母。
pub const DEFAULT_BYTES_PER_TOKEN: u32 = 4;

impl ByteTokenizer {
    /// 缺省分母（4 字节 / token）。
    pub const fn new() -> Self {
        Self {
            bytes_per_token: DEFAULT_BYTES_PER_TOKEN,
        }
    }

    /// 自定义分母（0 视为 1，防除零）。
    pub const fn with_bytes_per_token(bytes_per_token: u32) -> Self {
        Self {
            bytes_per_token: if bytes_per_token == 0 {
                1
            } else {
                bytes_per_token
            },
        }
    }
}

impl Default for ByteTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Tokenizer for ByteTokenizer {
    fn count_tokens(&self, text: &str) -> u32 {
        let bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        let per = u64::from(self.bytes_per_token);
        u32::try_from(bytes.div_ceil(per)).unwrap_or(u32::MAX)
    }
}

/// 每条消息计入预算的模板开销（角色标记 ≈ 4 token，`<|im_start|>` 标记式口径的保守值）。
pub const MESSAGE_OVERHEAD_TOKENS: u32 = 4;

/// 预算结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetOutcome {
    /// 截断后的消息（保持原相对顺序）。
    pub messages: Vec<Message>,
    /// 保留消息的 token 计数（内容 + 每条 [`MESSAGE_OVERHEAD_TOKENS`]）。
    pub prompt_tokens: u32,
    /// 被弃掉的最旧非 system 消息数。
    pub dropped_messages: u32,
    /// 截断后是否已进预算（system 单独仍超预算时为 false）。
    pub fits: bool,
}

/// 预算截断纯函数：`max_prompt_tokens` 是 prompt 侧预算（生成侧 `max_tokens` 归采样参数）。
pub fn fit_to_budget(
    turn: &Turn,
    tokenizer: &dyn Tokenizer,
    max_prompt_tokens: u32,
) -> BudgetOutcome {
    let costs: Vec<u64> = turn
        .messages
        .iter()
        .map(|m| u64::from(tokenizer.count_tokens(&m.content)) + u64::from(MESSAGE_OVERHEAD_TOKENS))
        .collect();
    let budget = u64::from(max_prompt_tokens);
    let mut total: u64 = costs.iter().sum();
    let mut keep = vec![true; turn.messages.len()];
    let mut dropped: u32 = 0;
    if total > budget {
        for (i, message) in turn.messages.iter().enumerate() {
            if total <= budget {
                break;
            }
            if message.role == Role::System {
                continue;
            }
            keep[i] = false;
            dropped += 1;
            total -= costs[i];
        }
    }
    let messages: Vec<Message> = turn
        .messages
        .iter()
        .zip(keep.iter())
        .filter(|(_, kept)| **kept)
        .map(|(m, _)| m.clone())
        .collect();
    BudgetOutcome {
        messages,
        prompt_tokens: u32::try_from(total).unwrap_or(u32::MAX),
        dropped_messages: dropped,
        fits: total <= budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_owned(),
        }
    }

    #[test]
    fn byte_tokenizer_rounds_up_and_counts_utf8_bytes() {
        let tokenizer = ByteTokenizer::new();
        assert_eq!(tokenizer.count_tokens(""), 0);
        assert_eq!(tokenizer.count_tokens("abcd"), 1);
        assert_eq!(tokenizer.count_tokens("abcde"), 2);
        assert_eq!(tokenizer.count_tokens("端上推理"), 3, "12 字节 / 4");
        let coarse = ByteTokenizer::with_bytes_per_token(0);
        assert_eq!(coarse.count_tokens("ab"), 2, "分母 0 视为 1");
    }

    #[test]
    fn under_budget_turn_passes_through_unchanged() {
        let turn = Turn {
            messages: vec![
                message(Role::System, "abcd"),
                message(Role::User, "abcdefgh"),
            ],
        };
        let outcome = fit_to_budget(&turn, &ByteTokenizer::new(), 100);
        assert_eq!(outcome.messages, turn.messages);
        assert_eq!(outcome.prompt_tokens, (1 + 4) + (2 + 4));
        assert_eq!(outcome.dropped_messages, 0);
        assert!(outcome.fits);
    }

    #[test]
    fn over_budget_drops_oldest_non_system_first_and_keeps_system() {
        let turn = Turn {
            messages: vec![
                message(Role::System, "abcd"),     // 5
                message(Role::User, "abcdefgh"),   // 6（最旧，先弃）
                message(Role::Assistant, "abcd"),  // 5
                message(Role::User, "abcdefghij"), // 7（最新）
            ],
        };
        let outcome = fit_to_budget(&turn, &ByteTokenizer::new(), 17);
        assert_eq!(outcome.dropped_messages, 1);
        assert_eq!(outcome.prompt_tokens, 17);
        assert!(outcome.fits);
        assert_eq!(
            outcome
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["abcd", "abcd", "abcdefghij"],
            "system 保留、最旧 user 先弃、顺序不变"
        );

        let tighter = fit_to_budget(&turn, &ByteTokenizer::new(), 12);
        assert_eq!(tighter.dropped_messages, 2, "不够就继续弃次旧的");
        assert!(tighter.fits);
        assert_eq!(tighter.prompt_tokens, 12);
    }

    #[test]
    fn system_alone_over_budget_stops_truncation_and_reports_misfit() {
        let turn = Turn {
            messages: vec![
                message(Role::System, &"a".repeat(64)), // 16 + 4 = 20
                message(Role::User, "abcd"),
            ],
        };
        let outcome = fit_to_budget(&turn, &ByteTokenizer::new(), 10);
        assert_eq!(outcome.dropped_messages, 1, "非 system 全弃");
        assert!(!outcome.fits, "只剩 system 仍超预算");
        assert_eq!(outcome.messages.len(), 1);
        assert_eq!(outcome.messages[0].role, Role::System);
        assert_eq!(outcome.prompt_tokens, 20);
    }

    #[test]
    fn zero_budget_drops_everything_droppable() {
        let turn = Turn {
            messages: vec![message(Role::User, "abcd"), message(Role::User, "efgh")],
        };
        let outcome = fit_to_budget(&turn, &ByteTokenizer::new(), 0);
        assert!(outcome.messages.is_empty());
        assert_eq!(outcome.dropped_messages, 2);
        assert_eq!(outcome.prompt_tokens, 0);
        assert!(outcome.fits);
    }
}
