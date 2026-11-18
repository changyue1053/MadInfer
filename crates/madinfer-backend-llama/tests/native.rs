//! 真实引擎用例（cargo feature `llama-sys`；Cargo.toml `required-features` → 不带 feature 不编不跑）。
//!
//! 模型经环境变量 `MADINFER_LLAMA_MODEL`（GGUF 路径，`tools/models/fetch.sh <id>` 拉到 `~/.cache/madinfer/models/`）门控：
//! 缺席时模型用例失败，不允许 skip-as-green（`-- --nocapture` 可见）。链接自检与错误映射两例不需要模型。
//! 金样 `tests/golden/llama_host.txt`：宿主 macOS arm64 · CPU · greedy · 1 线程；Linux x86_64 使用单独向量，测试不允许自动重写期望值。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use madinfer_backend::{
    Backend, BackendError, CancelToken, ContextOptions, FinishReason, GenerateOptions, LoadOpts,
    ModelFormat, Quant, Turn,
};
use madinfer_backend_llama::{
    native_engine_available, native_engine_build_number, native_greedy_trace, LlamaBackend,
    LlamaOptions,
};
use madinfer_testkit::{contract, CollectingSink};

const MODEL_ENV: &str = "MADINFER_LLAMA_MODEL";
const GOLDEN_PROMPT: &str = "The capital of France is";
const GOLDEN_MAX_TOKENS: u32 = 32;
const GOLDEN_THREADS: u8 = 1;

fn golden_path() -> PathBuf {
    let name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "llama_linux_x86_64.txt",
        ("macos", "aarch64") => "llama_host.txt",
        _ => panic!("No reviewed native golden for this platform"),
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn model_path() -> Option<PathBuf> {
    match std::env::var_os(MODEL_ENV) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => panic!("MADINFER_LLAMA_MODEL is required: real inference was NOT tested"),
    }
}

fn small_options() -> LlamaOptions {
    LlamaOptions {
        max_context_tokens: 512,
        ..LlamaOptions::default()
    }
}

fn greedy(max_tokens: u32) -> GenerateOptions {
    GenerateOptions {
        temperature_x100: 0,
        top_p_bp: 10_000,
        max_tokens,
        stop: Vec::new(),
        seed: Some(0),
    }
}

/// third_party/PINS.toml 的 `tag = "b<n>"` → n。
fn pinned_tag_number() -> i32 {
    let pins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/PINS.toml");
    let text = std::fs::read_to_string(&pins).expect("read PINS.toml");
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("tag"))
        .expect("PINS.toml [llama_cpp] tag 行");
    let value = line.split('"').nth(1).expect("tag 引号值");
    value
        .strip_prefix('b')
        .expect("tag 形如 b<n>")
        .parse()
        .expect("tag 数字")
}

#[test]
fn native_engine_is_linked_and_matches_the_pin() {
    assert!(
        native_engine_available(),
        "桥在场且 build number 与钉版一致"
    );
    assert_eq!(native_engine_build_number(), pinned_tag_number());
}

#[test]
fn native_load_maps_missing_and_corrupt_files() {
    let missing = LlamaBackend::native(small_options(), "/nonexistent/madinfer-i-c-15.gguf");
    assert!(missing.is_native());
    assert_eq!(
        missing.load(&LoadOpts::default()).err(),
        Some(BackendError::ModelMissing)
    );

    let bogus = std::env::temp_dir().join("madinfer-i-c-15-bogus.gguf");
    std::fs::write(&bogus, b"not a gguf file").expect("write bogus");
    let corrupt = LlamaBackend::native(small_options(), &bogus);
    assert_eq!(
        corrupt.load(&LoadOpts::default()).err(),
        Some(BackendError::ModelCorrupt)
    );
    let _ = std::fs::remove_file(&bogus);
}

#[test]
fn native_probe_declares_no_constraint_support() {
    let backend = LlamaBackend::native(small_options(), "/nonexistent.gguf");
    let caps = backend.probe();
    assert_eq!(caps.constraint, madinfer_backend::ConstraintSupport::None);
    assert!(caps.supports_abort);
    assert_eq!(caps.max_context_tokens, 512);
    assert!(!LlamaBackend::new(small_options()).is_native());
}

/// 金样文件：`key = value` 行；`#` 注释。
fn parse_golden(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .map(|l| {
            let (k, v) = l.split_once('=').unwrap();
            (k.trim().to_owned(), v.trim().to_owned())
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[test]
fn native_golden_matches_llama_host_txt() {
    let Some(model) = model_path() else { return };
    let trace = native_greedy_trace(&model, GOLDEN_PROMPT, GOLDEN_MAX_TOKENS, GOLDEN_THREADS)
        .expect("trace");
    let ids: Vec<String> = trace.tokens.iter().map(ToString::to_string).collect();
    let prompt_ids: Vec<String> = trace
        .prompt_tokens
        .iter()
        .map(ToString::to_string)
        .collect();

    let golden = parse_golden(&std::fs::read_to_string(golden_path()).expect("read golden"));
    assert_eq!(golden["prompt"], GOLDEN_PROMPT);
    assert_eq!(
        golden["llama_build"],
        native_engine_build_number().to_string()
    );
    assert_eq!(
        golden["prompt_tokens"],
        prompt_ids.join(" "),
        "prompt 词元化漂移"
    );
    assert_eq!(golden["tokens"], ids.join(" "), "greedy 词元序列漂移");
    assert_eq!(golden["text_hex"], hex(trace.text.as_bytes()), "文本漂移");
    assert_eq!(trace.tokens.len(), GOLDEN_MAX_TOKENS as usize);
}

#[test]
fn native_greedy_is_deterministic_across_loads_and_contexts() {
    let Some(model) = model_path() else { return };
    let a = native_greedy_trace(&model, GOLDEN_PROMPT, 16, 1).expect("a");
    let b = native_greedy_trace(&model, GOLDEN_PROMPT, 16, 1).expect("b");
    assert_eq!(a, b, "两次独立加载 + 解码逐位相等");
    let c = native_greedy_trace(&model, GOLDEN_PROMPT, 16, 4).expect("c");
    assert_eq!(
        a.tokens, c.tokens,
        "1 线程与 4 线程同序列（本机读数；跨线程数确定性见报告 §1）"
    );

    // 经 Context trait：同一模型两个上下文、同一上下文两轮，文本与统计相等。
    let backend = LlamaBackend::native(small_options(), &model);
    let loaded = backend.load(&LoadOpts::default()).expect("load");
    let run = |ctx: &mut dyn madinfer_backend::Context| {
        let mut sink = CollectingSink::new();
        let stats = ctx
            .generate(
                &Turn::user(GOLDEN_PROMPT),
                &greedy(16),
                None,
                &mut sink,
                &CancelToken::new(),
            )
            .expect("generate");
        (sink.text(), stats, sink.pieces.len())
    };
    let mut ctx = loaded
        .open_context(&ContextOptions::default())
        .expect("open");
    let first = run(ctx.as_mut());
    let second = run(ctx.as_mut());
    assert_eq!(first, second, "同上下文两轮相等（每轮清 KV）");
    assert_eq!(
        first.0,
        a.text.replace('\u{FFFD}', ""),
        "trait 文本与轨迹文本一致（残字节除外）"
    );
    assert_eq!(first.1.gen_tokens, 16);
    assert_eq!(first.1.finish, FinishReason::Length);
    assert!(first.2 >= 1 && first.2 <= 16, "每片 ≥ 1 词元");
    loaded.unload();
}

#[test]
fn native_meta_and_footprint_come_from_the_engine() {
    let Some(model) = model_path() else { return };
    let backend = LlamaBackend::native(small_options(), &model);
    let loaded = backend.load(&LoadOpts::default()).expect("load");
    let meta = loaded.meta();
    assert_eq!(meta.format, ModelFormat::Gguf);
    assert!(meta.params_m > 0, "参数量来自引擎：{meta:?}");
    assert!(meta.max_context_tokens >= 512, "训练上下文：{meta:?}");
    assert_ne!(meta.quant, Quant::W8A8);

    let before = loaded.footprint();
    assert!(before.mapped_mb >= before.resident_mb, "{before:?}");
    assert!(before.resident_mb > 0, "驻留 = 张量字节：{before:?}");
    let ctx = loaded
        .open_context(&ContextOptions::default())
        .expect("open");
    let after = loaded.footprint();
    assert!(
        after.resident_mb > before.resident_mb,
        "打开上下文加 KV：{before:?} → {after:?}"
    );
    assert!(after.mapped_mb >= after.resident_mb);
    drop(ctx);
    assert_eq!(loaded.footprint(), before, "释放上下文后回落");
    loaded.unload();
}

#[test]
fn native_contract_six_with_testkit() {
    let Some(model) = model_path() else { return };
    let backend = LlamaBackend::native(small_options(), &model);
    contract::abort_within_one_token(&backend);
    contract::no_tokens_after_cancel(&backend);
    contract::constraint_matches_caps(&backend);
    contract::concurrency_limit_is_enforced(&backend);
    contract::errors_map_to_stable_codes();
    contract::footprint_is_monotone(&backend);
}

#[test]
fn native_stop_string_and_sink_stop_finish_reasons() {
    let Some(model) = model_path() else { return };
    let backend = LlamaBackend::native(small_options(), &model);
    let loaded = backend.load(&LoadOpts::default()).expect("load");
    let mut ctx = loaded
        .open_context(&ContextOptions::default())
        .expect("open");

    // 全量文本里挑一个必然出现的子串作停止串（第一个非空白片）。
    let full = native_greedy_trace(&model, GOLDEN_PROMPT, 16, 1).expect("trace");
    let stop_word = full
        .text
        .split_whitespace()
        .nth(1)
        .expect("至少两个词")
        .to_owned();
    let mut sink = CollectingSink::new();
    let options = GenerateOptions {
        stop: vec![stop_word.clone()],
        ..greedy(16)
    };
    let stats = ctx
        .generate(
            &Turn::user(GOLDEN_PROMPT),
            &options,
            None,
            &mut sink,
            &CancelToken::new(),
        )
        .expect("generate");
    assert_eq!(stats.finish, FinishReason::Stop);
    assert!(
        !sink.text().contains(&stop_word),
        "含停止串的片不发：{:?}",
        sink.text()
    );
    assert!(sink.pieces.last().is_none_or(|p| p.is_last));

    let mut stopping = CollectingSink {
        stop_on_piece: Some(2),
        ..CollectingSink::default()
    };
    let stats = ctx
        .generate(
            &Turn::user(GOLDEN_PROMPT),
            &greedy(16),
            None,
            &mut stopping,
            &CancelToken::new(),
        )
        .expect("generate");
    assert_eq!(stats.finish, FinishReason::SinkStopped);
    assert_eq!(stopping.pieces.len(), 2);
    loaded.unload();
}

/// 读数用例（报告 §1 风险清单）：加载耗时、解码速度。测试夹具豁免墙钟（clippy.toml 豁免协议第四类）。
#[test]
#[allow(clippy::disallowed_methods)]
fn native_readout_load_time_and_decode_speed() {
    let Some(model) = model_path() else { return };
    let t0 = std::time::Instant::now();
    let backend = LlamaBackend::native(small_options(), &model);
    let loaded = backend.load(&LoadOpts::default()).expect("load");
    let load_ms = t0.elapsed().as_millis();
    let mut ctx = loaded
        .open_context(&ContextOptions::default())
        .expect("open");
    let mut sink = CollectingSink::new();
    let t1 = std::time::Instant::now();
    let stats = ctx
        .generate(
            &Turn::user(GOLDEN_PROMPT),
            &greedy(GOLDEN_MAX_TOKENS),
            None,
            &mut sink,
            &CancelToken::new(),
        )
        .expect("generate");
    let gen_ms = t1.elapsed().as_millis().max(1);
    let footprint = loaded.footprint();
    eprintln!(
        "readout: load_ms={load_ms} prompt_tokens={} gen_tokens={} gen_ms={gen_ms} tok_per_s={:.1} footprint={footprint:?} meta={:?}",
        stats.prompt_tokens,
        stats.gen_tokens,
        f64::from(stats.gen_tokens) * 1000.0 / gen_ms as f64,
        loaded.meta()
    );
    assert!(load_ms < 60_000, "加载 {load_ms} ms 超 60 s");
    loaded.unload();
}
