//! 真实引擎金样进 `cargo test`（madai-w17 I-C-16 ④；本模块只在 feature `llama-sys` 下编）。
//!
//! 门控：`MADINFER_LLAMA_MODEL`（GGUF 路径，`tools/models/fetch.sh <id>`）缺席 → 失败，不允许 skip-as-green。
//! 金样事实源仍是 `crates/madinfer-backend-llama/tests/golden/llama_host.txt`（宿主 macOS arm64 · CPU · greedy）：本用例走**核站全链**
//! （`InferCore::create(backend = Llama, model_path)` → 路由 → worker `load(LoadOpts::weights_path)` → `stream`）取前 8 词元，
//! 断言文本是金样 32 词元文本的前缀——同一模型、同一 prompt、greedy，链路不同结果不得漂移。

use std::path::{Path, PathBuf};

use super::*;

const MODEL_ENV: &str = "MADINFER_LLAMA_MODEL";
const GOLDEN_PROMPT: &str = "The capital of France is";
const GOLDEN_TOKENS: u32 = 8;

fn model_path() -> Option<String> {
    match std::env::var(MODEL_ENV) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => panic!("MADINFER_LLAMA_MODEL is required: real inference was NOT tested"),
    }
}

/// 金样文件的 `text_hex = …` 行 → 文本。
fn golden_text() -> String {
    let name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "llama_linux_x86_64.txt",
        ("macos", "aarch64") => "llama_host.txt",
        _ => panic!("No reviewed native golden for this platform"),
    };
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../madinfer-backend-llama/tests/golden")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("read golden");
    let hex = text
        .lines()
        .find_map(|l| l.strip_prefix("text_hex"))
        .and_then(|rest| rest.split_once('='))
        .map(|(_, v)| v.trim().to_owned())
        .expect("golden text_hex 行");
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect();
    String::from_utf8(bytes).expect("golden utf8")
}

fn llama_options(model_path: &str) -> CoreOptions {
    CoreOptions {
        backend: BackendKindRecord::Llama,
        model_path: Some(model_path.to_owned()),
        ..options(&["summarize"], None)
    }
}

#[test]
fn llama_station_streams_golden_prefix_and_reports_backend_kind() {
    let Some(model) = model_path() else { return };
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), llama_options(&model)).expect("create llama");

    let probe = core.probe();
    assert_eq!(probe.backend, BackendKindRecord::Llama);
    assert_eq!(probe.entries.len(), 1);
    assert_eq!(probe.entries[0].state, CapStateRecord::Available);
    assert_eq!(
        probe.entries[0].constraint,
        ConstraintSupportRecord::None,
        "真实路径未接 GBNF 采样器，声明 None（I-C-15 判断 5）"
    );
    let stats = core.stats().expect("stats");
    assert_eq!(stats.backend_kind, BackendKindRecord::Llama);
    assert_eq!(stats.context_pool_entries, 0, "加载前池空");

    let (tx, rx) = mpsc::channel();
    let mut req = request(
        PrivacyRecord::Public,
        QosRecord::Interactive,
        GOLDEN_PROMPT,
        GOLDEN_TOKENS,
    );
    req.options.temperature_x100 = Some(0);
    req.options.seed = Some(0);
    req.context = Some("conv:golden".to_owned());
    core.stream(req, Box::new(ChannelSink(tx))).expect("stream");
    let events = collect_until_terminal(&rx);
    assert_quiet(&rx);
    assert_eq!(started_count(&events), 1);
    let Some(Recorded::Complete(complete)) = events.last() else {
        panic!("应正常完结：{events:?}");
    };
    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            Recorded::Delta(d) => Some(d.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(complete.text, deltas, "Complete.text = Delta 拼接");
    assert_eq!(complete.stats.gen_tokens, GOLDEN_TOKENS);
    assert_eq!(complete.stats.prompt_tokens, 5, "金样 prompt_tokens 5 词元");
    let golden = golden_text();
    assert!(!complete.text.is_empty());
    assert!(
        golden.starts_with(&complete.text),
        "前 {GOLDEN_TOKENS} 词元文本须是金样前缀：got={:?} golden={golden:?}",
        complete.text
    );

    // 池记账（I-C-14 口径）：首键在模型加载前预留，按 Backend::context_kv_mb 常数 8 记；加载后 ModelLoaded 带回 Model::context_kv_mb
    // 真值（n_ctx 4096 下 smollm2-135m ≈ 90 MB），之后的新键按真值记——第二个键的读数即真值，≠ 8 且在 Flagship 缺省预算 128 内。
    let first = wait_until(&core, |s| s.context_pool_entries >= 1);
    assert_eq!(first.context_pool_entries, 1, "{first:?}");
    assert_eq!(
        first.context_pool_kv_mb, 8,
        "首键按加载前估值记账（残项，报告 §6）：{first:?}"
    );

    let (tx, rx) = mpsc::channel();
    let mut req = request(
        PrivacyRecord::Public,
        QosRecord::Interactive,
        GOLDEN_PROMPT,
        GOLDEN_TOKENS,
    );
    req.options.temperature_x100 = Some(0);
    req.context = Some("conv:golden-2".to_owned());
    core.stream(req, Box::new(ChannelSink(tx)))
        .expect("stream 2");
    let Some(Recorded::Complete(second)) = collect_until_terminal(&rx).pop() else {
        panic!("第二轮应正常完结");
    };
    assert_eq!(second.text, complete.text, "同模型同 prompt 同文本");
    // 真实引擎席位 concurrency = 1：worker 为第二键腾掉首键句柄、首键离池——池里只剩第二键，其 KV 即 Model::context_kv_mb 真值。
    let pooled = wait_until(&core, |s| s.context_pool_kv_mb != 8);
    assert_eq!(
        pooled.context_pool_entries, 1,
        "席位 1，首键被腾：{pooled:?}"
    );
    let truth_mb = pooled.context_pool_kv_mb;
    assert!(
        truth_mb > 8,
        "第二键按 Model::context_kv_mb 真值记账（> 常数 8）：{pooled:?}"
    );
    assert!(
        pooled.context_pool_kv_mb <= pooled.context_pool_kv_budget_mb,
        "在预算内：{pooled:?}"
    );
    eprintln!(
        "readout: backend={} gen_tokens={} ttft_ms={} decode_ms={} text={:?} first_key_kv_mb={} second_key_kv_mb={truth_mb} budget_mb={}",
        stats.backend_kind.wire(),
        complete.stats.gen_tokens,
        complete.stats.ttft_ms,
        complete.stats.decode_ms,
        complete.text,
        first.context_pool_kv_mb,
        pooled.context_pool_kv_budget_mb
    );
}

/// 终态回执与池结清在 control 线程上先后两步；轮询到读数满足条件再断言（上限 2 s）。
fn wait_until(core: &InferCore, done: impl Fn(&CoreStatsRecord) -> bool) -> CoreStatsRecord {
    let mut stats = core.stats().expect("stats");
    for _ in 0..100 {
        if done(&stats) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        stats = core.stats().expect("stats");
    }
    stats
}

#[test]
fn llama_station_is_deterministic_across_two_stations() {
    let Some(model) = model_path() else { return };
    let run = || {
        let bridge = leak_bridge(ConsentDecisionRecord::Deny);
        let core = InferCore::create(Box::new(bridge), llama_options(&model)).expect("create");
        let (tx, rx) = mpsc::channel();
        let mut req = request(
            PrivacyRecord::Public,
            QosRecord::Interactive,
            GOLDEN_PROMPT,
            GOLDEN_TOKENS,
        );
        req.options.temperature_x100 = Some(0);
        core.stream(req, Box::new(ChannelSink(tx))).expect("stream");
        match collect_until_terminal(&rx).pop() {
            Some(Recorded::Complete(c)) => c.text,
            other => panic!("应正常完结：{other:?}"),
        }
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "两站两次加载同文本（greedy）");
}
