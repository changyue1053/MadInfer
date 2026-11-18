//! 调试面用例（madai-w10 I-C-09 J5-a；I-S-05 U-2 / U-4；madai-w11 I-C-10 两 profile 同导出；madai-w12 I-C-11 逐词元节拍；
//! madai-w16 I-C-14 验签 deadline 注入）：`InferCore::create_debug` / `InferCore::test_stop_control` /
//! `InferCore::debug_set_token_delay_ms` / `InferCore::debug_set_verify_deadline_ms`——debug 构建真语义，
//! release 构建恒 4005 `FeatureDisabled`（`release_face` 组，`cargo test --release -p madinfer-ffi` 跑）。三端壳的对应用例照本文件口径写。

use super::*;
use crate::debug::{DEBUG_TOKEN_DELAY_MAX_MS, DEBUG_VERIFY_DEADLINE_MAX_MS};

/// release 构建：符号在、函数体门控——两入口恒 4005，不建站、不碰宿主、不停线程。
#[cfg(not(debug_assertions))]
mod release_face {
    use super::*;

    #[test]
    fn debug_face_returns_feature_disabled_in_release_builds() {
        // release 构建建站须带受信公钥（否则 4003），与生产宿主同口径。
        let bridge = leak_bridge(ConsentDecisionRecord::Deny);
        let mut with_keys = options(&["summarize"], None);
        with_keys.trusted_keys = test_trusted_keys();
        assert!(matches!(
            InferCore::create_debug(Box::new(bridge), with_keys.clone(), true),
            Err(InferFfiError::FeatureDisabled)
        ));
        assert!(matches!(
            InferCore::create_debug(Box::new(bridge), with_keys.clone(), false),
            Err(InferFfiError::FeatureDisabled)
        ));
        assert!(
            bridge.kv.read().unwrap().is_empty(),
            "release 下 create_debug 不碰宿主（无 install 标记写入）"
        );
        assert_eq!(InferFfiError::FeatureDisabled.code(), 4005);

        // 生产建站的句柄：test_stop_control 恒 4005，control 线程仍在（apply_config 仍有回执）。
        let core = InferCore::create(Box::new(bridge), with_keys).expect("create");
        assert_eq!(
            core.test_stop_control(),
            Err(InferFfiError::FeatureDisabled)
        );
        assert!(
            core.apply_config(b"{".to_vec()).is_ok(),
            "control 线程未被停"
        );
        // madai-w12 I-C-11：debug_set_token_delay_ms 同样符号在、函数体门控——形式合法 / 越界 / 零值一律 4005（不先 4001）。
        for ms in [0, 100, DEBUG_TOKEN_DELAY_MAX_MS + 1] {
            assert_eq!(
                core.debug_set_token_delay_ms(ms),
                Err(InferFfiError::FeatureDisabled),
                "ms={ms}"
            );
        }
        // madai-w16 I-C-14：debug_set_verify_deadline_ms 同口径——带验签器的站也恒 4005，deadline 仍是常量（apply 仍有回执）。
        for ms in [0, 1, DEBUG_VERIFY_DEADLINE_MAX_MS + 1] {
            assert_eq!(
                core.debug_set_verify_deadline_ms(ms),
                Err(InferFfiError::FeatureDisabled),
                "ms={ms}"
            );
        }
        assert!(core.apply_config(b"{".to_vec()).is_ok(), "验签器未被触碰");
        drop(core);
    }
}

#[cfg(debug_assertions)]
#[test]
fn debug_set_verify_deadline_ms_makes_verify_timeout_reproducible_without_a_slow_machine() {
    // madai-w16 I-C-14（I-D-14 §7）：宿主验签卡住 + deadline 1 ms → 立即（≪ 2 000 ms）Rejected{SignatureInvalid, Received,
    // detail verify=timeout, detail_kind VerifyTimeout}，本站 stats().verify_timeouts 与进程级计数同一处 +1（同源性）；
    // 迟到回执哑火；0 恢复缺省后同一站 apply 成功、超时计数不再变。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    bridge
        .verify_blocking
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut with_keys = options(&["summarize"], None);
    with_keys.trusted_keys = test_trusted_keys();
    let core = InferCore::create(Box::new(bridge), with_keys).expect("建站");
    let timeouts_before = verify_timeout_count();
    let late_before = verify_late_reply_count();
    core.debug_set_verify_deadline_ms(1).expect("定 deadline");

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
            detail_kind: ApplyDetailKindRecord::VerifyTimeout,
        }
    );
    assert!(
        waited_ms < VERIFY_DEADLINE_MS / 2,
        "注入的 deadline 生效，不等满缺省 2 000 ms：waited_ms={waited_ms}"
    );
    assert_eq!(
        core.stats().expect("核在").verify_timeouts,
        1,
        "本站计数 +1"
    );
    // 进程级计数与本站同一处 fetch_add，但同进程并行用例的别的站也加 → 只判 ≥。
    assert!(
        verify_timeout_count() > timeouts_before,
        "进程级计数同一处 fetch_add"
    );

    // 放行宿主：迟到回执哑火计数 +1；0 恢复缺省 deadline 后 apply 成功，超时计数不再变。
    bridge
        .verify_release
        .store(true, std::sync::atomic::Ordering::SeqCst);
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
        "本站宿主回了这一次（迟到）"
    );
    while verify_late_reply_count() < late_before + 1 && now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        verify_late_reply_count() > late_before,
        "迟到回执哑火（进程级，只判 ≥）"
    );
    core.debug_set_verify_deadline_ms(0).expect("恢复缺省");
    let ok = core.apply_config(signed_config_doc(5, "k1")).expect("核在");
    assert!(ok.applied, "{ok:?}");
    assert_eq!(ok.active_version, 5);
    assert_eq!(core.stats().expect("核在").verify_timeouts, 1);
    drop(core);
}

#[cfg(debug_assertions)]
#[test]
fn debug_set_verify_deadline_ms_rejects_out_of_range_and_stations_without_verifier() {
    assert_eq!(DEBUG_VERIFY_DEADLINE_MAX_MS, 10_000);
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let mut with_keys = options(&["summarize"], None);
    with_keys.trusted_keys = test_trusted_keys();
    let verified = InferCore::create(Box::new(bridge), with_keys).expect("建站");
    assert_eq!(
        verified.debug_set_verify_deadline_ms(DEBUG_VERIFY_DEADLINE_MAX_MS + 1),
        Err(InferFfiError::InvalidArgument),
        "上限之上 4001（防 control 线程无界等宿主）"
    );
    verified
        .debug_set_verify_deadline_ms(DEBUG_VERIFY_DEADLINE_MAX_MS)
        .expect("上限含");
    verified
        .debug_set_verify_deadline_ms(0)
        .expect("零值合法：恢复缺省");
    // 上限内的 deadline 下宿主按时回执 → 应用照常（deadline 只是上限，不是延迟）。
    verified
        .debug_set_verify_deadline_ms(50)
        .expect("定 deadline");
    let ok = verified
        .apply_config(signed_config_doc(3, "k1"))
        .expect("核在");
    assert!(ok.applied, "{ok:?}");
    assert_eq!(verified.stats().expect("核在").verify_timeouts, 0);

    // 无验签器的站（trusted_keys 空）：生产建站与 create_debug 都回 4001，站仍正常工作。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let production =
        InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    assert_eq!(
        production.debug_set_verify_deadline_ms(1),
        Err(InferFfiError::InvalidArgument)
    );
    assert_eq!(
        debug_core(true).debug_set_verify_deadline_ms(1),
        Err(InferFfiError::InvalidArgument)
    );
    assert!(production.apply_config(b"{".to_vec()).is_ok(), "站仍在");
}

#[cfg(debug_assertions)]
fn debug_core(script_backend: bool) -> Arc<InferCore> {
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    InferCore::create_debug(
        Box::new(bridge),
        options(&["summarize"], None),
        script_backend,
    )
    .expect("create_debug")
}

#[cfg(debug_assertions)]
fn structured_with_prompt(prompt: &str) -> RequestRecord {
    let mut request = structured_request(MEETING_SCHEMA);
    request.messages[0].content = prompt.to_owned();
    request
}

#[cfg(debug_assertions)]
#[test]
fn debug_script_backend_echoes_prompt_so_hosts_can_build_2003_with_partial() {
    let core = debug_core(true);
    // probe 形态与生产站同：仿真核与脚本后端都声明 Emulated 约束，壳侧 probe 断言不用分叉。
    assert!(core
        .probe()
        .entries
        .iter()
        .all(|e| e.constraint == ConstraintSupportRecord::Emulated));

    // ① 2003 之二：后端产出"合法 JSON 但缺 required（count / tags）"→ Started 后终态校验不过，Failed 2003 携 partial = 原文，
    //    无 Complete。这是 I-S-05 U-2 点名三端 ffi 面此前造不出的分支（此前只能靠 max_tokens 截断）。
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_with_prompt(r#"{"title":"x"}"#),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1, "{events:?}");
    let Some(Recorded::Failed(failed)) = events.last() else {
        panic!("终态应为 Failed 2003：{events:?}");
    };
    assert_eq!(
        (failed.code, failed.wire.as_str()),
        (2003, "schema_invalid")
    );
    assert_eq!(failed.partial_text.as_deref(), Some(r#"{"title":"x"}"#));
    assert!(!events.iter().any(|e| matches!(e, Recorded::Complete(_))));
    assert_quiet(&rx);

    // ② 齐全的合法 JSON → Complete 携已过校验的结构化输出（键序由核内序列化决定，按值比对）。
    let (tx, rx) = mpsc::channel();
    core.stream(
        structured_with_prompt(r#"{"title":"x","count":1,"tags":["a"]}"#),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    match events.last() {
        Some(Recorded::Complete(complete)) => {
            let OutputRecord::Structured { json } = &complete.output else {
                panic!("结构化输出：{complete:?}");
            };
            let got: serde_json::Value = serde_json::from_str(json).expect("json");
            assert_eq!(
                got,
                serde_json::json!({"title": "x", "count": 1, "tags": ["a"]})
            );
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_quiet(&rx);

    // ③ 文本请求：输出 = prompt 原文（空白原样），词元 = 空白段数。
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(
            PrivacyRecord::Public,
            QosRecord::Interactive,
            "hello  world",
            16,
        ),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    match events.last() {
        Some(Recorded::Complete(complete)) => {
            assert_eq!(complete.text, "hello  world");
            assert_eq!(
                complete.stats.gen_tokens, 3,
                "\"hello \" / \" \" / \"world\""
            );
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_quiet(&rx);
}

#[cfg(debug_assertions)]
#[test]
fn create_debug_without_script_backend_is_the_production_station() {
    // debug_script_backend = false → 与 create 同一条装配路径（backend-llama 仿真核），probe 逐字段相同。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let production =
        InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    let debug = debug_core(false);
    assert_eq!(debug.probe(), production.probe());
    // 形式错误仍在 spawn 前拒绝。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    assert!(matches!(
        InferCore::create_debug(Box::new(bridge), options(&[], None), true),
        Err(InferFfiError::InvalidArgument)
    ));
}

#[cfg(debug_assertions)]
#[test]
fn test_stop_control_turns_apply_config_into_4004_and_is_idempotent() {
    // I-S-05 U-4：真核路径上的 4004——control 线程退出、InferCore 对象仍在。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    assert!(core.apply_config(b"{".to_vec()).is_ok(), "停前：有回执");

    core.test_stop_control().expect("首次停");
    assert_eq!(
        core.apply_config(b"{".to_vec()),
        Err(InferFfiError::CoreStopped),
        "停后：4004"
    );
    // 停后 stream：形式合法仍发句柄，但核不再受理——无任何回调（不是 K1 违约，是故障态；壳侧据 4004 判核已停）。
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "hi", 4),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream 形式合法");
    assert!(rx.recv_timeout(QUIET).is_err(), "停后 stream 无回调");
    core.test_stop_control().expect("幂等");
    assert_eq!(
        core.apply_config(Vec::new()),
        Err(InferFfiError::CoreStopped)
    );
    // Drop 仍走 Shutdown + join，不挂、不 panic。
    drop(core);
}

#[cfg(debug_assertions)]
#[test]
fn test_stop_control_turns_drop_contexts_into_4004() {
    // madai-w14 I-C-12：drop_contexts 与 apply_config 同一"等核回执"形态——control 线程退出后恒 4004，不挂。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    assert_eq!(
        core.drop_contexts("acct:a/".to_owned()),
        Ok(0),
        "停前：有回执"
    );
    core.test_stop_control().expect("停");
    assert_eq!(
        core.drop_contexts("acct:a/".to_owned()),
        Err(InferFfiError::CoreStopped),
        "停后：4004"
    );
    drop(core);
}

#[cfg(debug_assertions)]
#[test]
fn test_stop_control_turns_stats_into_4004() {
    // madai-w15 I-C-13：stats 与 apply_config / drop_contexts 同一"等核回执"形态——control 线程退出后恒 4004，不挂。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let core = InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    assert!(core.stats().is_ok(), "停前：有回执");
    core.test_stop_control().expect("停");
    assert_eq!(core.stats(), Err(InferFfiError::CoreStopped), "停后：4004");
    drop(core);
}

/// 取消后终态须在此时限内到达：远小于一个 3 000 ms 节拍——证明片间等待被取消旗打断，而不是睡满再查。
#[cfg(debug_assertions)]
const CANCEL_SETTLE: Duration = Duration::from_millis(1_000);

#[cfg(debug_assertions)]
fn delta_texts(events: &[Recorded]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Recorded::Delta(d) => Some(d.text.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(debug_assertions)]
#[test]
fn debug_set_token_delay_ms_gives_one_frame_per_token_when_above_the_frame_window() {
    // madai-w12 I-C-11（I-D-10 §7-2）：节拍 200 ms ≫ 合帧窗 20 ms（options()）→ 每个词元独立成帧，帧数 = 词元数、逐帧文本 = 词元。
    let core = debug_core(true);
    core.debug_set_token_delay_ms(200).expect("定节拍");
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "a b c", 16),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(started_count(&events), 1);
    assert_eq!(
        delta_texts(&events),
        vec!["a ".to_owned(), "b ".to_owned(), "c".to_owned()],
        "一词元一帧：{events:?}"
    );
    match events.last() {
        Some(Recorded::Complete(complete)) => {
            assert_eq!(complete.text, "a b c");
            assert_eq!(complete.stats.gen_tokens, 3);
        }
        other => panic!("终态应为 Complete：{other:?}"),
    }
    assert_quiet(&rx);
    // 0 恢复无节拍：同一站、同一请求仍完整回显（帧数不再钉——无节拍时随合帧窗）。
    core.debug_set_token_delay_ms(0).expect("清节拍");
    let (tx, rx) = mpsc::channel();
    core.stream(
        request(PrivacyRecord::Public, QosRecord::Interactive, "a b c", 16),
        Box::new(ChannelSink(tx)),
    )
    .expect("stream");
    let events = collect_until_terminal(&rx);
    assert_eq!(delta_texts(&events).concat(), "a b c");
    assert!(matches!(events.last(), Some(Recorded::Complete(_))));
}

#[cfg(debug_assertions)]
#[test]
fn debug_set_token_delay_ms_makes_cancel_after_first_delta_deterministic() {
    // I-D-10 §7-2 点名的分支："Delta 后取消携 partial"——节拍 3 000 ms：首帧（首词元）到达时 worker 正在片间等待，cancel 后
    // 等待被取消旗打断（≤ 5 ms 切片）、worker 以 Cancelled 收口 → 恰一次 Failed(3008)，partial = 已出 Delta 拼接，且在 1 s 内到达。
    let core = debug_core(true);
    core.debug_set_token_delay_ms(3_000).expect("定节拍");
    let (tx, rx) = mpsc::channel();
    let handle = core
        .stream(
            request(
                PrivacyRecord::Public,
                QosRecord::Interactive,
                "one two three four",
                16,
            ),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
    let first = rx.recv_timeout(WAIT).expect("Started");
    assert!(matches!(first, Recorded::Started(_)), "{first:?}");
    let second = rx.recv_timeout(WAIT).expect("首帧");
    let Recorded::Delta(ref first_delta) = second else {
        panic!("首帧应为 Delta：{second:?}");
    };
    assert_eq!(first_delta.text, "one ");
    core.cancel(handle);
    let terminal = rx
        .recv_timeout(CANCEL_SETTLE)
        .expect("取消后终态须在 1 s 内到达（片间等待被取消旗打断，不睡满 3 s 节拍）");
    let Recorded::Failed(failed) = &terminal else {
        panic!("取消赢：终态应为 Failed(3008)：{terminal:?}");
    };
    assert_eq!((failed.code, failed.wire.as_str()), (3008, "cancelled"));
    assert_eq!(
        failed.partial_text.as_deref(),
        Some("one "),
        "partial = 已出 Delta 拼接（节拍内取消，第二词元未出）"
    );
    assert_quiet(&rx);
    core.cancel(handle); // 已终态：幂等 no-op。
    assert_quiet(&rx);
}

#[cfg(debug_assertions)]
#[test]
fn debug_set_token_delay_ms_rejects_out_of_range_and_non_script_stations() {
    let script = debug_core(true);
    assert_eq!(DEBUG_TOKEN_DELAY_MAX_MS, 10_000);
    assert_eq!(
        script.debug_set_token_delay_ms(DEBUG_TOKEN_DELAY_MAX_MS + 1),
        Err(InferFfiError::InvalidArgument),
        "上限之上 4001（防 worker 无界 sleep 拖死 destroy）"
    );
    script
        .debug_set_token_delay_ms(DEBUG_TOKEN_DELAY_MAX_MS)
        .expect("上限含");
    script
        .debug_set_token_delay_ms(0)
        .expect("零值合法：清节拍");
    // 非脚本后端站：生产建站与 create_debug(.., false) 都回 4001，且站仍正常工作。
    let bridge = leak_bridge(ConsentDecisionRecord::Deny);
    let production =
        InferCore::create(Box::new(bridge), options(&["summarize"], None)).expect("create");
    assert_eq!(
        production.debug_set_token_delay_ms(100),
        Err(InferFfiError::InvalidArgument)
    );
    assert_eq!(
        debug_core(false).debug_set_token_delay_ms(100),
        Err(InferFfiError::InvalidArgument)
    );
    let (tx, rx) = mpsc::channel();
    production
        .stream(
            request(PrivacyRecord::Public, QosRecord::Interactive, "hi", 4),
            Box::new(ChannelSink(tx)),
        )
        .expect("stream");
    assert!(matches!(
        collect_until_terminal(&rx).last(),
        Some(Recorded::Complete(_))
    ));
}
