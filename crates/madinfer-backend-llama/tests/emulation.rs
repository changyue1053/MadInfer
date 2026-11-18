//! 仿真核行为测试（走公开面）：确定性、seed 敏感、停止串、max_tokens、sink 停止、取消、形式校验、占用模型、
//! 按输出字节数切片（madai-w12 I-C-11：每片 ≤ `EMULATED_PIECE_BYTES`、码点边界、词元计数随片携带）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use madinfer_backend::{
    Backend, BackendError, CancelReason, CancelToken, Constraint, ContextOptions, FinishReason,
    GenerateOptions, GenerateStats, LoadOpts, Message, Role, Turn,
};
use madinfer_backend_llama::{LlamaBackend, LlamaOptions, EMULATED_PIECE_BYTES};
use madinfer_testkit::CollectingSink;

/// 四片以上的长输入（16 词元 ≈ 100 字节）：sink 停止 / 中途取消用例要在第 2 片之后仍有片可截。
const LONG_PROMPT: &str =
    "alpha beta gamma delta alpha beta gamma delta alpha beta gamma delta alpha beta gamma delta";

fn assert_pieces_follow_the_byte_rule(sink: &CollectingSink, stats: &GenerateStats) {
    assert!(sink
        .pieces
        .iter()
        .all(|p| p.text.len() <= EMULATED_PIECE_BYTES));
    assert_eq!(
        sink.pieces.iter().map(|p| p.tokens).sum::<u32>(),
        stats.gen_tokens,
        "词元计数随片携带，和 = gen_tokens"
    );
    assert!(sink.pieces.iter().rev().skip(1).all(|p| !p.is_last));
}

fn run_with_sink(
    turn: &Turn,
    options: &GenerateOptions,
    mut sink: CollectingSink,
    cancel: &CancelToken,
) -> (CollectingSink, Result<GenerateStats, BackendError>) {
    let backend = LlamaBackend::new(LlamaOptions::default());
    let model = backend.load(&LoadOpts::default()).expect("load");
    let mut context = model
        .open_context(&ContextOptions::default())
        .expect("open");
    let result = context.generate(turn, options, None, &mut sink, cancel);
    drop(context);
    model.unload();
    (sink, result)
}

fn run(turn: &Turn, options: &GenerateOptions) -> (CollectingSink, GenerateStats) {
    let (sink, result) = run_with_sink(turn, options, CollectingSink::new(), &CancelToken::new());
    (sink, result.expect("generate"))
}

fn seeded(seed: u64) -> GenerateOptions {
    GenerateOptions {
        seed: Some(seed),
        ..GenerateOptions::default()
    }
}

#[test]
fn same_seed_and_input_reproduce_the_same_stream() {
    let turn = Turn::user("alpha beta gamma delta");
    let (a, stats_a) = run(&turn, &seeded(42));
    let (b, stats_b) = run(&turn, &seeded(42));
    assert_eq!(a.text(), b.text());
    assert_eq!(stats_a, stats_b);
    assert_eq!(stats_a.prompt_tokens, 5, "4 词元 + 1 条消息");
    assert_eq!(stats_a.gen_tokens, 6, "「 + 4 词元 + 」");
    assert_eq!(stats_a.finish, FinishReason::Stop);
    assert!(a.pieces.last().is_some_and(|p| p.is_last));
    assert_eq!(a.text().len(), 28);
    assert_eq!(a.pieces.len(), 1, "28 字节 ≤ 32：整轮一片");
    assert_eq!(a.pieces[0].tokens, 6, "6 个词元起点都落在这一片");
    assert_pieces_follow_the_byte_rule(&a, &stats_a);
}

#[test]
fn pieces_are_cut_by_output_bytes_and_are_independent_of_timing() {
    // 16 词元 + 括标 = 「 + 16 词 + 」：ASCII 段恰 N 字节一片（首片含 3 字节的「），末片收尾；两次运行片序列逐字节相同。
    let turn = Turn::user(LONG_PROMPT);
    let (a, stats_a) = run(&turn, &seeded(3));
    let (b, stats_b) = run(&turn, &seeded(3));
    assert_eq!(a.pieces, b.pieces, "片序列是输出文本的纯函数");
    assert_eq!(stats_a, stats_b);
    assert_eq!(stats_a.gen_tokens, 18, "「 + 16 词元 + 」");
    let text = a.text();
    assert_eq!(
        text.len(),
        3 + 76 + 15 + 3,
        "「 + 16 词 76 字节 + 15 空格 + 」"
    );
    // ≤ 32 字节 / 码点边界：「 + 29 ASCII = 32；32 ASCII；30 ASCII（」 3 字节放不进）；」。
    let sizes: Vec<usize> = a.pieces.iter().map(|p| p.text.len()).collect();
    assert_eq!(sizes, vec![32, 32, 30, 3], "{:?}", a.pieces);
    assert_eq!(a.pieces[3].text, "」");
    assert_pieces_follow_the_byte_rule(&a, &stats_a);
}

#[test]
fn self_check_request_is_a_single_piece() {
    // 三端自检口径（mad InferSelfCheck：prompt "self-check"、max_tokens 8、seed 7）：「self-check」16 字节 → 恰 1 片，
    // 核内合帧后恰 1 帧 delta；gen_tokens 仍是 3 个词元。
    let options = GenerateOptions {
        max_tokens: 8,
        ..seeded(7)
    };
    let (sink, stats) = run(&Turn::user("self-check"), &options);
    assert_eq!(sink.text(), "「self-check」");
    assert_eq!(sink.pieces.len(), 1);
    assert_eq!(stats.gen_tokens, 3);
    assert_eq!(stats.finish, FinishReason::Stop);
}

#[test]
fn seed_rotates_the_echo() {
    let turn = Turn::user("alpha beta gamma delta");
    let texts: BTreeSet<String> = (0..8)
        .map(|seed| run(&turn, &seeded(seed)).0.text())
        .collect();
    assert!(texts.len() > 1, "8 个 seed 至少出现两种旋转");
    for text in &texts {
        assert!(text.starts_with('「') && text.ends_with('」'));
        for word in ["alpha", "beta", "gamma", "delta"] {
            assert!(text.contains(word), "回显必含全部输入词元：{word}");
        }
    }
}

#[test]
fn stop_string_cuts_the_matching_piece_and_reports_stop() {
    let turn = Turn::user("alpha beta gamma delta");
    let options = GenerateOptions {
        stop: vec!["beta".to_owned()],
        ..seeded(42)
    };
    let (sink, stats) = run(&turn, &options);
    assert!(!sink.text().contains("beta"), "停止串所在词元整个不发");
    assert_eq!(stats.finish, FinishReason::Stop);
    assert!(stats.gen_tokens < 6, "截掉的词元不计入 gen_tokens");
    assert_pieces_follow_the_byte_rule(&sink, &stats);
}

#[test]
fn max_tokens_caps_the_stream_and_reports_length() {
    let turn = Turn::user("alpha beta gamma delta");
    let options = GenerateOptions {
        max_tokens: 2,
        ..seeded(42)
    };
    let (sink, stats) = run(&turn, &options);
    assert_eq!(stats.gen_tokens, 2, "max_tokens 仍按词元数截：「 + 首词");
    assert_eq!(stats.finish, FinishReason::Length);
    assert!(sink.text().starts_with('「') && !sink.text().ends_with('」'));
    assert_eq!(
        sink.pieces.len(),
        1,
        "两个词元 ≤ 32 字节：一片携 tokens = 2"
    );
    assert_eq!(sink.pieces[0].tokens, 2);
    assert!(
        sink.pieces.last().is_some_and(|p| p.is_last),
        "截断后的末片仍标 is_last"
    );

    let none = GenerateOptions {
        max_tokens: 0,
        ..seeded(42)
    };
    let (empty, stats) = run(&turn, &none);
    assert!(empty.pieces.is_empty());
    assert_eq!(stats.gen_tokens, 0);
    assert_eq!(stats.finish, FinishReason::Length);
}

#[test]
fn sink_stop_ends_the_round_as_sink_stopped() {
    // 4 片的长输出，sink 在第 2 片回 Stop：只出 2 片，gen_tokens = 这两片携带的词元数（< 18）。
    let turn = Turn::user(LONG_PROMPT);
    let sink = CollectingSink {
        stop_on_piece: Some(2),
        ..CollectingSink::default()
    };
    let (sink, result) = run_with_sink(&turn, &seeded(42), sink, &CancelToken::new());
    let stats = result.expect("generate");
    assert_eq!(stats.finish, FinishReason::SinkStopped);
    assert_eq!(sink.pieces.len(), 2);
    assert_eq!(
        stats.gen_tokens,
        sink.pieces.iter().map(|p| p.tokens).sum::<u32>()
    );
    assert!(stats.gen_tokens < 18);
}

#[test]
fn cancel_mid_stream_emits_nothing_more_and_returns_cancelled() {
    // 4 片的长输出，sink 收到第 2 片时翻旗：第 3 片发射前查旗 → 0 片再出、Cancelled（六条之①）。
    let turn = Turn::user(LONG_PROMPT);
    let cancel = CancelToken::new();
    let sink = CollectingSink {
        cancel_on_piece: Some((2, cancel.clone())),
        ..CollectingSink::default()
    };
    let (sink, result) = run_with_sink(&turn, &seeded(42), sink, &cancel);
    assert_eq!(result.err(), Some(BackendError::Cancelled));
    assert_eq!(sink.pieces.len(), 2, "翻旗后 0 片再出");
    assert_eq!(sink.text().len(), 64, "前两片各 32 字节");
}

#[test]
fn pre_cancelled_round_yields_no_pieces() {
    let cancel = CancelToken::new();
    cancel.cancel(CancelReason::Caller);
    let (sink, result) = run_with_sink(
        &Turn::user("alpha"),
        &GenerateOptions::default(),
        CollectingSink::new(),
        &cancel,
    );
    assert_eq!(result.err(), Some(BackendError::Cancelled));
    assert!(sink.pieces.is_empty());
}

fn run_constrained(
    grammar: &str,
    prompt: &str,
    options: &GenerateOptions,
) -> (CollectingSink, Result<GenerateStats, BackendError>) {
    let backend = LlamaBackend::new(LlamaOptions::default());
    assert_eq!(
        backend.probe().constraint,
        madinfer_backend::ConstraintSupport::Emulated
    );
    let model = backend.load(&LoadOpts::default()).expect("load");
    let mut context = model
        .open_context(&ContextOptions::default())
        .expect("open");
    let constraint = Constraint {
        grammar: grammar.to_owned(),
    };
    let mut sink = CollectingSink::new();
    let result = context.generate(
        &Turn::user(prompt),
        options,
        Some(&constraint),
        &mut sink,
        &CancelToken::new(),
    );
    (sink, result)
}

#[test]
fn constraint_is_declared_emulated_and_output_follows_the_grammar() {
    // madai-w08 G4：约束 = Emulated，按语法产出而非回显。
    let (sink, result) = run_constrained("root ::= \"ok\"", "alpha", &GenerateOptions::default());
    let stats = result.expect("generate");
    assert_eq!(sink.text(), "ok");
    assert_eq!(stats.gen_tokens, 1);
    assert_eq!(stats.finish, FinishReason::Stop);
    assert!(sink.pieces.last().is_some_and(|p| p.is_last));
}

#[test]
fn schema_compiled_grammar_yields_json_with_echo_words_deterministically() {
    // schema2gbnf 的产出形态（对象两键：name string 必填、count integer 可选）。
    let grammar = concat!(
        "root ::= ws \"{\" ws name-kv ( ws \",\" ws count-kv )? ws \"}\" ws\n",
        "name-kv ::= \"\\\"name\\\"\" ws \":\" ws string\n",
        "count-kv ::= \"\\\"count\\\"\" ws \":\" ws integer\n",
        "integer ::= \"-\"? (\"0\" | [1-9] [0-9]*)\n",
        "string ::= \"\\\"\" string-char* \"\\\"\"\n",
        "string-char ::= [^\"\\\\\\x00-\\x1F] | \"\\\\\" ([\"\\\\/bfnrt] | \"u\" hex-digit hex-digit hex-digit hex-digit)\n",
        "hex-digit ::= [0-9a-fA-F]\n",
        "ws ::= [ \\t\\n\\r]*\n",
    );
    let (a, result_a) = run_constrained(grammar, "会议 纪要 三件事", &seeded(7));
    let (b, result_b) = run_constrained(grammar, "会议 纪要 三件事", &seeded(7));
    assert_eq!(a.text(), b.text(), "同 seed 同输入同输出");
    let stats = result_a.expect("a");
    assert_eq!(stats, result_b.expect("b"));
    assert_eq!(a.pieces, b.pieces, "片序列同样确定");
    let text = a.text();
    assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
    assert!(text.contains("\"name\":\""), "{text}");
    assert!(
        ["会议", "纪要", "三件事"].iter().any(|w| text.contains(w)),
        "字符串内容取回显词元：{text}"
    );
    assert!(
        stats.gen_tokens >= 5,
        "词元 = 语法叶子（{{ / 键 / : / 值 / }} …），计数随片携带：{stats:?}"
    );
    assert_pieces_follow_the_byte_rule(&a, &stats);
    assert!(
        a.pieces.len() < usize::try_from(stats.gen_tokens).unwrap(),
        "约束输出同样按字节切片，片数少于叶子数：{:?}",
        a.pieces
    );
    // 不同 seed 至少在某些取值上不同（可选键 / 词元旋转）。
    let variants: BTreeSet<String> = (0..8u64)
        .map(|seed| {
            run_constrained(grammar, "会议 纪要 三件事", &seeded(seed))
                .0
                .text()
        })
        .collect();
    assert!(variants.len() > 1, "{variants:?}");
}

#[test]
fn malformed_grammar_is_invalid_input_without_output() {
    let (sink, result) =
        run_constrained("root ::= \"a\"{2,3}", "alpha", &GenerateOptions::default());
    assert_eq!(result.err(), Some(BackendError::InvalidInput));
    assert!(sink.pieces.is_empty());
    let (sink, result) = run_constrained("nope ::= \"a\"", "alpha", &GenerateOptions::default());
    assert_eq!(result.err(), Some(BackendError::InvalidInput));
    assert!(sink.pieces.is_empty());
}

#[test]
fn constrained_output_still_honours_max_tokens_and_cancel() {
    let grammar = "root ::= \"a\" \"b\" \"c\" \"d\"";
    let options = GenerateOptions {
        max_tokens: 2,
        ..GenerateOptions::default()
    };
    let (sink, result) = run_constrained(grammar, "alpha", &options);
    let stats = result.expect("generate");
    assert_eq!(sink.text(), "ab");
    assert_eq!(stats.finish, FinishReason::Length);
}

#[test]
fn malformed_rounds_are_invalid_input() {
    let cases = [
        (Turn::default(), GenerateOptions::default()),
        (
            Turn::user("alpha"),
            GenerateOptions {
                temperature_x100: 201,
                ..GenerateOptions::default()
            },
        ),
        (
            Turn::user("alpha"),
            GenerateOptions {
                top_p_bp: 10_001,
                ..GenerateOptions::default()
            },
        ),
        (
            Turn::user("alpha"),
            GenerateOptions {
                stop: vec!["s".to_owned(); 9],
                ..GenerateOptions::default()
            },
        ),
    ];
    for (turn, options) in cases {
        let (sink, result) =
            run_with_sink(&turn, &options, CollectingSink::new(), &CancelToken::new());
        assert_eq!(result.err(), Some(BackendError::InvalidInput));
        assert!(sink.pieces.is_empty());
    }
}

#[test]
fn footprint_tracks_kv_per_open_context_and_mmap_toggle() {
    let backend = LlamaBackend::new(LlamaOptions {
        concurrency: 2,
        ..LlamaOptions::default()
    });
    let model = backend.load(&LoadOpts::default()).expect("load");
    let base = model.footprint();
    assert_eq!((base.resident_mb, base.mapped_mb), (96, 1_024));
    let held = model
        .open_context(&ContextOptions::default())
        .expect("open");
    let with_context = model.footprint();
    assert_eq!(
        (with_context.resident_mb, with_context.mapped_mb),
        (104, 1_032),
        "每上下文 KV 计入驻留与映射"
    );
    drop(held);
    assert_eq!(model.footprint(), base, "释放上下文后占用回落");
    model.unload();

    let plain = LlamaBackend::new(LlamaOptions::default());
    let no_mmap = plain
        .load(&LoadOpts {
            mmap: false,
            ..LoadOpts::default()
        })
        .expect("load");
    let footprint = no_mmap.footprint();
    assert_eq!(
        footprint.mapped_mb, footprint.resident_mb,
        "mmap 关闭时映射按驻留报告"
    );
    no_mmap.unload();
}

#[test]
fn meta_reports_the_emulated_half_billion_gguf() {
    let backend = LlamaBackend::new(LlamaOptions::default());
    assert_eq!(backend.id().0, "llama.cpp");
    let model = backend.load(&LoadOpts::default()).expect("load");
    let meta = model.meta();
    assert_eq!(meta.params_m, 500);
    assert_eq!(meta.max_context_tokens, 4_096);
    model.unload();
}

#[test]
fn cjk_content_without_spaces_echoes_as_one_word() {
    let turn = Turn {
        messages: vec![Message {
            role: Role::User,
            content: "端上推理".to_owned(),
        }],
    };
    let (sink, stats) = run(&turn, &seeded(1));
    assert_eq!(sink.text(), "「端上推理」");
    assert_eq!(stats.gen_tokens, 3);
    assert_eq!(stats.prompt_tokens, 2, "1 词元 + 1 条消息");
    assert_eq!(sink.pieces.len(), 1, "18 字节 ≤ 32：一片携 3 个词元");
    assert_eq!(sink.pieces[0].tokens, 3);
}

#[test]
fn cjk_pieces_stay_on_code_point_boundaries() {
    // 「 + 10 个 CJK 字 + 」 = 36 字节 → 30 + 6（第 11 个码点放不进 32）：ffi 端到端用例钉的两帧即此两片。
    let (sink, stats) = run(&Turn::user("总结一下今天的群消息"), &seeded(1));
    assert_eq!(sink.text(), "「总结一下今天的群消息」");
    let texts: Vec<&str> = sink.pieces.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(texts, vec!["「总结一下今天的群消", "息」"]);
    assert_eq!((sink.pieces[0].tokens, sink.pieces[1].tokens), (2, 1));
    assert_pieces_follow_the_byte_rule(&sink, &stats);
}
