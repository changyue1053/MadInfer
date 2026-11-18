//! cabi 单测（模块内：能看见私有取值表）：enum 取值 = 声明序、头文件 ↔ Rust 常量同源、`#[repr(C)]` 布局对 clang
//! `sizeof` / `offsetof` 读数、防火墙自检、信号转换。C 面行为测试（K1 / 3008 / 3007 / 记忆 / 容量协议 / destroy 收口）
//! 在 tests/cabi.rs 以 `extern "C"` 回调宿主替身覆盖。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 测试模块放开（根 Cargo.toml lints 注）

use std::mem::{align_of, offset_of, size_of};

use super::*;

const HEADER: &str = include_str!("../../include/madinfer.h");

#[test]
fn enum_values_equal_declaration_order() {
    // 取值表每一位 = 下标（C enum 取值 = uniffi 枚举声明序，头文件 §2）。
    for (i, v) in TIERS.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in OSES.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in PRIVACIES.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in QOS.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in ROLES.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in CONSENT_DECISIONS.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in THERMALS.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in MEMORY_PRESSURES.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in NET_STATES.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    for (i, v) in APP_STATES.iter().enumerate() {
        assert_eq!(*v as usize, i);
    }
    // 出向枚举（record → C）与头文件常量逐个核对。
    assert_eq!(crate::SourceRecord::SystemModel as u8, 0);
    assert_eq!(crate::SourceRecord::Bundled as u8, 1);
    assert_eq!(crate::SourceRecord::Cloud as u8, 2);
    assert_eq!(crate::RouteReasonRecord::PrivacyForcedLocal as u8, 0);
    assert_eq!(crate::RouteReasonRecord::Fallback as u8, 5);
    assert_eq!(crate::CapStateRecord::Available as u8, 0);
    assert_eq!(crate::CapStateRecord::Disabled as u8, 4);
    assert_eq!(crate::LatencyRecord::Fast as u8, 0);
    assert_eq!(crate::LatencyRecord::Slow as u8, 2);
    assert_eq!(ConsentDecisionRecord::Deny as u8, 2);
    // madai-w08 I-C-07：配置应用出向枚举。
    assert_eq!(crate::ApplyStageRecord::Received as u8, 0);
    assert_eq!(crate::ApplyStageRecord::Applied as u8, 4);
    assert_eq!(crate::ApplyStageRecord::Rejected as u8, 5);
    assert_eq!(crate::ApplyRejectReasonRecord::UnknownKey as u8, 0);
    assert_eq!(crate::ApplyRejectReasonRecord::SemanticInvalid as u8, 6);
    // madai-w08 I-C-07 G4：结构化输出出向枚举 ↔ madinfer_constraint_support / madinfer_output_kind。
    assert_eq!(crate::ConstraintSupportRecord::None as u8, 0);
    assert_eq!(crate::ConstraintSupportRecord::Emulated as u8, 1);
    assert_eq!(crate::ConstraintSupportRecord::Native as u8, 2);
    assert!(HEADER.contains("MADINFER_OUTPUT_TEXT = 0, MADINFER_OUTPUT_STRUCTURED = 1"));
    assert!(HEADER.contains("MADINFER_CONSTRAINT_EMULATED = 1"));
    // 越界取值 → 4001。
    assert_eq!(pick(&TIERS, 4), Err(InferFfiError::InvalidArgument));
    assert_eq!(
        pick(&CONSENT_DECISIONS, 3),
        Err(InferFfiError::InvalidArgument)
    );
}

#[test]
fn header_constants_match_rust_side() {
    // 头文件即契约：常量 / 形态串 / 八符号原型与 Rust 侧同源（改任一处本测即红）。
    assert!(HEADER.contains(&format!(
        "#define MADINFER_ABI_VERSION {INFER_ABI_VERSION}u"
    )));
    assert_eq!(INFER_ABI_VERSION, crate::tests::compat_abi_version());
    assert!(
        MADINFER_CABI_FORM_TAG.ends_with(format!("abi={INFER_ABI_VERSION}\0").as_bytes()),
        "cabi 留痕字面量的 abi 段须与握手常量同版"
    );
    assert!(HEADER.contains(&format!(
        "#define MADINFER_CABI_FORM \"{MADINFER_CABI_FORM}\""
    )));
    assert!(std::str::from_utf8(MADINFER_CABI_FORM_TAG)
        .unwrap()
        .contains(&format!(
            "form={MADINFER_CABI_FORM} abi={INFER_ABI_VERSION}"
        )));
    assert!(HEADER.contains("#define MADINFER_OK 0"));
    for error in InferFfiError::ALL {
        let name = match error {
            InferFfiError::InvalidArgument => "MADINFER_ERROR_INVALID_ARGUMENT",
            InferFfiError::Panicked => "MADINFER_ERROR_PANICKED",
            InferFfiError::CoreStartFailed => "MADINFER_ERROR_CORE_START_FAILED",
            InferFfiError::CoreStopped => "MADINFER_ERROR_CORE_STOPPED",
            InferFfiError::FeatureDisabled => "MADINFER_ERROR_FEATURE_DISABLED",
        };
        assert!(
            HEADER.contains(&format!("#define {name} {}", error.code())),
            "{name} 与 InferFfiError 同表"
        );
    }
    assert!(HEADER.contains("#define MADINFER_CODE_CONSENT_DENIED 3007"));
    assert!(HEADER.contains("#define MADINFER_CODE_CANCELLED 3008"));
    assert!(HEADER.contains(&format!(
        "#define MADINFER_CAPABILITY_MAX {MADINFER_CAPABILITY_MAX}u"
    )));
    assert!(HEADER.contains(&format!(
        "#define MADINFER_VERSION_MAX {MADINFER_VERSION_MAX}u"
    )));
    assert!(HEADER.contains(&format!(
        "#define MADINFER_KV_VALUE_MAX {MADINFER_KV_VALUE_MAX}u"
    )));
    assert!(HEADER.contains(&format!("#define MADINFER_STOP_MAX {MADINFER_STOP_MAX}u")));
    let prototypes = [
        "uint32_t madinfer_abi_version(void);",
        "madinfer_core_handle madinfer_core_create(const madinfer_host_vtable* host, const madinfer_core_options* options, int32_t* out_error_code);",
        "void madinfer_core_destroy(madinfer_core_handle core);",
        "int32_t madinfer_probe(madinfer_core_handle core, madinfer_probe_snapshot* out_buf);",
        "uint64_t madinfer_stream_start(madinfer_core_handle core, const madinfer_request* request, const madinfer_sink_vtable* sink, int32_t* out_error_code);",
        "int32_t madinfer_stream_cancel(madinfer_core_handle core, uint64_t request);",
        "int32_t madinfer_host_reply(madinfer_core_handle core, uint64_t host_request, const madinfer_host_reply_record* reply);",
        "int32_t madinfer_signal(madinfer_core_handle core, const madinfer_os_signal* signal);",
        "int32_t madinfer_apply_config(madinfer_core_handle core, const uint8_t* document, uint32_t document_len, madinfer_apply_outcome* out);",
        "int32_t madinfer_drop_contexts(madinfer_core_handle core, madinfer_str prefix, uint32_t* out_dropped);",
    ];
    for prototype in prototypes {
        assert!(HEADER.contains(prototype), "头文件缺原型：{prototype}");
    }
    // madai-w14 I-C-12：第 10 符号只加不升版——头文件须写明 ABI 仍 2 的依据，且 §5 / §6 都说明"等核回执、禁在回调里调"。
    assert!(
        HEADER.contains("#define MADINFER_ABI_VERSION 2u"),
        "只加符号不升版"
    );
    assert!(
        HEADER.contains("只加符号不在其列"),
        "文件头 madai-w14 契约注须引升版规则原文说明不升版"
    );
    assert!(
        HEADER.contains("madinfer_apply_config / madinfer_drop_contexts 例外"),
        "§5 入向线程条须点名第 10 符号等核回执"
    );
    assert!(HEADER.contains(&format!(
        "#define MADINFER_APPLY_DETAIL_MAX {MADINFER_APPLY_DETAIL_MAX}u"
    )));
    // madai-w11 I-C-10：验签 deadline 常量 / 调试面 release 错误码两处同源；§5 "调试面" 条在。
    assert!(HEADER.contains(&format!(
        "#define MADINFER_VERIFY_DEADLINE_MS {}u",
        crate::VERIFY_DEADLINE_MS
    )));
    assert!(HEADER.contains("verify=timeout"), "§5 须写明超时留痕");
    assert!(
        HEADER.contains("MADINFER_ERROR_FEATURE_DISABLED (4005)"),
        "§5 调试面条须写明 release 错误码"
    );
    // madai-w10 I-C-09 J1（I-H-05 反馈 ①）+ madai-w11 I-C-10 deadline：第 9 符号的验签回投线程口径写进 §5 与 §6，
    // 两处同文、缺一即红。
    for sentence in [
        "把 host.verify_ed25519 投到专用瞬时线程 madinfer-ffi-verify",
        "recv_timeout(MADINFER_VERIFY_DEADLINE_MS)",
        "禁在验签回投所依赖的线程上同步调",
        "每次 apply 至多一次 verify_ed25519",
        "并发 apply 在核内按入邮箱序串行、按同序回执",
    ] {
        assert_eq!(
            HEADER.matches(sentence).count(),
            2,
            "§5 与 §6 须同文各一次：{sentence}"
        );
    }
    // 符号面恒十个（madai-w14 I-C-12 前九个）：头文件里顶格（非注释、非 vtable 成员——成员缩进四格）且以 ");" 收尾的
    // madinfer_* 函数原型数。
    let declared = HEADER
        .lines()
        .filter(|line| {
            !line.starts_with([' ', '*', '/', '#'])
                && line.contains(" madinfer_")
                && line.trim_end().ends_with(");")
        })
        .count();
    assert_eq!(
        declared, 12,
        "twelve production prototypes including explicit backend selection/query"
    );
}

#[test]
fn debug_header_declares_two_symbols_outside_the_production_header() {
    // madai-w10 I-C-09 J5-a：调试面另立头文件，生产头文件不声明它们（生产符号面不变）；调试头恰两个顶格原型，与 cabi/debug.rs 同名。
    const DEBUG_HEADER: &str = include_str!("../../include/madinfer_debug.h");
    let prototypes = [
        "madinfer_core_handle madinfer_core_create_debug(const madinfer_host_vtable* host, const madinfer_core_options* options, uint8_t debug_script_backend, int32_t* out_error_code);",
        "int32_t madinfer_test_stop_control(madinfer_core_handle core);",
    ];
    for prototype in prototypes {
        assert!(
            DEBUG_HEADER.contains(prototype),
            "调试头缺原型：{prototype}"
        );
        assert!(
            !HEADER.contains(prototype),
            "生产头文件不得声明调试符号：{prototype}"
        );
    }
    let declared = DEBUG_HEADER
        .lines()
        .filter(|line| {
            !line.starts_with([' ', '*', '/', '#'])
                && line.contains(" madinfer_")
                && line.trim_end().ends_with(");")
        })
        .count();
    assert_eq!(declared, 2, "调试面恰两符号");
    assert!(DEBUG_HEADER.contains("#include \"madinfer.h\""));
    assert!(
        !HEADER.contains("madinfer_core_create_debug(")
            && !HEADER.contains("madinfer_test_stop_control("),
        "生产头文件只准以文字提及调试符号，不得出现调用形态"
    );
}

#[test]
fn repr_c_layout_matches_clang_readout() {
    // 读数来源：clang（host aarch64-apple-darwin 与 ohos aarch64-unknown-linux-ohos 同为 LP64、同一布局规则）对
    // include/madinfer.h 的 sizeof / _Alignof / offsetof（I-C-06 报告 §3 留痕）。任何字段增删 / 换序即红。
    assert_eq!((size_of::<RawStr>(), align_of::<RawStr>()), (16, 8));
    assert_eq!(size_of::<RawTrustedKey>(), 32);
    assert_eq!(offset_of!(RawTrustedKey, public_key_hex), 16);

    assert_eq!(size_of::<RawCoreOptions>(), 80);
    assert_eq!(offset_of!(RawCoreOptions, tier), 16);
    assert_eq!(offset_of!(RawCoreOptions, os), 17);
    assert_eq!(offset_of!(RawCoreOptions, frame_ms), 20);
    assert_eq!(offset_of!(RawCoreOptions, sampling_bp), 24);
    assert_eq!(offset_of!(RawCoreOptions, capabilities), 32);
    assert_eq!(offset_of!(RawCoreOptions, capabilities_len), 40);
    assert_eq!(offset_of!(RawCoreOptions, has_consent_endpoint), 44);
    assert_eq!(offset_of!(RawCoreOptions, consent_endpoint), 48);
    assert_eq!(offset_of!(RawCoreOptions, trusted_keys), 64);
    assert_eq!(offset_of!(RawCoreOptions, trusted_keys_len), 72);

    assert_eq!(size_of::<RawMessage>(), 24);
    assert_eq!(offset_of!(RawMessage, content), 8);

    assert_eq!(size_of::<RawOptions>(), 40);
    assert_eq!(offset_of!(RawOptions, temperature_x100), 2);
    assert_eq!(offset_of!(RawOptions, has_top_p_bp), 4);
    assert_eq!(offset_of!(RawOptions, top_p_bp), 6);
    assert_eq!(offset_of!(RawOptions, has_max_tokens), 8);
    assert_eq!(offset_of!(RawOptions, max_tokens), 12);
    assert_eq!(offset_of!(RawOptions, stop), 16);
    assert_eq!(offset_of!(RawOptions, stop_len), 24);
    assert_eq!(offset_of!(RawOptions, has_seed), 28);
    assert_eq!(offset_of!(RawOptions, seed), 32);

    // madai-w08 G4 追加四字段：80 → 128（clang 读数）。
    assert_eq!(size_of::<RawRequest>(), 128);
    assert_eq!(offset_of!(RawRequest, business), 16);
    assert_eq!(offset_of!(RawRequest, privacy), 18);
    assert_eq!(offset_of!(RawRequest, qos), 19);
    assert_eq!(offset_of!(RawRequest, deadline_ms), 20);
    assert_eq!(offset_of!(RawRequest, messages), 24);
    assert_eq!(offset_of!(RawRequest, messages_len), 32);
    assert_eq!(offset_of!(RawRequest, options), 40);
    assert_eq!(offset_of!(RawRequest, output_kind), 80);
    assert_eq!(offset_of!(RawRequest, schema_json), 88);
    assert_eq!(offset_of!(RawRequest, has_context), 104);
    assert_eq!(offset_of!(RawRequest, context), 112);

    // madai-w08 G4 追加 constraint：72 → 76。
    assert_eq!(
        (
            size_of::<RawCapabilityEntry>(),
            align_of::<RawCapabilityEntry>()
        ),
        (76, 4)
    );
    assert_eq!(offset_of!(RawCapabilityEntry, capability_len), 64);
    assert_eq!(offset_of!(RawCapabilityEntry, state), 68);
    assert_eq!(offset_of!(RawCapabilityEntry, has_source), 69);
    assert_eq!(offset_of!(RawCapabilityEntry, source), 70);
    assert_eq!(offset_of!(RawCapabilityEntry, latency), 71);
    assert_eq!(offset_of!(RawCapabilityEntry, constraint), 72);

    assert_eq!(size_of::<RawProbeSnapshot>(), 80);
    assert_eq!(offset_of!(RawProbeSnapshot, core_version), 4);
    assert_eq!(offset_of!(RawProbeSnapshot, core_version_len), 36);
    assert_eq!(offset_of!(RawProbeSnapshot, tier), 40);
    assert_eq!(offset_of!(RawProbeSnapshot, snapshot_version), 48);
    assert_eq!(offset_of!(RawProbeSnapshot, entries), 56);
    assert_eq!(offset_of!(RawProbeSnapshot, entries_capacity), 64);
    assert_eq!(offset_of!(RawProbeSnapshot, entries_len), 68);
    assert_eq!(offset_of!(RawProbeSnapshot, entries_total), 72);

    assert_eq!(size_of::<RawStarted>(), 40);
    assert_eq!(offset_of!(RawStarted, source), 8);
    assert_eq!(offset_of!(RawStarted, has_package), 9);
    assert_eq!(offset_of!(RawStarted, package), 16);
    assert_eq!(offset_of!(RawStarted, route_reason), 32);
    assert_eq!(offset_of!(RawStarted, queued_ms), 36);

    // madai-w08 G4 追加 structured_patch：40 → 56。
    assert_eq!(size_of::<RawDelta>(), 56);
    assert_eq!(offset_of!(RawDelta, seq), 8);
    assert_eq!(offset_of!(RawDelta, text), 16);
    assert_eq!(offset_of!(RawDelta, tokens), 32);
    assert_eq!(offset_of!(RawDelta, has_structured_patch), 36);
    assert_eq!(offset_of!(RawDelta, structured_patch), 40);

    assert_eq!((size_of::<RawStats>(), align_of::<RawStats>()), (28, 4));
    assert_eq!(offset_of!(RawStats, has_source), 24);
    assert_eq!(offset_of!(RawStats, source), 25);
    assert_eq!(offset_of!(RawStats, fallback_steps), 26);
    assert_eq!(offset_of!(RawStats, prefix_hit), 27);

    // madai-w08 G4 追加 output_kind / output：56 → 72。
    assert_eq!(size_of::<RawComplete>(), 72);
    assert_eq!(offset_of!(RawComplete, text), 8);
    assert_eq!(offset_of!(RawComplete, stats), 24);
    assert_eq!(offset_of!(RawComplete, output_kind), 52);
    assert_eq!(offset_of!(RawComplete, output), 56);

    assert_eq!(size_of::<RawFailed>(), 64);
    assert_eq!(offset_of!(RawFailed, code), 8);
    assert_eq!(offset_of!(RawFailed, wire), 16);
    assert_eq!(offset_of!(RawFailed, has_retry_after_ms), 32);
    assert_eq!(offset_of!(RawFailed, retry_after_ms), 36);
    assert_eq!(offset_of!(RawFailed, has_partial_text), 40);
    assert_eq!(offset_of!(RawFailed, partial_text), 48);

    assert_eq!(size_of::<RawTelemetryBatch>(), 24);
    assert_eq!(offset_of!(RawTelemetryBatch, event_count), 4);
    assert_eq!(offset_of!(RawTelemetryBatch, payload), 8);
    assert_eq!(offset_of!(RawTelemetryBatch, payload_len), 16);

    assert_eq!(size_of::<RawConsent>(), 40);
    assert_eq!(offset_of!(RawConsent, privacy), 16);
    assert_eq!(offset_of!(RawConsent, endpoint_id), 24);

    assert_eq!(
        (size_of::<RawHostReply>(), align_of::<RawHostReply>()),
        (2, 1)
    );
    assert_eq!(offset_of!(RawHostReply, consent_decision), 1);

    assert_eq!(
        (size_of::<RawOsSignal>(), align_of::<RawOsSignal>()),
        (16, 4)
    );
    assert_eq!(offset_of!(RawOsSignal, thermal_state), 1);
    assert_eq!(offset_of!(RawOsSignal, battery_level_pct), 2);
    assert_eq!(offset_of!(RawOsSignal, battery_charging), 3);
    assert_eq!(offset_of!(RawOsSignal, battery_low_power_mode), 4);
    assert_eq!(offset_of!(RawOsSignal, memory_level), 5);
    assert_eq!(offset_of!(RawOsSignal, app_state), 6);
    assert_eq!(offset_of!(RawOsSignal, has_app_budget_ms), 7);
    assert_eq!(offset_of!(RawOsSignal, app_budget_ms), 8);
    assert_eq!(offset_of!(RawOsSignal, network_state), 12);
    assert_eq!(offset_of!(RawOsSignal, idle), 13);
    assert_eq!(offset_of!(RawOsSignal, main_link_busy), 14);

    // madai-w08 I-C-07 追加（clang 读数：size=152 align=8）。
    assert_eq!(
        (size_of::<RawApplyOutcome>(), align_of::<RawApplyOutcome>()),
        (152, 8)
    );
    assert_eq!(offset_of!(RawApplyOutcome, applied), 0);
    assert_eq!(offset_of!(RawApplyOutcome, active_version), 8);
    assert_eq!(offset_of!(RawApplyOutcome, stage), 16);
    assert_eq!(offset_of!(RawApplyOutcome, has_reject_reason), 17);
    assert_eq!(offset_of!(RawApplyOutcome, reject_reason), 18);
    assert_eq!(offset_of!(RawApplyOutcome, detail_len), 20);
    assert_eq!(offset_of!(RawApplyOutcome, detail), 24);

    // madai-w08 G7 追加 on_worker_started：56 → 64。
    assert_eq!(size_of::<RawHostVtable>(), 64);
    assert_eq!(offset_of!(RawHostVtable, now_ms), 8);
    assert_eq!(offset_of!(RawHostVtable, kv_get), 16);
    assert_eq!(offset_of!(RawHostVtable, kv_put), 24);
    assert_eq!(offset_of!(RawHostVtable, telemetry_send), 32);
    assert_eq!(offset_of!(RawHostVtable, authorize), 40);
    assert_eq!(offset_of!(RawHostVtable, verify_ed25519), 48);
    assert_eq!(offset_of!(RawHostVtable, on_worker_started), 56);
    assert_eq!(crate::WorkerClassRecord::Control as u8, 0);
    assert_eq!(crate::WorkerClassRecord::Background as u8, 2);
    assert_eq!(crate::WorkerClassRecord::Delivery as u8, 3);
    assert!(HEADER.contains("MADINFER_WORKER_BACKGROUND = 2"));
    assert!(HEADER.contains("MADINFER_WORKER_DELIVERY = 3"));
    // madai-w09 I-C-08：§5b 线程类别口径钉在事实源（三端壳照此映射；镜像 sync 门逐字节比对），枚举不拆、不升 ABI。
    assert!(HEADER.contains("BACKGROUND worker → **utility 档，禁 background 档**"));

    assert_eq!(size_of::<RawSinkVtable>(), 40);
    assert_eq!(offset_of!(RawSinkVtable, on_started), 8);
    assert_eq!(offset_of!(RawSinkVtable, on_delta), 16);
    assert_eq!(offset_of!(RawSinkVtable, on_complete), 24);
    assert_eq!(offset_of!(RawSinkVtable, on_failed), 32);
}

#[test]
fn panic_firewall_probe_returns_4002_and_process_survives() {
    assert_eq!(panic_firewall_probe(), 4002);
    assert_eq!(
        panic_firewall_probe(),
        i32::from(InferFfiError::Panicked.code())
    );
}

#[test]
fn os_signal_conversion_covers_seven_kinds_and_rejects_out_of_range() {
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
    assert_eq!(
        os_signal_of(&base),
        Ok(OsSignalRecord::Thermal {
            state: ThermalRecord::Critical
        })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 1, ..base }),
        Ok(OsSignalRecord::Battery {
            level_pct: Some(20),
            charging: false,
            low_power_mode: true
        })
    );
    // madai-w08 G7：255 哨兵 = 未知；101～254 越界。
    assert_eq!(
        os_signal_of(&RawOsSignal {
            kind: 1,
            battery_level_pct: MADINFER_BATTERY_LEVEL_UNKNOWN,
            ..base
        }),
        Ok(OsSignalRecord::Battery {
            level_pct: None,
            charging: false,
            low_power_mode: true
        })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal {
            kind: 1,
            battery_level_pct: 101,
            ..base
        }),
        Err(InferFfiError::InvalidArgument)
    );
    assert!(HEADER.contains(&format!(
        "#define MADINFER_BATTERY_LEVEL_UNKNOWN {MADINFER_BATTERY_LEVEL_UNKNOWN}u"
    )));
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 2, ..base }),
        Ok(OsSignalRecord::Memory {
            level: MemoryPressureRecord::Warning
        })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 3, ..base }),
        Ok(OsSignalRecord::App {
            state: AppStateRecord::Background,
            budget_ms: Some(30_000)
        })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal {
            kind: 3,
            has_app_budget_ms: 0,
            ..base
        }),
        Ok(OsSignalRecord::App {
            state: AppStateRecord::Background,
            budget_ms: None
        })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 4, ..base }),
        Ok(OsSignalRecord::Network {
            state: NetStateRecord::Metered
        })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 5, ..base }),
        Ok(OsSignalRecord::Idle { idle: true })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 6, ..base }),
        Ok(OsSignalRecord::MainLinkBusy { busy: false })
    );
    assert_eq!(
        os_signal_of(&RawOsSignal { kind: 7, ..base }),
        Err(InferFfiError::InvalidArgument)
    );
    assert_eq!(
        os_signal_of(&RawOsSignal {
            kind: 0,
            thermal_state: 4,
            ..base
        }),
        Err(InferFfiError::InvalidArgument)
    );
}

#[test]
fn raw_str_round_trips_utf8_and_rejects_null_with_length() {
    let text = "端侧推理 madinfer";
    let raw = RawStr::borrow(text);
    assert_eq!(raw.len as usize, text.len());
    // SAFETY: raw 借用自存活的 &str。
    assert_eq!(unsafe { raw.to_owned_string() }.unwrap(), text);
    let empty = RawStr {
        ptr: std::ptr::null(),
        len: 0,
    };
    // SAFETY: len = 0 时 ptr 可为 NULL（头文件 §2）。
    assert_eq!(unsafe { empty.to_owned_string() }.unwrap(), "");
    let bad = RawStr {
        ptr: std::ptr::null(),
        len: 3,
    };
    // SAFETY: 非空长度 + 空指针是被拒绝的输入，不解引用。
    assert_eq!(
        unsafe { bad.to_owned_string() },
        Err(InferFfiError::InvalidArgument)
    );
    let invalid = [0xffu8, 0xfe];
    let raw = RawStr {
        ptr: invalid.as_ptr(),
        len: 2,
    };
    // SAFETY: 指向存活的 2 字节数组。
    assert_eq!(
        unsafe { raw.to_owned_string() },
        Err(InferFfiError::InvalidArgument)
    );
}
