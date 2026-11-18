//! 数值金样（C7 卡⑦的仿真核形态）：固定 seed + greedy（temperature 0）输出与内嵌金样逐字节比对。
//! 金样锁的是仿真核的确定性行为——任何改动使输出漂移都会在此判红，需在 PR 里显式换金样。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use madinfer_backend::{
    Backend, CancelToken, ContextOptions, FinishReason, GenerateOptions, LoadOpts, Message, Role,
    Turn,
};
use madinfer_backend_llama::{LlamaBackend, LlamaOptions};
use madinfer_testkit::CollectingSink;

fn golden_turn() -> Turn {
    Turn {
        messages: vec![
            Message {
                role: Role::System,
                content: "回声测试".to_owned(),
            },
            Message {
                role: Role::User,
                content: "苹果 香蕉 樱桃 山楂".to_owned(),
            },
        ],
    }
}

fn greedy(seed: u64) -> GenerateOptions {
    GenerateOptions {
        temperature_x100: 0,
        top_p_bp: 10_000,
        max_tokens: 512,
        stop: Vec::new(),
        seed: Some(seed),
    }
}

fn run(seed: u64) -> (String, u32, u32, FinishReason) {
    let backend = LlamaBackend::new(LlamaOptions::default());
    let model = backend.load(&LoadOpts::default()).expect("load");
    let mut context = model
        .open_context(&ContextOptions::default())
        .expect("open");
    let mut sink = CollectingSink::new();
    let stats = context
        .generate(
            &golden_turn(),
            &greedy(seed),
            None,
            &mut sink,
            &CancelToken::new(),
        )
        .expect("generate");
    (
        sink.text(),
        stats.prompt_tokens,
        stats.gen_tokens,
        stats.finish,
    )
}

/// seed = 42 的内嵌金样。
const GOLDEN_SEED_42: &str = "「山楂 回声测试 苹果 香蕉 樱桃」";
/// seed = 7 的内嵌金样（与 seed 42 旋转不同，锁 seed 敏感性）。
const GOLDEN_SEED_7: &str = "「樱桃 山楂 回声测试 苹果 香蕉」";

#[test]
fn greedy_seed_42_matches_the_embedded_golden() {
    let (text, prompt_tokens, gen_tokens, finish) = run(42);
    assert_eq!(text, GOLDEN_SEED_42);
    assert_eq!(prompt_tokens, 7, "5 词元 + 2 条消息");
    assert_eq!(gen_tokens, 7, "「 + 5 词元 + 」");
    assert_eq!(finish, FinishReason::Stop);
}

#[test]
fn greedy_seed_7_matches_the_embedded_golden_and_differs_from_42() {
    let (text, _, _, _) = run(7);
    assert_eq!(text, GOLDEN_SEED_7);
    assert_ne!(text, GOLDEN_SEED_42, "两个金样锁住 seed 敏感性");
}
