//! ConfigStore 应用管线测试：设计文档 §5.4 P4 的应用条件逐条判红 / 放行；原子切换与回滚。

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use madinfer_config::{
    build_apply_fsm, ApplyOutcome, ApplyStage, ConfigEffect, ConfigStore, Registry, RejectReason,
    TrustAllVerifier, APPLY_FSM_TABLE_SIZE,
};
use madinfer_core_types::{BackendId, CapabilityId};
use madinfer_router::Target;
use semver::Version;

fn doc(version: u64, buckets: [u16; 2], min_sdk: &str, extra_when: &str) -> Vec<u8> {
    format!(
        r#"{{
  "schema": 1, "version": {version}, "issued_at": "2026-09-04T02:00:00Z", "min_sdk": "{min_sdk}",
  "rollout": {{ "buckets": [{}, {}] }},
  "mapping": [
    {{ "capability": "summarize",
      "candidates": [
        {{ "priority": 0, "target": {{ "kind": "system", "backend": "ios.foundation_models" }}, "when": {{ "os": "ios", "availability": "available" }} }},
        {{ "priority": 1, "target": {{ "kind": "bundled", "backend": "llama.cpp", "package": "catchup-summary@1.4.2" }}, "when": {{ "tier_min": "mid" {extra_when} }} }},
        {{ "priority": 2, "target": {{ "kind": "cloud", "endpoint": "cloud.enterprise" }}, "when": {{}} }}
      ] }},
    {{ "capability": "embed.text",
      "candidates": [ {{ "priority": 0, "target": {{ "kind": "bundled", "backend": "llama.cpp", "package": "embed-bge-s@2.0.0" }}, "when": {{ "tier_min": "low" }} }} ] }}
  ],
  "thresholds": {{
    "deadline_ms": {{ "interactive": 3000, "async": 20000, "background": 120000 }},
    "frame_ms": 80,
    "kv_budget_mb": {{ "flagship": 512, "mid": 256, "low": 0 }},
    "quotas": {{ "1": {{ "window_s": 3600, "cost_ms_mb": 600000000 }} }},
    "thermal": {{ "background_max": "fair" }},
    "gc_keep_days": 7,
    "future_field_ignored": true
  }},
  "switches": {{ "capabilities": {{ "summarize": true, "transcribe": false }}, "cloud_only": false, "all_off": false }},
  "signature": {{ "alg": "ed25519", "key_id": "pk-platform-2026-01", "sig": "c2ln" }}
}}"#,
        buckets[0], buckets[1]
    )
    .into_bytes()
}

fn registry() -> Registry {
    Registry {
        capabilities: ["summarize", "embed.text", "transcribe"]
            .into_iter()
            .map(|s| CapabilityId::new(s).expect("id"))
            .collect(),
        backends: ["ios.foundation_models", "android.aicore", "llama.cpp"]
            .into_iter()
            .map(|s| BackendId(s.to_owned()))
            .collect::<BTreeSet<_>>(),
        endpoints: ["cloud.enterprise".to_owned()].into_iter().collect(),
    }
}

fn manager(bucket: u16) -> ConfigStore {
    ConfigStore::new(
        bucket,
        Version::new(2, 3, 0),
        Box::new(TrustAllVerifier),
        registry(),
    )
}

#[test]
fn apply_fsm_table_is_locked() {
    assert_eq!(build_apply_fsm().table_size(), APPLY_FSM_TABLE_SIZE);
}

#[test]
fn valid_document_is_applied_atomically_with_persist_and_broadcast() {
    let mut m = manager(137);
    let (outcome, effects) = m.apply(&doc(20260904003, [0, 199], ">=2.3.0", ""), 1_000);
    assert_eq!(
        outcome,
        ApplyOutcome::Applied {
            version: 20260904003
        }
    );
    assert_eq!(m.active().version, 20260904003);
    assert_eq!(
        m.previous().expect("previous").version,
        0,
        "编译期缺省进 previous"
    );
    assert_eq!(m.active_table().entries.len(), 2);
    let summarize = m
        .active_table()
        .entry(&CapabilityId::new("summarize").expect("id"))
        .expect("entry");
    assert!(matches!(
        summarize.candidates[2].target,
        Target::Cloud { .. }
    ));
    assert!(summarize.candidates[0].when.requires_available);
    assert!(effects
        .iter()
        .any(|e| matches!(e, ConfigEffect::PersistActive(_))));
    assert!(effects
        .iter()
        .any(|e| matches!(e, ConfigEffect::PersistPrevious(_))));
    assert!(effects.contains(&ConfigEffect::Broadcast {
        version: 20260904003
    }));
    assert!(effects.contains(&ConfigEffect::RecordApplied {
        version: 20260904003
    }));
    assert_eq!(
        m.last_trace(),
        [
            "Received --SignatureAccepted--> Verified @1000ms",
            "Verified --ParseSucceeded--> Parsed @1000ms",
            "Parsed --ValidationPassed--> Validated @1000ms",
            "Validated --Switched--> Applied @1000ms",
        ]
    );
    let switches = m.active().router_switches();
    assert!(switches
        .disabled_capabilities
        .contains(&CapabilityId::new("transcribe").expect("id")));
    assert_eq!(m.stats().applied, 1);
}

#[test]
fn every_application_condition_rejects_independently() {
    // 版本不递增
    let mut m = manager(10);
    m.apply(&doc(5, [0, 999], ">=2.0.0", ""), 0);
    let (o, fx) = m.apply(&doc(5, [0, 999], ">=2.0.0", ""), 1);
    assert_eq!(
        o,
        ApplyOutcome::Rejected {
            reason: RejectReason::VersionNotNewer {
                received: 5,
                active: 5
            },
            stage: ApplyStage::Parsed
        }
    );
    assert!(fx.contains(&ConfigEffect::RecordRejected {
        version: Some(5),
        reason: "version_not_newer"
    }));
    assert_eq!(m.active().version, 5, "拒绝不改活动配置");

    // min_sdk
    let (o, _) = manager(10).apply(&doc(6, [0, 999], ">=3.0.0", ""), 0);
    assert!(matches!(
        o,
        ApplyOutcome::Rejected {
            reason: RejectReason::MinSdkUnsatisfied(_),
            ..
        }
    ));

    // 分桶
    let (o, _) = manager(500).apply(&doc(6, [0, 199], ">=2.0.0", ""), 0);
    assert_eq!(
        o,
        ApplyOutcome::Rejected {
            reason: RejectReason::OutOfRollout { bucket: 500 },
            stage: ApplyStage::Parsed
        }
    );

    // 语义：when 含 privacy（隐私门禁区）
    let (o, _) = manager(10).apply(&doc(6, [0, 999], ">=2.0.0", r#", "privacy": "public""#), 0);
    assert!(
        matches!(
            &o,
            ApplyOutcome::Rejected {
                reason: RejectReason::Semantic(madinfer_config::doc::SemanticError::PrivacyInWhen(
                    _
                )),
                ..
            }
        ),
        "{o:?}"
    );

    // 语义：未注册后端
    let mut m = manager(10);
    let bytes = String::from_utf8(doc(6, [0, 999], ">=2.0.0", ""))
        .expect("utf8")
        .replace("llama.cpp", "mlx");
    let (o, _) = m.apply(bytes.as_bytes(), 0);
    assert!(
        matches!(&o, ApplyOutcome::Rejected { reason: RejectReason::Semantic(madinfer_config::doc::SemanticError::UnknownBackend(b)), .. } if b == "mlx")
    );

    // 形式：frame_ms 越界 → 解析阶段
    let bytes = String::from_utf8(doc(6, [0, 999], ">=2.0.0", ""))
        .expect("utf8")
        .replace(r#""frame_ms": 80"#, r#""frame_ms": 5"#);
    let (o, _) = manager(10).apply(bytes.as_bytes(), 0);
    assert_eq!(
        o,
        ApplyOutcome::Rejected {
            reason: RejectReason::Semantic(
                madinfer_config::doc::SemanticError::ThresholdOutOfRange("frame_ms")
            ),
            stage: ApplyStage::Verified
        }
    );

    // 验签：sig 为空 → TrustAll 也拒
    let bytes = String::from_utf8(doc(6, [0, 999], ">=2.0.0", ""))
        .expect("utf8")
        .replace(r#""sig": "c2ln""#, r#""sig": """#);
    let (o, fx) = manager(10).apply(bytes.as_bytes(), 0);
    assert_eq!(
        o,
        ApplyOutcome::Rejected {
            reason: RejectReason::SignatureInvalid { timed_out: false },
            stage: ApplyStage::Received
        }
    );
    assert!(fx.contains(&ConfigEffect::RecordRejected {
        version: None,
        reason: "signature_invalid"
    }));

    // 非法 JSON
    let (o, _) = manager(10).apply(b"{ not json", 0);
    assert!(matches!(
        o,
        ApplyOutcome::Rejected {
            reason: RejectReason::SchemaInvalid(_),
            stage: ApplyStage::Received
        }
    ));
}

#[test]
fn rollback_swaps_active_and_previous() {
    let mut m = manager(10);
    m.apply(&doc(1, [0, 999], ">=2.0.0", ""), 0);
    m.apply(&doc(2, [0, 999], ">=2.0.0", ""), 1);
    assert_eq!(m.active().version, 2);
    let fx = m.rollback().expect("has previous");
    assert_eq!(m.active().version, 1);
    assert!(m.previous().is_none(), "回滚一次只回一步");
    assert!(fx.contains(&ConfigEffect::Broadcast { version: 1 }));
    assert_eq!(m.active_table().entries.len(), 2, "回滚后映射表随之切回");
    assert!(m.rollback().is_none());
    assert_eq!(m.stats().rolled_back, 1);
}

#[test]
fn bucket_is_stable_and_in_range() {
    let a = ConfigStore::bucket_from_install_id(b"install-0001");
    let b = ConfigStore::bucket_from_install_id(b"install-0001");
    assert_eq!(a, b);
    assert!(a < 1000);
    assert_ne!(a, ConfigStore::bucket_from_install_id(b"install-0002"));
}

#[test]
fn bucket_is_xxh3_64_mod_1000_with_pinned_goldens() {
    // madai-w06 E2：金样钉住协议算法（xxhash-rust 的 xxh3-64 % 1000）；换实现 / 升版本读数漂移即红——
    // 分桶跨端一致是契约，不是实现细节。
    let cases: [(&[u8], u16); 5] = [
        (b"", 138),
        (b"iminfer", 722),
        (b"install-0001", 412),
        (b"9f86d081-a3c2-4f1e", 880),
        ("催更".as_bytes(), 990),
    ];
    for (input, expected) in cases {
        assert_eq!(ConfigStore::bucket_from_install_id(input), expected);
    }
    assert!(u32::from(ConfigStore::bucket_from_install_id(b"any")) < 1_000);
}
