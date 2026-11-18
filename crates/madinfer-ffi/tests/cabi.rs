//! C 面行为测试（I-C-06）：以 `extern "C"` 回调函数扮演鸿蒙 NAPI 宿主，经八符号走真核（backend-llama 仿真核）。
//! 覆盖：abi = 1、create / destroy、probe 容量协议两态、stream K1 序 + 观测类型化批次、cancel 3008 恰一次 + 幂等、
//! authorize 请求 / 回投对（Deny 3007、AllowRemember 记忆、回调内同步回投、重复 / 未知 4001）、destroy 时授权中 3008、
//! 形式错误 4001 无回调、panic 防火墙 4002、kv_get 4 KB 容量协议、signal 入口、回调内 cancel 不死锁。
//!
//! `unsafe` 说明：本文件是 C 宿主的替身——回调解引用 user_data / 事件指针、入口传裸指针——与 src/cabi.rs 一起构成
//! C 边界的两侧；其余 crate 与 lib.rs 仍零 `unsafe`（workspace lints `unsafe_code = "deny"`，此处测试目标放开）。

#![allow(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // 测试放开（根 Cargo.toml lints 注）

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::RwLock;
use std::time::Duration;

use madinfer_ffi::cabi::{
    madinfer_abi_version, madinfer_apply_config, madinfer_core_create, madinfer_core_destroy,
    madinfer_drop_contexts, madinfer_host_reply, madinfer_probe, madinfer_signal,
    madinfer_stream_cancel, madinfer_stream_start, panic_firewall_probe, RawApplyOutcome,
    RawCapabilityEntry, RawComplete, RawConsent, RawCore, RawCoreOptions, RawDelta, RawFailed,
    RawHostReply, RawHostVtable, RawMessage, RawOptions, RawOsSignal, RawProbeSnapshot, RawRequest,
    RawSinkVtable, RawStarted, RawStr, RawTelemetryBatch, RawTrustedKey, MADINFER_APPLY_DETAIL_MAX,
    MADINFER_BATTERY_LEVEL_UNKNOWN, MADINFER_CABI_FORM, MADINFER_KV_VALUE_MAX, MADINFER_OK,
};
use madinfer_ffi::{
    consent_memory_key, verify_late_reply_count, verify_timeout_count, KV_CONSENT_REMEMBERED,
    KV_INSTALL_MARKER_KEY, SIM_PACKAGE_REF, VERIFY_DEADLINE_MS, VERIFY_THREAD_NAME,
    VERIFY_TIMEOUT_DETAIL,
};
use madinfer_host::{Clock, MonotonicClock};

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);
const INVALID_ARGUMENT: i32 = 4001;
const CONSENT_DENIED: u16 = 3007;
const CANCELLED: u16 = 3008;

// ---------------------------------------------------------------------------
// 宿主替身（host vtable 的 user_data）
// ---------------------------------------------------------------------------

/// 宿主观察到的一次授权请求。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConsentSeen {
    host_request: u64,
    capability: String,
    privacy: u8,
    endpoint_id: String,
}

struct Host {
    clock: MonotonicClock,
    kv: RwLock<BTreeMap<String, Vec<u8>>>,
    kv_put_keys: RwLock<Vec<String>>,
    kv_get_capacity: AtomicU32,
    /// 置位后 install 标记键按"全长 5000 字节"应答（容量协议：写 min(全长, capacity)，*out_len = 全长）。
    oversize_install_marker: bool,
    telemetry: RwLock<Vec<(u32, u32, Vec<u8>)>>,
    consent_tx: Sender<ConsentSeen>,
    /// 置位后在 authorize 回调里**同步**反调 madinfer_host_reply（头文件 §5 回调重入合法）：(core 句柄, 决议, 回码记录)。
    auto_reply: RwLock<Option<(usize, u8)>>,
    auto_reply_rc: AtomicU32,
    verify_calls: AtomicU64,
    /// 验签回调所在线程名（逐次）——madai-w10 I-C-09 J1 / madai-w11 I-C-10：头文件 §5 / §6 钉"在瞬时线程 madinfer-ffi-verify 上调"。
    verify_threads: RwLock<Vec<String>>,
    /// 置位后 verify_ed25519 阻塞到 verify_release 放行（宿主验签卡死的替身；madai-w11 I-C-10 deadline 用例）。
    verify_blocking: AtomicBool,
    verify_release: AtomicBool,
    /// verify_ed25519 已返回次数（含超时后迟到返回的）。
    verify_returned: AtomicU64,
    /// 线程钩子回执：(worker_class, 回调所在线程名)——madai-w08 G7。
    worker_starts: RwLock<Vec<(u8, String)>>,
}

fn leak_host(oversize_install_marker: bool) -> (&'static Host, Receiver<ConsentSeen>) {
    let (consent_tx, consent_rx) = mpsc::channel();
    let host = Box::leak(Box::new(Host {
        clock: MonotonicClock::new(),
        kv: RwLock::new(BTreeMap::new()),
        kv_put_keys: RwLock::new(Vec::new()),
        kv_get_capacity: AtomicU32::new(0),
        oversize_install_marker,
        telemetry: RwLock::new(Vec::new()),
        consent_tx,
        auto_reply: RwLock::new(None),
        auto_reply_rc: AtomicU32::new(u32::MAX),
        verify_calls: AtomicU64::new(0),
        verify_threads: RwLock::new(Vec::new()),
        verify_blocking: AtomicBool::new(false),
        verify_release: AtomicBool::new(false),
        verify_returned: AtomicU64::new(0),
        worker_starts: RwLock::new(Vec::new()),
    }));
    (host, consent_rx)
}

/// # Safety
///
/// `raw.len > 0` 时 `raw.ptr` 指向 `len` 字节有效内存（回调实参借用语义）。
unsafe fn string_of(raw: RawStr) -> String {
    if raw.len == 0 {
        return String::new();
    }
    // SAFETY: 契约同本函数。
    let bytes = unsafe { std::slice::from_raw_parts(raw.ptr, raw.len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

unsafe extern "C" fn host_now_ms(user_data: *mut c_void) -> u64 {
    // SAFETY: user_data 是 leak_host 发放的 &'static Host。
    let host = unsafe { &*user_data.cast::<Host>() };
    host.clock.now_ms()
}

unsafe extern "C" fn host_kv_get(
    user_data: *mut c_void,
    key: RawStr,
    out_value: *mut u8,
    capacity: u32,
    out_len: *mut u32,
) -> i32 {
    // SAFETY: user_data 是 &'static Host；key 借用到本回调返回；out_value 有 capacity 字节；out_len 可写。
    let host = unsafe { &*user_data.cast::<Host>() };
    let key = unsafe { string_of(key) };
    host.kv_get_capacity.store(capacity, Ordering::SeqCst);
    if host.oversize_install_marker && key == KV_INSTALL_MARKER_KEY {
        let full: u32 = 5000;
        let written = full.min(capacity) as usize;
        // SAFETY: 宿主契约——只写 min(全长, capacity) 字节。
        unsafe {
            ptr::write_bytes(out_value, 0xAB, written);
            *out_len = full;
        }
        return 1;
    }
    match host.kv.read().unwrap().get(&key) {
        Some(value) => {
            let written = value.len().min(capacity as usize);
            // SAFETY: 同上。
            unsafe {
                ptr::copy_nonoverlapping(value.as_ptr(), out_value, written);
                *out_len = value.len() as u32;
            }
            1
        }
        None => 0,
    }
}

unsafe extern "C" fn host_kv_put(user_data: *mut c_void, key: RawStr, value: *const u8, len: u32) {
    // SAFETY: user_data 是 &'static Host；key / value 借用到本回调返回。
    let host = unsafe { &*user_data.cast::<Host>() };
    let key = unsafe { string_of(key) };
    let value = if len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(value, len as usize) }.to_vec()
    };
    host.kv_put_keys.write().unwrap().push(key.clone());
    host.kv.write().unwrap().insert(key, value);
}

unsafe extern "C" fn host_telemetry_send(user_data: *mut c_void, batch: *const RawTelemetryBatch) {
    // SAFETY: user_data 是 &'static Host；batch 及其 payload 借用到本回调返回。
    let host = unsafe { &*user_data.cast::<Host>() };
    let batch = unsafe { *batch };
    let payload = if batch.payload_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(batch.payload, batch.payload_len as usize) }.to_vec()
    };
    host.telemetry
        .write()
        .unwrap()
        .push((batch.schema_version, batch.event_count, payload));
}

unsafe extern "C" fn host_authorize(
    user_data: *mut c_void,
    host_request: u64,
    context: *const RawConsent,
) {
    // SAFETY: user_data 是 &'static Host；context 借用到本回调返回。
    let host = unsafe { &*user_data.cast::<Host>() };
    let context = unsafe { *context };
    let seen = ConsentSeen {
        host_request,
        capability: unsafe { string_of(context.capability) },
        privacy: context.privacy,
        endpoint_id: unsafe { string_of(context.endpoint_id) },
    };
    let _ = host.consent_tx.send(seen);
    // 回调重入：在 authorize 里同步回投（只入队纪律的边界用例——核侧回调期间不持锁，故合法）。
    if let Some((core, decision)) = *host.auto_reply.read().unwrap() {
        let reply = RawHostReply {
            kind: 0,
            consent_decision: decision,
        };
        // SAFETY: core 是本测试尚未 destroy 的句柄。
        let rc = unsafe { madinfer_host_reply(core as *mut RawCore, host_request, &reply) };
        host.auto_reply_rc.store(rc as u32, Ordering::SeqCst);
    }
}

unsafe extern "C" fn host_verify_ed25519(
    user_data: *mut c_void,
    message: *const u8,
    message_len: u32,
    signature: *const u8,
    signature_len: u32,
    _public_key: *const u8,
    public_key_len: u32,
) -> i32 {
    // SAFETY: user_data 是 &'static Host；三段缓冲借用到本回调返回。
    let host = unsafe { &*user_data.cast::<Host>() };
    host.verify_calls.fetch_add(1, Ordering::SeqCst);
    host.verify_threads
        .write()
        .unwrap()
        .push(std::thread::current().name().unwrap_or("").to_owned());
    let message = unsafe { std::slice::from_raw_parts(message, message_len as usize) };
    let signature = unsafe { std::slice::from_raw_parts(signature, signature_len as usize) };
    // 替身口径（同 src/tests.rs）：签名 == 消息前 64 字节且公钥非空 → 1。
    let expected: Vec<u8> = message.iter().copied().take(64).collect();
    let verified = i32::from(public_key_len > 0 && signature == expected.as_slice());
    while host.verify_blocking.load(Ordering::SeqCst) && !host.verify_release.load(Ordering::SeqCst)
    {
        // 宿主验签卡死的替身（瞬时线程上；control 线程按 deadline 离场）。
        std::thread::sleep(Duration::from_millis(1));
    }
    host.verify_returned.fetch_add(1, Ordering::SeqCst);
    verified
}

unsafe extern "C" fn host_on_worker_started(user_data: *mut c_void, worker_class: u8) {
    // SAFETY: user_data 是 &'static Host。
    let host = unsafe { &*user_data.cast::<Host>() };
    let thread = std::thread::current().name().unwrap_or("").to_owned();
    host.worker_starts
        .write()
        .unwrap()
        .push((worker_class, thread));
}

fn vtable(host: &'static Host) -> RawHostVtable {
    RawHostVtable {
        user_data: ptr::from_ref(host).cast_mut().cast::<c_void>(),
        now_ms: Some(host_now_ms),
        kv_get: Some(host_kv_get),
        kv_put: Some(host_kv_put),
        telemetry_send: Some(host_telemetry_send),
        authorize: Some(host_authorize),
        verify_ed25519: Some(host_verify_ed25519),
        on_worker_started: Some(host_on_worker_started),
    }
}

// ---------------------------------------------------------------------------
// sink 替身（sink vtable 的 user_data）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ev {
    Started {
        request: u64,
        source: u8,
        package: Option<String>,
        route_reason: u8,
    },
    Delta {
        request: u64,
        seq: u32,
        text: String,
        structured_patch: Option<String>,
    },
    Complete {
        request: u64,
        text: String,
        fallback_steps: u8,
        source: Option<u8>,
        output_kind: u8,
        output: String,
    },
    Failed {
        request: u64,
        code: u16,
        wire: String,
        partial: Option<String>,
    },
}

impl Ev {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. })
    }
}

struct Sink {
    tx: Sender<Ev>,
    /// 置位后在 on_delta 里反调 madinfer_stream_cancel（回调内取消不死锁用例）。
    cancel_core: AtomicPtr<RawCore>,
    cancel_request: AtomicU64,
}

fn leak_sink() -> (&'static Sink, Receiver<Ev>) {
    let (tx, rx) = mpsc::channel();
    let sink = Box::leak(Box::new(Sink {
        tx,
        cancel_core: AtomicPtr::new(ptr::null_mut()),
        cancel_request: AtomicU64::new(0),
    }));
    (sink, rx)
}

unsafe extern "C" fn sink_on_started(user_data: *mut c_void, event: *const RawStarted) {
    // SAFETY: user_data 是 leak_sink 发放的 &'static Sink；event 借用到本回调返回。
    let sink = unsafe { &*user_data.cast::<Sink>() };
    let event = unsafe { *event };
    let _ = sink.tx.send(Ev::Started {
        request: event.request,
        source: event.source,
        package: (event.has_package != 0).then(|| unsafe { string_of(event.package) }),
        route_reason: event.route_reason,
    });
}

unsafe extern "C" fn sink_on_delta(user_data: *mut c_void, event: *const RawDelta) {
    // SAFETY: 同 sink_on_started。
    let sink = unsafe { &*user_data.cast::<Sink>() };
    let event = unsafe { *event };
    let core = sink.cancel_core.load(Ordering::SeqCst);
    if !core.is_null() {
        // 回调里再调入口：只入队立即返回（K12 / 头文件 §5 回调重入）。
        // SAFETY: core 是本测试尚未 destroy 的句柄。
        unsafe { madinfer_stream_cancel(core, sink.cancel_request.load(Ordering::SeqCst)) };
    }
    let _ = sink.tx.send(Ev::Delta {
        request: event.request,
        seq: event.seq,
        text: unsafe { string_of(event.text) },
        structured_patch: (event.has_structured_patch != 0)
            .then(|| unsafe { string_of(event.structured_patch) }),
    });
}

unsafe extern "C" fn sink_on_complete(user_data: *mut c_void, event: *const RawComplete) {
    // SAFETY: 同 sink_on_started。
    let sink = unsafe { &*user_data.cast::<Sink>() };
    let event = unsafe { *event };
    let _ = sink.tx.send(Ev::Complete {
        request: event.request,
        text: unsafe { string_of(event.text) },
        fallback_steps: event.stats.fallback_steps,
        source: (event.stats.has_source != 0).then_some(event.stats.source),
        output_kind: event.output_kind,
        output: unsafe { string_of(event.output) },
    });
}

unsafe extern "C" fn sink_on_failed(user_data: *mut c_void, event: *const RawFailed) {
    // SAFETY: 同 sink_on_started。
    let sink = unsafe { &*user_data.cast::<Sink>() };
    let event = unsafe { *event };
    let _ = sink.tx.send(Ev::Failed {
        request: event.request,
        code: event.code,
        wire: unsafe { string_of(event.wire) },
        partial: (event.has_partial_text != 0).then(|| unsafe { string_of(event.partial_text) }),
    });
}

fn sink_vtable(sink: &'static Sink) -> RawSinkVtable {
    RawSinkVtable {
        user_data: ptr::from_ref(sink).cast_mut().cast::<c_void>(),
        on_started: Some(sink_on_started),
        on_delta: Some(sink_on_delta),
        on_complete: Some(sink_on_complete),
        on_failed: Some(sink_on_failed),
    }
}

// ---------------------------------------------------------------------------
// 入参夹具（借用切片活到夹具 Drop；测试按 'static 泄漏字符串以简化生命期）
// ---------------------------------------------------------------------------

fn leak_str(text: impl Into<String>) -> &'static str {
    Box::leak(text.into().into_boxed_str())
}

struct OptionsFixture {
    _capabilities: Vec<RawStr>,
    _keys: Vec<RawTrustedKey>,
    raw: RawCoreOptions,
}

fn options(
    capabilities: &[&'static str],
    consent_endpoint: Option<&'static str>,
    trusted_keys: &[(&'static str, &'static str)],
) -> OptionsFixture {
    let capabilities: Vec<RawStr> = capabilities.iter().map(|c| RawStr::borrow(c)).collect();
    let keys: Vec<RawTrustedKey> = trusted_keys
        .iter()
        .map(|(id, hex)| RawTrustedKey {
            key_id: RawStr::borrow(id),
            public_key_hex: RawStr::borrow(hex),
        })
        .collect();
    let raw = RawCoreOptions {
        instance_name: RawStr::borrow("madinfer-cabi-test"),
        tier: 2, // Flagship
        os: 2,   // Ohos
        frame_ms: 20,
        sampling_bp: 10_000,
        capabilities: capabilities.as_ptr(),
        capabilities_len: capabilities.len() as u32,
        has_consent_endpoint: u8::from(consent_endpoint.is_some()),
        consent_endpoint: RawStr::borrow(consent_endpoint.unwrap_or("")),
        trusted_keys: keys.as_ptr(),
        trusted_keys_len: keys.len() as u32,
    };
    OptionsFixture {
        _capabilities: capabilities,
        _keys: keys,
        raw,
    }
}

struct RequestFixture {
    _messages: Vec<RawMessage>,
    _stop: Vec<RawStr>,
    raw: RawRequest,
}

fn request(privacy: u8, qos: u8, prompt: &'static str, max_tokens: u32) -> RequestFixture {
    let messages = vec![RawMessage {
        role: 1, // User
        content: RawStr::borrow(prompt),
    }];
    let stop: Vec<RawStr> = Vec::new();
    let raw = RawRequest {
        capability: RawStr::borrow("summarize"),
        business: 7,
        privacy,
        qos,
        deadline_ms: 10_000,
        messages: messages.as_ptr(),
        messages_len: messages.len() as u32,
        options: RawOptions {
            has_temperature_x100: 0,
            temperature_x100: 0,
            has_top_p_bp: 0,
            top_p_bp: 0,
            has_max_tokens: 1,
            max_tokens,
            stop: stop.as_ptr(),
            stop_len: 0,
            has_seed: 1,
            seed: 7,
        },
        output_kind: 0, // Text
        schema_json: RawStr::borrow(""),
        has_context: 0,
        context: RawStr::borrow(""),
    };
    RequestFixture {
        _messages: messages,
        _stop: stop,
        raw,
    }
}

/// Structured 请求夹具（madai-w08 G4）：output_kind = 1 + schema 文本。
fn structured_request(schema_json: &'static str, max_tokens: u32) -> RequestFixture {
    let mut fixture = request(
        PUBLIC,
        INTERACTIVE,
        "把今天的群消息整理成会议纪要",
        max_tokens,
    );
    fixture.raw.output_kind = 1;
    fixture.raw.schema_json = RawStr::borrow(schema_json);
    fixture
}

const PUBLIC: u8 = 0;
const INTERNAL: u8 = 1;
const E2EE: u8 = 2;
const INTERACTIVE: u8 = 0;
const ASYNC: u8 = 1;
const DENY: u8 = 2;
const ALLOW: u8 = 0;
const ALLOW_REMEMBER: u8 = 1;

fn create(host: &'static Host, fixture: &OptionsFixture) -> *mut RawCore {
    let vt = vtable(host);
    let mut error = -1;
    // SAFETY: vt / fixture.raw 在调用期间有效；error 可写。
    let core = unsafe { madinfer_core_create(&vt, &fixture.raw, &mut error) };
    assert!(!core.is_null(), "create 失败：{error}");
    assert_eq!(error, MADINFER_OK);
    core
}

fn start(core: *mut RawCore, fixture: &RequestFixture, sink: &'static Sink) -> u64 {
    let vt = sink_vtable(sink);
    let mut error = -1;
    // SAFETY: core 有效；fixture.raw / vt 在调用期间有效；error 可写。
    let handle = unsafe { madinfer_stream_start(core, &fixture.raw, &vt, &mut error) };
    assert!(handle > 0, "stream_start 失败：{error}");
    assert_eq!(error, MADINFER_OK);
    handle
}

fn destroy(core: *mut RawCore) {
    // SAFETY: core 是本测试发放且未回收的句柄。
    unsafe { madinfer_core_destroy(core) };
}

fn collect_until_terminal(rx: &Receiver<Ev>) -> Vec<Ev> {
    let mut events = Vec::new();
    loop {
        let event = rx.recv_timeout(WAIT).expect("终态前不该超时");
        let terminal = event.is_terminal();
        events.push(event);
        if terminal {
            return events;
        }
    }
}

fn assert_quiet(rx: &Receiver<Ev>) {
    match rx.recv_timeout(QUIET) {
        Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout) => {}
        Ok(event) => panic!("终态后仍有事件：{event:?}"),
    }
}

fn started_count(events: &[Ev]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Ev::Started { .. }))
        .count()
}

fn reply(core: *mut RawCore, host_request: u64, kind: u8, decision: u8) -> i32 {
    let record = RawHostReply {
        kind,
        consent_decision: decision,
    };
    // SAFETY: core 有效；record 在调用期间有效。
    unsafe { madinfer_host_reply(core, host_request, &record) }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[test]
fn abi_version_is_1_and_form_is_napi_rust() {
    assert_eq!(madinfer_abi_version(), 2);
    assert_eq!(MADINFER_CABI_FORM, "napi-rust");
}

#[test]
fn create_rejects_null_and_malformed_input_with_4001_before_spawn() {
    let (host, _consents) = leak_host(false);
    let good = options(&["summarize"], None, &[]);
    let vt = vtable(host);
    let mut error = 0;
    // SAFETY: 空指针是被拒绝的输入；其余实参有效。
    unsafe {
        assert!(madinfer_core_create(ptr::null(), &good.raw, &mut error).is_null());
        assert_eq!(error, INVALID_ARGUMENT);
        assert!(madinfer_core_create(&vt, ptr::null(), &mut error).is_null());
        assert_eq!(error, INVALID_ARGUMENT);
        // out_error_code 可为 NULL。
        assert!(madinfer_core_create(ptr::null(), &good.raw, ptr::null_mut()).is_null());
    }
    // madai-w08 G7：on_worker_started 可为 NULL（不设优先级），建站照常（独立宿主：本例末尾断言本宿主未被写入）。
    {
        let (quiet_host, _c) = leak_host(false);
        let mut no_hooks = vtable(quiet_host);
        no_hooks.on_worker_started = None;
        // SAFETY: 同上。
        let core = unsafe { madinfer_core_create(&no_hooks, &good.raw, &mut error) };
        assert!(
            !core.is_null(),
            "on_worker_started 为 NULL 不算形式错误：{error}"
        );
        destroy(core);
        assert!(quiet_host.worker_starts.read().unwrap().is_empty());
    }
    // vtable 前六个函数指针任一为 NULL → 4001（F2 新增的 verify_ed25519 同样必填）。
    let mut missing = vt;
    missing.verify_ed25519 = None;
    // SAFETY: 同上。
    unsafe {
        assert!(madinfer_core_create(&missing, &good.raw, &mut error).is_null());
    }
    assert_eq!(error, INVALID_ARGUMENT);
    // 非法能力名 / 空词表 / 档位越界 / 非 UTF-8 / 非 hex 公钥。
    for bad in [
        options(&["BAD NAME"], None, &[]),
        options(&[], None, &[]),
        options(&["summarize"], None, &[("k1", "zz")]),
    ] {
        // SAFETY: 同上。
        unsafe {
            assert!(madinfer_core_create(&vt, &bad.raw, &mut error).is_null());
        }
        assert_eq!(error, INVALID_ARGUMENT);
    }
    let mut bad_tier = good.raw;
    bad_tier.tier = 4;
    let invalid_utf8 = [0xffu8, 0xfe];
    let mut bad_name = good.raw;
    bad_name.instance_name = RawStr {
        ptr: invalid_utf8.as_ptr(),
        len: 2,
    };
    for bad in [bad_tier, bad_name] {
        // SAFETY: 同上。
        unsafe {
            assert!(madinfer_core_create(&vt, &bad, &mut error).is_null());
        }
        assert_eq!(error, INVALID_ARGUMENT);
    }
    assert!(
        host.kv_put_keys.read().unwrap().is_empty(),
        "全部在 spawn 前拒绝：宿主未被写入"
    );
}

#[test]
fn create_probe_capacity_protocol_two_states_then_destroy() {
    let (host, _consents) = leak_host(false);
    let fixture = options(
        &["summarize", "embed.text"],
        None,
        &[("k1", leak_str("ab".repeat(32)))],
    );
    let core = create(host, &fixture);
    // 建站经 kv_get（4 KB 容量）→ 缺席 → kv_put install 标记。
    assert_eq!(
        host.kv_get_capacity.load(Ordering::SeqCst),
        MADINFER_KV_VALUE_MAX,
        "kv_get 以 MADINFER_KV_VALUE_MAX 容量调用"
    );
    assert_eq!(
        host.kv
            .read()
            .unwrap()
            .get(KV_INSTALL_MARKER_KEY)
            .map(Vec::len),
        Some(8),
        "install 标记 = now_ms 大端 8 字节"
    );

    let blank = RawProbeSnapshot {
        abi_version: 0,
        core_version: [0; 32],
        core_version_len: 0,
        tier: 0,
        snapshot_version: 0,
        entries: ptr::null_mut(),
        entries_capacity: 0,
        entries_len: 0,
        entries_total: 0,
    };
    // 态一：容量 0（宿主先问总数）→ 0 且 len 0 / total 2，不算错。
    let mut probe = blank;
    // SAFETY: core 有效；probe 可写；capacity 0 不写 entries。
    assert_eq!(unsafe { madinfer_probe(core, &mut probe) }, MADINFER_OK);
    assert_eq!(probe.abi_version, 2);
    assert_eq!(
        &probe.core_version[..probe.core_version_len as usize],
        env!("CARGO_PKG_VERSION").as_bytes()
    );
    assert_eq!(probe.tier, 2, "档位回填 Flagship");
    assert!(probe.snapshot_version >= 1);
    assert_eq!((probe.entries_len, probe.entries_total), (0, 2));
    // 态一'：容量 1 < total → 写 1 条、total 仍 2（宿主据此扩容重探）。
    let mut one = [RawCapabilityEntry {
        capability: [0; 64],
        capability_len: 0,
        state: 9,
        has_source: 9,
        source: 9,
        latency: 9,
        constraint: 9,
    }];
    let mut probe = blank;
    probe.entries = one.as_mut_ptr();
    probe.entries_capacity = 1;
    // SAFETY: entries 指向 1 个可写条目。
    assert_eq!(unsafe { madinfer_probe(core, &mut probe) }, MADINFER_OK);
    assert_eq!((probe.entries_len, probe.entries_total), (1, 2));
    assert_eq!(
        &one[0].capability[..one[0].capability_len as usize],
        b"embed.text",
        "条目按能力名排序"
    );
    // 态二：容量足够 → len == total，条目全填。
    let mut four = [one[0]; 4];
    let mut probe = blank;
    probe.entries = four.as_mut_ptr();
    probe.entries_capacity = 4;
    // SAFETY: entries 指向 4 个可写条目。
    assert_eq!(unsafe { madinfer_probe(core, &mut probe) }, MADINFER_OK);
    assert_eq!((probe.entries_len, probe.entries_total), (2, 2));
    let names: Vec<&[u8]> = four[..2]
        .iter()
        .map(|e| &e.capability[..e.capability_len as usize])
        .collect();
    assert_eq!(names, [b"embed.text".as_slice(), b"summarize".as_slice()]);
    for entry in &four[..2] {
        assert_eq!(entry.state, 0, "Available");
        assert_eq!((entry.has_source, entry.source), (1, 1), "Bundled");
        assert!(entry.latency <= 2);
        assert_eq!(entry.constraint, 1, "仿真核 MADINFER_CONSTRAINT_EMULATED");
    }
    // 形式错误：capacity > 0 而 entries 为 NULL / out_buf 为 NULL / core 为 NULL。
    let mut bad = blank;
    bad.entries_capacity = 3;
    // SAFETY: 被拒绝的输入不解引用。
    unsafe {
        assert_eq!(madinfer_probe(core, &mut bad), INVALID_ARGUMENT);
        assert_eq!(madinfer_probe(core, ptr::null_mut()), INVALID_ARGUMENT);
        assert_eq!(
            madinfer_probe(ptr::null_mut(), &mut probe),
            INVALID_ARGUMENT
        );
    }
    destroy(core);
    // NULL 幂等。
    destroy(ptr::null_mut());
    assert_eq!(
        host.verify_calls.load(Ordering::SeqCst),
        0,
        "受信公钥注入只在建站登记；无配置 / 算法包到达则宿主验签不被调"
    );
}

#[test]
fn stream_keeps_k1_order_and_telemetry_arrives_as_typed_batch_on_destroy() {
    let (host, _consents) = leak_host(false);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let req = request(E2EE, INTERACTIVE, "总结一下今天的群消息", 24);
    let handle = start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1, "Started 恰一次：{events:?}");
    let Ev::Started {
        request,
        source,
        package,
        route_reason,
    } = &events[0]
    else {
        panic!("Started 必须第一：{events:?}");
    };
    assert_eq!(*request, handle);
    assert_eq!(*source, 1, "Bundled");
    assert_eq!(package.as_deref(), Some(SIM_PACKAGE_REF));
    // 建站未带云候选：隐私门无可剔除项，路由原因落 Default(4)；取值在 madinfer_route_reason 表内。
    assert_eq!(*route_reason, 4, "无云候选 → Default");
    let deltas: Vec<(u32, &str)> = events
        .iter()
        .filter_map(|e| match e {
            Ev::Delta { seq, text, .. } => Some((*seq, text.as_str())),
            _ => None,
        })
        .collect();
    // madai-w12 I-C-11（R-w11-5）：仿真核按输出字节数切片（每片 ≤ 32 字节、码点边界）——「总结一下今天的群消息」36 字节
    // 恰 2 片，≤ 2 片时合帧后帧数 = 片数（首片立即放行，次片或折叠后随终态强制放行、或窗口过后放行——两路都恰 1 帧），
    // 与调度时序无关：帧数与逐帧文本可钉。
    assert_eq!(
        deltas,
        vec![(0, "「总结一下今天的群消"), (1, "息」")],
        "两片两帧、逐帧文本确定：{events:?}"
    );
    assert!(
        events.iter().all(|e| !matches!(
            e,
            Ev::Delta {
                structured_patch: Some(_),
                ..
            }
        )),
        "Text 请求 has_structured_patch = 0"
    );
    // madai-w08 G7：线程钩子在核线程上回调，control 恰一次 + 推理 worker 至少一次。
    {
        let starts = host.worker_starts.read().unwrap();
        assert_eq!(
            starts
                .iter()
                .filter(|(class, thread)| *class == 0 && thread == "madinfer-core")
                .count(),
            1,
            "MADINFER_WORKER_CONTROL 恰一次：{starts:?}"
        );
        assert!(
            starts
                .iter()
                .any(|(class, thread)| (1..=2).contains(class)
                    && thread.starts_with("madinfer-worker-")),
            "推理 worker 上报：{starts:?}"
        );
    }
    match events.last() {
        Some(Ev::Complete {
            request,
            text,
            fallback_steps,
            source,
            output_kind,
            output,
        }) => {
            assert_eq!(*request, handle);
            let joined: String = deltas.iter().map(|(_, t)| *t).collect();
            assert_eq!(*text, joined, "Delta 拼接 == 完整输出");
            assert_eq!((*output_kind, output), (0, text), "Text：output 同 text");
            assert_eq!(*fallback_steps, 0);
            assert_eq!(*source, Some(1));
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_quiet(&rx);
    destroy(core); // 关停排水：观测批次经 telemetry_send 刷到宿主。
    let batches = host.telemetry.read().unwrap();
    assert!(!batches.is_empty(), "观测批次到宿主");
    for (schema, count, payload) in batches.iter() {
        assert_eq!(*schema, 1, "telemetry.v1 头字段类型化过界");
        assert!(*count >= 1);
        assert!(!payload.is_empty());
        assert!(
            !String::from_utf8_lossy(payload).contains("总结"),
            "用户内容禁入观测载荷"
        );
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
fn structured_request_streams_patches_and_complete_carries_json_output() {
    // madai-w08 G4 C 面一例：Structured 走完 K1、每 delta has_structured_patch = 1、patch 逐帧应用 == complete.output（kind 1）；
    // 非法 schema → 2003 无 Started；output_kind 越界 → 4001 无回调。
    let (host, _consents) = leak_host(false);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let schema = r#"{"type":"object","properties":{"title":{"type":"string"},"count":{"type":"integer"},"tags":{"type":"array","items":{"type":"string"}}},"required":["title","count","tags"]}"#;
    let req = structured_request(schema, 256);
    let handle = start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1, "{events:?}");
    assert!(matches!(events[0], Ev::Started { .. }));
    let mut doc = serde_json::json!({});
    let mut expected_seq = 0;
    for event in &events {
        if let Ev::Delta {
            seq,
            structured_patch,
            ..
        } = event
        {
            assert_eq!(*seq, expected_seq, "seq 连续");
            expected_seq += 1;
            let patch = structured_patch
                .as_deref()
                .expect("Structured 请求每帧 has_structured_patch = 1");
            apply_patch_text(&mut doc, patch);
        }
    }
    let Some(Ev::Complete {
        request: completed,
        text,
        output_kind,
        output,
        ..
    }) = events.last()
    else {
        panic!("终态应为 Complete：{events:?}");
    };
    assert_eq!(*completed, handle);
    assert_eq!(*output_kind, 1, "MADINFER_OUTPUT_STRUCTURED");
    assert_eq!(text, output, "兼容字段 text 同文");
    let value: serde_json::Value = serde_json::from_str(output).expect("合法 JSON");
    assert!(value["title"].is_string() && value["count"].is_i64() && value["tags"].is_array());
    assert_eq!(doc, value, "逐帧 patch 应用后 == complete.output");
    assert_quiet(&rx);

    // 非法 schema：形式合法（schema 是载荷）→ 事件面 2003，无 Started。
    let (sink, rx) = leak_sink();
    let bad = structured_request(r#"{"anyOf":[]}"#, 64);
    start(core, &bad, sink);
    let events = collect_until_terminal(&rx);
    assert_eq!(events.len(), 1, "无 Started：{events:?}");
    assert!(
        matches!(&events[0], Ev::Failed { code: 2003, wire, partial: None, .. } if wire == "schema_invalid"),
        "{events:?}"
    );
    assert_quiet(&rx);

    // output_kind 越界 / context 超长 → 4001，无回调。
    let (sink, rx) = leak_sink();
    let vt = sink_vtable(sink);
    let mut out_of_range = structured_request(schema, 64);
    out_of_range.raw.output_kind = 2;
    let long_context = leak_str("x".repeat(129));
    let mut too_long = request(PUBLIC, INTERACTIVE, "hi", 8);
    too_long.raw.has_context = 1;
    too_long.raw.context = RawStr::borrow(long_context);
    for bad in [&out_of_range, &too_long] {
        let mut error = 0;
        // SAFETY: core 有效；bad.raw / vt 在调用期间有效。
        let handle = unsafe { madinfer_stream_start(core, &bad.raw, &vt, &mut error) };
        assert_eq!((handle, error), (0, INVALID_ARGUMENT));
    }
    assert_quiet(&rx);
    destroy(core);
}

#[test]
fn stream_rejects_malformed_request_with_4001_and_no_callback() {
    let (host, _consents) = leak_host(false);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let vt = sink_vtable(sink);
    let good = request(PUBLIC, INTERACTIVE, "x", 8);
    let mut error = 0;
    // SAFETY: 空指针 / 非法取值是被拒绝的输入；其余实参有效。
    unsafe {
        assert_eq!(madinfer_stream_start(core, ptr::null(), &vt, &mut error), 0);
        assert_eq!(error, INVALID_ARGUMENT);
        assert_eq!(
            madinfer_stream_start(core, &good.raw, ptr::null(), &mut error),
            0
        );
        assert_eq!(error, INVALID_ARGUMENT);
        assert_eq!(
            madinfer_stream_start(ptr::null_mut(), &good.raw, &vt, &mut error),
            0
        );
        assert_eq!(error, INVALID_ARGUMENT);
    }
    let mut missing = vt;
    missing.on_failed = None;
    let mut business_zero = good.raw;
    business_zero.business = 0;
    let mut privacy_out = good.raw;
    privacy_out.privacy = 4;
    let mut stop_over = good.raw;
    stop_over.options.stop_len = 9; // 上限先于切片构造：不触碰 stop 指针。
    let mut role_out = good.raw;
    let bad_message = [RawMessage {
        role: 4,
        content: RawStr::borrow("x"),
    }];
    role_out.messages = bad_message.as_ptr();
    role_out.messages_len = 1;
    let mut unknown_capability = good.raw;
    unknown_capability.capability = RawStr::borrow("Bad Name");
    // SAFETY: 同上。
    unsafe {
        assert_eq!(
            madinfer_stream_start(core, &good.raw, &missing, &mut error),
            0
        );
        assert_eq!(error, INVALID_ARGUMENT);
        for bad in [
            business_zero,
            privacy_out,
            stop_over,
            role_out,
            unknown_capability,
        ] {
            assert_eq!(madinfer_stream_start(core, &bad, &vt, &mut error), 0);
            assert_eq!(error, INVALID_ARGUMENT);
        }
    }
    assert_quiet(&rx);
    destroy(core);
}

#[test]
fn cancel_while_awaiting_consent_is_3008_once_idempotent_and_late_reply_pairs_once() {
    let (host, consents) = leak_host(false);
    let fixture = options(&["summarize"], Some("cloud.enterprise"), &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let req = request(INTERNAL, ASYNC, "取消路径样例", 16);
    let handle = start(core, &req, sink);
    let seen = consents.recv_timeout(WAIT).expect("授权请求到宿主");
    assert_eq!(seen.capability, "summarize");
    assert_eq!(seen.privacy, INTERNAL);
    assert_eq!(seen.endpoint_id, "cloud.enterprise");
    assert!(seen.host_request > 0);
    // SAFETY: core 有效。
    unsafe {
        assert_eq!(madinfer_stream_cancel(core, handle), MADINFER_OK);
        assert_eq!(madinfer_stream_cancel(core, handle), MADINFER_OK, "幂等");
    }
    let events = collect_until_terminal(&rx);
    assert_eq!(
        events,
        [Ev::Failed {
            request: handle,
            code: CANCELLED,
            wire: "cancelled".to_owned(),
            partial: None,
        }],
        "取消先于 Started：只有 Failed(3008)，无 partial"
    );
    assert_quiet(&rx);
    // SAFETY: core 有效；NULL 是被拒绝的输入。
    unsafe {
        assert_eq!(
            madinfer_stream_cancel(core, handle),
            MADINFER_OK,
            "已终态 no-op"
        );
        assert_eq!(
            madinfer_stream_cancel(core, 9_999),
            MADINFER_OK,
            "未知句柄 no-op"
        );
        assert_eq!(
            madinfer_stream_cancel(ptr::null_mut(), handle),
            INVALID_ARGUMENT
        );
    }
    // 迟到的回投：首次配对成功（决议落在已终态请求上被丢弃，无第二个终态）；再次 4001。
    assert_eq!(reply(core, seen.host_request, 0, ALLOW), MADINFER_OK);
    assert_eq!(reply(core, seen.host_request, 0, ALLOW), INVALID_ARGUMENT);
    assert_quiet(&rx);
    destroy(core);
}

#[test]
fn consent_deny_is_3007_and_host_reply_pairs_exactly_once() {
    let (host, consents) = leak_host(false);
    let fixture = options(&["summarize"], Some("cloud.enterprise"), &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let req = request(INTERNAL, ASYNC, "升云授权样例", 16);
    let handle = start(core, &req, sink);
    let seen = consents.recv_timeout(WAIT).expect("授权请求到宿主");
    // 形式错误先于配对：未知 id / id 0 / kind 不符 / 决议越界 / 空指针 → 4001 且无副作用。
    assert_eq!(
        reply(core, seen.host_request + 100, 0, DENY),
        INVALID_ARGUMENT
    );
    assert_eq!(reply(core, 0, 0, DENY), INVALID_ARGUMENT);
    assert_eq!(reply(core, seen.host_request, 1, DENY), INVALID_ARGUMENT);
    assert_eq!(reply(core, seen.host_request, 0, 3), INVALID_ARGUMENT);
    // SAFETY: 空指针是被拒绝的输入。
    unsafe {
        assert_eq!(
            madinfer_host_reply(core, seen.host_request, ptr::null()),
            INVALID_ARGUMENT
        );
        assert_eq!(
            madinfer_host_reply(ptr::null_mut(), seen.host_request, ptr::null()),
            INVALID_ARGUMENT
        );
    }
    assert!(rx.try_recv().is_err(), "无副作用：请求仍在等回投");
    // madai-w10 I-C-09 J5-a（I-S-05 U-5）：授权专用线程先走 on_worker_started(CONTROL) 再调 authorize——此刻宿主已收到
    // 授权请求，钩子必已回调。
    assert_eq!(
        host.worker_starts
            .read()
            .unwrap()
            .iter()
            .filter(|(class, thread)| *class == 0 && thread == "madinfer-ffi-consent")
            .count(),
        1,
        "consent 线程上报 MADINFER_WORKER_CONTROL 恰一次"
    );
    assert_eq!(reply(core, seen.host_request, 0, DENY), MADINFER_OK);
    let events = collect_until_terminal(&rx);
    assert_eq!(events.len(), 1, "拒绝：只有 Failed，无 Started：{events:?}");
    assert!(matches!(
        &events[0],
        Ev::Failed { request, code: CONSENT_DENIED, wire, .. } if *request == handle && wire == "consent_denied"
    ));
    assert_eq!(
        reply(core, seen.host_request, 0, DENY),
        INVALID_ARGUMENT,
        "重复回投 4001"
    );
    assert_quiet(&rx);
    assert!(
        host.kv
            .read()
            .unwrap()
            .keys()
            .all(|k| k == KV_INSTALL_MARKER_KEY),
        "Deny 不写记忆键"
    );
    destroy(core);
}

#[test]
fn allow_remember_writes_memory_key_and_next_request_skips_host() {
    let (host, consents) = leak_host(false);
    let fixture = options(&["summarize"], Some("cloud.enterprise"), &[]);
    let core = create(host, &fixture);
    let key = consent_memory_key("summarize", "cloud.enterprise");

    let (sink, rx) = leak_sink();
    let req = request(INTERNAL, ASYNC, "记忆位样例", 16);
    let handle = start(core, &req, sink);
    let seen = consents.recv_timeout(WAIT).expect("首次问宿主");
    assert_eq!(
        reply(core, seen.host_request, 0, ALLOW_REMEMBER),
        MADINFER_OK
    );
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    match &events[0] {
        Ev::Started {
            request,
            route_reason,
            ..
        } => {
            assert_eq!(*request, handle);
            assert_eq!(*route_reason, 5, "云目标缺后端 → Fallback");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        events.last(),
        Some(Ev::Complete {
            fallback_steps: 1,
            ..
        })
    ));
    assert_quiet(&rx);
    assert_eq!(
        host.kv.read().unwrap().get(&key),
        Some(&vec![KV_CONSENT_REMEMBERED]),
        "F9：核侧 kv_put 记忆键"
    );

    // 同键第二次：宿主不再被问（授权通道静默），直接走授权通过路径。
    let (sink2, rx2) = leak_sink();
    let req2 = request(INTERNAL, ASYNC, "记忆位样例二", 16);
    start(core, &req2, sink2);
    let events = collect_until_terminal(&rx2);
    assert_eq!(started_count(&events), 1);
    assert!(matches!(
        events.last(),
        Some(Ev::Complete {
            fallback_steps: 1,
            ..
        })
    ));
    assert!(
        consents.recv_timeout(QUIET).is_err(),
        "记忆命中：无第二次 authorize"
    );
    assert_quiet(&rx2);
    destroy(core);
}

#[test]
fn host_reply_inside_authorize_callback_is_legal_reentrancy() {
    let (host, consents) = leak_host(false);
    let fixture = options(&["summarize"], Some("cloud.enterprise"), &[]);
    let core = create(host, &fixture);
    *host.auto_reply.write().unwrap() = Some((core as usize, ALLOW));
    let (sink, rx) = leak_sink();
    let req = request(INTERNAL, ASYNC, "回调内回投样例", 16);
    start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    let seen = consents.recv_timeout(WAIT).expect("授权请求到宿主");
    assert!(seen.host_request > 0);
    assert_eq!(
        host.auto_reply_rc.load(Ordering::SeqCst),
        0,
        "authorize 回调内同步 madinfer_host_reply 配对成功（核侧回调期间不持锁）"
    );
    assert_eq!(started_count(&events), 1);
    assert!(matches!(
        events.last(),
        Some(Ev::Complete {
            fallback_steps: 1,
            ..
        })
    ));
    assert_quiet(&rx);
    *host.auto_reply.write().unwrap() = None;
    destroy(core);
}

#[test]
fn destroy_with_request_awaiting_consent_ends_with_single_3008() {
    let (host, consents) = leak_host(false);
    let fixture = options(&["summarize"], Some("cloud.enterprise"), &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let req = request(INTERNAL, ASYNC, "关停收口样例", 16);
    let handle = start(core, &req, sink);
    let seen = consents.recv_timeout(WAIT).expect("授权请求到宿主");
    assert!(
        rx.try_recv().is_err(),
        "宿主未回投：请求停在 AwaitingConsent"
    );
    // destroy：授权中的请求以 on_failed(3008) 收口，且回调在 destroy 返回前完成。
    destroy(core);
    let events: Vec<Ev> = rx.try_iter().collect();
    assert_eq!(
        events,
        [Ev::Failed {
            request: handle,
            code: CANCELLED,
            wire: "cancelled".to_owned(),
            partial: None,
        }],
        "destroy 时授权中请求 3008 恰一次、无 Started（I-H-02 U-6 进 K1）"
    );
    assert_quiet(&rx);
    assert!(seen.host_request > 0);
    // 观测批次随关停排水到宿主（授权事件 + 请求终态）。
    assert!(!host.telemetry.read().unwrap().is_empty());
}

#[test]
fn panic_firewall_returns_4002_and_process_survives() {
    // C 面入口全部经 cabi::guarded；防火墙自检以同一函数注入 panic（不占符号面：Rust 侧可见、非 C 符号）。
    assert_eq!(panic_firewall_probe(), 4002);
}

#[test]
fn kv_get_capacity_protocol_truncates_oversize_value_to_4kb() {
    // 宿主违反"值 ≤ 4 KB"：全长 5000 → 核只收 min(全长, capacity) = 4096 字节，且判"有值"（不重写 install 标记）。
    let (host, _consents) = leak_host(true);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);
    assert_eq!(
        host.kv_get_capacity.load(Ordering::SeqCst),
        MADINFER_KV_VALUE_MAX
    );
    assert!(
        host.kv_put_keys.read().unwrap().is_empty(),
        "install 标记读到（截断值）即保留，不写"
    );
    destroy(core);
}

#[test]
fn signal_entry_accepts_seven_kinds_and_rejects_out_of_range() {
    let (host, _consents) = leak_host(false);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);
    let base = RawOsSignal {
        kind: 0,
        thermal_state: 3,
        battery_level_pct: 20,
        battery_charging: 0,
        battery_low_power_mode: 1,
        memory_level: 1,
        app_state: 1,
        has_app_budget_ms: 1,
        app_budget_ms: 30_000,
        network_state: 1,
        idle: 1,
        main_link_busy: 0,
    };
    // SAFETY: core 有效；signal 在调用期间有效；空指针是被拒绝的输入。
    unsafe {
        for _ in 0..32 {
            for kind in 0..=6u8 {
                assert_eq!(
                    madinfer_signal(core, &RawOsSignal { kind, ..base }),
                    MADINFER_OK
                );
            }
        }
        assert_eq!(
            madinfer_signal(core, &RawOsSignal { kind: 7, ..base }),
            INVALID_ARGUMENT
        );
        // madai-w08 G7：电量 255 = 未知（合法）；101～254 越界。
        assert_eq!(
            madinfer_signal(
                core,
                &RawOsSignal {
                    kind: 1,
                    battery_level_pct: MADINFER_BATTERY_LEVEL_UNKNOWN,
                    ..base
                }
            ),
            MADINFER_OK
        );
        assert_eq!(
            madinfer_signal(
                core,
                &RawOsSignal {
                    kind: 1,
                    battery_level_pct: 101,
                    ..base
                }
            ),
            INVALID_ARGUMENT
        );
        assert_eq!(
            madinfer_signal(
                core,
                &RawOsSignal {
                    kind: 2,
                    memory_level: 3,
                    ..base
                }
            ),
            INVALID_ARGUMENT
        );
        assert_eq!(madinfer_signal(core, ptr::null()), INVALID_ARGUMENT);
        assert_eq!(madinfer_signal(ptr::null_mut(), &base), INVALID_ARGUMENT);
    }
    // 信号风暴后核仍健康：探测照答。
    let mut probe = RawProbeSnapshot {
        abi_version: 0,
        core_version: [0; 32],
        core_version_len: 0,
        tier: 0,
        snapshot_version: 0,
        entries: ptr::null_mut(),
        entries_capacity: 0,
        entries_len: 0,
        entries_total: 0,
    };
    // SAFETY: core 有效；probe 可写。
    assert_eq!(unsafe { madinfer_probe(core, &mut probe) }, MADINFER_OK);
    assert_eq!(probe.entries_total, 1);
    destroy(core);
}

#[test]
fn cancel_from_inside_on_delta_does_not_deadlock_and_terminal_is_exactly_once() {
    let (host, _consents) = leak_host(false);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);
    let (sink, rx) = leak_sink();
    let prompt = leak_str("回调内取消不死锁样例 ".repeat(64));
    let req = request(PUBLIC, INTERACTIVE, prompt, 4_096);
    sink.cancel_core.store(core, Ordering::SeqCst);
    let handle = start(core, &req, sink);
    sink.cancel_request.store(handle, Ordering::SeqCst);
    let events = collect_until_terminal(&rx);
    let terminals: Vec<&Ev> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminals.len(), 1, "终态恰一次：{events:?}");
    // 首帧前 handle 可能尚未 store（0 → 未知句柄 no-op），后续帧的 cancel 生效；仿真核先到终点也算通过——
    // 本测锁的是"不死锁 + 终态恰一次"。取消赢时 3008 且带 partial（已出 Delta）。
    match terminals[0] {
        Ev::Failed {
            code: CANCELLED,
            partial,
            ..
        } => assert!(partial.as_ref().is_some_and(|t| !t.is_empty())),
        Ev::Complete { .. } => {}
        other => panic!("非法终态：{other:?}"),
    }
    assert_quiet(&rx);
    sink.cancel_core.store(ptr::null_mut(), Ordering::SeqCst);
    destroy(core);
}

// ---------------------------------------------------------------------------
// 第 9 符号 madinfer_apply_config（madai-w08 I-C-07 G2）
// ---------------------------------------------------------------------------

/// 测试用 base64（标准字母表 + 填充；与 src/tests.rs 同款）。
fn base64_encode(bytes: &[u8]) -> String {
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

/// 按替身口径签名的 config.v1 最小文档（summarize → bundled llama.cpp）：sig = base64(签名载荷前 64 字节)。
fn signed_config_doc(version: u64, key_id: &str) -> Vec<u8> {
    let text = format!(
        r#"{{
  "schema": 1, "version": {version}, "issued_at": "2026-09-05T02:00:00Z", "min_sdk": ">=0.1.0",
  "rollout": {{ "buckets": [0, 999] }},
  "mapping": [ {{ "capability": "summarize", "candidates": [
    {{ "priority": 0, "target": {{ "kind": "bundled", "backend": "llama.cpp", "package": "{SIM_PACKAGE_REF}" }}, "when": {{ "tier_min": "low" }} }} ] }} ],
  "thresholds": {{
    "deadline_ms": {{ "interactive": 3000, "async": 20000, "background": 120000 }},
    "frame_ms": 80, "kv_budget_mb": {{ "flagship": 512, "mid": 256, "low": 0 }}, "quotas": {{}},
    "thermal": {{ "background_max": "fair" }}, "gc_keep_days": 7
  }},
  "switches": {{ "capabilities": {{}}, "cloud_only": false, "all_off": false }},
  "signature": {{ "alg": "ed25519", "key_id": "k1", "sig": "" }}
}}"#
    );
    let mut doc: madinfer_config::ConfigDoc = serde_json::from_str(&text).expect("测试文档合法");
    let payload = doc.signing_payload();
    doc.signature = madinfer_config::Signature {
        alg: "ed25519".to_owned(),
        key_id: key_id.to_owned(),
        sig: base64_encode(&payload[..64]),
    };
    serde_json::to_vec(&doc).expect("序列化")
}

fn apply(core: *mut RawCore, document: &[u8]) -> (i32, RawApplyOutcome) {
    let mut out = RawApplyOutcome {
        applied: 9,
        active_version: u64::MAX,
        stage: 9,
        has_reject_reason: 9,
        reject_reason: 9,
        detail_len: 9,
        detail: [0xee; MADINFER_APPLY_DETAIL_MAX],
    };
    // SAFETY: core 有效；document 在调用期间有效；out 可写。
    let rc =
        unsafe { madinfer_apply_config(core, document.as_ptr(), document.len() as u32, &mut out) };
    (rc, out)
}

fn detail_of(out: &RawApplyOutcome) -> &str {
    std::str::from_utf8(&out.detail[..out.detail_len as usize]).expect("detail utf-8")
}

#[test]
fn apply_config_verifies_via_host_callback_and_reports_outcome_atomically() {
    let (host, _consents) = leak_host(false);
    let key_hex = leak_str("ab".repeat(32));
    let fixture = options(&["summarize"], None, &[("k1", key_hex)]);
    let core = create(host, &fixture);
    assert_eq!(host.verify_calls.load(Ordering::SeqCst), 0);

    // 验真 → 应用：verify_ed25519 被调恰一次。
    let (rc, out) = apply(core, &signed_config_doc(5, "k1"));
    assert_eq!(rc, MADINFER_OK);
    assert_eq!(
        (
            out.applied,
            out.active_version,
            out.stage,
            out.has_reject_reason,
            out.detail_len
        ),
        (1, 5, 4 /* APPLIED */, 0, 0)
    );
    assert_eq!(host.verify_calls.load(Ordering::SeqCst), 1);

    // 篡改（issued_at 在载荷前 64 字节内）→ 签名不符（1），停在 RECEIVED（0），活动版本仍 5。
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&signed_config_doc(6, "k1")).unwrap();
    tampered["issued_at"] = serde_json::json!("2026-09-05T03:00:00Z");
    let (rc, out) = apply(core, &serde_json::to_vec(&tampered).unwrap());
    assert_eq!(rc, MADINFER_OK);
    assert_eq!(
        (
            out.applied,
            out.active_version,
            out.stage,
            out.has_reject_reason,
            out.reject_reason
        ),
        (0, 5, 0, 1, 1)
    );
    assert_eq!(host.verify_calls.load(Ordering::SeqCst), 2);

    // 未知 key_id → UNKNOWN_KEY（0），不调宿主验签，detail = key_id。
    let (rc, out) = apply(core, &signed_config_doc(7, "k9"));
    assert_eq!(rc, MADINFER_OK);
    assert_eq!(
        (out.applied, out.has_reject_reason, out.reject_reason),
        (0, 1, 0)
    );
    assert_eq!(detail_of(&out), "k9");
    assert_eq!(out.active_version, 5);
    assert_eq!(
        host.verify_calls.load(Ordering::SeqCst),
        2,
        "未知 key 不进宿主验签"
    );

    // 版本不递增 → VERSION_NOT_NEWER（3）停在 PARSED（2）。
    let (rc, out) = apply(core, &signed_config_doc(5, "k1"));
    assert_eq!(rc, MADINFER_OK);
    assert_eq!((out.reject_reason, out.stage), (3, 2));
    assert_eq!(detail_of(&out), "received=5 active=5");
    // J1（I-C-09）+ I-C-10 deadline（头文件 §5 / §6 口径）：验签回调每次 apply 至多一次——四次 apply 三次回调（未知 key
    // 不进宿主），全部在瞬时线程 madinfer-ffi-verify 上，且每条线程先以 CONTROL 走 on_worker_started。
    assert_eq!(
        *host.verify_threads.read().unwrap(),
        vec![VERIFY_THREAD_NAME.to_owned(); 3],
        "verify_ed25519 须在瞬时线程上、每 apply 至多一次"
    );
    assert_eq!(
        host.worker_starts
            .read()
            .unwrap()
            .iter()
            .filter(|(class, thread)| *class == 0 && thread == VERIFY_THREAD_NAME)
            .count(),
        3,
        "验签瞬时线程每条一次 on_worker_started(CONTROL)"
    );

    // 空文档 → SCHEMA_INVALID（2）；document 指针为 NULL 且 len = 0 合法。
    // SAFETY: core 有效；len = 0 时 document 允许 NULL；out 可写。
    let mut out = RawApplyOutcome {
        applied: 0,
        active_version: 0,
        stage: 0,
        has_reject_reason: 0,
        reject_reason: 0,
        detail_len: 0,
        detail: [0; MADINFER_APPLY_DETAIL_MAX],
    };
    let rc = unsafe { madinfer_apply_config(core, ptr::null(), 0, &mut out) };
    assert_eq!(rc, MADINFER_OK);
    assert_eq!((out.applied, out.reject_reason), (0, 2));

    // 形式错误：out 为 NULL / len > 0 而 document 为 NULL / core 为 NULL → 4001。
    // SAFETY: 传 NULL 是本例目的；非 NULL 参数有效。
    unsafe {
        assert_eq!(
            madinfer_apply_config(core, ptr::null(), 0, ptr::null_mut()),
            INVALID_ARGUMENT
        );
        assert_eq!(
            madinfer_apply_config(core, ptr::null(), 3, &mut out),
            INVALID_ARGUMENT
        );
        assert_eq!(
            madinfer_apply_config(ptr::null_mut(), ptr::null(), 0, &mut out),
            INVALID_ARGUMENT
        );
    }

    // 新配置生效后请求仍走通 K1。
    let (sink, rx) = leak_sink();
    let req = request(PUBLIC, INTERACTIVE, "配置后", 4);
    start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert!(
        matches!(events.last(), Some(Ev::Complete { .. })),
        "{events:?}"
    );
    destroy(core);

    // 观测：config_apply 1 条、config_rejected 4 条随 destroy 排水到宿主。
    let mut applied = 0;
    let mut rejected = 0;
    for (_, _, payload) in host.telemetry.read().unwrap().iter() {
        let doc: serde_json::Value = serde_json::from_slice(payload).unwrap();
        for e in doc["events"].as_array().unwrap() {
            match e["type"].as_str() {
                Some("config_apply") => applied += 1,
                Some("config_rejected") => rejected += 1,
                _ => {}
            }
        }
    }
    assert_eq!((applied, rejected), (1, 4));
}

#[test]
fn apply_config_verify_deadline_rejects_with_timeout_detail_and_station_stays_alive() {
    let _serial = VERIFY_LIFETIME_TEST_LOCK.lock().unwrap();
    // I-S-05 U-3 / w10 R1 的 C 面：宿主 verify_ed25519 卡死 → control 线程等满 MADINFER_VERIFY_DEADLINE_MS 后以
    // SIGNATURE_INVALID（1）/ RECEIVED（0）/ detail "verify=timeout" 拒，核站其余符号照常；宿主迟到返回按代数票哑火计数。
    let (host, _consents) = leak_host(false);
    host.verify_blocking.store(true, Ordering::SeqCst);
    let key_hex = leak_str("ab".repeat(32));
    let fixture = options(&["summarize"], None, &[("k1", key_hex)]);
    let core = create(host, &fixture);
    let timeouts_before = verify_timeout_count();
    let late_before = verify_late_reply_count();

    let started = host.clock.now_ms();
    let (rc, out) = apply(core, &signed_config_doc(5, "k1"));
    let waited_ms = host.clock.now_ms() - started;
    assert_eq!(rc, MADINFER_OK, "deadline 后仍是 0 = 拿到结论");
    assert_eq!(
        (
            out.applied,
            out.active_version,
            out.stage,
            out.has_reject_reason,
            out.reject_reason
        ),
        (
            0, 0, 0, /* RECEIVED */
            1, 1 /* SIGNATURE_INVALID */
        )
    );
    assert_eq!(detail_of(&out), VERIFY_TIMEOUT_DETAIL);
    assert!(
        waited_ms >= VERIFY_DEADLINE_MS,
        "须等满 deadline：waited_ms={waited_ms}"
    );
    assert_eq!(verify_timeout_count(), timeouts_before + 1);
    assert_eq!(
        host.verify_returned.load(Ordering::SeqCst),
        0,
        "宿主验签仍卡着"
    );
    assert_eq!(
        *host.verify_threads.read().unwrap(),
        vec![VERIFY_THREAD_NAME.to_owned()]
    );

    // 核站其余入口照常：stream K1 走通、probe 可答。
    let (sink, rx) = leak_sink();
    let req = request(PUBLIC, INTERACTIVE, "卡死期间", 4);
    start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert!(matches!(events.last(), Some(Ev::Complete { .. })));

    // 放行宿主：迟到回执落在过期票上 → 哑火计数 +1；活动配置不变（无"事后应用"）。
    host.verify_release.store(true, Ordering::SeqCst);
    let deadline = host.clock.now_ms() + 5_000;
    while verify_late_reply_count() < late_before + 1 && host.clock.now_ms() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        verify_late_reply_count(),
        late_before + 1,
        "迟到回执哑火计数"
    );
    let (rc, out) = apply(core, &signed_config_doc(5, "k1"));
    assert_eq!(rc, MADINFER_OK);
    assert_eq!(
        (out.applied, out.active_version),
        (1, 5),
        "放行后期限内回执 → 应用"
    );
    destroy(core);
}

// ---------------------------------------------------------------------------
// 第 10 符号 madinfer_drop_contexts（madai-w14 I-C-12；只加符号、ABI 仍 2）
// ---------------------------------------------------------------------------

/// 调第 10 符号：prefix 按 `madinfer_str` 借用 Rust 串（同 kv_get / kv_put 的 key 形态）。
fn drop_contexts(core: *mut RawCore, prefix: &str) -> (i32, u32) {
    let mut dropped = u32::MAX;
    // SAFETY: core 由调用方保证有效或为 NULL（被拒输入）；prefix 借用只在调用期间；out 可写。
    let rc = unsafe { madinfer_drop_contexts(core, RawStr::borrow(prefix), &mut dropped) };
    (rc, dropped)
}

#[test]
fn drop_contexts_replies_count_synchronously_and_rejects_malformed_input_with_4001() {
    // 核在 control 线程调 ContextPool::drop_prefix 并同步回条数：池尚无预留方（reserve / release 未接进管线）→ 0 条；
    // 空前缀 / 无匹配也是 0 条；out_dropped 可为 NULL；core 为 NULL、len > 0 而 ptr 为 NULL、非 UTF-8 → 4001 且不动核。
    let (host, _consents) = leak_host(false);
    let fixture = options(&["summarize"], None, &[]);
    let core = create(host, &fixture);

    assert_eq!(drop_contexts(core, "acct:a/"), (MADINFER_OK, 0));
    assert_eq!(drop_contexts(core, ""), (MADINFER_OK, 0), "空前缀：0 条");
    assert_eq!(drop_contexts(core, "acct:a/conv:1"), (MADINFER_OK, 0));
    // SAFETY: 传 NULL / 非法字节是本例目的；其余参数有效。
    unsafe {
        assert_eq!(
            madinfer_drop_contexts(core, RawStr::borrow("acct:a/"), ptr::null_mut()),
            MADINFER_OK,
            "out_dropped 可为 NULL"
        );
        assert_eq!(
            madinfer_drop_contexts(ptr::null_mut(), RawStr::borrow("acct:a/"), ptr::null_mut()),
            INVALID_ARGUMENT,
            "core 为 NULL"
        );
        assert_eq!(
            madinfer_drop_contexts(
                core,
                RawStr {
                    ptr: ptr::null(),
                    len: 3
                },
                ptr::null_mut()
            ),
            INVALID_ARGUMENT,
            "len > 0 而 ptr 为 NULL"
        );
        let bad = [0xffu8, 0xfe, 0x61];
        assert_eq!(
            madinfer_drop_contexts(
                core,
                RawStr {
                    ptr: bad.as_ptr(),
                    len: bad.len() as u32
                },
                ptr::null_mut()
            ),
            INVALID_ARGUMENT,
            "非 UTF-8"
        );
    }
    // 调用后核站照常：请求仍走通 K1。
    let (sink, rx) = leak_sink();
    let req = request(PUBLIC, INTERACTIVE, "丢弃后", 4);
    start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert!(
        matches!(events.last(), Some(Ev::Complete { .. })),
        "{events:?}"
    );
    destroy(core);
}

// ---------------------------------------------------------------------------
// 调试面（madai-w10 I-C-09 J5-a；madai-w11 I-C-10 起两 profile 都导出——include/madinfer_debug.h 两符号，release 函数体回 4005）
// ---------------------------------------------------------------------------

/// release 构建（`cargo test --release -p madinfer-ffi --test cabi`）：两符号在、函数体门控恒 4005；形式错误仍先 4001。
#[cfg(not(debug_assertions))]
mod release_face {
    use super::*;
    use madinfer_ffi::cabi::debug::{madinfer_core_create_debug, madinfer_test_stop_control};

    const FEATURE_DISABLED: i32 = 4005;

    #[test]
    fn debug_symbols_return_4005_in_release_builds() {
        // release 构建建站须带受信公钥（否则 4003），与生产宿主同口径。
        let (host, _consents) = leak_host(false);
        let key_hex = leak_str("ab".repeat(32));
        let fixture = options(&["summarize"], None, &[("k1", key_hex)]);
        let vt = vtable(host);
        let mut error = -1;
        // SAFETY: vt / fixture.raw 在调用期间有效；error 可写。
        let core = unsafe { madinfer_core_create_debug(&vt, &fixture.raw, 1, &mut error) };
        assert!(core.is_null());
        assert_eq!(error, FEATURE_DISABLED);
        assert!(
            host.kv_put_keys.read().unwrap().is_empty(),
            "release 下 create_debug 不碰宿主"
        );
        // 形式错误先于门控：取值越界 / 空指针 → 4001。
        // SAFETY: 越界取值 / NULL 是本例目的；其余参数有效。
        unsafe {
            assert!(madinfer_core_create_debug(&vt, &fixture.raw, 2, &mut error).is_null());
            assert_eq!(error, INVALID_ARGUMENT);
            assert!(madinfer_core_create_debug(ptr::null(), &fixture.raw, 1, &mut error).is_null());
            assert_eq!(error, INVALID_ARGUMENT);
        }
        // 生产句柄：test_stop_control 恒 4005、不停线程（apply_config 仍有回执）；NULL 仍 4001。
        let core = create(host, &fixture);
        // SAFETY: core 有效；NULL 是被拒绝的输入。
        unsafe {
            assert_eq!(madinfer_test_stop_control(core), FEATURE_DISABLED);
            assert_eq!(
                madinfer_test_stop_control(ptr::null_mut()),
                INVALID_ARGUMENT
            );
        }
        let (rc, _) = apply(core, b"{");
        assert_eq!(rc, MADINFER_OK, "control 线程未被停");
        destroy(core);
    }
}

#[cfg(debug_assertions)]
mod debug_face {
    use super::*;
    use madinfer_ffi::cabi::debug::{madinfer_core_create_debug, madinfer_test_stop_control};

    const CORE_STOPPED: i32 = 4004;
    const SCHEMA: &str = r#"{"type":"object","properties":{"title":{"type":"string"},"items":{"type":"array","items":{"type":"integer"}}},"required":["title","items"]}"#;

    fn create_debug(host: &'static Host, fixture: &OptionsFixture, script: u8) -> *mut RawCore {
        let vt = vtable(host);
        let mut error = -1;
        // SAFETY: vt / fixture.raw 在调用期间有效；error 可写。
        let core = unsafe { madinfer_core_create_debug(&vt, &fixture.raw, script, &mut error) };
        assert!(!core.is_null(), "create_debug 失败：{error}");
        assert_eq!(error, MADINFER_OK);
        core
    }

    fn structured_with_prompt(prompt: &'static str) -> RequestFixture {
        let mut fixture = request(PUBLIC, INTERACTIVE, prompt, 16);
        fixture.raw.output_kind = 1;
        fixture.raw.schema_json = RawStr::borrow(SCHEMA);
        fixture
    }

    #[test]
    fn create_debug_with_script_backend_turns_prompt_into_backend_output() {
        // I-S-05 U-2 的 C 面：脚本后端回显 prompt——缺 required 的合法 JSON → Started → Failed 2003 携 partial（2003 之二）；
        // 齐全 JSON → Complete 携结构化输出；文本请求原文回显。
        let (host, _consents) = leak_host(false);
        let fixture = options(&["summarize"], None, &[]);
        let core = create_debug(host, &fixture, 1);

        let (sink, rx) = leak_sink();
        let req = structured_with_prompt(r#"{"title":"x"}"#);
        let handle = start(core, &req, sink);
        let events = collect_until_terminal(&rx);
        assert_eq!(started_count(&events), 1, "{events:?}");
        assert!(
            matches!(
                events.last(),
                Some(Ev::Failed { request, code: 2003, wire, partial: Some(partial), .. })
                    if *request == handle && wire == "schema_invalid" && partial == r#"{"title":"x"}"#
            ),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| matches!(e, Ev::Complete { .. })));
        assert_quiet(&rx);

        let (sink, rx) = leak_sink();
        let req = structured_with_prompt(r#"{"title":"x","items":[1,2]}"#);
        start(core, &req, sink);
        let events = collect_until_terminal(&rx);
        match events.last() {
            Some(Ev::Complete {
                output_kind,
                output,
                ..
            }) => {
                assert_eq!(*output_kind, 1, "结构化输出");
                let got: serde_json::Value = serde_json::from_str(output).unwrap();
                assert_eq!(got, serde_json::json!({"title": "x", "items": [1, 2]}));
            }
            other => panic!("终态应为 Complete：{other:?}"),
        }
        assert_quiet(&rx);

        let (sink, rx) = leak_sink();
        let req = request(PUBLIC, INTERACTIVE, "hello world", 16);
        start(core, &req, sink);
        let events = collect_until_terminal(&rx);
        assert!(
            matches!(events.last(), Some(Ev::Complete { text, .. }) if text == "hello world"),
            "{events:?}"
        );
        destroy(core);

        // script = 0 → 与 madinfer_core_create 同一装配路径（仿真核）；script 取值越界 → 4001 且不建站。
        let core = create_debug(host, &fixture, 0);
        destroy(core);
        let vt = vtable(host);
        let mut error = -1;
        // SAFETY: 取值越界是本例目的；其余参数有效。
        assert!(unsafe { madinfer_core_create_debug(&vt, &fixture.raw, 2, &mut error) }.is_null());
        assert_eq!(error, INVALID_ARGUMENT);
        // SAFETY: NULL 是被拒绝的输入。
        assert!(
            unsafe { madinfer_core_create_debug(ptr::null(), &fixture.raw, 1, &mut error) }
                .is_null()
        );
        assert_eq!(error, INVALID_ARGUMENT);
    }

    #[test]
    fn test_stop_control_turns_apply_config_into_4004_and_is_idempotent() {
        // I-S-05 U-4 的 C 面：生产建站的句柄也能停 control 线程——之后 madinfer_apply_config 恒 4004、stream 无回调、destroy 照常。
        let (host, _consents) = leak_host(false);
        let fixture = options(&["summarize"], None, &[]);
        let core = create(host, &fixture);
        let (rc, _) = apply(core, b"{");
        assert_eq!(rc, MADINFER_OK, "停前：有回执");

        // SAFETY: core 有效。
        assert_eq!(unsafe { madinfer_test_stop_control(core) }, MADINFER_OK);
        let (rc, _) = apply(core, b"{");
        assert_eq!(rc, CORE_STOPPED, "停后：4004");
        let (sink, rx) = leak_sink();
        let req = request(PUBLIC, INTERACTIVE, "hi", 4);
        start(core, &req, sink);
        assert_quiet(&rx);
        // SAFETY: core 有效；NULL 是被拒绝的输入。
        unsafe {
            assert_eq!(madinfer_test_stop_control(core), MADINFER_OK, "幂等");
            assert_eq!(
                madinfer_test_stop_control(ptr::null_mut()),
                INVALID_ARGUMENT
            );
        }
        let (rc, _) = apply(core, &[]);
        assert_eq!(rc, CORE_STOPPED);
        destroy(core);
    }

    #[test]
    fn test_stop_control_turns_drop_contexts_into_4004() {
        // madai-w14 I-C-12：第 10 符号与第 9 符号同一"等核回执"形态——control 线程退出后恒 4004、不挂、out 不写。
        let (host, _consents) = leak_host(false);
        let fixture = options(&["summarize"], None, &[]);
        let core = create(host, &fixture);
        assert_eq!(
            drop_contexts(core, "acct:a/"),
            (MADINFER_OK, 0),
            "停前：有回执"
        );
        // SAFETY: core 有效。
        assert_eq!(unsafe { madinfer_test_stop_control(core) }, MADINFER_OK);
        let (rc, dropped) = drop_contexts(core, "acct:a/");
        assert_eq!(rc, CORE_STOPPED, "停后：4004");
        assert_eq!(dropped, u32::MAX, "失败不写 out");
        destroy(core);
    }
}

#[test]
fn explicit_backend_c_api_reports_emulation_and_rejects_unknown_kind() {
    use madinfer_ffi::cabi::{madinfer_backend_kind, madinfer_core_create_with_backend};
    let (host, _) = leak_host(false);
    let opts = options(&["summarize"], None, &[]);
    let mut error = -1;
    // SAFETY: local fixtures remain live until destroy; null model is empty borrowed slice.
    unsafe {
        let core = madinfer_core_create_with_backend(
            &vtable(host),
            &opts.raw,
            0,
            RawStr::borrow(""),
            &mut error,
        );
        assert!(!core.is_null());
        assert_eq!(error, 0);
        let mut kind = 99;
        assert_eq!(madinfer_backend_kind(core, &mut kind), 0);
        assert_eq!(kind, 0);
        assert_eq!(madinfer_backend_kind(core, ptr::null_mut()), 4001);
        madinfer_core_destroy(core);
        let invalid = madinfer_core_create_with_backend(
            &vtable(host),
            &opts.raw,
            99,
            RawStr::borrow(""),
            &mut error,
        );
        assert!(invalid.is_null());
        assert_eq!(error, 4001);
    }
}

#[cfg(not(feature = "llama-sys"))]
#[test]
fn explicit_native_c_api_never_falls_back_without_native_feature() {
    use madinfer_ffi::cabi::madinfer_core_create_with_backend;
    let (host, _) = leak_host(false);
    let opts = options(&["summarize"], None, &[]);
    let mut error = -1;
    // SAFETY: live fixtures, borrowed path and writable error.
    let core = unsafe {
        madinfer_core_create_with_backend(
            &vtable(host),
            &opts.raw,
            1,
            RawStr::borrow("model.gguf"),
            &mut error,
        )
    };
    assert!(core.is_null());
    assert_eq!(error, 4005);
}

#[test]
fn destroy_drains_timed_out_host_verifier_before_returning() {
    let _serial = VERIFY_LIFETIME_TEST_LOCK.lock().unwrap();
    let (host, _) = leak_host(false);
    host.verify_blocking.store(true, Ordering::SeqCst);
    let fixture = options(&["summarize"], None, &[("k1", leak_str("ab".repeat(32)))]);
    let core = create(host, &fixture);
    let (_, outcome) = apply(core, &signed_config_doc(5, "k1"));
    assert_eq!(detail_of(&outcome), VERIFY_TIMEOUT_DETAIL);
    assert_eq!(host.verify_returned.load(Ordering::SeqCst), 0);
    let address = core as usize;
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        // SAFETY: this thread is the sole owner; no other core calls occur.
        unsafe {
            madinfer_core_destroy(address as *mut RawCore);
        }
        tx.send(()).unwrap();
    });
    let early_return = rx.recv_timeout(Duration::from_millis(100)).is_ok();
    host.verify_release.store(true, Ordering::SeqCst);
    if !early_return {
        rx.recv_timeout(WAIT)
            .expect("destroy drains after callback return");
    }
    worker.join().unwrap();
    assert!(
        !early_return,
        "destroy returned while callback still borrowed host user_data"
    );
    assert_eq!(host.verify_returned.load(Ordering::SeqCst), 1);
}

#[allow(clippy::disallowed_types)] // Test-only serialization of process-global diagnostic counters.
static VERIFY_LIFETIME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "llama-sys")]
#[test]
fn explicit_native_c_api_streams_real_model_and_reports_native_kind() {
    use madinfer_ffi::cabi::{madinfer_backend_kind, madinfer_core_create_with_backend};
    let model = std::env::var("MADINFER_LLAMA_MODEL").expect("native model required, no skip");
    let (host, _) = leak_host(false);
    let opts = options(&["summarize"], None, &[]);
    let mut error = -1;
    // SAFETY: live fixtures and model string; only this thread owns/destroys core.
    let core = unsafe {
        madinfer_core_create_with_backend(
            &vtable(host),
            &opts.raw,
            1,
            RawStr::borrow(&model),
            &mut error,
        )
    };
    assert!(!core.is_null(), "native create error {error}");
    let mut kind = 99;
    // SAFETY: live core and writable output.
    assert_eq!(unsafe { madinfer_backend_kind(core, &mut kind) }, 0);
    assert_eq!(kind, 1);
    let (sink, rx) = leak_sink();
    let req = request(PUBLIC, INTERACTIVE, "The capital of France is", 8);
    start(core, &req, sink);
    let events = collect_until_terminal(&rx);
    destroy(core);
    assert!(
        matches!(events.last(), Some(Ev::Complete { text, .. }) if text.starts_with(" Paris.")),
        "{events:?}"
    );
}
