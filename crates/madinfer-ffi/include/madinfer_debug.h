/*
 * madinfer_debug.h —— MadInfer C ABI 的**调试面**（madai-w10 I-C-09 J5-a；I-S-05 反馈 U-2 / U-4）。
 *
 * madai-w11 I-C-10 起：下列两个符号在 madinfer-ffi 的 debug 与 release 产物里**都导出**（"导出无条件声明、函数体门控"，
 * 形态同 mad imcore attach_tcp_link）——单核形态下 KMP 绑定自 debug 宿主 cdylib 生成、release .so 若缺符号会让 JNA 整库
 * 装载失败（w11 首日 §0），故符号面两 profile 同数（build_ohos.sh 断言 madinfer_* = 12：生产十符号 + 本文件两符号；madai-w14 前为 11）。函数体：
 *   - debug 构建（cargo 未带 --release，Rust cfg(debug_assertions)）：下文 D1 / D2 语义；
 *   - release 构建：D1 回 NULL + MADINFER_ERROR_FEATURE_DISABLED (4005)、D2 回 4005；形式错误（空指针 / 取值越界）仍先回 4001。
 * madinfer.h 的生产符号面（madai-w14 起十符号）、MADINFER_ABI_VERSION、全部布局与常量不变（madinfer.h §5 "调试面" 条同文）。
 * 宿主纪律：
 *   - 生产代码路径禁止引用本头文件；调用处用宿主自己的调试宏包住。链到生产产物时不再是链接期缺符号，而是运行期 4005——
 *     宿主自检据 4005 判"链到的是生产产物"。
 * 与 uniffi 面同一份实现：InferCore.createDebug / testStopControl（两 profile 生成同一份绑定，release 下抛 FeatureDisabled）。
 * madai-w12 I-C-11：uniffi 面多一个 InferCore.debugSetTokenDelayMs(ms)（D1 建的脚本后端站定逐词元节拍，节拍 > frame_ms 即
 * 一词元一帧；上限 10 000、越界 4001、非脚本站 4001、release 4005）。本头文件**不**追加对应符号；
 * 鸿蒙壳要同一能力须走符号提案 `int32_t madinfer_debug_set_token_delay_ms(madinfer_core_handle core, uint64_t token_delay_ms)`
 * （语义同 uniffi 面；core 为 NULL 4001）——总控裁定后另批实施。madai-w14 I-C-12 生产面第 10 符号 madinfer_drop_contexts 已占第 12 个
 * 导出位（build_ohos.sh 断言 12），本提案顺延为第 13 符号候选，届时 build_ohos.sh 期望 13、三端 nm 期望同批改。
 */
#ifndef MADINFER_DEBUG_H
#define MADINFER_DEBUG_H

#include "madinfer.h"

#ifdef __cplusplus
extern "C" {
#endif

/**
 * D1 调试建站：同 madinfer_core_create（§6 第 2 符号），多一个建站选项 debug_script_backend——
 *   1 = 站内唯一后端换成 testkit 脚本后端：后端产出 = 请求最后一条 user 消息原文，按空白切词元（空白归前一词元，拼接逐字节等于
 *       原文；紧凑 JSON 即单词元）。结构化请求送"合法 JSON 但缺 required 字段"的 prompt → started → on_failed(2003 schema_invalid)
 *       携 partial = 原文（终态校验不过，即 2003 之二）；送齐全 JSON → on_complete 携结构化输出；文本请求原文回显。
 *       站内其余装配（映射 / 槽表 / 验签 / 授权 / 观测）与生产相同，probe 形态相同（Emulated 约束）；telemetry backend=debug_script。
 *   0 = 与 madinfer_core_create 同一装配路径（backend-llama 仿真核）。
 *   其余取值 → NULL + 4001。调试构建未开 feature debug_script_backend 而传 1 → NULL + 4003。
 *   release 构建 → NULL + 4005（MADINFER_ERROR_FEATURE_DISABLED；空指针 / 取值越界仍先 4001）。
 * 返回值 / out_error_code / 句柄回收（madinfer_core_destroy）语义同 madinfer_core_create。
 */
madinfer_core_handle madinfer_core_create_debug(const madinfer_host_vtable* host, const madinfer_core_options* options, uint8_t debug_script_backend, int32_t* out_error_code);

/**
 * D2 停 control 线程但保留句柄（真核路径上的 4004）：返回后 madinfer_apply_config 恒回 MADINFER_ERROR_CORE_STOPPED (4004)，
 * madinfer_stream_start 仍发句柄但 sink 不会收到任何回调（故障态，不是 K1 违约），cancel / signal 静默；幂等；
 * madinfer_core_destroy 照常回收。生产建站（madinfer_core_create）发放的句柄同样可停。
 * core 为 NULL 回 4001，其余恒 0；核内 panic 收口回 4002。release 构建：core 非 NULL 恒回 4005，不停线程。
 */
int32_t madinfer_test_stop_control(madinfer_core_handle core);

#ifdef __cplusplus
}
#endif

#endif /* MADINFER_DEBUG_H */
