//! I-CD-01 单测：K1 序、cancel 恰一次（3008）、panic 收口、回调线程任意性（回调里再调 API 不死锁）、
//! 授权回调（任意线程回执）、错误码空间。走真核（`InferCore::create` → backend-llama 仿真核）。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 测试模块放开（根 Cargo.toml lints 注）

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, RwLock};
use std::time::Duration;

use madinfer_core_types::{DegradeKind, FatalKind, RetryKind};

use super::*;

/// 调试面用例（create_debug / test_stop_control 两 profile 都导出；debug 真语义 / release 恒 4005，模块内按 cfg 分两组）。
mod debug;
/// 真实引擎金样用例（madai-w17 I-C-16；feature `llama-sys` + `MADINFER_LLAMA_MODEL` 门控，缺席 skip 留痕）。
#[cfg(feature = "llama-sys")]
mod llama;

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);

/// 录制事件（断言面）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Recorded {
    Started(StartedRecord),
    Delta(DeltaRecord),
    Complete(CompleteRecord),
    Failed(FailedRecord),
}

impl Recorded {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete(_) | Self::Failed(_))
    }
}

/// 通道 sink（只入队立即返回，K12 纪律的测试化）。
struct ChannelSink(Sender<Recorded>);

impl EventSink for ChannelSink {
    fn on_started(&self, event: StartedRecord) {
        let _ = self.0.send(Recorded::Started(event));
    }
    fn on_delta(&self, event: DeltaRecord) {
        let _ = self.0.send(Recorded::Delta(event));
    }
    fn on_complete(&self, event: CompleteRecord) {
        let _ = self.0.send(Recorded::Complete(event));
    }
    fn on_failed(&self, event: FailedRecord) {
        let _ = self.0.send(Recorded::Failed(event));
    }
}

/// 录制宿主桥：单调钟 + 内存 KV + 观测批次录制 + 脚本化授权（记录回执线程名；可选阻塞——
/// 模拟"弹窗等用户"，靠 [`Self::consent_release`] 放行）。
struct RecordingBridge {
    clock: madinfer_host::MonotonicClock,
    kv: RwLock<std::collections::BTreeMap<String, Vec<u8>>>,
    batches: RwLock<Vec<TelemetryBatchRecord>>,
    consent_answer: ConsentDecisionRecord,
    consent_calls: RwLock<Vec<(ConsentRecord, String)>>,
    consent_blocking: bool,
    consent_entered: std::sync::atomic::AtomicBool,
    consent_release: std::sync::atomic::AtomicBool,
    /// 线程钩子回执：(类别, 回调所在线程名)——madai-w08 G7。
    worker_starts: RwLock<Vec<(WorkerClassRecord, String)>>,
    /// 验签回调所在线程名（逐次）——madai-w10 I-C-09 J1 / madai-w11 I-C-10：契约钉"在瞬时线程 madinfer-ffi-verify 上调"。
    verify_threads: RwLock<Vec<String>>,
    /// 置位后 verify_ed25519 阻塞到 [`Self::verify_release`] 放行（模拟宿主验签卡死；madai-w11 I-C-10 deadline 用例）。
    verify_blocking: std::sync::atomic::AtomicBool,
    verify_release: std::sync::atomic::AtomicBool,
    /// verify_ed25519 已返回的次数（含超时后迟到返回的）。
    verify_returned: std::sync::atomic::AtomicU64,
}

impl RecordingBridge {
    fn new(consent_answer: ConsentDecisionRecord) -> Self {
        Self {
            clock: madinfer_host::MonotonicClock::new(),
            kv: RwLock::new(std::collections::BTreeMap::new()),
            batches: RwLock::new(Vec::new()),
            consent_answer,
            consent_calls: RwLock::new(Vec::new()),
            consent_blocking: false,
            consent_entered: std::sync::atomic::AtomicBool::new(false),
            consent_release: std::sync::atomic::AtomicBool::new(false),
            worker_starts: RwLock::new(Vec::new()),
            verify_threads: RwLock::new(Vec::new()),
            verify_blocking: std::sync::atomic::AtomicBool::new(false),
            verify_release: std::sync::atomic::AtomicBool::new(false),
            verify_returned: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn blocking(mut self) -> Self {
        self.consent_blocking = true;
        self
    }
}

impl HostBridge for &'static RecordingBridge {
    fn now_ms(&self) -> u64 {
        madinfer_host::Clock::now_ms(&self.clock)
    }
    fn kv_get(&self, key: String) -> Option<Vec<u8>> {
        self.kv.read().unwrap().get(&key).cloned()
    }
    fn kv_put(&self, key: String, value: Vec<u8>) {
        self.kv.write().unwrap().insert(key, value);
    }
    fn telemetry_send(&self, batch: TelemetryBatchRecord) {
        self.batches.write().unwrap().push(batch);
    }
    fn authorize(&self, context: ConsentRecord) -> ConsentDecisionRecord {
        self.consent_entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        while self.consent_blocking
            && !self
                .consent_release
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            // 授权线程模拟弹窗等待（专用线程上，control 线程不受影响）。
            std::thread::sleep(Duration::from_millis(1));
        }
        let thread = std::thread::current().name().unwrap_or("").to_owned();
        self.consent_calls.write().unwrap().push((context, thread));
        self.consent_answer
    }

    fn verify_ed25519(&self, message: Vec<u8>, signature: Vec<u8>, public_key: Vec<u8>) -> bool {
        let thread = std::thread::current().name().unwrap_or("").to_owned();
        self.verify_threads.write().unwrap().push(thread);
        while self
            .verify_blocking
            .load(std::sync::atomic::Ordering::SeqCst)
            && !self
                .verify_release
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            // 宿主验签卡死的替身（瞬时线程上；control 线程按 deadline 离场）。
            std::thread::sleep(Duration::from_millis(1));
        }
        // 替身口径：签名 == 消息前 64 字节（不足则整段）且公钥非空 → 真；验解码链与 key_id 查表，不是密码学。
        let expected: Vec<u8> = message.iter().copied().take(64).collect();
        let verified = !public_key.is_empty() && signature == expected;
        self.verify_returned
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        verified
    }

    fn on_worker_started(&self, class: WorkerClassRecord) {
        let thread = std::thread::current().name().unwrap_or("").to_owned();
        self.worker_starts.write().unwrap().push((class, thread));
    }
}

fn leak_bridge(consent_answer: ConsentDecisionRecord) -> &'static RecordingBridge {
    // 测试夹具：泄漏一份进程级桥（Box<dyn HostBridge> 要 'static；测试进程随例退出）。
    Box::leak(Box::new(RecordingBridge::new(consent_answer)))
}

fn leak_blocking_bridge(consent_answer: ConsentDecisionRecord) -> &'static RecordingBridge {
    Box::leak(Box::new(RecordingBridge::new(consent_answer).blocking()))
}

fn options(capabilities: &[&str], consent_endpoint: Option<&str>) -> CoreOptions {
    CoreOptions {
        instance_name: "madinfer-ffi-test".to_owned(),
        tier: TierRecord::Flagship,
        os: OsRecord::Jvm,
        frame_ms: 20,
        sampling_bp: 10_000,
        capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
        consent_endpoint: consent_endpoint.map(str::to_owned),
        trusted_keys: Vec::new(),
        backend: BackendKindRecord::Emulated,
        model_path: None,
    }
}

fn request(privacy: PrivacyRecord, qos: QosRecord, prompt: &str, max_tokens: u32) -> RequestRecord {
    RequestRecord {
        capability: "summarize".to_owned(),
        business: 7,
        privacy,
        qos,
        deadline_ms: 10_000,
        messages: vec![MessageRecord {
            role: RoleRecord::User,
            content: prompt.to_owned(),
        }],
        options: OptionsRecord {
            temperature_x100: None,
            top_p_bp: None,
            max_tokens: Some(max_tokens),
            stop: Vec::new(),
            seed: Some(7),
        },
        output: OutputSpecRecord::Text,
        context: None,
    }
}

fn collect_until_terminal(rx: &Receiver<Recorded>) -> Vec<Recorded> {
    let mut events = Vec::new();
    loop {
        let event = rx.recv_timeout(WAIT).expect("终态前不该超时");
        let terminal = event.is_terminal();
        events.push(event);
        if terminal {
            break;
        }
    }
    events
}

fn assert_quiet(rx: &Receiver<Recorded>) {
    match rx.recv_timeout(QUIET) {
        Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout) => {}
        Ok(event) => panic!("终态后仍有事件：{event:?}"),
    }
}

fn started_count(events: &[Recorded]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Recorded::Started(_)))
        .count()
}

#[test]
fn abi_version_matches_compat() {
    // COMPAT.toml [madinfer].abi_version 与握手常量同源；bump 必须两处同批（madai-w08 1 → 2 首次执行）。
    assert_eq!(infer_abi_version(), INFER_ABI_VERSION);
    assert_eq!(INFER_ABI_VERSION, compat_abi_version());
}

/// 读仓根 COMPAT.toml 的 `abi_version = N`（编译期内嵌，测试只认事实源）。
pub(crate) fn compat_abi_version() -> u32 {
    const COMPAT: &str = include_str!("../../../COMPAT.toml");
    COMPAT
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("abi_version")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .and_then(|rest| rest.trim().split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|digits| digits.parse::<u32>().ok())
        })
        .expect("COMPAT.toml [madinfer].abi_version")
}

#[test]
fn ffi_error_codes_stay_in_4xxx_disjoint_from_core_types() {
    let core_codes: Vec<u16> = RetryKind::ALL
        .iter()
        .map(|k| k.code())
        .chain(DegradeKind::ALL.iter().map(|k| k.code()))
        .chain(FatalKind::ALL.iter().map(|k| k.code()))
        .collect();
    for error in InferFfiError::ALL {
        let code = error.code();
        assert!((4000..5000).contains(&code), "{error} 越出 4xxx");
        assert!(!core_codes.contains(&code), "{error} 与核内错误码相撞");
    }
    assert_eq!(
        InferFfiError::ALL.map(|e| (e.code(), e.wire())),
        [
            (4001, "invalid_argument"),
            (4002, "panicked"),
            (4003, "core_start_failed"),
            (4004, "core_stopped"),
            (4005, "feature_disabled"),
        ]
    );
}

#[test]
fn probe_answers_from_capability_index_and_kv_marker_is_written() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(
        Box::new(bridge),
        options(&["summarize", "embed.text"], None),
    )
    .expect("create");
    let probe = core.probe();
    assert_eq!(probe.abi_version, INFER_ABI_VERSION);
    assert_eq!(probe.core_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(probe.tier, TierRecord::Flagship);
    assert!(probe.snapshot_version >= 1);
    assert_eq!(probe.entries.len(), 2, "两能力全登记");
    for entry in &probe.entries {
        assert_eq!(entry.state, CapStateRecord::Available, "仿真核已注册即可用");
        assert_eq!(entry.source, Some(SourceRecord::Bundled));
    }
    assert!(
        bridge.kv_get(KV_INSTALL_MARKER_KEY.to_owned()).is_some(),
        "建站写 install 标记（KvStore 通路）"
    );
    assert_eq!(probe.backend, BackendKindRecord::Emulated, "缺省仿真核");
    assert_eq!(
        core.stats().expect("stats").backend_kind,
        BackendKindRecord::Emulated,
        "stats 与 probe 同一读数"
    );
}

// ---------------------------------------------------------------------- 后端形态三态（madai-w17 I-C-16）

#[test]
fn emulated_station_ignores_model_path() {
    // model_path 只对 Llama 生效：仿真核站带一条不存在的路径也照常建站、照常出结果。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(
        Box::new(bridge),
        CoreOptions {
            model_path: Some("/nonexistent/madinfer-i-c-16.gguf".to_owned()),
            ..options(&["summarize"], None)
        },
    )
    .expect("create");
    assert_eq!(core.probe().backend, BackendKindRecord::Emulated);
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "hello", 4),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert!(
        matches!(events.last(), Some(Recorded::Complete(_))),
        "仿真核正常完结：{events:?}"
    );
}

/// feature `llama-sys` 未编入：`backend = Llama` → 4005，不静默回仿真核、不碰宿主（install 标记未写）。
#[cfg(not(feature = "llama-sys"))]
#[test]
fn llama_station_without_feature_is_feature_disabled() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let result = InferCore::create(
        Box::new(bridge),
        CoreOptions {
            backend: BackendKindRecord::Llama,
            model_path: Some("/nonexistent/madinfer-i-c-16.gguf".to_owned()),
            ..options(&["summarize"], None)
        },
    );
    assert_eq!(result.err(), Some(InferFfiError::FeatureDisabled));
    assert!(
        bridge.kv_get(KV_INSTALL_MARKER_KEY.to_owned()).is_none(),
        "拒绝建站前不碰宿主"
    );
}

/// feature `llama-sys` 编入：`backend = Llama` 而 `model_path` 缺席 / 空串 / 不是文件 → 4003，不碰宿主。不需要模型文件。
#[cfg(feature = "llama-sys")]
#[test]
fn llama_station_without_model_path_is_core_start_failed() {
    for model_path in [
        None,
        Some(String::new()),
        Some("/nonexistent/madinfer-i-c-16.gguf".to_owned()),
    ] {
        let bridge = leak_bridge(ConsentDecisionRecord::Deny);
        let result = InferCore::create(
            Box::new(bridge),
            CoreOptions {
                backend: BackendKindRecord::Llama,
                model_path: model_path.clone(),
                ..options(&["summarize"], None)
            },
        );
        assert_eq!(
            result.err(),
            Some(InferFfiError::CoreStartFailed),
            "model_path={model_path:?}"
        );
        assert!(
            bridge.kv_get(KV_INSTALL_MARKER_KEY.to_owned()).is_none(),
            "拒绝建站前不碰宿主"
        );
    }
}

#[test]
fn backend_kind_wire_names_are_stable() {
    assert_eq!(BackendKindRecord::Emulated.wire(), "emulated");
    assert_eq!(BackendKindRecord::Llama.wire(), "llama");
}

// ---------------------------------------------------------------------- G4 结构化输出（madai-w08 I-C-07）

const MEETING_SCHEMA: &str = r#"{"type":"object","properties":{"title":{"type":"string"},"count":{"type":"integer"},"tags":{"type":"array","items":{"type":"string"}}},"required":["title","count","tags"]}"#;

fn structured_request(schema_json: &str) -> RequestRecord {
    RequestRecord {
        output: OutputSpecRecord::Structured {
            schema_json: schema_json.to_owned(),
        },
        ..request(
            PrivacyRecord::Public,
            QosRecord::Interactive,
            "把今天的群消息整理成会议纪要",
            256,
        )
    }
}

/// 参考应用器（子集口径同 KMP CatchupPatchDocument）：一帧的 RFC 6902 数组文本应用到文档。
fn apply_patch_text(doc: &mut serde_json::Value, patch: &str) {
    let ops: Vec<serde_json::Value> = serde_json::from_str(patch).expect("patch 是 JSON 数组");
    for op in ops {
        let path = op["path"].as_str().expect("path");
        let segments: Vec<String> = path[1..]
            .split('/')
            .map(|s| s.replace("~1", "/").replace("~0", "~"))
            .collect();
        let (last, parents) = segments.split_last().expect("非空路径");
        let mut container = &mut *doc;
        for segment in parents {
            container = container.get_mut(segment).expect("中间段必存在");
        }
        match op["op"].as_str().expect("op") {
            "add" if last == "-" => container
                .as_array_mut()
                .expect("array")
                .push(op["value"].clone()),
            "add" | "replace" => {
                container
                    .as_object_mut()
                    .expect("object")
                    .insert(last.clone(), op["value"].clone());
            }
            "remove" => {
                container.as_object_mut().expect("object").remove(last);
            }
            other => panic!("子集外 op {other}"),
        }
    }
}

#[test]
fn structured_request_walks_k1_with_patches_and_probe_declares_emulated() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    for entry in &core.probe().entries {
        assert_eq!(
            entry.constraint,
            ConstraintSupportRecord::Emulated,
            "仿真核声明 Emulated 约束：{entry:?}"
        );
    }
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_request(MEETING_SCHEMA),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1, "{events:?}");
    assert!(matches!(events[0], Recorded::Started(_)));
    let mut doc = serde_json::json!({});
    let mut expected_seq = 0;
    for event in &events {
        if let Recorded::Delta(delta) = event {
            assert_eq!(delta.seq, expected_seq, "seq 连续");
            expected_seq += 1;
            let patch = delta
                .structured_patch
                .as_deref()
                .expect("Structured 请求每帧携带 patch");
            apply_patch_text(&mut doc, patch);
        }
    }
    let Some(Recorded::Complete(complete)) = events.last() else {
        panic!("终态应为 Complete：{events:?}");
    };
    let OutputRecord::Structured { json } = &complete.output else {
        panic!("Complete.output 应为 Structured：{complete:?}");
    };
    assert_eq!(&complete.text, json, "兼容字段 text 同文");
    let value: serde_json::Value = serde_json::from_str(json).expect("合法 JSON");
    assert!(value["title"].is_string() && value["count"].is_i64() && value["tags"].is_array());
    assert_eq!(doc, value, "逐帧 patch 应用后 == Complete 的 json");
    assert_quiet(&rx);
    // 同 seed 同 schema → 同文（仿真核确定性）。
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_request(MEETING_SCHEMA),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let again = collect_until_terminal(&rx);
    let Some(Recorded::Complete(second)) = again.last() else {
        panic!("{again:?}");
    };
    assert_eq!(second.output, complete.output);
}

#[test]
fn invalid_schema_is_2003_before_started_and_text_requests_carry_no_patch() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_request(r#"{"anyOf":[]}"#),
        Box::new(ChannelSink(tx)),
    )
    .expect("形式合法：schema 是载荷，编不过在事件面报");
    let events = collect_until_terminal(&rx);
    assert_eq!(events.len(), 1, "无 Started：{events:?}");
    let Recorded::Failed(failed) = &events[0] else {
        panic!("{events:?}");
    };
    assert_eq!(
        (failed.code, failed.wire.as_str()),
        (2003, "schema_invalid")
    );
    assert_eq!(failed.partial_text, None);
    assert_quiet(&rx);
    // context 形式非法（> 128 字节）→ 4001 于 Started 前。
    let (tx, _rx) = mpsc::channel();
    let too_long = RequestRecord {
        context: Some("x".repeat(129)),
        ..request(PrivacyRecord::Public, QosRecord::Interactive, "hi", 8)
    };
    assert!(matches!(
        core.stream(too_long, Box::new(ChannelSink(tx))),
        Err(InferFfiError::InvalidArgument)
    ));
    // Text 请求：Delta 无 patch，Complete.output 是 Text。
    let (tx, rx) = mpsc::channel();
    core.stream(
        RequestRecord {
            context: Some("conv:42".to_owned()),
            ..request(
                PrivacyRecord::Public,
                QosRecord::Interactive,
                "总结一下",
                16,
            )
        },
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert!(events
        .iter()
        .all(|e| !matches!(e, Recorded::Delta(d) if d.structured_patch.is_some())));
    assert!(matches!(
        events.last(),
        Some(Recorded::Complete(CompleteRecord {
            output: OutputRecord::Text { .. },
            ..
        }))
    ));
}

#[test]
fn thread_hooks_fire_on_each_core_thread_with_its_class() {
    // madai-w08 G7：HostBridge::on_worker_started 在核的每条线程启动时于该线程上调一次。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    // 走一条请求让 worker 池确实起线程。
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "hi", 4),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    collect_until_terminal(&rx);
    let starts = bridge.worker_starts.read().unwrap().clone();
    assert!(
        starts.iter().any(
            |(class, thread)| *class == WorkerClassRecord::Control && thread == "madinfer-core"
        ),
        "control 线程上报 Control：{starts:?}"
    );
    assert!(
        starts.iter().any(|(class, thread)| matches!(
            class,
            WorkerClassRecord::Interactive | WorkerClassRecord::Background
        ) && thread.starts_with("madinfer-worker-")),
        "worker 线程上报推理类别：{starts:?}"
    );
    assert_eq!(
        starts
            .iter()
            .filter(|(class, _)| *class == WorkerClassRecord::Control)
            .count(),
        1,
        "control 恰一次"
    );
}

#[test]
fn create_rejects_invalid_capability_before_spawn() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    assert!(matches!(
        InferCore::create(Box::new(bridge), options(&["BAD NAME"], None)),
        Err(InferFfiError::InvalidArgument)
    ));
    assert!(matches!(
        InferCore::create(Box::new(bridge), options(&[], None)),
        Err(InferFfiError::InvalidArgument)
    ));
}

#[test]
fn stream_keeps_k1_order_and_telemetry_reaches_bridge_as_typed_record() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let (tx, rx) = mpsc::channel();
    let handle = core
        .stream(
            request(
                PrivacyRecord::E2eePlaintext,
                QosRecord::Interactive,
                "总结一下今天的群消息",
                24,
            ),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1, "Started 恰一次：{events:?}");
    let Recorded::Started(started) = &events[0] else {
        panic!("Started 必须第一：{events:?}");
    };
    assert_eq!(started.request, handle);
    assert_eq!(started.source, SourceRecord::Bundled);
    assert_eq!(started.package.as_deref(), Some(SIM_PACKAGE_REF));
    let deltas: Vec<&DeltaRecord> = events
        .iter()
        .filter_map(|e| match e {
            Recorded::Delta(d) => Some(d),
            _ => None,
        })
        .collect();
    // madai-w12 I-C-11（R-w11-5）：仿真核按输出字节数切片（每片 ≤ 32 字节、码点边界）——36 字节输出恰 2 片，≤ 2 片时
    // 合帧后帧数 = 片数、与调度时序无关：帧数与逐帧文本可钉（超过 2 片的输出仍只准钉首帧，见 madinfer.h §5b）。
    assert_eq!(
        deltas
            .iter()
            .map(|d| (d.seq, d.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "「总结一下今天的群消"), (1, "息」")],
        "两片两帧、逐帧文本确定：{events:?}"
    );
    match events.last() {
        Some(Recorded::Complete(complete)) => {
            assert_eq!(complete.request, handle);
            let joined: String = deltas.iter().map(|d| d.text.as_str()).collect();
            assert_eq!(complete.text, joined, "Delta 拼接 == 完整输出");
            assert_eq!(complete.stats.fallback_steps, 0);
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_quiet(&rx);
    drop(core); // 关停（Drop → Shutdown + join）：观测排水随之刷出。
    let batches = bridge.batches.read().unwrap();
    assert!(!batches.is_empty(), "观测批次到桥");
    for batch in batches.iter() {
        assert_eq!(batch.schema_version, 1, "telemetry.v1 头字段类型化过界");
        assert!(batch.event_count >= 1);
        assert!(!batch.payload.is_empty());
        let payload = String::from_utf8(batch.payload.clone()).expect("utf8");
        assert!(!payload.contains("总结"), "用户内容禁入观测载荷");
    }
}

#[test]
fn cancel_while_awaiting_consent_ends_with_single_failed_3008_and_is_idempotent() {
    // 确定性取消点：授权弹窗未回执时请求停在 AwaitingConsent（仿真核生成太快，
    // 中途取消的时序竞争由回调内取消用例与 madinfer-api pipeline 测试覆盖）。
    let bridge = leak_blocking_bridge(ConsentDecisionRecord::Allow);
    let core = InferCore::create(
        Box::new(bridge),
        options(&["summarize"], Some("cloud.enterprise")),
    )
    .expect("create");
    let (tx, rx) = mpsc::channel();
    let handle = core
        .stream(
            request(
                PrivacyRecord::Internal,
                QosRecord::Async,
                "取消路径样例",
                16,
            ),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
    // 等授权线程真进入弹窗等待，再取消。
    let mut rounds = 0u32;
    while !bridge
        .consent_entered
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        rounds += 1;
        assert!(rounds < 5_000, "授权回调未被调起");
        std::thread::sleep(Duration::from_millis(1));
    }
    core.cancel(handle);
    core.cancel(handle); // 幂等：第二次 no-op。
    let events = collect_until_terminal(&rx);
    assert_eq!(events.len(), 1, "取消先于 Started：只有终态：{events:?}");
    match &events[0] {
        Recorded::Failed(failed) => {
            assert_eq!(failed.code, 3008, "取消 = Failed(Cancelled, 3008)");
            assert_eq!(failed.wire, "cancelled");
            assert_eq!(failed.partial_text, None, "未出任何 Delta：无 partial");
        }
        other => panic!("终态应为 Failed(3008)：{other:?}"),
    }
    assert_quiet(&rx);
    core.cancel(handle); // 已终态：幂等 no-op。
    core.cancel(9_999); // 未知句柄：幂等 no-op。
                        // 放行弹窗：迟到的 ConsentDecided(Allow) 落在已终态请求上，被静默丢弃（无第二个终态）。
    bridge
        .consent_release
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_quiet(&rx);
}

#[test]
fn cancel_after_first_delta_keeps_terminal_exactly_once_with_partial_on_failed() {
    // 中途取消（时序竞争：仿真核可能先跑完）——锁的是"终态恰一次"，取消赢时 3008 + partial。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let (tx, rx) = mpsc::channel();
    let long_prompt = "端侧推理取消路径样例文本 ".repeat(64);
    let handle = core
        .stream(
            request(
                PrivacyRecord::Public,
                QosRecord::Interactive,
                &long_prompt,
                4_096,
            ),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
    let first = rx.recv_timeout(WAIT).expect("Started");
    assert!(matches!(first, Recorded::Started(_)));
    let second = rx.recv_timeout(WAIT).expect("首帧");
    core.cancel(handle);
    let mut events = vec![first, second];
    events.extend(collect_until_terminal(&rx));
    let terminals: Vec<&Recorded> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminals.len(), 1, "终态恰一次：{events:?}");
    match terminals[0] {
        Recorded::Failed(failed) => {
            assert_eq!(failed.code, 3008);
            assert!(
                failed.partial_text.as_ref().is_some_and(|t| !t.is_empty()),
                "已出 Delta 后取消：partial 随 Failed 返回"
            );
        }
        Recorded::Complete(_) => {} // 仿真核先到终点：取消退化为 no-op（幂等口径）。
        other => panic!("非法终态：{other:?}"),
    }
    assert_quiet(&rx);
}

#[test]
fn cancel_from_inside_callback_does_not_deadlock() {
    // 回调线程任意性：EventSink 回调在 control 线程执行，回调体里反调 cancel（只入队）不得死锁。
    struct CancellingSink {
        tx: Sender<Recorded>,
        core: RwLock<Option<Arc<InferCore>>>,
        handle: AtomicU64,
    }
    impl EventSink for CancellingSink {
        fn on_started(&self, event: StartedRecord) {
            let _ = self.tx.send(Recorded::Started(event));
        }
        fn on_delta(&self, event: DeltaRecord) {
            let handle = self.handle.load(Ordering::SeqCst);
            if let Some(core) = self.core.read().unwrap().as_ref() {
                core.cancel(handle); // 回调里再调 API：只入队立即返回。
            }
            let _ = self.tx.send(Recorded::Delta(event));
        }
        fn on_complete(&self, event: CompleteRecord) {
            let _ = self.tx.send(Recorded::Complete(event));
        }
        fn on_failed(&self, event: FailedRecord) {
            let _ = self.tx.send(Recorded::Failed(event));
        }
    }
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let (tx, rx) = mpsc::channel();
    let sink = Box::leak(Box::new(CancellingSink {
        tx,
        core: RwLock::new(Some(Arc::clone(&core))),
        handle: AtomicU64::new(0),
    }));
    struct SinkRef(&'static CancellingSink);
    impl EventSink for SinkRef {
        fn on_started(&self, event: StartedRecord) {
            self.0.on_started(event);
        }
        fn on_delta(&self, event: DeltaRecord) {
            self.0.on_delta(event);
        }
        fn on_complete(&self, event: CompleteRecord) {
            self.0.on_complete(event);
        }
        fn on_failed(&self, event: FailedRecord) {
            self.0.on_failed(event);
        }
    }
    let long_prompt = "回调内取消不死锁样例 ".repeat(64);
    let handle = core
        .stream(
            request(
                PrivacyRecord::Public,
                QosRecord::Interactive,
                &long_prompt,
                4_096,
            ),
            Box::new(SinkRef(sink)),
        )
        .expect("stream");
    sink.handle.store(handle, Ordering::SeqCst);
    let events = collect_until_terminal(&rx);
    let terminals: Vec<&Recorded> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminals.len(), 1, "终态恰一次");
    // 首帧前 handle 可能尚未 store（0）：那次 cancel 是未知句柄 no-op，后续帧的 cancel 生效；
    // 若生成太快已 Complete 也算通过——本测锁的是"不死锁 + 终态恰一次"，不是取消必然赢。
    assert!(matches!(
        terminals[0],
        Recorded::Failed(FailedRecord { code: 3008, .. }) | Recorded::Complete(_)
    ));
    assert_quiet(&rx);
    sink.core.write().unwrap().take();
}

#[test]
fn consent_flow_calls_authorize_off_control_thread_deny_is_3007_allow_falls_back() {
    for (answer, deny) in [
        (ConsentDecisionRecord::Deny, true),
        (ConsentDecisionRecord::Allow, false),
    ] {
        let bridge = leak_bridge(answer);
        let core = InferCore::create(
            Box::new(bridge),
            options(&["summarize"], Some("cloud.enterprise")),
        )
        .expect("create");
        let (tx, rx) = mpsc::channel();
        core.stream(
            request(
                PrivacyRecord::Internal,
                QosRecord::Async,
                "升云授权样例",
                16,
            ),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
        let events = collect_until_terminal(&rx);
        let calls = bridge.consent_calls.read().unwrap();
        assert_eq!(calls.len(), 1, "授权恰问一次");
        assert_eq!(calls[0].0.endpoint_id, "cloud.enterprise");
        assert_eq!(calls[0].0.privacy, PrivacyRecord::Internal);
        assert_eq!(
            calls[0].1, "madinfer-ffi-consent",
            "authorize 在专用线程执行，不占 control 线程"
        );
        // madai-w10 I-C-09 J5-a（I-S-05 U-5）：授权专用线程也走线程钩子——以 Control 类别、在该线程上、先于 authorize 调一次，
        // 壳才能给它设 QoS 且"是否核线程"守卫覆盖到授权回调体。
        let starts = bridge.worker_starts.read().unwrap();
        assert_eq!(
            starts
                .iter()
                .filter(|(class, thread)| *class == WorkerClassRecord::Control
                    && thread == "madinfer-ffi-consent")
                .count(),
            1,
            "consent 线程上报 Control 恰一次：{starts:?}"
        );
        drop(starts);
        if deny {
            assert_eq!(events.len(), 1, "拒绝：只有 Failed，无 Started");
            assert!(matches!(
                &events[0],
                Recorded::Failed(FailedRecord { code: 3007, .. })
            ));
        } else {
            // Allow：云后端（C8）未接 → 后端缺失回退到仿真核（CoreOptions 演示位口径）。
            assert_eq!(started_count(&events), 1);
            match events.last() {
                Some(Recorded::Complete(complete)) => {
                    assert_eq!(complete.stats.fallback_steps, 1, "云目标缺后端 → 回退一步");
                    assert_eq!(complete.stats.source, Some(SourceRecord::Bundled));
                }
                other => panic!("终态应为 Complete：{other:?}"),
            }
        }
        assert_quiet(&rx);
    }
}

#[test]
fn allow_remember_writes_consent_key_and_skips_host_on_next_request() {
    // F9：记忆位归核侧——AllowRemember → kv_put("madinfer.consent.<capability>.<endpoint>", [1])；同键再问不打扰宿主。
    let bridge = leak_bridge(ConsentDecisionRecord::AllowRemember);
    let core = InferCore::create(
        Box::new(bridge),
        options(&["summarize", "embed.text"], Some("cloud.enterprise")),
    )
    .expect("create");
    let key = consent_memory_key("summarize", "cloud.enterprise");
    assert_eq!(key, "madinfer.consent.summarize.cloud.enterprise");
    assert!(bridge.kv_get(key.clone()).is_none(), "建站不预写记忆");

    let run = |core: &InferCore, capability: &str| {
        let (tx, rx) = mpsc::channel();
        let mut req = request(PrivacyRecord::Internal, QosRecord::Async, "记忆位样例", 16);
        req.capability = capability.to_owned();
        core.stream(req, Box::new(ChannelSink(tx))).expect("stream");
        let events = collect_until_terminal(&rx);
        assert_eq!(started_count(&events), 1);
        match events.last() {
            Some(Recorded::Complete(complete)) => {
                assert_eq!(
                    complete.stats.fallback_steps, 1,
                    "授权通过 → 云目标缺后端回退一步"
                );
            }
            other => panic!("终态应为 Complete：{other:?}"),
        }
        assert_quiet(&rx);
    };

    // 首问：宿主答 AllowRemember → 记忆落 kv。
    run(&core, "summarize");
    assert_eq!(bridge.consent_calls.read().unwrap().len(), 1, "首次问宿主");
    assert_eq!(
        bridge.kv_get(key.clone()),
        Some(vec![KV_CONSENT_REMEMBERED]),
        "记忆键写入（值 = 单字节 1）"
    );
    // 同键再问：不打扰宿主，仍走授权通过路径。
    run(&core, "summarize");
    assert_eq!(
        bridge.consent_calls.read().unwrap().len(),
        1,
        "记忆命中：宿主不再被问"
    );
    // 异键（另一能力）：记忆不串键，宿主再被问一次并各自落键。
    run(&core, "embed.text");
    assert_eq!(
        bridge.consent_calls.read().unwrap().len(),
        2,
        "异键仍问宿主"
    );
    assert!(bridge
        .kv_get(consent_memory_key("embed.text", "cloud.enterprise"))
        .is_some());
    // 记忆键只在 AllowRemember 写：kv 里除 install 标记外恰两把记忆键。
    let kv = bridge.kv.read().unwrap();
    assert_eq!(
        kv.keys()
            .filter(|k| k.starts_with(KV_CONSENT_PREFIX))
            .count(),
        2
    );
    drop(kv);
    // 记忆值不是 1 → 不算记忆（宿主 kv 被改写 / 清空后回到再问）。
    bridge.kv_put(key.clone(), vec![0]);
    run(&core, "summarize");
    assert_eq!(
        bridge.consent_calls.read().unwrap().len(),
        3,
        "值非 1 视为未记忆"
    );
}

#[test]
fn deny_and_plain_allow_do_not_write_consent_memory() {
    for answer in [ConsentDecisionRecord::Deny, ConsentDecisionRecord::Allow] {
        let bridge = leak_bridge(answer);
        let core = InferCore::create(
            Box::new(bridge),
            options(&["summarize"], Some("cloud.enterprise")),
        )
        .expect("create");
        let (tx, rx) = mpsc::channel();
        core.stream(
            request(PrivacyRecord::Internal, QosRecord::Async, "无记忆样例", 8),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
        collect_until_terminal(&rx);
        assert!(
            bridge
                .kv_get(consent_memory_key("summarize", "cloud.enterprise"))
                .is_none(),
            "{answer:?} 不写记忆键"
        );
    }
}

#[test]
fn panics_are_contained_at_the_boundary() {
    // 出口收口：test_panic 注入核内 panic，回 4002 且进程存活（本测跑完即物证）。
    assert_eq!(test_panic(), Err(InferFfiError::Panicked));

    // 投递点收口：宿主回调体 panic 不拖死 control 线程——请求照常走到终态（后续回调仍投递）。
    struct PanickingOnDeltaSink(Sender<Recorded>);
    impl EventSink for PanickingOnDeltaSink {
        fn on_started(&self, event: StartedRecord) {
            let _ = self.0.send(Recorded::Started(event));
        }
        fn on_delta(&self, _event: DeltaRecord) {
            panic!("缺陷宿主：回调体 panic");
        }
        fn on_complete(&self, event: CompleteRecord) {
            let _ = self.0.send(Recorded::Complete(event));
        }
        fn on_failed(&self, event: FailedRecord) {
            let _ = self.0.send(Recorded::Failed(event));
        }
    }
    let before = sink_panic_count();
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(
            PrivacyRecord::Public,
            QosRecord::Interactive,
            "panic 收口样例文本",
            8,
        ),
        Box::new(PanickingOnDeltaSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert!(
        matches!(events.last(), Some(Recorded::Complete(_))),
        "control 线程存活并送达终态：{events:?}"
    );
    assert!(sink_panic_count() > before, "投递点收口计数增长");
    assert_quiet(&rx);
}

#[test]
fn invalid_request_is_rejected_with_4001_before_any_event() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let (tx, rx) = mpsc::channel();
    let mut bad = request(PrivacyRecord::Public, QosRecord::Interactive, "x", 8);
    bad.business = 0;
    assert_eq!(
        core.stream(bad, Box::new(ChannelSink(tx))),
        Err(InferFfiError::InvalidArgument)
    );
    assert_quiet(&rx);
}

#[test]
fn bridge_verifier_decodes_and_routes_to_host_callback() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let key_hex = "ab".repeat(32);
    let keys = vec![TrustedKeyRecord {
        key_id: "k1".to_owned(),
        public_key_hex: key_hex,
    }];
    let mut with_keys = options(&["summarize"], None);
    with_keys.trusted_keys = keys.clone();
    let core = InferCore::create(Box::new(bridge), with_keys)
        .expect("受信公钥非空：调试与生产构建都可建站");
    assert_eq!(core.probe().entries.len(), 1);
    let verifier = BridgeVerifier::new(
        Arc::new(Bridge(Box::new(bridge))),
        &keys,
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(VERIFY_DEADLINE_MS)),
    )
    .expect("hex 合法");
    let payload: Vec<u8> = (0u8..80).collect();
    let good = Signature {
        alg: "ed25519".to_owned(),
        key_id: "k1".to_owned(),
        sig: base64_encode_for_test(&payload[..64]),
    };
    assert_eq!(
        verifier.verify(&payload, &good),
        VerifyOutcome::Verified,
        "替身口径：签名 == 载荷前 64 字节"
    );
    assert!(verifier.knows_key("k1") && !verifier.knows_key("k2"));
    let unknown = Signature {
        key_id: "k2".to_owned(),
        ..good.clone()
    };
    assert_eq!(
        verifier.verify(&payload, &unknown),
        VerifyOutcome::Rejected,
        "未知 key_id 为假"
    );
    let mut tampered = payload.clone();
    tampered[0] ^= 1;
    assert_eq!(
        verifier.verify(&tampered, &good),
        VerifyOutcome::Rejected,
        "篡改载荷为假"
    );
    let wrong_alg = Signature {
        alg: "rsa".to_owned(),
        ..good
    };
    assert_eq!(
        verifier.verify(&payload, &wrong_alg),
        VerifyOutcome::Rejected,
        "算法不符为假"
    );
    // madai-w11 I-C-10：宿主回调不在调用线程执行——两次真正进宿主的验签（验真 / 篡改）都在瞬时线程上，且每次线程体先走
    // on_worker_started(Control)；未知 key / 算法不符在解码链就拒，不进宿主、不起线程。
    assert_eq!(
        *bridge.verify_threads.read().unwrap(),
        vec![VERIFY_THREAD_NAME.to_owned(); 2]
    );
    assert_eq!(
        bridge
            .worker_starts
            .read()
            .unwrap()
            .iter()
            .filter(|(class, thread)| {
                *class == WorkerClassRecord::Control && thread == VERIFY_THREAD_NAME
            })
            .count(),
        2,
        "验签瞬时线程每条一次 on_worker_started(Control)"
    );
    let bad_key = vec![TrustedKeyRecord {
        key_id: "k".to_owned(),
        public_key_hex: "zz".to_owned(),
    }];
    assert!(
        BridgeVerifier::new(
            Arc::new(Bridge(Box::new(bridge))),
            &bad_key,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(VERIFY_DEADLINE_MS)),
        )
        .is_err(),
        "非 hex 公钥拒绝建站"
    );
}

#[test]
fn os_signal_entry_is_non_blocking_and_keeps_core_healthy() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("建站");
    for _ in 0..64 {
        core.signal(OsSignalRecord::Thermal {
            state: ThermalRecord::Critical,
        });
        core.signal(OsSignalRecord::Memory {
            level: MemoryPressureRecord::Warning,
        });
        core.signal(OsSignalRecord::App {
            state: AppStateRecord::Background,
            budget_ms: Some(30_000),
        });
        core.signal(OsSignalRecord::Network {
            state: NetStateRecord::Metered,
        });
        core.signal(OsSignalRecord::Battery {
            level_pct: Some(20),
            charging: false,
            low_power_mode: true,
        });
        // madai-w08 G7：电量未知态也是合法信号。
        core.signal(OsSignalRecord::Battery {
            level_pct: None,
            charging: true,
            low_power_mode: false,
        });
        core.signal(OsSignalRecord::Idle { idle: true });
        core.signal(OsSignalRecord::MainLinkBusy { busy: false });
    }
    assert_eq!(core.probe().entries.len(), 1, "信号风暴后探测仍答");
}

/// madai-w09 I-C-08：电量未知 None 原样进核（不再以 100 透传），已知值与充电 / 低电量模式逐字段透传。
#[test]
fn battery_unknown_crosses_into_core_as_none_not_full() {
    let unknown = os_signal_of(OsSignalRecord::Battery {
        level_pct: None,
        charging: true,
        low_power_mode: false,
    });
    assert_eq!(
        unknown,
        OsSignal::Battery {
            level_pct: None,
            charging: true,
            low_power_mode: false,
        },
        "None 进核仍是 None（此前为 Some(100)）"
    );
    let known = os_signal_of(OsSignalRecord::Battery {
        level_pct: Some(20),
        charging: false,
        low_power_mode: true,
    });
    assert_eq!(
        known,
        OsSignal::Battery {
            level_pct: Some(20),
            charging: false,
            low_power_mode: true,
        }
    );
}

/// 测试受信公钥（k1 ↔ 32 字节 0xab）。
fn test_trusted_keys() -> Vec<TrustedKeyRecord> {
    vec![TrustedKeyRecord {
        key_id: "k1".to_owned(),
        public_key_hex: "ab".repeat(32),
    }]
}

/// config.v1 最小文档（映射 summarize → bundled llama.cpp，即建站注册的唯一后端），签名字段占位。
fn unsigned_config_doc(version: u64) -> String {
    format!(
        r#"{{
  "schema": 1, "version": {version}, "issued_at": "2026-09-05T02:00:00Z", "min_sdk": ">=0.1.0",
  "rollout": {{ "buckets": [0, 999] }},
  "mapping": [
    {{ "capability": "summarize",
      "candidates": [
        {{ "priority": 0, "target": {{ "kind": "bundled", "backend": "llama.cpp", "package": "{SIM_PACKAGE_REF}" }}, "when": {{ "tier_min": "low" }} }}
      ] }}
  ],
  "thresholds": {{
    "deadline_ms": {{ "interactive": 3000, "async": 20000, "background": 120000 }},
    "frame_ms": 80,
    "kv_budget_mb": {{ "flagship": 512, "mid": 256, "low": 0 }},
    "quotas": {{}},
    "thermal": {{ "background_max": "fair" }},
    "gc_keep_days": 7
  }},
  "switches": {{ "capabilities": {{}}, "cloud_only": false, "all_off": false }},
  "signature": {{ "alg": "ed25519", "key_id": "k1", "sig": "" }}
}}"#
    )
}

/// 按替身口径签名：sig = base64(签名载荷前 64 字节)；`key_id` 可指定以构造"未知 key"例。
fn signed_config_doc(version: u64, key_id: &str) -> Vec<u8> {
    let mut doc: madinfer_config::ConfigDoc =
        serde_json::from_str(&unsigned_config_doc(version)).expect("测试文档合法");
    let payload = doc.signing_payload();
    doc.signature = Signature {
        alg: "ed25519".to_owned(),
        key_id: key_id.to_owned(),
        sig: base64_encode_for_test(&payload[..64]),
    };
    serde_json::to_vec(&doc).expect("序列化")
}

#[test]
fn apply_config_verifies_through_bridge_and_switches_atomically() {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let mut with_keys = options(&["summarize"], None);
    with_keys.trusted_keys = test_trusted_keys();
    let core = InferCore::create(Box::new(bridge), with_keys).expect("建站");

    // 验真 → 应用。
    let ok = core.apply_config(signed_config_doc(5, "k1")).expect("核在");
    assert_eq!(
        ok,
        ApplyOutcomeRecord {
            applied: true,
            active_version: 5,
            stage: ApplyStageRecord::Applied,
            reject_reason: None,
            detail: String::new(),
            detail_kind: ApplyDetailKindRecord::None,
        }
    );

    // 篡改载荷（issued_at 改字；它按键序排在载荷最前，落在替身校验的前 64 字节内）→ 签名不符，活动版本仍 5。
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&signed_config_doc(6, "k1")).expect("json");
    tampered["issued_at"] = serde_json::json!("2026-09-05T03:00:00Z");
    let rejected = core
        .apply_config(serde_json::to_vec(&tampered).expect("序列化"))
        .expect("核在");
    assert!(!rejected.applied, "{rejected:?}");
    assert_eq!(
        rejected.reject_reason,
        Some(ApplyRejectReasonRecord::SignatureInvalid)
    );
    assert_eq!(rejected.stage, ApplyStageRecord::Received);
    assert_eq!(rejected.active_version, 5, "拒绝不动活动配置");
    assert_eq!(
        (rejected.detail_kind, rejected.detail.as_str()),
        (ApplyDetailKindRecord::VerifyRejected, ""),
        "madai-w15 I-C-13：验签假的类型化种类（detail 空串）"
    );
    assert_eq!(
        core.stats().expect("核在").verify_timeouts,
        0,
        "madai-w15 I-C-13：验签假不计超时——与 deadline 用例在 KMP 面可区分"
    );

    // 未知 key_id → 拒绝，detail 带 key_id 供宿主记日志。
    let unknown = core.apply_config(signed_config_doc(7, "k9")).expect("核在");
    assert_eq!(
        unknown.reject_reason,
        Some(ApplyRejectReasonRecord::UnknownKey)
    );
    assert_eq!(unknown.detail, "k9");
    assert_eq!(unknown.detail_kind, ApplyDetailKindRecord::UnknownKey);
    assert_eq!(unknown.active_version, 5);

    // 版本不递增 → 拒绝在 Parsed 阶段。
    let stale = core.apply_config(signed_config_doc(5, "k1")).expect("核在");
    assert_eq!(
        stale.reject_reason,
        Some(ApplyRejectReasonRecord::VersionNotNewer)
    );
    assert_eq!(stale.stage, ApplyStageRecord::Parsed);
    assert_eq!(stale.detail, "received=5 active=5");
    assert_eq!(stale.detail_kind, ApplyDetailKindRecord::VersionNotNewer);

    // 非 JSON → SchemaInvalid。
    let junk = core.apply_config(b"{".to_vec()).expect("核在");
    assert_eq!(
        junk.reject_reason,
        Some(ApplyRejectReasonRecord::SchemaInvalid)
    );
    assert_eq!(junk.detail_kind, ApplyDetailKindRecord::Parse);
    assert!(!junk.detail.is_empty(), "解析错误描述随 detail 带出");

    // 新配置生效后请求仍走通 K1（映射表被原子替换）。
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Internal, QosRecord::Interactive, "配置后", 4),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert!(
        matches!(events.last(), Some(Recorded::Complete(_))),
        "{events:?}"
    );

    // J1（I-C-09）+ I-C-10 deadline：验签回调每次 apply 至多一次——验真 / 篡改 / 版本不递增各一次（解析通过才验签），
    // 未知 key_id 与非 JSON 不进宿主；五次 apply 共三次回调，全部在瞬时线程 madinfer-ffi-verify 上（不再在 control 线程 madinfer-core）。
    assert_eq!(
        *bridge.verify_threads.read().unwrap(),
        vec![VERIFY_THREAD_NAME.to_owned(); 3],
        "verify_ed25519 须在瞬时线程上、每 apply 至多一次"
    );
    assert!(
        !bridge
            .verify_threads
            .read()
            .unwrap()
            .iter()
            .any(|t| t == "madinfer-core"),
        "control 线程不得自己调宿主验签"
    );

    // 观测：config_apply 一条、config_rejected 四条。
    drop(core);
    let batches = bridge.batches.read().unwrap();
    let mut applied = 0;
    let mut rejected_count = 0;
    for batch in batches.iter() {
        let doc: serde_json::Value = serde_json::from_slice(&batch.payload).expect("batch json");
        for e in doc["events"].as_array().expect("events") {
            match e["type"].as_str() {
                Some("config_apply") => applied += 1,
                Some("config_rejected") => rejected_count += 1,
                _ => {}
            }
        }
    }
    assert_eq!((applied, rejected_count), (1, 4));
}

#[test]
fn apply_config_verify_deadline_rejects_with_timeout_detail_and_core_stays_alive() {
    // I-S-05 U-3 / w10 R1：宿主 verify_ed25519 卡死 → 此前 control 线程随之卡死、核站整体不可用。
    // 现在：control 线程等满 VERIFY_DEADLINE_MS → Rejected{SignatureInvalid, stage Received, detail verify=timeout}，
    // 核站其余入口照常；宿主迟到返回按代数票哑火计数。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    bridge
        .verify_blocking
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut with_keys = options(&["summarize"], None);
    with_keys.trusted_keys = test_trusted_keys();
    let core = InferCore::create(Box::new(bridge), with_keys).expect("建站");
    let timeouts_before = verify_timeout_count();
    let late_before = verify_late_reply_count();

    let now = || madinfer_host::Clock::now_ms(&bridge.clock);
    let started = now();
    let outcome = core
        .apply_config(signed_config_doc(5, "k1"))
        .expect("核在：deadline 后有回执");
    let waited_ms = now() - started;
    assert_eq!(
        outcome,
        ApplyOutcomeRecord {
            applied: false,
            active_version: 0,
            stage: ApplyStageRecord::Received,
            reject_reason: Some(ApplyRejectReasonRecord::SignatureInvalid),
            detail: VERIFY_TIMEOUT_DETAIL.to_owned(),
            // madai-w15 I-C-13：超时与验签假同 reject_reason / stage，KMP 面靶本字段区分。
            detail_kind: ApplyDetailKindRecord::VerifyTimeout,
        }
    );
    assert!(
        waited_ms >= VERIFY_DEADLINE_MS,
        "须等满 deadline 才拒：waited_ms={waited_ms}"
    );
    // 进程级计数被同进程并行用例的别的站也加（madai-w16 I-C-14 的 deadline 注入例），只判 ≥；精确 +1 看本站 stats。
    assert!(
        verify_timeout_count() > timeouts_before,
        "verify=timeout 进程级计数 +≥1"
    );
    assert_eq!(
        core.stats().expect("核在").verify_timeouts,
        1,
        "madai-w15 I-C-13：本站计数 +1（与进程级计数同一处 fetch_add）"
    );
    assert_eq!(
        bridge
            .verify_returned
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "宿主验签仍卡着"
    );

    // 核站其余入口不受卡死的验签影响：stream 走通 K1、probe 可答。
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "卡死期间", 4),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert!(matches!(events.last(), Some(Recorded::Complete(_))));
    assert_eq!(core.probe().entries.len(), 1);

    // 放行宿主：迟到回执落在已过期的票上 → 哑火计数 +1，活动配置不变（不会"事后应用"）。
    bridge
        .verify_release
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // 等本站宿主真的返回（本例自己的桥；进程级 late 计数会被并行用例的站先加，不能拿它当等待条件）。
    let deadline = now() + 5_000;
    while bridge
        .verify_returned
        .load(std::sync::atomic::Ordering::SeqCst)
        < 1
        && now() < deadline
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        bridge
            .verify_returned
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "本站宿主只回了这一次"
    );
    // 迟到回执哑火计数在宿主返回后紧接着加（同一线程体内 send 失败即计）；进程级只判 ≥。
    while verify_late_reply_count() < late_before + 1 && now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        verify_late_reply_count() > late_before,
        "迟到回执哑火计数（进程级，只判 ≥）"
    );
    // 放行后再 apply：验签在期限内回执 → 应用成功；瞬时线程每次一条；本站超时计数不再变。
    let ok = core.apply_config(signed_config_doc(5, "k1")).expect("核在");
    assert!(ok.applied, "{ok:?}");
    assert_eq!(ok.active_version, 5);
    assert_eq!(
        *bridge.verify_threads.read().unwrap(),
        vec![VERIFY_THREAD_NAME.to_owned(); 2]
    );
    assert_eq!(core.stats().expect("核在").verify_timeouts, 1);
    drop(core);
}

#[test]
fn apply_detail_kind_maps_every_reject_reason_and_keeps_detail_text_unchanged() {
    // madai-w15 I-C-13：detail_kind 是 detail 串的类型化种类——每个 RejectReason 恰映射一值（SignatureInvalid 按 timed_out 分两值），
    // detail 文本形态与 madai-w08 / w11 口径逐字不变（C 面只抄 detail，故 C 面不受影响）。
    let semantic = madinfer_config::doc::SemanticError::ThresholdOutOfRange("quotas.window_s");
    let cases: Vec<(
        RejectReason,
        ApplyRejectReasonRecord,
        &str,
        ApplyDetailKindRecord,
    )> = vec![
        (
            RejectReason::UnknownKey("k9".to_owned()),
            ApplyRejectReasonRecord::UnknownKey,
            "k9",
            ApplyDetailKindRecord::UnknownKey,
        ),
        (
            RejectReason::SignatureInvalid { timed_out: true },
            ApplyRejectReasonRecord::SignatureInvalid,
            VERIFY_TIMEOUT_DETAIL,
            ApplyDetailKindRecord::VerifyTimeout,
        ),
        (
            RejectReason::SignatureInvalid { timed_out: false },
            ApplyRejectReasonRecord::SignatureInvalid,
            "",
            ApplyDetailKindRecord::VerifyRejected,
        ),
        (
            RejectReason::SchemaInvalid("expected value at line 1".to_owned()),
            ApplyRejectReasonRecord::SchemaInvalid,
            "expected value at line 1",
            ApplyDetailKindRecord::Parse,
        ),
        (
            RejectReason::VersionNotNewer {
                received: 5,
                active: 5,
            },
            ApplyRejectReasonRecord::VersionNotNewer,
            "received=5 active=5",
            ApplyDetailKindRecord::VersionNotNewer,
        ),
        (
            RejectReason::MinSdkUnsatisfied(">=9.0.0".to_owned()),
            ApplyRejectReasonRecord::MinSdkUnsatisfied,
            ">=9.0.0",
            ApplyDetailKindRecord::MinSdkUnsatisfied,
        ),
        (
            RejectReason::OutOfRollout { bucket: 731 },
            ApplyRejectReasonRecord::OutOfRollout,
            "bucket=731",
            ApplyDetailKindRecord::OutOfRollout,
        ),
        (
            RejectReason::Semantic(semantic.clone()),
            ApplyRejectReasonRecord::SemanticInvalid,
            "ThresholdOutOfRange(\"quotas.window_s\")",
            ApplyDetailKindRecord::Semantic,
        ),
    ];
    let mut kinds_seen = Vec::new();
    for (reason, expected_reason, expected_detail, expected_kind) in cases {
        let record = apply_outcome_record_of(&ApplyReport {
            outcome: ApplyOutcome::Rejected {
                reason: reason.clone(),
                stage: ApplyStage::Received,
            },
            active_version: 3,
        });
        assert_eq!(record.reject_reason, Some(expected_reason), "{reason:?}");
        assert_eq!(record.detail, expected_detail, "{reason:?}：detail 串不变");
        assert_eq!(record.detail_kind, expected_kind, "{reason:?}");
        assert!(!record.applied);
        assert_eq!(record.active_version, 3);
        kinds_seen.push(expected_kind);
    }
    kinds_seen.sort_by_key(|k| *k as u8);
    kinds_seen.dedup();
    assert_eq!(kinds_seen.len(), 8, "八个拒因种类两两不同");
    let applied = apply_outcome_record_of(&ApplyReport {
        outcome: ApplyOutcome::Applied { version: 9 },
        active_version: 9,
    });
    assert_eq!(
        (applied.detail_kind, applied.detail.as_str()),
        (ApplyDetailKindRecord::None, "")
    );
    assert!(
        !kinds_seen.contains(&ApplyDetailKindRecord::None),
        "None 只给应用成功"
    );
}

#[test]
fn apply_config_without_trusted_keys_uses_trust_all_in_debug_builds() {
    // 受信公钥表为空：调试构建 TrustAllVerifier（签名非空即过）；生产构建建站已被 4003 拒（见 create 文档），
    // 因此生产构建不存在"无验签器却能 apply_config"的路径。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("建站");
    let outcome = core
        .apply_config(signed_config_doc(2, "any-key"))
        .expect("核在");
    assert!(outcome.applied, "{outcome:?}");
    assert_eq!(outcome.active_version, 2);
}

#[test]
fn context_pool_t6_chain_is_observable_on_the_uniffi_face() {
    // madai-w15 I-C-13：T6 核侧全链在 uniffi 面可测——同键两次 context_pool_hits +1；关户 drop_contexts("acct:a/") 回 1、
    // context_pool_dropped 1；B 以同一会话 id 进来 hits 不增（Reserve::Created）；别的账户不受影响。
    // madai-w16 I-C-14：每上下文 KV 取仿真核真值 8 MB（LlamaOptions.kv_per_context_mb）——未配置预算（0）时 8 > 0 → OverBudget、
    // 不进池不驻留（rejected 计数、hits 恒 0）；本例先 apply 带 kv_budget_mb.flagship = 512 的配置，之后 kv_mb = 8 × 条目。
    // 仿真核席位 1：换键时 worker 腾席位、池随之离键——A 与 B 交替时条目恒 1、回到旧键是新建。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("建站");
    let run = |context: &str| {
        let mut keyed = request(PrivacyRecord::Public, QosRecord::Interactive, "同键", 4);
        keyed.context = Some(context.to_owned());
        let (tx, rx) = mpsc::channel();
        core.stream(keyed, Box::new(ChannelSink(tx)))
            .expect("stream");
        let events = collect_until_terminal(&rx);
        assert!(
            matches!(events.last(), Some(Recorded::Complete(_))),
            "{context}: {events:?}"
        );
        core.stats().expect("核在")
    };

    // 未配置预算：本站 Flagship 取内置缺省 128（madai-w16 首日 K3，DEFAULT_KV_BUDGET_MB）——真值 8 进池驻留，生产站配置到达前
    // 也有有界复用（此前 0.14.0 口径"预算 0 不进池"改为"显式 0 才关池"）。
    let unbudgeted = run("acct:a/conv:1");
    assert_eq!(
        (
            unbudgeted.context_pool_hits,
            unbudgeted.context_pool_entries,
            unbudgeted.context_pool_kv_mb,
            unbudgeted.context_pool_kv_budget_mb
        ),
        (0, 1, 8, 128),
        "内置缺省预算 128：首轮新建、进池驻留"
    );
    let applied = core.apply_config(signed_config_doc(1, "k1")).expect("核在");
    assert!(applied.applied, "{applied:?}");
    assert_eq!(core.stats().expect("核在").context_pool_kv_budget_mb, 512);

    let first = run("acct:a/conv:1");
    assert_eq!(
        (first.context_pool_hits, first.context_pool_entries),
        (1, 1),
        "配置换档不清池：同键复用"
    );
    assert_eq!(first.context_pool_kv_mb, 8, "仿真核真值 8 MB/上下文");
    let second = run("acct:a/conv:1");
    assert_eq!(second.context_pool_hits, 2, "同键复用");
    assert_eq!(second.context_pool_entries, 1);
    let other = run("acct:b/conv:1");
    assert_eq!(
        (other.context_pool_hits, other.context_pool_entries),
        (2, 1),
        "席位 1：B 顶掉 A，池条目 = worker 实际驻留"
    );
    assert_eq!(
        (other.context_pool_kv_mb, other.context_pool_kv_budget_mb),
        (8, 512),
        "kv_mb = 8 × 条目"
    );

    // A 已因腾席位离池：关户 A 回 0（无 A 条目）；B 关户回 1。
    assert_eq!(
        core.drop_contexts("acct:a/".to_owned()),
        Ok(0),
        "A 已不在池"
    );
    assert_eq!(core.drop_contexts("acct:b/".to_owned()), Ok(1), "B 关户");
    let dropped = core.stats().expect("核在");
    assert_eq!(dropped.context_pool_dropped, 1);
    assert_eq!(dropped.context_pool_entries, 0);

    // B 关户后同键进来：新建（hits 不增）；再次同键：命中。
    let reissued = run("acct:b/conv:1");
    assert_eq!(reissued.context_pool_hits, 2, "T6：drop 后同键不复用");
    assert_eq!(reissued.context_pool_entries, 1);
    let b_again = run("acct:b/conv:1");
    assert_eq!(b_again.context_pool_hits, 3, "同键再来命中");
    assert_eq!(
        core.drop_contexts("acct:b/".to_owned()),
        Ok(1),
        "新建的 B 条目再次可丢"
    );
    assert_eq!(core.stats().expect("核在").context_pool_dropped, 2);
    drop(core);
}

#[test]
fn drop_contexts_replies_synchronously_from_control_thread_on_live_station() {
    // madai-w14 I-C-12：uniffi 面 drop_contexts 经邮箱在 control 线程调 ContextPool::drop_prefix、同步回条数。没跑过带 context
    // 键的请求 → 池空 → 任何前缀都是 0；空前缀不动池也回 0；调用后核站照常（apply_config 仍有回执）。接线后的全链见
    // context_pool_t6_chain_is_observable_on_the_uniffi_face。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("建站");
    assert_eq!(core.drop_contexts("acct:a/".to_owned()), Ok(0));
    assert_eq!(core.drop_contexts(String::new()), Ok(0), "空前缀：0");
    assert_eq!(
        core.drop_contexts("acct:a/conv:1".to_owned()),
        Ok(0),
        "不存在的前缀：0"
    );
    assert!(core.apply_config(b"{".to_vec()).is_ok(), "核站仍在");
}

#[test]
fn stats_replies_live_snapshot_from_control_thread_on_production_station() {
    // madai-w15 I-C-13：stats 经邮箱在 control 线程一次取齐；无受信公钥的站 verify_timeouts 恒 0；一轮请求后 inflight 归零、
    // uptime 单调；无 context 键的请求不进上下文池（entries / hits 0）。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("建站");
    let idle = core.stats().expect("核在");
    assert_eq!(
        idle,
        CoreStatsRecord {
            backend_kind: BackendKindRecord::Emulated,
            context_pool_hits: 0,
            context_pool_dropped: 0,
            context_pool_entries: 0,
            context_pool_kv_mb: 0,
            context_pool_kv_budget_mb: 128,
            verify_timeouts: 0,
            inflight_jobs: 0,
            uptime_ms: idle.uptime_ms,
        }
    );

    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "读数", 4),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert!(matches!(events.last(), Some(Recorded::Complete(_))));
    let done = core.stats().expect("核在");
    assert_eq!(done.inflight_jobs, 0, "终态后在飞归零");
    assert_eq!(done.context_pool_entries, 0, "无 context 键：不进池");
    assert_eq!(done.context_pool_hits, 0);
    assert_eq!(done.verify_timeouts, 0);
    assert!(done.uptime_ms >= idle.uptime_ms, "uptime 单调");
    drop(core);
}

/// 测试用 base64 编码（标准字母表 + 填充）。
fn base64_encode_for_test(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
