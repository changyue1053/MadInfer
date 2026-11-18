//! 核 actor 端到端：FakeBackend / NoopBackend + 映射表 + MonotonicClock，走 `Core::builder().spawn()` → `stream` → 事件。
//! 断言 K1 顺序保证（Started 恰一次且在首个 Delta 前 → Delta* → 终态恰一次 → 之后无事件）、取消、回退、授权、隐私门。

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use madinfer_api::{
    Core, CoreBuilder, CoreStats, Event, EventSink, GenRequest, Output, OutputSpec,
};
use madinfer_backend::{BackendCaps, BackendError, ConstraintSupport, GenerateOptions, Turn};
use madinfer_core_types::SlotClass;
use madinfer_core_types::{
    BackendId, Budget, BusinessId, CapabilityId, ContextKey, Os, PackageRef, PrivacyTag, QoS,
    RequestId, Source, Tier,
};
use madinfer_host::{ConsentDecision, MonotonicClock};
use madinfer_router::{
    Candidate, Conditions, EndpointKind, MappingEntry, MappingTable, RouteReason, Target,
};
use madinfer_sched::SlotTable;
use madinfer_testkit::{
    default_caps, FakeBackend, FakeScript, NoopBackend, RecordingTelemetrySink, ScriptedAuthorizer,
};

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);

struct ChannelSink(Sender<Event>);

impl EventSink for ChannelSink {
    fn on_event(&mut self, event: Event) {
        let _ = self.0.send(event);
    }
}

fn cap() -> CapabilityId {
    CapabilityId::new("summarize").expect("id")
}

fn pkg() -> PackageRef {
    PackageRef::parse("catchup-summary@1.4.2").expect("pkg")
}

fn bundled(backend: &str, priority: u8) -> Candidate {
    Candidate {
        priority,
        target: Target::Bundled {
            backend: BackendId(backend.to_owned()),
            package: pkg(),
        },
        when: Conditions {
            os: None,
            tier_min: Some(Tier::Mid),
            requires_available: true,
        },
    }
}

fn system(backend: &str, priority: u8) -> Candidate {
    Candidate {
        priority,
        target: Target::System {
            backend: BackendId(backend.to_owned()),
        },
        when: Conditions {
            requires_available: true,
            ..Conditions::default()
        },
    }
}

fn cloud(endpoint: &str, priority: u8) -> Candidate {
    Candidate {
        priority,
        target: Target::Cloud {
            endpoint: endpoint.to_owned(),
            kind: EndpointKind::Enterprise,
        },
        when: Conditions {
            requires_available: true,
            ..Conditions::default()
        },
    }
}

fn mapping(candidates: Vec<Candidate>) -> MappingTable {
    MappingTable {
        entries: vec![MappingEntry {
            capability: cap(),
            candidates,
        }],
    }
}

fn request(privacy: PrivacyTag) -> GenRequest {
    GenRequest {
        capability: cap(),
        business: BusinessId::new(7).expect("b"),
        privacy,
        budget: Budget::new(5_000, QoS::Interactive),
        turn: Turn::user("总结一下今天的群消息"),
        options: GenerateOptions::default(),
        output: OutputSpec::Text,
        constraint: None,
        context: None,
        trace: None,
    }
}

fn builder(telemetry: &Arc<RecordingTelemetrySink>) -> CoreBuilder {
    Core::builder()
        .clock(Arc::new(MonotonicClock::new()))
        .telemetry_sink(Arc::clone(telemetry) as Arc<dyn madinfer_host::TelemetrySink>)
        .tier(Tier::Flagship)
        .os(Os::Ios)
        .installed_package(pkg())
        .slots(SlotTable::new([
            (SlotClass::Cpu, 1),
            (SlotClass::External(0), 1),
            (SlotClass::External(1), 1),
        ]))
}

fn collect_until_terminal(rx: &Receiver<Event>) -> Vec<Event> {
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

/// 终态后不得再有事件：sink 在终态投递后被 Drop（发送端随之断开），或至少在安静期内无事件。
fn assert_quiet(rx: &Receiver<Event>) {
    match rx.recv_timeout(QUIET) {
        Err(RecvTimeoutError::Disconnected) | Err(RecvTimeoutError::Timeout) => {}
        Ok(event) => panic!("终态后仍有事件：{event:?}"),
    }
}

fn started(events: &[Event]) -> Vec<&Event> {
    events
        .iter()
        .filter(|e| matches!(e, Event::Started { .. }))
        .collect()
}

fn delta_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Delta(d) => Some(d.text.as_str()),
            _ => None,
        })
        .collect()
}

fn telemetry_types(sink: &RecordingTelemetrySink) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    for batch in sink.batches() {
        let doc: serde_json::Value = serde_json::from_str(&batch).expect("batch json");
        assert_eq!(doc["schema"], 1);
        for e in doc["events"].as_array().expect("events") {
            out.push((e["type"].as_str().expect("type").to_owned(), e.clone()));
        }
    }
    out
}

fn shutdown(handle: madinfer_api::CoreHandle) -> CoreStats {
    let stats = handle.shutdown().expect("core back");
    assert_eq!(stats.request_fsm_table_size, 20);
    assert_eq!(stats.unexpected_request_events, 0, "请求 FSM 无非法事件");
    stats
}

#[test]
fn stream_emits_started_once_before_first_delta_then_deltas_then_complete() {
    let script = ["会", "议", "纪", "要", "：", "三", "件", "事", "。", "完"];
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&script)));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        // 云端候选被 E2EE 隐私门剔除 → 原因 privacy_forced_local；链里只剩 bundled。
        .mapping(mapping(vec![
            bundled("fake", 0),
            cloud("cloud.enterprise", 1),
        ]))
        .spawn()
        .expect("spawn");
    let (tx, rx) = mpsc::channel();
    let id = core.stream(
        request(PrivacyTag::E2eePlaintext),
        Box::new(ChannelSink(tx)),
    );
    assert_eq!(id, RequestId(1));

    let events = collect_until_terminal(&rx);
    assert!(events.iter().all(|e| e.request() == id));
    assert_eq!(started(&events).len(), 1, "Started 恰一次：{events:?}");
    assert!(
        matches!(
            events[0],
            Event::Started {
                source: Source::Bundled,
                route_reason: RouteReason::PrivacyForcedLocal,
                ..
            }
        ),
        "Started 在首个 Delta 前且来源 / 原因正确：{:?}",
        events[0]
    );
    if let Event::Started { package, .. } = &events[0] {
        assert_eq!(package.as_ref(), Some(&pkg()));
    }
    assert_eq!(delta_text(&events), script.concat(), "Delta 拼接 == 脚本");
    let deltas: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            Event::Delta(d) => Some(d.seq),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        (0..u32::try_from(deltas.len()).expect("len")).collect::<Vec<_>>(),
        "seq 从 0 递增"
    );
    match events.last() {
        Some(Event::Complete { output, stats, .. }) => {
            assert_eq!(*output, Output::Text(script.concat()));
            assert_eq!(stats.gen_tokens, 10);
            assert_eq!(stats.source, Some(Source::Bundled));
            assert_eq!(stats.fallback_steps, 0);
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Complete { .. }))
            .count(),
        1
    );
    assert_quiet(&rx);

    let stats = shutdown(core);
    assert_eq!(stats.sched.done, 1, "调度台账 done=1");
    assert_eq!(stats.sched.failed, 0);
    assert_eq!(
        (stats.requests_accepted, stats.completed, stats.failed),
        (1, 1, 0)
    );
    assert_eq!(stats.models.loads, 1);
    assert_eq!(fake.generate_calls(), 1);
    assert_eq!(stats.framer.offered, 10);

    let types = telemetry_types(&telemetry);
    let names: BTreeSet<&str> = types.iter().map(|(t, _)| t.as_str()).collect();
    assert!(names.contains("request_start"), "{names:?}");
    assert!(names.contains("request_end"), "{names:?}");
    assert!(names.contains("route_decision"), "{names:?}");
    let end = &types
        .iter()
        .find(|(t, _)| t == "request_end")
        .expect("request_end")
        .1;
    assert_eq!(end["outcome"], "complete");
    assert_eq!(end["gen_tokens"], 10);
    assert_eq!(end["backend"], "fake");
    assert_eq!(end["package"], "catchup-summary@1.4.2");
    assert_eq!(
        end["privacy"],
        serde_json::Value::Null,
        "request_end 无隐私字段"
    );
    for (_, e) in &types {
        for (k, v) in e.as_object().expect("obj") {
            if let Some(s) = v.as_str() {
                assert!(!s.contains(char::is_whitespace), "{k} 含空白：{s}");
                assert!(!s.contains('会'), "用户内容进了观测：{k}={s}");
            }
        }
    }
}

// ---------------------------------------------------------------------- G4 结构化输出（madai-w08 I-C-07）

const MEETING_SCHEMA: &str = r#"{"type":"object","properties":{"title":{"type":"string"},"items":{"type":"array","items":{"type":"integer"}},"done":{"type":"boolean"}},"required":["title","items"]}"#;

fn structured_request(schema_json: &str) -> GenRequest {
    GenRequest {
        output: OutputSpec::Structured {
            schema_json: schema_json.to_owned(),
        },
        ..request(PrivacyTag::Public)
    }
}

fn constrained_fake(id: &str, script: &[&str]) -> Arc<FakeBackend> {
    Arc::new(
        FakeBackend::new(id, FakeScript::tokens(script).with_token_delay_ms(1)).with_caps(
            BackendCaps {
                constraint: ConstraintSupport::Native,
                ..default_caps()
            },
        ),
    )
}

/// 片间隔超过合帧窗口（80 ms）→ 每片独立成帧，逐帧携带 patch。
fn slow_constrained_fake(id: &str, script: &[&str]) -> Arc<FakeBackend> {
    Arc::new(
        FakeBackend::new(id, FakeScript::tokens(script).with_token_delay_ms(100)).with_caps(
            BackendCaps {
                constraint: ConstraintSupport::Native,
                ..default_caps()
            },
        ),
    )
}

/// 参考应用器（子集口径同 KMP CatchupPatchDocument）：op 逐条应用到文档。
fn apply_patch(doc: &mut serde_json::Value, ops: &[String]) {
    for text in ops {
        let op: serde_json::Value = serde_json::from_str(text).expect("op json");
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
fn structured_request_streams_patches_that_replay_to_the_complete_json() {
    let script = [
        "{\"ti",
        "tle\":\"会议\",",
        "\"items\":[1",
        ",2",
        ",3],",
        "\"done\"",
        ":true}",
    ];
    let fake = slow_constrained_fake("fake", &script);
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");
    let (tx, rx) = mpsc::channel();
    let id = core.stream(
        structured_request(MEETING_SCHEMA),
        Box::new(ChannelSink(tx)),
    );
    let events = collect_until_terminal(&rx);
    assert_eq!(started(&events).len(), 1, "{events:?}");
    assert!(matches!(events[0], Event::Started { .. }));
    let mut doc = serde_json::json!({});
    let mut seqs = Vec::new();
    let mut patched_frames = 0;
    for event in &events {
        if let Event::Delta(d) = event {
            seqs.push(d.seq);
            if !d.patch.is_empty() {
                patched_frames += 1;
            }
            apply_patch(&mut doc, &d.patch);
        }
    }
    assert_eq!(
        seqs,
        (0..u32::try_from(seqs.len()).expect("len")).collect::<Vec<_>>(),
        "seq 连续"
    );
    assert_eq!(
        seqs.len(),
        script.len(),
        "片间隔超窗口 → 一片一帧：{events:?}"
    );
    // "{\"ti" 与 "\"done\"" 两片没有完整前缀增量，其余五帧各携 patch。
    assert_eq!(patched_frames, 5, "{events:?}");
    match events.last() {
        Some(Event::Complete { output, .. }) => {
            let Output::Structured(json) = output else {
                panic!("Complete 应为 Structured：{output:?}");
            };
            assert_eq!(json, r#"{"done":true,"items":[1,2,3],"title":"会议"}"#);
            assert_eq!(
                doc,
                serde_json::from_str::<serde_json::Value>(json).expect("json"),
                "逐帧 patch 应用后 == Complete 的 json"
            );
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_eq!(delta_text(&events), script.concat(), "Delta.text 仍是原文");
    assert_quiet(&rx);
    let stats = shutdown(core);
    assert_eq!(
        (stats.completed, stats.failed, stats.structured_rejected),
        (1, 0, 0)
    );
    assert_eq!(fake.generate_calls(), 1);
    let _ = id;
}

#[test]
fn invalid_schema_fails_2003_before_started_and_text_requests_are_unaffected() {
    let fake = constrained_fake("fake", &["{}"]);
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");
    // 非法 schema（anyOf 空分支）→ 2003，且无 Started、后端未被调用。
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_request(r#"{"anyOf":[]}"#),
        Box::new(ChannelSink(tx)),
    );
    let events = collect_until_terminal(&rx);
    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0] {
        Event::Failed { error, partial, .. } => {
            assert_eq!(error.code(), 2003);
            assert_eq!(*partial, None);
        }
        other => panic!("应直接 Failed：{other:?}"),
    }
    assert_quiet(&rx);
    assert_eq!(fake.generate_calls(), 0);
    // schema 不是 JSON 也一样。
    let (tx, rx) = mpsc::channel();
    core.stream(structured_request("not json"), Box::new(ChannelSink(tx)));
    let events = collect_until_terminal(&rx);
    assert!(
        matches!(&events[0], Event::Failed { error, .. } if error.code() == 2003),
        "{events:?}"
    );
    // Text 请求不受影响：Complete 仍是 Text，Delta 无 patch。
    let (tx, rx) = mpsc::channel();
    core.stream(request(PrivacyTag::Public), Box::new(ChannelSink(tx)));
    let events = collect_until_terminal(&rx);
    assert_eq!(started(&events).len(), 1);
    assert!(events
        .iter()
        .all(|e| !matches!(e, Event::Delta(d) if !d.patch.is_empty())));
    assert!(matches!(
        events.last(),
        Some(Event::Complete {
            output: Output::Text(_),
            ..
        })
    ));
    let stats = shutdown(core);
    assert_eq!(
        (stats.requests_accepted, stats.completed, stats.failed),
        (3, 1, 2)
    );
    assert_eq!(stats.structured_rejected, 0, "受理期拒绝不计终态校验");
}

#[test]
fn structured_output_failing_final_validation_is_2003_with_partial() {
    // 后端产出合法 JSON 但缺 required 的 items → 终态校验不过：Failed 2003 携 partial，无 Complete。
    let fake = constrained_fake("fake", &["{\"title\":", "\"x\"}"]);
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_request(MEETING_SCHEMA),
        Box::new(ChannelSink(tx)),
    );
    let events = collect_until_terminal(&rx);
    assert_eq!(started(&events).len(), 1);
    match events.last() {
        Some(Event::Failed { error, partial, .. }) => {
            assert_eq!(error.code(), 2003);
            assert_eq!(*partial, Some(Output::Text("{\"title\":\"x\"}".to_owned())));
        }
        other => panic!("应 Failed 2003：{other:?}"),
    }
    assert!(!events.iter().any(|e| matches!(e, Event::Complete { .. })));
    assert_quiet(&rx);
    let stats = shutdown(core);
    assert_eq!(
        (stats.completed, stats.failed, stats.structured_rejected),
        (0, 1, 1)
    );
}

#[test]
fn cancel_mid_stream_ends_with_single_failed_3008_and_backend_sees_cancel() {
    let script: Vec<String> = (0..200).map(|i| format!("t{i} ")).collect();
    let fake = Arc::new(FakeBackend::new(
        "fake",
        FakeScript::tokens(&script).with_token_delay_ms(2),
    ));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");
    let (tx, rx) = mpsc::channel();
    let id = core.stream(request(PrivacyTag::Public), Box::new(ChannelSink(tx)));

    let first = rx.recv_timeout(WAIT).expect("Started");
    assert!(matches!(first, Event::Started { .. }));
    let second = rx.recv_timeout(WAIT).expect("首个 Delta");
    assert!(matches!(second, Event::Delta(_)));
    core.cancel(id);

    let mut events = vec![first, second];
    events.extend(collect_until_terminal(&rx));
    let terminals: Vec<&Event> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminals.len(), 1);
    match terminals[0] {
        Event::Failed { error, partial, .. } => {
            assert_eq!(error.code(), 3008);
            assert!(
                matches!(partial, Some(Output::Text(t)) if !t.is_empty()),
                "已出 Delta，partial 随 Failed 返回"
            );
        }
        other => panic!("终态应为 Failed(3008)：{other:?}"),
    }
    assert_eq!(started(&events).len(), 1);
    assert_quiet(&rx);

    let stats = shutdown(core);
    assert_eq!(fake.cancelled_count(), 1, "后端在下一 token 边界看到取消");
    assert!(
        fake.ledger()
            .tokens_emitted
            .load(std::sync::atomic::Ordering::SeqCst)
            < 200,
        "没有把脚本跑完"
    );
    assert_eq!(stats.sched.cancelled, 1, "调度台账：Job 以 Cancelled 终态");
    assert_eq!((stats.failed, stats.cancelled, stats.completed), (1, 1, 0));
    let types = telemetry_types(&telemetry);
    let end = &types
        .iter()
        .find(|(t, _)| t == "request_end")
        .expect("request_end")
        .1;
    assert_eq!(end["outcome"], "cancelled");
    assert_eq!(end["code"], 3008);
}

#[test]
fn first_target_failing_before_any_delta_falls_back_and_started_names_second_target() {
    let bad = Arc::new(FakeBackend::new(
        "fake.bad",
        FakeScript::tokens(&["never"]).failing_at(0, BackendError::ModelCorrupt),
    ));
    let noop = Arc::new(NoopBackend::new("noop"));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&bad) as Arc<dyn madinfer_backend::Backend>)
        .backend(noop as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake.bad", 0), system("noop", 1)]))
        .spawn()
        .expect("spawn");
    let (tx, rx) = mpsc::channel();
    let id = core.stream(request(PrivacyTag::Internal), Box::new(ChannelSink(tx)));

    let events = collect_until_terminal(&rx);
    assert_eq!(
        started(&events).len(),
        1,
        "回退不产生第二个 Started：{events:?}"
    );
    assert!(
        matches!(
            events[0],
            Event::Started {
                source: Source::SystemModel,
                route_reason: RouteReason::Fallback,
                package: None,
                ..
            }
        ),
        "Started 的 source 是第二目标、原因为 Fallback：{:?}",
        events[0]
    );
    match events.last() {
        Some(Event::Complete { stats, .. }) => {
            assert_eq!(stats.fallback_steps, 1);
            assert_eq!(stats.source, Some(Source::SystemModel));
            assert_eq!(stats.gen_tokens, 0);
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_eq!(events.len(), 2, "无 Delta：Started 紧贴 Complete");
    assert!(events.iter().all(|e| e.request() == id));
    assert_quiet(&rx);

    let stats = shutdown(core);
    assert_eq!(stats.fallbacks, 1);
    assert_eq!(
        (stats.sched.failed, stats.sched.done),
        (1, 1),
        "两个 Job：一败一成"
    );
    assert_eq!(bad.generate_calls(), 1);
    let types = telemetry_types(&telemetry);
    let end = &types
        .iter()
        .find(|(t, _)| t == "request_end")
        .expect("request_end")
        .1;
    assert_eq!(end["fallback_steps"], 1);
    assert_eq!(end["source"], "system_model");
    assert_eq!(end["backend"], "noop");
}

#[test]
fn internal_privacy_to_enterprise_cloud_asks_consent_once_and_denial_is_3007() {
    for (decision, expect_complete) in [
        (ConsentDecision::Allow, true),
        (ConsentDecision::Deny, false),
    ] {
        let authorizer = Arc::new(ScriptedAuthorizer::answering(decision));
        let telemetry = Arc::new(RecordingTelemetrySink::new());
        let core = builder(&telemetry)
            .authorizer(Arc::clone(&authorizer) as Arc<dyn madinfer_host::Authorizer>)
            .backend(Arc::new(NoopBackend::new("cloud.enterprise"))
                as Arc<dyn madinfer_backend::Backend>)
            .mapping(mapping(vec![cloud("cloud.enterprise", 0)]))
            .spawn()
            .expect("spawn");
        let (tx, rx) = mpsc::channel();
        core.stream(request(PrivacyTag::Internal), Box::new(ChannelSink(tx)));
        let events = collect_until_terminal(&rx);
        assert_eq!(authorizer.requests().len(), 1, "授权恰问一次");
        assert_eq!(authorizer.requests()[0].endpoint_id, "cloud.enterprise");
        if expect_complete {
            assert!(matches!(
                events[0],
                Event::Started {
                    source: Source::Cloud,
                    ..
                }
            ));
            assert!(matches!(events.last(), Some(Event::Complete { .. })));
        } else {
            assert_eq!(events.len(), 1, "拒绝：只有 Failed，无 Started");
            assert!(matches!(&events[0], Event::Failed { error, .. } if error.code() == 3007));
        }
        assert_quiet(&rx);
        let stats = shutdown(core);
        assert_eq!(stats.sched.submitted, u64::from(expect_complete));
        let types = telemetry_types(&telemetry);
        assert!(types.iter().any(|(t, _)| t == "consent"));
    }
}

#[test]
fn e2ee_request_with_cloud_only_chain_fails_at_route_with_3006_and_no_job() {
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(
            Arc::new(NoopBackend::new("cloud.enterprise")) as Arc<dyn madinfer_backend::Backend>
        )
        .mapping(mapping(vec![cloud("cloud.enterprise", 0)]))
        .spawn()
        .expect("spawn");
    let (tx, rx) = mpsc::channel();
    let id = core.stream(
        request(PrivacyTag::E2eePlaintext),
        Box::new(ChannelSink(tx)),
    );
    let events = collect_until_terminal(&rx);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], Event::Failed { request, error, partial: None } if *request == id && error.code() == 3006),
        "{events:?}"
    );
    assert_quiet(&rx);
    let stats = shutdown(core);
    assert_eq!(stats.sched.submitted, 0, "隐私门在路由层拦下，不进调度器");
    assert_eq!(stats.failed, 1);
}

/// config.v1 最小合法文档：只映射 summarize → bundled fake。签名字段随调试构建 TrustAll 放行。
fn config_doc(version: u64) -> Vec<u8> {
    config_doc_with_flagship_budget(version, 512)
}

/// 同 [`config_doc`]，flagship 档 KV 预算可指定（madai-w16 首日 K3：显式 0 = 关池，用来钉"预算不足按无 KV 模式跑"）。
fn config_doc_with_flagship_budget(version: u64, flagship_kv_mb: u32) -> Vec<u8> {
    format!(
        r#"{{
  "schema": 1, "version": {version}, "issued_at": "2026-09-05T02:00:00Z", "min_sdk": ">=0.1.0",
  "rollout": {{ "buckets": [0, 999] }},
  "mapping": [
    {{ "capability": "summarize",
      "candidates": [
        {{ "priority": 0, "target": {{ "kind": "bundled", "backend": "fake", "package": "catchup-summary@1.4.2" }}, "when": {{ "tier_min": "low" }} }}
      ] }}
  ],
  "thresholds": {{
    "deadline_ms": {{ "interactive": 3000, "async": 20000, "background": 120000 }},
    "frame_ms": 80,
    "kv_budget_mb": {{ "flagship": {flagship_kv_mb}, "mid": 256, "low": 0 }},
    "quotas": {{}},
    "thermal": {{ "background_max": "fair" }},
    "gc_keep_days": 7
  }},
  "switches": {{ "capabilities": {{}}, "cloud_only": false, "all_off": false }},
  "signature": {{ "alg": "ed25519", "key_id": "pk-platform-2026-01", "sig": "c2ln" }}
}}"#
    )
    .into_bytes()
}

/// 只认一把 key 且要求 sig == 载荷前 64 字节的替身验签器（与 madinfer-ffi tests 同口径）。
struct FixedKeyVerifier;

impl madinfer_config::SignatureVerifier for FixedKeyVerifier {
    fn knows_key(&self, key_id: &str) -> bool {
        key_id == "pk-platform-2026-01"
    }
    fn verify(
        &self,
        _payload: &[u8],
        _signature: &madinfer_config::Signature,
    ) -> madinfer_config::VerifyOutcome {
        madinfer_config::VerifyOutcome::Rejected
    }
}

#[test]
fn apply_config_returns_outcome_and_switches_active_version() {
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&["ok"])));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");

    let report = core.apply_config(config_doc(7)).expect("control 线程在");
    assert_eq!(
        report.outcome,
        madinfer_config::ApplyOutcome::Applied { version: 7 },
        "{report:?}"
    );
    assert_eq!(report.active_version, 7);

    // 同版本再来一次：版本不递增 → 拒绝，活动版本不动。
    let again = core.apply_config(config_doc(7)).expect("control 线程在");
    assert!(
        matches!(
            &again.outcome,
            madinfer_config::ApplyOutcome::Rejected {
                reason: madinfer_config::RejectReason::VersionNotNewer {
                    received: 7,
                    active: 7
                },
                ..
            }
        ),
        "{again:?}"
    );
    assert_eq!(again.active_version, 7);

    // 解析都过不了的字节：SchemaInvalid，活动版本不动。
    let junk = core
        .apply_config(b"not json".to_vec())
        .expect("control 线程在");
    assert!(
        matches!(
            &junk.outcome,
            madinfer_config::ApplyOutcome::Rejected {
                reason: madinfer_config::RejectReason::SchemaInvalid(_),
                ..
            }
        ),
        "{junk:?}"
    );
    assert_eq!(junk.active_version, 7);

    // 新配置生效后请求仍走通 K1（映射表被原子替换为文档里的 fake 候选）。
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyTag::E2eePlaintext),
        Box::new(ChannelSink(tx)),
    );
    let events = collect_until_terminal(&rx);
    assert!(
        matches!(events.last(), Some(Event::Complete { .. })),
        "{events:?}"
    );
    shutdown(core);

    let types: Vec<String> = telemetry_types(&telemetry)
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    assert!(types.iter().any(|t| t == "config_apply"), "{types:?}");
    assert!(types.iter().any(|t| t == "config_rejected"), "{types:?}");
}

#[test]
fn apply_config_rejects_when_verifier_says_no() {
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&["ok"])));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .verifier(Box::new(FixedKeyVerifier))
        .spawn()
        .expect("spawn");

    let report = core.apply_config(config_doc(3)).expect("control 线程在");
    assert!(
        matches!(
            &report.outcome,
            madinfer_config::ApplyOutcome::Rejected {
                reason: madinfer_config::RejectReason::SignatureInvalid { timed_out: false },
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.active_version, 0, "拒绝不改活动版本");
    shutdown(core);
}

#[cfg(debug_assertions)]
#[test]
fn stop_control_leaves_handle_alive_and_turns_reply_entries_into_none() {
    // madai-w10 I-C-09 J5-a（I-S-05 U-4）：调试入口复现"control 线程已退出、句柄仍在"的故障态——之后需回执的入口恒 None
    // （ffi 映射 4004），流式请求静默无回调；幂等；句柄仍可正常 shutdown / Drop。
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&["ok"])));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");
    assert!(
        core.apply_config(b"{".to_vec()).is_some(),
        "停前：非 JSON 也有回执（Rejected）"
    );

    core.stop_control();
    assert_eq!(core.apply_config(b"{".to_vec()), None, "停后：恒 None");
    let (tx, rx) = mpsc::channel();
    core.stream(request(PrivacyTag::Public), Box::new(ChannelSink(tx)));
    assert!(
        matches!(
            rx.recv_timeout(QUIET),
            Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout)
        ),
        "停后 stream 无任何回调"
    );
    core.stop_control();
    assert_eq!(core.apply_config(Vec::new()), None, "幂等");
    let stats = core.shutdown().expect("线程已退出也能收回台账");
    assert_eq!(stats.requests_accepted, 0, "停后的请求未被受理");
}

#[test]
fn drop_contexts_replies_from_control_thread_and_is_none_after_stop() {
    // madai-w14 I-C-12：DropContexts 经邮箱在 control 线程调 ContextPool::drop_prefix、同步回条数。没跑过带 context 键的请求
    // → 池空 → 0；空前缀 0 且不动池；停后 None（ffi 4004）；台账 contexts.dropped 随 shutdown 收回。（接线后的全链见
    // context_pool_follows_job_lifecycle_…）
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&["ok"])));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");
    assert_eq!(
        core.drop_contexts("acct:a/".to_owned()),
        Some(0),
        "无预留方：0"
    );
    assert_eq!(core.drop_contexts(String::new()), Some(0), "空前缀：0");
    #[cfg(debug_assertions)]
    {
        core.stop_control();
        assert_eq!(
            core.drop_contexts("acct:a/".to_owned()),
            None,
            "停后：None（ffi 4004）"
        );
    }
    let stats = core.shutdown().expect("台账");
    assert_eq!(stats.contexts.dropped, 0);
}

fn keyed_request(context: &str) -> GenRequest {
    let mut request = request(PrivacyTag::Public);
    request.context = Some(ContextKey::new(context).expect("context 键 1～128 字节"));
    request
}

/// 跑完一轮带 `context` 键的请求（K1 到 Complete），返回终态后的台账快照。
fn run_keyed(core: &madinfer_api::CoreHandle, context: &str) -> CoreStats {
    let (tx, rx) = mpsc::channel();
    core.stream(keyed_request(context), Box::new(ChannelSink(tx)));
    let events = collect_until_terminal(&rx);
    assert!(
        matches!(events.last(), Some(Event::Complete { .. })),
        "{context}: {events:?}"
    );
    core.stats().expect("control 线程在")
}

#[test]
fn context_pool_follows_job_lifecycle_budget_from_config_and_drop_prefix_breaks_reuse() {
    // madai-w15 I-C-13：ContextPool 接进管线——T6 核侧全链（运行时契约设计稿 §5 T6 / R-8）：
    // 同键两次请求 hits +1 → 关户 drop_prefix → 同键必是新建、hits 不增、dropped ≥ 1；别的账户不受影响。
    // 预算取 config thresholds.kv_budget_mb[tier]（本站 Flagship → 512）；覆盖位 100 MB/上下文：v6 显式 flagship 0 → OverBudget 按无 KV 模式跑，
    // 配置后逐键记账，第 6 个空闲键触发 LRU 腾预算。madai-w16 I-C-14：后端席位 8 让 worker 能同时驻留多个上下文——池条目数 =
    // worker 实际驻留句柄数（testkit contexts_live），同键第二轮不再 open_context，腾出 / 丢弃的键在后端侧真被 Drop。
    let fake = Arc::new(
        FakeBackend::new("fake", FakeScript::tokens(&["ok"])).with_caps(BackendCaps {
            concurrency: 8,
            ..default_caps()
        }),
    );
    let ledger = fake.ledger();
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .context_kv_mb(100)
        .spawn()
        .expect("spawn");

    // 未配置该档：本站 Flagship 取内置缺省 128（madai-w16 首日 K3，DEFAULT_KV_BUDGET_MB）——生产站不依赖配置到达才有有界复用。
    assert_eq!(
        core.stats().expect("在").context_kv_budget_mb,
        128,
        "未配置该档 → 内置缺省 DEFAULT_KV_BUDGET_MB[flagship]"
    );
    // 显式 0 = 本档关池：v6 配置 flagship 0 → 预算 0；100 > 0 → OverBudget，请求照常完成、不进池、不驻留（逐轮开关）。
    assert_eq!(
        core.apply_config(config_doc_with_flagship_budget(6, 0))
            .expect("control 线程在")
            .outcome,
        madinfer_config::ApplyOutcome::Applied { version: 6 }
    );
    let before = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(before.context_kv_budget_mb, 0, "显式 0 = 关池");
    assert_eq!(before.contexts.rejected, 1, "预算不足按无 KV 模式跑");
    assert_eq!(before.context_entries, 0);
    assert_eq!(before.context_kv_mb, 0);
    assert_eq!(before.completed, 1, "无 KV 模式不影响请求终态");
    assert_eq!(
        (
            ledger.contexts_opened.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (1, 0),
        "无预留：开 1 次、轮末即 Drop"
    );

    // 应用配置：flagship 512 → 预算换档。
    let report = core.apply_config(config_doc(7)).expect("control 线程在");
    assert_eq!(
        report.outcome,
        madinfer_config::ApplyOutcome::Applied { version: 7 }
    );
    assert_eq!(
        core.stats().expect("在").context_kv_budget_mb,
        512,
        "thresholds.kv_budget_mb[flagship]"
    );

    // 同键两次：新建 → 复用（hits +1）；终态后条目置闲、仍在池（KV 保留）——worker 驻留句柄，第二轮不再 open_context。
    let first = run_keyed(&core, "acct:a/conv:1");
    assert_eq!((first.contexts.misses, first.contexts.hits), (1, 0));
    assert_eq!((first.context_entries, first.context_kv_mb), (1, 100));
    assert_eq!(first.inflight_jobs, 0, "终态 release 后不再在飞");
    assert_eq!(ledger.contexts_live(), 1, "有预留：轮末仍驻留");
    let second = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(second.contexts.hits, 1, "同键复用");
    assert_eq!((second.context_entries, second.context_kv_mb), (1, 100));
    assert_eq!(
        ledger.open_context_calls.load(Ordering::SeqCst),
        2,
        "同键第二轮复用驻留句柄：后端 open_context 只多 0 次（1 次是配置前的无预留轮）"
    );
    let other = run_keyed(&core, "acct:b/conv:1");
    assert_eq!((other.contexts.misses, other.contexts.hits), (2, 1));
    assert_eq!((other.context_entries, other.context_kv_mb), (2, 200));
    assert_eq!(ledger.contexts_live(), 2, "池条目 = worker 实际驻留");

    // 关户 A：drop_prefix 回 1、dropped 1、A 的条目离池（空闲 → KV 立即结清）；回键经广播让 worker Drop 句柄。
    assert_eq!(core.drop_contexts("acct:a/".to_owned()), Some(1));
    let dropped = core.stats().expect("在");
    assert_eq!(dropped.contexts.dropped, 1);
    assert_eq!((dropped.context_entries, dropped.context_kv_mb), (1, 100));

    // B 以 A 的会话 id 进来：必是新建（hits 不增）——worker 邮箱 FIFO：先 Drop 旧 A 句柄、再为新 A 开一个；B 自己的键仍命中。
    let reissued = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(reissued.contexts.hits, 1, "drop 后同键不复用");
    assert_eq!(reissued.contexts.misses, 3);
    assert_eq!((reissued.context_entries, reissued.context_kv_mb), (2, 200));
    assert_eq!(
        (
            ledger.contexts_dropped.load(Ordering::SeqCst),
            ledger.open_context_calls.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (2, 4, 2),
        "关户键在后端侧真释放（配置前 1 + A 1），新 A 重新打开"
    );
    let b_again = run_keyed(&core, "acct:b/conv:1");
    assert_eq!(b_again.contexts.hits, 2, "别的账户不受影响");

    // 预算：再来 3 个不同键到 5 条 / 500 MB；第 6 个键须腾 LRU 空闲条目（evicted_for_budget 1），条目数与已用不涨，
    // 被腾的键在后端侧 Drop（worker 先收回键再收工单）。
    for context in ["acct:c/conv:1", "acct:d/conv:1", "acct:e/conv:1"] {
        run_keyed(&core, context);
    }
    let full = core.stats().expect("在");
    assert_eq!((full.context_entries, full.context_kv_mb), (5, 500));
    assert_eq!(full.contexts.evicted_for_budget, 0);
    assert_eq!(ledger.contexts_live(), 5);
    let evicting = run_keyed(&core, "acct:f/conv:1");
    assert_eq!(evicting.contexts.evicted_for_budget, 1, "腾 LRU 空闲上下文");
    assert_eq!((evicting.context_entries, evicting.context_kv_mb), (5, 500));
    assert!(
        evicting.context_kv_mb <= evicting.context_kv_budget_mb,
        "不变量：已用 ≤ 预算"
    );
    assert_eq!(
        (
            ledger.contexts_dropped.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (3, 5),
        "被腾的键后端侧 Drop，驻留数仍 = 池条目"
    );

    let stats = shutdown(core);
    assert_eq!(stats.contexts.hits, 2);
    assert_eq!(stats.contexts.dropped, 1);
    assert_eq!(stats.contexts.rejected, 1);
    assert_eq!(stats.contexts.evicted_for_budget, 1);
    assert_eq!(stats.completed, 10, "10 轮请求全部 Complete");
    assert_eq!(ledger.contexts_live(), 0, "关停：驻留句柄全部 Drop");
}

#[test]
fn worker_reuses_resident_context_on_same_key_and_vacates_backend_slot_for_other_keys() {
    // madai-w16 I-C-14：后端席位 1（仿真核 / testkit 缺省）——同键两轮 hits 1 且后端 open_context 只调 1 次；换键时 worker 按 LRU
    // 腾席位（Drop 旧句柄）并把键回投，池随之离键：条目 / KV 与 worker 实际驻留一致，再回到旧键是新建而非命中。
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&["ok"])));
    let ledger = fake.ledger();
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .context_kv_mb(100)
        .spawn()
        .expect("spawn");
    core.apply_config(config_doc(7)).expect("control 线程在");

    run_keyed(&core, "acct:a/conv:1");
    let second = run_keyed(&core, "acct:a/conv:1");
    assert_eq!((second.contexts.misses, second.contexts.hits), (1, 1));
    assert_eq!(
        (
            ledger.open_context_calls.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (1, 1),
        "同键两轮后端只开 1 次、句柄驻留"
    );

    // 换键：席位 1 → 先腾 A（Drop）再开 B；池按 worker 回投的事实让 A 离池。
    let other = run_keyed(&core, "acct:b/conv:1");
    assert_eq!((other.contexts.misses, other.contexts.hits), (2, 1));
    assert_eq!((other.context_entries, other.context_kv_mb), (1, 100));
    assert_eq!(
        (
            ledger.open_context_calls.load(Ordering::SeqCst),
            ledger.contexts_dropped.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (2, 1, 1)
    );

    // 回到 A：席位被 B 占着 → 新建（misses +1、hits 不增），B 被腾。
    let back = run_keyed(&core, "acct:a/conv:1");
    assert_eq!((back.contexts.misses, back.contexts.hits), (3, 1));
    assert_eq!((back.context_entries, back.context_kv_mb), (1, 100));
    assert_eq!(
        (
            ledger.open_context_calls.load(Ordering::SeqCst),
            ledger.contexts_dropped.load(Ordering::SeqCst)
        ),
        (3, 2)
    );
    assert!(
        back.context_kv_mb <= back.context_kv_budget_mb,
        "不变量：已用 ≤ 预算"
    );
    let stats = shutdown(core);
    assert_eq!(stats.completed, 4);
    assert_eq!(stats.contexts.evicted_for_budget, 0, "腾席位不是腾预算");
    assert_eq!(ledger.contexts_live(), 0);
}

#[test]
fn context_kv_mb_takes_the_backend_reading_and_budget_evicts_lru_by_true_value() {
    // madai-w16 I-C-14：不设覆盖位 → 每上下文 KV 取后端读数——加载前 Backend::context_kv_mb、加载后 Model::context_kv_mb（定格）；
    // 预算 512 / 真值 200 → 第 3 个键腾 LRU（evicted_for_budget 1），已用恒 ≤ 预算，被腾键在 worker 侧真 Drop。
    let fake = Arc::new(
        FakeBackend::new(
            "fake",
            FakeScript::tokens(&["ok"]).with_kv_per_context_mb(200),
        )
        .with_caps(BackendCaps {
            concurrency: 4,
            ..default_caps()
        }),
    );
    let ledger = fake.ledger();
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");

    // 预算 0（未配置）：真值 200 > 0 → 不进池、不驻留——生产站配置前的口径。
    let unbudgeted = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(
        (
            unbudgeted.contexts.rejected,
            unbudgeted.context_entries,
            unbudgeted.context_kv_mb
        ),
        (1, 0, 0)
    );
    assert_eq!(ledger.contexts_live(), 0);
    core.apply_config(config_doc(7)).expect("control 线程在");

    let a = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(
        (a.context_entries, a.context_kv_mb),
        (1, 200),
        "后端真值 200"
    );
    // 加载后以模型申报为准：后端脚本改 300 不影响已加载模型（ModelLoaded 时定格 200）。
    fake.set_script(FakeScript::tokens(&["ok"]).with_kv_per_context_mb(300));
    let b = run_keyed(&core, "acct:b/conv:1");
    assert_eq!(
        (b.context_entries, b.context_kv_mb),
        (2, 400),
        "Model::context_kv_mb 优先于 Backend::context_kv_mb"
    );

    // 第 3 键：剩 112 < 200 → 腾 LRU 空闲（A）；条目 / 已用不涨，A 在后端侧 Drop。
    let c = run_keyed(&core, "acct:c/conv:1");
    assert_eq!(c.contexts.evicted_for_budget, 1);
    assert_eq!((c.context_entries, c.context_kv_mb), (2, 400));
    assert!(
        c.context_kv_mb <= c.context_kv_budget_mb,
        "不变量：已用 ≤ 预算"
    );
    assert_eq!(
        (
            ledger.contexts_dropped.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (2, 2),
        "配置前的临时句柄 1 + 被腾的 A 1；驻留 = 条目"
    );
    // A 回来：被腾过 → 新建（hits 不增），再腾 LRU（B）。
    let back = run_keyed(&core, "acct:a/conv:1");
    assert_eq!((back.contexts.misses, back.contexts.hits), (4, 0));
    assert_eq!(back.contexts.evicted_for_budget, 2);
    assert_eq!((back.context_entries, back.context_kv_mb), (2, 400));
    let stats = shutdown(core);
    assert_eq!(stats.completed, 5);
    assert_eq!(stats.contexts.rejected, 1);
    assert_eq!(ledger.contexts_live(), 0);
}

#[test]
fn drop_contexts_releases_resident_backend_context_on_worker_before_next_order() {
    // madai-w16 I-C-14：关户回键 → worker 真释放（句柄 Drop）——worker 邮箱 FIFO 使释放先于下一份同键工单，新一轮是重新打开；
    // 生成失败的轮不驻留（context_retained 假）→ 池按回执让键离池，之后同键是新建。
    let fake = Arc::new(FakeBackend::new("fake", FakeScript::tokens(&["ok"])));
    let ledger = fake.ledger();
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .context_kv_mb(100)
        .spawn()
        .expect("spawn");
    core.apply_config(config_doc(7)).expect("control 线程在");

    run_keyed(&core, "acct:a/conv:1");
    let held = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(held.contexts.hits, 1);
    assert_eq!(ledger.contexts_live(), 1);

    assert_eq!(core.drop_contexts("acct:a/".to_owned()), Some(1), "关户");
    let reopened = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(reopened.contexts.hits, 1, "drop 后同键不复用");
    assert_eq!((reopened.context_entries, reopened.context_kv_mb), (1, 100));
    assert_eq!(
        (
            ledger.contexts_dropped.load(Ordering::SeqCst),
            ledger.open_context_calls.load(Ordering::SeqCst),
            ledger.contexts_live()
        ),
        (1, 2, 1),
        "旧句柄在下一份工单前 Drop，新一轮重新打开"
    );

    // 失败轮：第 1 片前报 Crashed → 请求失败、worker 不驻留 → 池离键（条目 0、KV 结清）；再来同键是新建。
    fake.set_script(FakeScript::tokens(&["ok"]).crashing_at(0));
    let (tx, rx) = mpsc::channel();
    core.stream(keyed_request("acct:a/conv:1"), Box::new(ChannelSink(tx)));
    let events = collect_until_terminal(&rx);
    assert!(
        matches!(events.last(), Some(Event::Failed { .. })),
        "{events:?}"
    );
    let failed = core.stats().expect("在");
    assert_eq!(failed.contexts.hits, 2, "投递时池判复用");
    assert_eq!(
        (failed.context_entries, failed.context_kv_mb),
        (0, 0),
        "生成失败不驻留：键离池、KV 结清"
    );
    assert_eq!(ledger.contexts_live(), 0);
    fake.set_script(FakeScript::tokens(&["ok"]));
    let fresh = run_keyed(&core, "acct:a/conv:1");
    assert_eq!((fresh.contexts.misses, fresh.contexts.hits), (3, 2));
    assert_eq!((fresh.context_entries, fresh.context_kv_mb), (1, 100));
    let stats = shutdown(core);
    assert_eq!((stats.completed, stats.failed), (4, 1));
    assert_eq!(ledger.contexts_live(), 0);
}

#[test]
fn drop_contexts_on_in_flight_job_detaches_key_now_and_settles_kv_at_terminal() {
    // madai-w15 I-C-13：在飞 drop——键立即离池（entries 0、dropped 1），KV 记账到 Job 终态 release 才结清（kv_mb 100 → 0）；
    // 终态后同键请求是新建、hits 不增。
    let fake = Arc::new(FakeBackend::new(
        "fake",
        FakeScript::tokens(&["一", "二", "三", "四", "五", "六", "七", "八"])
            .with_token_delay_ms(100),
    ));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .context_kv_mb(100)
        .spawn()
        .expect("spawn");
    core.apply_config(config_doc(7)).expect("control 线程在");

    let (tx, rx) = mpsc::channel();
    core.stream(keyed_request("acct:a/conv:1"), Box::new(ChannelSink(tx)));
    let first = rx.recv_timeout(WAIT).expect("Started");
    assert!(matches!(first, Event::Started { .. }), "{first:?}");
    let running = core.stats().expect("在");
    assert_eq!(running.inflight_jobs, 1);
    assert_eq!((running.context_entries, running.context_kv_mb), (1, 100));

    assert_eq!(
        core.drop_contexts("acct:a/".to_owned()),
        Some(1),
        "在飞条目也计入"
    );
    let detached = core.stats().expect("在");
    assert_eq!(detached.contexts.dropped, 1);
    assert_eq!(detached.context_entries, 0, "键立即离池");
    assert_eq!(detached.context_kv_mb, 100, "终态前预算不放");
    assert_eq!(detached.inflight_jobs, 1, "任务本身照常在飞");

    let mut events = vec![first];
    events.extend(collect_until_terminal(&rx));
    assert!(
        matches!(events.last(), Some(Event::Complete { .. })),
        "{events:?}"
    );
    let settled = core.stats().expect("在");
    assert_eq!(settled.inflight_jobs, 0);
    assert_eq!(settled.context_kv_mb, 0, "终态结清、不写回");
    assert_eq!(settled.context_entries, 0);
    assert_eq!(settled.contexts.dropped, 1, "延迟结清后 dropped 仍计入");

    fake.set_token_delay_ms(0);
    let reissued = run_keyed(&core, "acct:a/conv:1");
    assert_eq!(reissued.contexts.hits, 0, "drop 后同键新建");
    assert_eq!(reissued.contexts.misses, 2);
    assert_eq!((reissued.context_entries, reissued.context_kv_mb), (1, 100));
    let ledger = shutdown(core);
    assert_eq!(ledger.contexts.dropped, 1);
}

#[test]
fn stats_snapshot_reads_live_gauges_from_control_thread_and_matches_shutdown_ledger() {
    // madai-w15 I-C-13：Stats 经邮箱在 control 线程一次取齐——在飞 Job 在执行中读到 1、终态后 0；uptime 随注入时钟单调；
    // 与 shutdown 收回的台账同一类型、同一取法；停后 None（ffi 4004）。
    // 8 片 × 100 ms 节拍：Started 随首帧（合帧窗 80 ms）到达时后端仍有 ≥ 600 ms 在飞，执行中的读数不靠碰运气。
    let fake = Arc::new(FakeBackend::new(
        "fake",
        FakeScript::tokens(&["一", "二", "三", "四", "五", "六", "七", "八"])
            .with_token_delay_ms(100),
    ));
    let telemetry = Arc::new(RecordingTelemetrySink::new());
    let core = builder(&telemetry)
        .backend(Arc::clone(&fake) as Arc<dyn madinfer_backend::Backend>)
        .mapping(mapping(vec![bundled("fake", 0)]))
        .spawn()
        .expect("spawn");

    let idle = core.stats().expect("control 线程在");
    assert_eq!(idle.inflight_jobs, 0);
    assert_eq!(idle.requests_accepted, 0);
    assert_eq!(idle.context_entries, 0);
    assert_eq!(idle.context_kv_mb, 0);
    assert_eq!(
        idle.context_kv_budget_mb, 128,
        "未应用配置：Flagship 内置缺省 128（madai-w16 首日 K3）"
    );
    assert_eq!(idle.request_fsm_table_size, 20, "常量字段随快照带出");

    let (tx, rx) = mpsc::channel();
    core.stream(request(PrivacyTag::Public), Box::new(ChannelSink(tx)));
    let first = rx.recv_timeout(WAIT).expect("Started");
    assert!(matches!(first, Event::Started { .. }), "{first:?}");
    let running = core.stats().expect("control 线程在");
    assert_eq!(running.inflight_jobs, 1, "执行中：一个在飞 Job");
    assert_eq!(running.requests_accepted, 1);
    assert_eq!(running.completed, 0);
    assert!(running.uptime_ms >= idle.uptime_ms, "uptime 单调");

    let mut events = vec![first];
    events.extend(collect_until_terminal(&rx));
    assert!(
        matches!(events.last(), Some(Event::Complete { .. })),
        "{events:?}"
    );
    let done = core.stats().expect("control 线程在");
    assert_eq!(done.inflight_jobs, 0, "终态后在飞归零");
    assert_eq!(done.completed, 1);
    assert_eq!(done.sched.done, 1, "子台账随快照一次取齐");
    assert!(done.uptime_ms >= running.uptime_ms);

    #[cfg(debug_assertions)]
    {
        core.stop_control();
        assert_eq!(core.stats(), None, "停后：None（ffi 4004）");
    }
    let ledger = core.shutdown().expect("台账");
    assert_eq!(ledger.completed, 1);
    assert_eq!(ledger.inflight_jobs, 0);
    assert!(ledger.uptime_ms >= done.uptime_ms);
}
