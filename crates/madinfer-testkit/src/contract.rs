//! 后端契约测试（设计文档 §10.1 / C19）：对任意 [`Backend`] 实现跑同一组用例。用 [`backend_contract_tests!`] 宏实例化，
//! 每条用例也可单独调用（`&dyn Backend`）。
//!
//! 六条口径：
//! 1. **abort 生效时延 ≤ 1 token**——sink 在收到第 k 片时翻旗，之后 sink 至多再收 1 片；若翻旗时还未到最后一片，`generate` 必返 `Cancelled`。
//! 2. **取消后无 token 到 sink**——调用前已翻旗：sink 零片、返回 `Cancelled`。
//! 3. **约束声明与行为一致**——`caps.constraint == None`：传入约束只准返回 `Ok`（忽略）或 `Err(Unsupported)`；
//!    `Native / Emulated`：传入约束必须 `Ok`。
//! 4. **并发上限被遵守**——同一 Model 打开 `caps.concurrency` 个上下文全部成功，第 N+1 个返回 `Busy`；释放一个后再开成功。
//! 5. **错误映射正确**——每个 `BackendError` 变体经 `From` 落入三分类正确码段（与后端实现无关，宏仍为每个实现生成一次以锁表）。
//! 6. **footprint 报告单调**——`load` 返回后 `mapped_mb ≥ resident_mb`；打开上下文后 `resident_mb` 不减少、仍 `mapped ≥ resident`。

use std::ops::Range;

use madinfer_backend::{
    Backend, BackendError, CancelReason, CancelToken, Constraint, ConstraintSupport,
    ContextOptions, GenerateOptions, LoadOpts, Turn,
};
use madinfer_core_types::InferError;

use crate::backend::CollectingSink;

/// 三分类码段（附录 B）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InferErrorBand {
    /// 1xxx。
    Retryable,
    /// 2xxx。
    Degradable,
    /// 3xxx。
    Fatal,
}

impl InferErrorBand {
    /// 某错误所属分类。
    pub const fn of(error: &InferError) -> Self {
        match error {
            InferError::Retryable { .. } => Self::Retryable,
            InferError::Degradable { .. } => Self::Degradable,
            InferError::Fatal { .. } => Self::Fatal,
        }
    }

    /// 码段。
    pub const fn range(self) -> Range<u16> {
        match self {
            Self::Retryable => 1000..2000,
            Self::Degradable => 2000..3000,
            Self::Fatal => 3000..4000,
        }
    }
}

fn load_model(backend: &dyn Backend) -> Box<dyn madinfer_backend::Model> {
    match backend.load(&LoadOpts::default()) {
        Ok(model) => model,
        Err(error) => panic!("contract: load failed with {error:?}"),
    }
}

fn open(model: &dyn madinfer_backend::Model) -> Box<dyn madinfer_backend::Context> {
    match model.open_context(&ContextOptions::default()) {
        Ok(context) => context,
        Err(error) => panic!("contract: open_context failed with {error:?}"),
    }
}

/// ① abort 生效时延 ≤ 1 token。
pub fn abort_within_one_token(backend: &dyn Backend) {
    let model = load_model(backend);
    let mut context = open(model.as_ref());
    let cancel = CancelToken::new();
    let trigger = 2usize;
    let mut sink = CollectingSink {
        cancel_on_piece: Some((trigger, cancel.clone())),
        ..CollectingSink::default()
    };
    let result = context.generate(
        &Turn::user("contract"),
        &GenerateOptions::default(),
        None,
        &mut sink,
        &cancel,
    );
    let received = sink.pieces.len();
    if received >= trigger {
        assert!(
            received <= trigger + 1,
            "翻旗后 sink 多收了 {} 片",
            received - trigger
        );
        let last_seen = sink.pieces.get(trigger - 1).is_some_and(|p| p.is_last);
        if !last_seen {
            assert_eq!(
                result.err(),
                Some(BackendError::Cancelled),
                "翻旗时未到最后一片，必须返回 Cancelled"
            );
        }
    } else {
        assert!(result.is_ok(), "未触发取消的脚本应成功：{result:?}");
    }
    model.unload();
}

/// ② 取消后无 token 到 sink。
pub fn no_tokens_after_cancel(backend: &dyn Backend) {
    let model = load_model(backend);
    let mut context = open(model.as_ref());
    let cancel = CancelToken::new();
    cancel.cancel(CancelReason::Caller);
    let mut sink = CollectingSink::new();
    let result = context.generate(
        &Turn::user("contract"),
        &GenerateOptions::default(),
        None,
        &mut sink,
        &cancel,
    );
    assert!(
        sink.pieces.is_empty(),
        "已取消仍产出 {} 片",
        sink.pieces.len()
    );
    assert_eq!(result.err(), Some(BackendError::Cancelled));
    model.unload();
}

/// ③ 约束声明与行为一致。
pub fn constraint_matches_caps(backend: &dyn Backend) {
    let caps = backend.probe();
    let model = load_model(backend);
    let mut context = open(model.as_ref());
    let constraint = Constraint {
        grammar: "root ::= \"ok\"".to_owned(),
    };
    let mut sink = CollectingSink::new();
    let result = context.generate(
        &Turn::user("contract"),
        &GenerateOptions::default(),
        Some(&constraint),
        &mut sink,
        &CancelToken::new(),
    );
    match caps.constraint {
        ConstraintSupport::None => assert!(
            matches!(result, Ok(_) | Err(BackendError::Unsupported)),
            "声明不支持约束：只准忽略或 Unsupported，实际 {result:?}"
        ),
        ConstraintSupport::Native | ConstraintSupport::Emulated => {
            assert!(result.is_ok(), "声明支持约束却失败：{result:?}")
        }
    }
    model.unload();
}

/// ④ 并发上限被遵守。
pub fn concurrency_limit_is_enforced(backend: &dyn Backend) {
    let caps = backend.probe();
    assert!(caps.concurrency >= 1, "concurrency 必须 ≥ 1");
    let model = load_model(backend);
    let mut held = Vec::new();
    for i in 0..caps.concurrency {
        match model.open_context(&ContextOptions::default()) {
            Ok(context) => held.push(context),
            Err(error) => panic!(
                "第 {} 个上下文（上限 {}）被拒：{error:?}",
                i + 1,
                caps.concurrency
            ),
        }
    }
    assert_eq!(
        model
            .open_context(&ContextOptions::default())
            .err()
            .map(|_| ()),
        Some(()),
        "超出上限应失败"
    );
    match model.open_context(&ContextOptions::default()) {
        Err(BackendError::Busy) => {}
        other => panic!("超出上限应返回 Busy，实际 {:?}", other.err()),
    }
    drop(held.pop());
    assert!(
        model.open_context(&ContextOptions::default()).is_ok(),
        "释放一个后应可再开"
    );
    drop(held);
    model.unload();
}

/// ⑤ 错误映射正确：每个变体落入正确码段，且分类与 §7.3 表一致。
pub fn errors_map_to_stable_codes() {
    for error in BackendError::ALL {
        let mapped = InferError::from(error);
        let code = mapped.code();
        let band = match error {
            BackendError::Busy
            | BackendError::Transient
            | BackendError::Network
            | BackendError::OutOfMemory => InferErrorBand::Retryable,
            BackendError::Timeout | BackendError::Refused => InferErrorBand::Degradable,
            BackendError::Unsupported
            | BackendError::ModelMissing
            | BackendError::ModelCorrupt
            | BackendError::Crashed
            | BackendError::Cancelled
            | BackendError::InvalidInput => InferErrorBand::Fatal,
        };
        assert_eq!(InferErrorBand::of(&mapped), band, "{error:?} → {mapped:?}");
        assert!(band.range().contains(&code), "{error:?} 码 {code} 越出码段");
    }
}

/// ⑥ footprint 报告单调。
pub fn footprint_is_monotone(backend: &dyn Backend) {
    let model = load_model(backend);
    let before = model.footprint();
    assert!(
        before.mapped_mb >= before.resident_mb,
        "load 后 mapped {} < resident {}",
        before.mapped_mb,
        before.resident_mb
    );
    let context = open(model.as_ref());
    let after = model.footprint();
    assert!(after.mapped_mb >= after.resident_mb);
    assert!(
        after.resident_mb >= before.resident_mb,
        "打开上下文后驻留不应减少"
    );
    drop(context);
    model.unload();
}

/// 为任意 Backend 实现生成六条契约 `#[test]`：`backend_contract_tests!(my_backend => FakeBackend::new("x", FakeScript::tokens(&["a", "b", "c"])));`
///
/// `$ctor` 每条用例各求值一次，须得到实现 `madinfer_backend::Backend` 的值（`Arc<dyn Backend>` / `Box<dyn Backend>` 亦可）。
/// 调用方 crate 需依赖 `madinfer-backend` 与 `madinfer-testkit`。
#[macro_export]
macro_rules! backend_contract_tests {
    ($name:ident => $ctor:expr) => {
        #[allow(non_snake_case)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;

            #[test]
            fn contract_1_abort_within_one_token() {
                let backend = $ctor;
                $crate::contract::abort_within_one_token(&backend);
            }

            #[test]
            fn contract_2_no_tokens_after_cancel() {
                let backend = $ctor;
                $crate::contract::no_tokens_after_cancel(&backend);
            }

            #[test]
            fn contract_3_constraint_matches_caps() {
                let backend = $ctor;
                $crate::contract::constraint_matches_caps(&backend);
            }

            #[test]
            fn contract_4_concurrency_limit_is_enforced() {
                let backend = $ctor;
                $crate::contract::concurrency_limit_is_enforced(&backend);
            }

            #[test]
            fn contract_5_errors_map_to_stable_codes() {
                let _backend = $ctor;
                $crate::contract::errors_map_to_stable_codes();
            }

            #[test]
            fn contract_6_footprint_is_monotone() {
                let backend = $ctor;
                $crate::contract::footprint_is_monotone(&backend);
            }
        }
    };
}
