//! 调试面（madai-w10 I-C-09 J5-a；madai-w11 I-C-10 起**导出无条件、函数体门控**）。
//!
//! 两个入口都是三端壳此前"造不出、只能桩验"的口子（I-S-05 反馈 U-2 / U-4）：
//!
//! - [`InferCore::create_debug`]：建站选项 `debug_script_backend`——为真时以 madinfer-testkit `FakeBackend`
//!   （`FakeScript::echo_prompt`，经 feature `debug_script_backend` 拉进调试构建）替代 backend-llama 仿真核，站内其余装配
//!   （映射 / 槽表 / 验签 / 授权 / 观测）与生产完全相同：后端产出 = 请求最后一条 user 消息原文，按空白切词元。
//!   给结构化请求送"合法 JSON 但缺 required 字段"的 prompt → Started → 终态校验不过 → `on_failed`(2003) 携 partial（2003 之二）；
//!   送齐全 JSON → Complete 携结构化输出；文本请求原文回显。为假时与 [`InferCore::create`] 同一装配路径（probe 逐字段相同）。
//! - [`InferCore::test_stop_control`]：让 control 线程退出但保留对象（[`madinfer_api::CoreHandle::stop_control`]），返回后
//!   `apply_config` 恒 4004、`stream` 无任何回调；幂等；`destroy` / Drop 照常 join。
//! - [`InferCore::debug_set_token_delay_ms`]（madai-w12 I-C-11，I-D-10 §7-2"仿真核无逐词元节拍、Delta 后取消携 partial 三端只能
//!   靶验"）：给脚本后端站定逐词元节拍——之后每轮生成的片间 `thread::sleep(token_delay_ms)`（testkit `FakeScript::token_delay_ms`，
//!   等待分 ≤ 5 ms 切片查取消旗），节拍 > 合帧窗（`CoreOptions.frame_ms`，缺省 80）即一词元一帧：壳侧收到首个 Delta 后 cancel，
//!   确定性得到 Failed(3008) 携 partial = 已出 Delta 拼接。只在 uniffi 面；C 面**不加符号**（`build_ohos.sh` 11 不变——第 12 符号
//!   `madinfer_debug_set_token_delay_ms` 是上游提案，见 `Mad/madai-w12-report-i-c-11.md` §7-1）。
//! - [`InferCore::debug_set_verify_deadline_ms`]（madai-w16 I-C-14，I-D-14 §7"`detail_kind = VerifyTimeout` / `stats().verify_timeouts`
//!   只能靶映射，撞 deadline 依赖慢机器"）：给**本站验签器**（`trusted_keys` 非空建的站）改 `apply_config` 等宿主验签回执的
//!   deadline（缺省 [`crate::VERIFY_DEADLINE_MS`] = 2 000 ms）——deadline 1 ms + 卡住的宿主 `verify_ed25519` 即确定性得到
//!   `Rejected(SignatureInvalid, detail_kind = VerifyTimeout)` 且 `stats().verify_timeouts` +1（两者同一处 `fetch_add`）。`0` 恢复缺省。
//!   与 `debug_set_token_delay_ms` 同口径：只在 uniffi 面、C 面不加符号、release 恒 4005。
//!
//! 门控形态（madai-w11 I-C-10，w11 首日 §0；参照 mad imcore `attach_tcp_link` 的 `FeatureDisabled`）：uniffi 导出与 C 符号在
//! debug / release 构建里**都存在**——单核形态下 KMP 绑定自 debug 宿主 cdylib 生成、release .so 缺符号会让 JNA 整库装载失败。
//! release 下四个入口的函数体只回 [`InferFfiError::FeatureDisabled`]（4005），脚本后端与 testkit 仍被 `cfg(debug_assertions)`
//! 擦除、不进生产产物。于是 `bindgen_*.sh` 从任一 profile 生成同一份绑定（脚本内 `diff -r` 钉住）。
//! C 面对应符号见 [`crate::cabi::debug`] 与 `include/madinfer_debug.h`。

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use crate::{CoreOptions, HostBridge, InferCore, InferFfiError};

/// 脚本后端的注册制 id（telemetry `backend=` 字段可见，与仿真核 `llama.cpp` 区分；probe 条目不携后端 id，形态不变）。
pub const DEBUG_SCRIPT_BACKEND_ID: &str = "debug_script";

/// [`InferCore::debug_set_token_delay_ms`] 的上限（含）：超过回 4001。防的事故——worker 线程按节拍 sleep，无界取值会让一轮
/// 生成挂到天荒地老、`destroy` 的 join 跟着挂；10 s 已远超任何"取消赢过仿真核"用例所需（合帧窗上限 500 ms 的 20 倍）。
pub const DEBUG_TOKEN_DELAY_MAX_MS: u64 = 10_000;

/// [`InferCore::debug_set_verify_deadline_ms`] 的上限（含）：超过回 4001。防的事故——deadline 是 control 线程等宿主验签的上限，
/// 无界取值等于回到 madai-w11 之前"宿主验签卡死拖死核站"；10 s 是缺省 2 000 ms 的 5 倍，足够"慢验签在期限内回执"一类用例。
pub const DEBUG_VERIFY_DEADLINE_MAX_MS: u64 = 10_000;

/// 调试建站留下的脚本后端句柄：debug 构建 + feature `debug_script_backend` 且 `debug_script_backend = true` 建的站才有内容；
/// 生产站（`create` / `create_debug(false)`）与 release 构建恒空（release 下本类型是零大小空壳，testkit 不进产物）。
#[derive(Default)]
pub(crate) struct DebugScript {
    #[cfg(all(debug_assertions, feature = "debug_script_backend"))]
    backend: Option<Arc<madinfer_testkit::FakeBackend>>,
}

#[uniffi::export]
impl InferCore {
    /// 调试建站（符号两 profile 都在）：debug 构建下 `debug_script_backend` 为真 → 站内唯一后端换成脚本后端（模块注）；为假 →
    /// 同 [`Self::create`]；本构建未开 feature `debug_script_backend` 而传真 → 4003（建站失败：本构建无脚本后端）；
    /// 形式错误 4001、panic 4002 同 `create`。**release 构建恒 4005 [`InferFfiError::FeatureDisabled`]**（不建站、不碰宿主）。
    #[uniffi::constructor]
    pub fn create_debug(
        host: Box<dyn HostBridge>,
        options: CoreOptions,
        debug_script_backend: bool,
    ) -> Result<Arc<Self>, InferFfiError> {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            build_debug(host, options, debug_script_backend)
        }));
        match outcome {
            Ok(result) => result.map(Arc::new),
            Err(_) => Err(InferFfiError::Panicked),
        }
    }

    /// 调试入口（符号两 profile 都在；I-S-05 U-4）：debug 构建下让 control 线程退出但保留本对象——返回后 [`Self::apply_config`]
    /// 恒 4004，`stream` / `cancel` / `signal` 静默无回调（故障态，不是 K1 违约）；幂等；`destroy` / Drop 照常 join；
    /// 恒 `Ok(())`，核内 panic 收口 4002。**release 构建恒 4005 [`InferFfiError::FeatureDisabled`]**（不停线程）。
    pub fn test_stop_control(&self) -> Result<(), InferFfiError> {
        catch_unwind(AssertUnwindSafe(|| stop_control(self)))
            .unwrap_or(Err(InferFfiError::Panicked))
    }

    /// 调试入口（符号两 profile 都在；madai-w12 I-C-11，I-D-10 §7-2）：给**脚本后端站**（[`Self::create_debug`] 且
    /// `debug_script_backend = true`）定逐词元节拍 `token_delay_ms`——对之后开始的每轮生成生效（进行中的一轮在下一片前读到新值），
    /// 片间 worker 线程 `sleep`（分 ≤ 5 ms 切片查取消旗，cancel 后不再拖满一个节拍）；节拍 > 合帧窗（`CoreOptions.frame_ms`，
    /// 缺省 80 ms）即一词元一帧，壳侧"首个 Delta 后 cancel → Failed(3008) 携 partial"从靶验变成确定性用例。`0` 恢复无节拍。
    /// 形式：`token_delay_ms` > [`DEBUG_TOKEN_DELAY_MAX_MS`]（10 000）→ 4001；本句柄不是脚本后端站（`create` /
    /// `create_debug(.., false)` 建的站，或本构建未开 feature `debug_script_backend`）→ 4001，不改任何状态；成功恒 `Ok(())`，
    /// 幂等；核内 panic 收口 4002。**release 构建恒 4005 [`InferFfiError::FeatureDisabled`]**。只在 uniffi 面，C 面无对应符号。
    pub fn debug_set_token_delay_ms(&self, token_delay_ms: u64) -> Result<(), InferFfiError> {
        catch_unwind(AssertUnwindSafe(|| {
            set_token_delay_ms(self, token_delay_ms)
        }))
        .unwrap_or(Err(InferFfiError::Panicked))
    }

    /// 调试入口（符号两 profile 都在；madai-w16 I-C-14，I-D-14 §7）：给**本站验签器**改 `apply_config` 等宿主 `verify_ed25519`
    /// 回执的 deadline 毫秒——对之后的每次 `apply_config` 生效（进行中的一次按其开始时读到的值）；`0` 恢复缺省
    /// [`crate::VERIFY_DEADLINE_MS`]。deadline 1 ms + 卡住的宿主验签 → `Rejected(SignatureInvalid, detail_kind = VerifyTimeout)`、
    /// `stats().verify_timeouts` +1，"撞 deadline"不再靶慢机器。
    /// 形式：`deadline_ms` > [`DEBUG_VERIFY_DEADLINE_MAX_MS`]（10 000）→ 4001；本站无验签器（`trusted_keys` 空建的站）→ 4001，
    /// 不改任何状态；成功恒 `Ok(())`，幂等；核内 panic 收口 4002。**release 构建恒 4005 [`InferFfiError::FeatureDisabled`]**。
    /// 只在 uniffi 面，C 面无对应符号。
    pub fn debug_set_verify_deadline_ms(&self, deadline_ms: u64) -> Result<(), InferFfiError> {
        catch_unwind(AssertUnwindSafe(|| {
            set_verify_deadline_ms(self, deadline_ms)
        }))
        .unwrap_or(Err(InferFfiError::Panicked))
    }
}

/// debug 构建：先校形式（上限），再看本站有没有验签器；`0` 恢复缺省。
#[cfg(debug_assertions)]
fn set_verify_deadline_ms(core: &InferCore, deadline_ms: u64) -> Result<(), InferFfiError> {
    if deadline_ms > DEBUG_VERIFY_DEADLINE_MAX_MS {
        return Err(InferFfiError::InvalidArgument);
    }
    let Some(deadline) = &core.verify_deadline_ms else {
        return Err(InferFfiError::InvalidArgument);
    };
    let effective = if deadline_ms == 0 {
        crate::VERIFY_DEADLINE_MS
    } else {
        deadline_ms
    };
    deadline.store(effective, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// release 构建：函数体门控，恒 4005、不碰任何状态。
#[cfg(not(debug_assertions))]
fn set_verify_deadline_ms(_core: &InferCore, _deadline_ms: u64) -> Result<(), InferFfiError> {
    Err(InferFfiError::FeatureDisabled)
}

/// debug 构建：真装配路径；脚本后端站把后端句柄留在 [`InferCore::debug_script`] 供 `debug_set_token_delay_ms` 用。
#[cfg(debug_assertions)]
fn build_debug(
    host: Box<dyn HostBridge>,
    options: CoreOptions,
    debug_script_backend: bool,
) -> Result<InferCore, InferFfiError> {
    if debug_script_backend {
        // 脚本后端站的 probe / stats 形态回 Emulated；要真实引擎就走 create（madai-w17 I-C-16）——两者同时要是形式错误。
        if options.backend != crate::BackendKindRecord::Emulated {
            return Err(InferFfiError::InvalidArgument);
        }
        let (backend, script) = script_backend()?;
        let mut core = InferCore::build_with(host, options, backend)?;
        core.debug_script = script;
        Ok(core)
    } else {
        InferCore::build(host, options)
    }
}

#[cfg(debug_assertions)]
impl DebugScript {
    /// 定节拍：脚本后端站改 `FakeScript::token_delay_ms`；非脚本后端站（含本构建未开 feature）→ 4001。
    fn set_token_delay_ms(&self, token_delay_ms: u64) -> Result<(), InferFfiError> {
        #[cfg(feature = "debug_script_backend")]
        if let Some(backend) = &self.backend {
            backend.set_token_delay_ms(token_delay_ms);
            return Ok(());
        }
        let _ = token_delay_ms;
        Err(InferFfiError::InvalidArgument)
    }
}

/// debug 构建：定节拍——先校形式（上限），再看句柄是否脚本后端站。
#[cfg(debug_assertions)]
fn set_token_delay_ms(core: &InferCore, token_delay_ms: u64) -> Result<(), InferFfiError> {
    if token_delay_ms > DEBUG_TOKEN_DELAY_MAX_MS {
        return Err(InferFfiError::InvalidArgument);
    }
    core.debug_script.set_token_delay_ms(token_delay_ms)
}

/// release 构建：函数体门控，恒 4005、不碰任何状态。
#[cfg(not(debug_assertions))]
fn set_token_delay_ms(_core: &InferCore, _token_delay_ms: u64) -> Result<(), InferFfiError> {
    Err(InferFfiError::FeatureDisabled)
}

/// release 构建：函数体门控，恒 4005；入参丢弃、宿主未被触碰（不写 install 标记）。
#[cfg(not(debug_assertions))]
fn build_debug(
    _host: Box<dyn HostBridge>,
    _options: CoreOptions,
    _debug_script_backend: bool,
) -> Result<InferCore, InferFfiError> {
    Err(InferFfiError::FeatureDisabled)
}

/// debug 构建：停 control 线程。
#[cfg(debug_assertions)]
fn stop_control(core: &InferCore) -> Result<(), InferFfiError> {
    core.handle.stop_control();
    Ok(())
}

/// release 构建：函数体门控，恒 4005、不停线程。
#[cfg(not(debug_assertions))]
fn stop_control(_core: &InferCore) -> Result<(), InferFfiError> {
    Err(InferFfiError::FeatureDisabled)
}

/// 脚本后端：能力声明对齐仿真核（Emulated 约束、可中止、CPU 槽），probe 形态与生产站一致。返回站内后端与留给
/// `debug_set_token_delay_ms` 的同一句柄。
#[cfg(all(debug_assertions, feature = "debug_script_backend"))]
fn script_backend() -> Result<(Arc<dyn madinfer_backend::Backend>, DebugScript), InferFfiError> {
    use madinfer_backend::{BackendCaps, ConstraintSupport};
    use madinfer_testkit::{default_caps, FakeBackend, FakeScript};
    let backend = Arc::new(
        FakeBackend::new(DEBUG_SCRIPT_BACKEND_ID, FakeScript::echo_prompt()).with_caps(
            BackendCaps {
                constraint: ConstraintSupport::Emulated,
                ..default_caps()
            },
        ),
    );
    Ok((
        Arc::clone(&backend) as Arc<dyn madinfer_backend::Backend>,
        DebugScript {
            backend: Some(backend),
        },
    ))
}

/// feature 关掉的调试构建：没有脚本后端可装，建站失败 4003（与"生产构建缺验签器"同码同义：本构建装不出这一站）。
#[cfg(all(debug_assertions, not(feature = "debug_script_backend")))]
fn script_backend() -> Result<(Arc<dyn madinfer_backend::Backend>, DebugScript), InferFfiError> {
    Err(InferFfiError::CoreStartFailed)
}
