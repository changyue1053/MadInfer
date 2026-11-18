//! C 面调试符号（madai-w10 I-C-09 J5-a；madai-w11 I-C-10 起**两 profile 都导出**——契约 `include/madinfer_debug.h`，
//! 与 uniffi 面 [`crate::debug`] 同一份实现）。
//!
//! 符号面：debug / release 产物都多出 `madinfer_core_create_debug` / `madinfer_test_stop_control` 两个 `madinfer_*` 导出
//! （`build_ohos.sh` SYMBOLS.txt 口径 = 12：生产十符号 + 两调试符号，两 profile 同数；madai-w14 I-C-12 前为 11）；
//! `madinfer.h` 的生产符号面只加第 10 符号、ABI 常量、布局全部不变。
//! release 下函数体门控：`create_debug` → NULL + 4005、`test_stop_control` → 4005（形式错误仍先 4001）——门控落在
//! [`InferCore::create_debug`] / [`InferCore::test_stop_control`] 里，本文件不另分叉。
//!
//! 纪律同父模块：`catch_unwind` 收口（panic → 4002）、判空后才解引用、SAFETY 注释逐处。

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use super::{
    create_station, set_error, station, RawCore, RawCoreOptions, RawHostVtable, INVALID_ARGUMENT,
    MADINFER_OK, PANICKED,
};
use crate::{InferCore, InferFfiError};

/// 调试建站（↔ [`InferCore::create_debug`]）：`debug_script_backend` = 1 时站内唯一后端换成 testkit 脚本后端
/// （产出 = 请求最后一条 user 消息原文，空白切词元；缺 required 的合法 JSON prompt → 2003 携 partial），= 0 时与
/// [`super::madinfer_core_create`] 同一装配路径；其余取值回 4001。本构建未开 feature `debug_script_backend` 而传 1 → 4003。
/// release 构建：判空 / 取值校验后恒 NULL + 4005（`MADINFER_ERROR_FEATURE_DISABLED`），宿主 vtable 不被调用。
/// 返回值 / `out_error_code` 语义同 `madinfer_core_create`；句柄同样由 `madinfer_core_destroy` 回收。
///
/// # Safety
///
/// 契约同 [`super::madinfer_core_create`]：`host` / `options` 为 NULL 时回 4001，非空时必须指向有效结构体且内部指针 / 长度对
/// 满足头文件 §2 借用语义；`out_error_code` 为 NULL 或指向可写 `int32_t`。
#[no_mangle]
pub unsafe extern "C" fn madinfer_core_create_debug(
    host: *const RawHostVtable,
    options: *const RawCoreOptions,
    debug_script_backend: u8,
    out_error_code: *mut i32,
) -> *mut RawCore {
    // SAFETY: 契约同本函数。
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
        let script_backend = match debug_script_backend {
            0 => false,
            1 => true,
            _ => return Err(InferFfiError::InvalidArgument),
        };
        create_station(host, options, |bridge, record| {
            InferCore::create_debug(bridge, record, script_backend)
        })
    }));
    let (handle, code) = match outcome {
        Ok(Ok(handle)) => (handle, MADINFER_OK),
        Ok(Err(error)) => (ptr::null_mut(), i32::from(error.code())),
        Err(_) => (ptr::null_mut(), PANICKED),
    };
    // SAFETY: out_error_code 为 NULL 或可写 int32_t（函数级 Safety）。
    unsafe { set_error(out_error_code, code) };
    handle
}

/// 调试入口（↔ [`InferCore::test_stop_control`]；I-S-05 U-4）：让 control 线程退出但保留句柄——返回后
/// `madinfer_apply_config` 恒 4004，`madinfer_stream_start` 仍发句柄但无任何回调（故障态）；幂等；`madinfer_core_destroy`
/// 照常回收。`core` 为 NULL 回 4001，其余恒 0；核内 panic 收口回 4002。生产建站（`madinfer_core_create`）的句柄同样可停。
/// release 构建：`core` 非 NULL 恒回 4005、不停线程。
///
/// # Safety
///
/// `core` 为 NULL 或由 `madinfer_core_create` / [`madinfer_core_create_debug`] 发放且未回收的句柄。
#[no_mangle]
pub unsafe extern "C" fn madinfer_test_stop_control(core: *mut RawCore) -> i32 {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 句柄有效性由宿主契约保证。
        let Some(station) = (unsafe { station(core) }) else {
            return INVALID_ARGUMENT;
        };
        match station.core.test_stop_control() {
            Ok(()) => MADINFER_OK,
            Err(error) => i32::from(error.code()),
        }
    }));
    outcome.unwrap_or(PANICKED)
}
