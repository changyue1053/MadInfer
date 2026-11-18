/*
 * madinfer.h —— MadInfer 手写 C ABI（鸿蒙 NAPI 兜底场景；imcore.rules“手写 C ABI 只留三场景”之一）。
 *
 * 事实源：本文件（MadInfer crates/madinfer-ffi/include/madinfer.h，madai-w07 I-C-06 自 mad 侧迁入；此前 madai-w06 I-H-02
 * 在 mad shell/shellkit-infer-ohos/native/include/madinfer.h 冻结）。mad 侧那份自本批起是镜像——两处同批改、形态同
 * schemas/ ↔ mad-repo/schema/infer/ 的镜像纪律（总控 sync 门逐字节比对）。本头文件即契约：Rust 侧实装（同目录
 * src/cabi.rs，`#[no_mangle] extern "C"`，over 与 uniffi 面同一个 InferCore）与 mad 侧 NAPI 模块
 * （shell/shellkit-infer-ohos/native/madinfer_napi.cpp）都以本文件为唯一事实源；W06 的 C 桩（madinfer_stub.c）随 Rust 实装
 * 到位退役，NAPI 模块只换链接对象（libmadinfer_ffi.a）与留痕形态串（MADINFER_CABI_FORM）。
 *
 * 与 uniffi 面（MadInfer main 6abe8fe 起，crates/madinfer-ffi/src/lib.rs）的对应——十符号：
 *   1 madinfer_abi_version    ↔ infer_abi_version()                      握手常量 = 2（madai-w08 升版，见下）
 *   2 madinfer_core_create    ↔ InferCore::create(host, options)          建站，注入宿主 vtable
 *   3 madinfer_core_destroy   ↔ InferCore 句柄 Drop（Shutdown + join）      关停
 *   4 madinfer_probe          ↔ InferCore::probe() -> ProbeRecord          能力 + 档位快照
 *   5 madinfer_stream_start   ↔ InferCore::stream(request, sink) -> u64    流式生成，返回请求句柄
 *   6 madinfer_stream_cancel  ↔ InferCore::cancel(handle)                  取消（幂等）
 *   7 madinfer_host_reply     ↔ HostBridge::authorize 的返回值             宿主请求对的回投半边（见 §4）
 *   8 madinfer_signal         ↔ InferCore::signal(OsSignalRecord)          资源信号入口（madai-w07 F1 / F2 追加）
 *   9 madinfer_apply_config   ↔ InferCore::apply_config(document)          签名 config.v1 文档进核，同步回执应用结论（madai-w08 I-C-07 G2）
 *  10 madinfer_drop_contexts  ↔ InferCore::drop_contexts(prefix) -> u32    按前缀丢弃生成上下文，同步回条数（madai-w14 I-C-12）
 * 结构体字段与 record 一一对应（同名、同序、同宽）；enum 取值 = uniffi 枚举声明序（自 0 起）。
 *
 * madai-w08 I-C-07 追加（只追加不改序）+ 总控 G11 收口：**MADINFER_ABI_VERSION 1 → 2**——四个 repr(C) 结构体 sizeof 变大
 * （request 80→128 / capability_entry 72→76 / delta 40→56 / complete 56→72）与 vtable 加槽（56→64）对按 v1 头文件编译的宿主
 * 二进制是破坏性变化，按下方“任何字段增删、enum 追加都要升版”的既定规则升版（COMPAT.toml [madinfer].abi_version = 2 同步）；
 * 树内三端消费者（KMP InferFfiClient 握手常量、鸿蒙 InferCodes.ABI_VERSION、自检 abi=2 留痕）同批改。ABI v2 追加面：
 *   - 错误码 4004 MADINFER_ERROR_CORE_STOPPED
 *   - 新增 madinfer_apply_outcome 与两个枚举 madinfer_apply_stage / madinfer_apply_reject_reason ↔ ApplyOutcomeRecord
 *   - 新增符号 9 madinfer_apply_config
 *   - G4 结构化输出（event.v1 E6 / I-E-04 §7-1）：新增枚举 madinfer_output_kind / madinfer_constraint_support；
 *     madinfer_request 末尾加 output_kind + schema_json + has_context/context ↔ RequestRecord.output / .context；
 *     madinfer_capability_entry 末尾加 constraint ↔ CapabilityEntryRecord.constraint；
 *     madinfer_delta 末尾加 has_structured_patch/structured_patch ↔ DeltaRecord.structured_patch（RFC 6902 数组文本）；
 *     madinfer_complete 末尾加 output_kind + output ↔ CompleteRecord.output（text 兼容字段保留、同文）。
 *     布局基线读数（clang LP64）：madinfer_request 80→128、madinfer_capability_entry 72→76、madinfer_delta 40→56、
 *     madinfer_complete 56→72；Rust 侧 cabi/tests.rs 同步。
 *   - G7：madinfer_host_vtable 末尾加 on_worker_started（可为 NULL；56→64）↔ HostBridge::on_worker_started，枚举
 *     madinfer_worker_class；madinfer_os_signal.battery_level_pct 加 MADINFER_BATTERY_LEVEL_UNKNOWN 哨兵（布局不动）。
 *
 * madai-w10 I-C-09 契约注（只加注不改面；ABI 仍 2、九符号 / 布局 / 常量不变）：§5 "验签回投" 条 + §6 第 9 符号线程契约
 * （madinfer_apply_config 在 control 线程经 verify_ed25519 回调宿主、禁在验签回投所依赖的线程同步调）；§4 / madinfer_worker_class
 * 注明授权专用线程 madinfer-ffi-consent 也走 on_worker_started(CONTROL)。调试面另立 madinfer_debug.h（只在调试构建产物里存在的
 * madinfer_core_create_debug / madinfer_test_stop_control 两符号；生产产物无、本文件不声明）。
 *
 * madai-w11 I-C-10 契约注（只加注与常量、不改面；ABI 仍 2、九符号原型 / 布局 / 枚举取值不变）：
 *   - 验签 deadline（I-S-05 U-3 / w10 R1）：control 线程不再在自己身上调 host.verify_ed25519，而是把它投到专用瞬时线程
 *     madinfer-ffi-verify（类别 MADINFER_WORKER_CONTROL，走 on_worker_started）并以 MADINFER_VERIFY_DEADLINE_MS 为上限等回执；
 *     超时 → 本次 apply 以 MADINFER_APPLY_REJECT_SIGNATURE_INVALID 拒（stage = RECEIVED，detail = "verify=timeout"），
 *     宿主验签卡死不再拖死核站。§5 "验签回投" 条与 §6 第 9 符号说明同文。
 *   - 调试面导出无条件（首日 §0）：madinfer_debug.h 的两符号在 debug / release 产物里都存在（build_ohos.sh 符号断言 11），
 *     release 下函数体回固定错误码 MADINFER_ERROR_FEATURE_DISABLED (4005)（形态同 mad imcore attach_tcp_link 的
 *     FeatureDisabled）。§1 错误码表加 4005，§5 "调试面" 条说明。
 *
 * madai-w12 I-C-11 契约注（只加注、不改面；ABI 仍 2、九符号原型 / 布局 / 枚举取值 / 符号数 11 不变）：
 *   - 仿真核切片（R-w11-5）：backend-llama 仿真后端把整轮输出按输出字节数确定性切片（每片 ≤ 32 字节、完整 UTF-8 码点边界、
 *     词元计数随片携带），片序列是输出文本的纯函数、与调度时序无关；≤ 2 片（输出 ≤ 64 字节）时 delta 帧数 = 片数且逐帧文本
 *     可钉，更长输出仍只准钉首帧。§5b "仿真核切片" 条同文（替代旧 "仿真核分帧" 条）。
 *   - 调试面逐词元节拍（I-D-10 §7-2）：uniffi 面 InferCore.debugSetTokenDelayMs(ms) 给脚本后端站定片间节拍，节拍 > frame_ms 即
 *     一词元一帧——"首个 delta 后 cancel → failed(3008) 携 partial" 成确定性用例。**只在 uniffi 面**：C 面不加符号（build_ohos.sh
 *     仍 11）；鸿蒙壳要消费须追加第 12 符号 madinfer_debug_set_token_delay_ms（上游提案，见 I-C-11 报告 §7），本批不实施。
 *   - ABI 3 候选文本（R-w11-6，**不实施**）：验签 deadline 超时与验签器回假今天都落 stage = RECEIVED、靶 detail "verify=timeout"
 *     区分——提案 madinfer_apply_stage **追加** MADINFER_APPLY_STAGE_VERIFY = 6（↔ ApplyStageRecord.Verify，只追加不改序）：
 *     "停在验签"——验签超时 / 验签假 → stage = VERIFY + reject_reason = SIGNATURE_INVALID（detail 保留 "verify=timeout" 作补充），
 *     VERIFIED(1) 语义不变（验签通过、进解析）。enum 追加即升版：MADINFER_ABI_VERSION 2 → 3，COMPAT.toml 候选行
 *     `abi_version = 3  # madai-w<N>：madinfer_apply_stage 追加 VERIFY = 6（R-w11-6）`，milestone 候选 tag v0.11.0-mad.1；
 *     树内消费者同批改：KMP InferFfiClient 握手常量、ApplyStage 映射加 Verify、鸿蒙 InferCodes.ABI_VERSION、自检 abi=3 留痕。
 *     §2 madinfer_apply_stage 与 §5 "验签回投" 条各留一行指回本注。
 *
 * madai-w14 I-C-12 追加（**只加符号、不改面**；ABI 仍 2——升版规则原文是"任何字段增删、enum 追加都要升版"，只加符号不在其列，
 * 先例 madai-w10 / w11 调试面两符号；九符号原型 / 布局 / 枚举取值不变）：
 *   - 新增符号 10 madinfer_drop_contexts(core, madinfer_str prefix, uint32_t* out_dropped) ↔ InferCore::drop_contexts(prefix) -> u32：
 *     运行时契约设计稿（mad-w34 §2 ⑦ / R-8 / T6）"关户必须 drop 该账户全部上下文"的核侧半边——宿主租约 onClose 以账户前缀
 *     acct:<accountId>/ 调本符号；键 starts_with(prefix) 的生成上下文（KV 缓存 + 适配器亲和）立即不可复用，之后同键预留必是新建、
 *     池台账 hits 不增；在飞任务持有的上下文延迟到任务终态才结清 KV。prefix 按 madinfer_str 按值传（同 §4 kv_get / kv_put 的 key），
 *     不依赖 NUL；空前缀 / 无匹配回 0 条。防："换账户复用前一账户上下文"（设计稿判定 5）。
 *   - 与第 9 符号同为"调用方等核回执"形态（等的是核自己的 control 线程，不等宿主）：核已停回 4004；禁在任何 host / sink 回调里调。
 *   - 符号名单：§6 九符号 → 十符号；build_ohos.sh SYMBOLS 断言 11 → 12（十符号 + 调试面两符号）；三端 nm 期望随之改
 *     （鸿蒙 NAPI 模块 dlsym 名单是否纳入第 10 符号归鸿蒙壳属主）。madinfer_debug.h 文字提及的"第 12 符号提案
 *     madinfer_debug_set_token_delay_ms"顺延为第 13 符号候选（仍不实施）。
 *
 * madai-w15 I-C-13 契约注（**只加注、不加符号、不改面**；ABI 仍 2、十符号原型 / 布局 / 枚举取值 / 符号数 12 不变）：
 *   - 可观测面只在 uniffi 面：InferCore.stats() -> CoreStatsRecord（上下文池命中 / 丢弃 / 条目 / KV 已用与预算、本站验签超时、
 *     在飞 Job、运行时长；control 线程一次取齐、同步回、核已停 4004）与 ApplyOutcomeRecord.detail_kind（§5b 注）。C 面无对应
 *     符号 / 结构——鸿蒙壳要消费须走第 13 / 14 符号提案（归鸿蒙壳属主），本批不实施。
 *   - ContextPool 自本批接进管线：请求 context 键在 Job 投递成功时预留、终态释放，预算取 config thresholds.kv_budget_mb[tier]；
 *     第 10 符号 madinfer_drop_contexts 的语义不变，返回条数自此在真流程 ≥ 1 可测（此前池无预留方恒 0）。
 *
 * madai-w16 I-C-14 契约注（**只加注、不加符号、不改面**；ABI 仍 2、十符号原型 / 布局 / 枚举取值 / 符号数 12 不变）：
 *   - 上下文后端驻留：worker 自本批按 context 键驻留后端生成上下文（KV 缓存），同键命中即复用；第 10 符号 madinfer_drop_contexts
 *     回的键自此在 worker 侧真释放后端 KV（此前只记账）。每上下文 KV 取后端申报值（仿真核 8 MB），预算 thresholds.kv_budget_mb[tier]
 *     未配置（0）时任何键都不进池、不驻留——宿主要在 uniffi 面 stats() 读到 context_pool_hits > 0 须先 apply 带该档预算的配置；
 *     madinfer_drop_contexts 对未进池的键回 0 条，语义不变（§5b "上下文驻留" 条同文）。
 *   - 调试面验签 deadline 注入（I-D-14 §7）：uniffi 面 InferCore.debugSetVerifyDeadlineMs(ms) 给本站验签器改等宿主回执的上限
 *     （缺省 MADINFER_VERIFY_DEADLINE_MS；上限 10 000、0 恢复缺省、无受信公钥的站 4001；release 恒 4005）——"验签超时"
 *     （detail "verify=timeout" / detail_kind VERIFY_TIMEOUT / stats().verify_timeouts）不再靶慢机器。**只在 uniffi 面**：C 面不加符号
 *     （build_ohos.sh 仍 12）；鸿蒙壳要消费须走符号提案（顺延为第 14 符号候选，归鸿蒙壳属主），本批不实施。
 *
 * madai-w17 I-C-16 契约注（**只加注、不加符号、不改面**；ABI 仍 2、十符号原型 / 布局 / 枚举取值 / 符号数 12 不变）：
 *   - 后端形态选择只在 uniffi 面：CoreOptions.backend（BackendKindRecord EMULATED 缺省 / LLAMA）+ CoreOptions.model_path（GGUF
 *     权重文件路径；debug / 内测形态壳直给，生产由资产链给已验签、已安装文件的路径——同一字段），ProbeRecord.backend 与
 *     CoreStatsRecord.backend_kind 回本站形态。LLAMA 要求 madinfer-ffi 编入 cargo feature llama-sys（未编入 → 4005 FEATURE_DISABLED，
 *     不静默回仿真核）且 model_path 指向在盘文件（缺席 / 非文件 → 4003 CORE_START_FAILED）。**C 旧入口 madinfer_core_create 固定仿真核；新增 create_with_backend 显式选 native**：
 *     madinfer_core_options 不加字段、madinfer_probe_snapshot 不携 backend；鸿蒙壳要真实引擎须走 ABI 3（结构体加字段），本批不实施。
 *   - 每上下文 KV 自本批取真实引擎的模型参数（Model::context_kv_mb = n_ctx × n_layer × 2 × head_dim × n_head_kv × 2 B）；仿真核仍 8 MB。
 *     首键在模型加载前预留、按加载前估值记账，加载后的新键按真值记（§5b "上下文驻留" 条同文）。
 *
 * madai-w07 F2 同步 F1 面变更（W06 冻结面在任何宿主发布前同批扩展，只追加不改序）：
 *   - madinfer_host_vtable 末尾加 verify_ed25519       ↔ HostBridge::verify_ed25519（验签算力在宿主，核只持受信公钥表）
 *   - madinfer_core_options 末尾加 trusted_keys 数组   ↔ CoreOptions.trusted_keys（madinfer_trusted_key = key_id / public_key_hex）
 *   - 新增 madinfer_os_signal 与四个状态枚举           ↔ OsSignalRecord 七变体（kind 判别 + 扁平成员，形态同 madinfer_host_reply_record）
 *   - 新增符号 8 madinfer_signal
 *   ABI v1 的布局基线自本文件起算（COMPAT.toml [madinfer].abi_version = 1）；此后任何字段增删、enum 追加都要升版（madai-w08 → v2 即首次执行）。
 *
 * 约定：
 *   - 字符串一律 UTF-8 字节 + 长度（madinfer_str），不依赖 NUL 结尾；借用语义——入参只在调用期间有效，回调实参只在
 *     回调期间有效，接收方要留存就自行拷贝。
 *   - 可选字段（uniffi Option<T>）的 C 形态是 has_<field>（0 / 1）+ <field> 两个成员；has_ 为 0 时 <field> 内容未定义。
 *   - 错误码：返回 int32_t 的入口 0 = 成功，非 0 = 4xxx（与 uniffi InferFfiError 同表，见 §1）；核内错误 1xxx～3xxx 只出现在
 *     madinfer_failed.code（MadInfer core-types error.rs 同表；取消 = 3008 cancelled，授权拒绝 = 3007 consent_denied）。
 *   - 载荷：模型生成文本（delta.text / complete.text / failed.partial_text）与消息内容是业务载荷，核不解读；观测批次
 *     payload 是核内已序列化的 telemetry.v1 字节，宿主原样转交上报通道、禁解析（泳道规则 5）。
 *   - 线程规则见 §5：所有回调可在任意线程被调，宿主只入队立即返回；符号面上不存在同步等待宿主返回的形态。
 *
 * ABI 版本：本文件全部结构体布局与枚举取值属于 ABI v2（v1 基线 = madai-w07 F2 本文件；v2 = madai-w08 I-C-07 追加面，见上）；
 * 任何字段增删、enum 追加都要升 MADINFER_ABI_VERSION 并同步 MadInfer COMPAT.toml [madinfer].abi_version。
 */
#ifndef MADINFER_H
#define MADINFER_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ===================================================================================================================
 * §1 版本与错误码
 * =================================================================================================================== */

/** ABI 版本（COMPAT.toml [madinfer].abi_version 同源；宿主 dlsym / 链接后第一个调用 madinfer_abi_version()，不等即拒绝继续）。 */
#define MADINFER_ABI_VERSION 2u

/** 成功。 */
#define MADINFER_OK 0
/** 4001 请求 / 参数形式错误（空指针、非法能力名、业务 id 为 0、stop 超 8 条、未知 / 已回投过的 host_request …）：Started 前拒绝。 */
#define MADINFER_ERROR_INVALID_ARGUMENT 4001
/** 4002 核内 panic 已在 FFI 边界收口：进程存活，仅本次调用失败。 */
#define MADINFER_ERROR_PANICKED 4002
/** 4003 建站失败（control 线程 / worker 池起不来，或生产构建缺验签器）。 */
#define MADINFER_ERROR_CORE_START_FAILED 4003
/** 4004 核已停（control 线程已退出后仍被调用；只有需要回执的入口 madinfer_apply_config / madinfer_drop_contexts 会遇到）。madai-w08 I-C-07 追加。 */
#define MADINFER_ERROR_CORE_STOPPED 4004
/**
 * 4005 本构建未编入该功能（madai-w11 I-C-10 追加）：调试面两符号（madinfer_debug.h）在 release 产物里存在但函数体门控——
 * madinfer_core_create_debug 回 NULL + 4005、madinfer_test_stop_control 回 4005；形式错误（空指针 / 取值越界）仍先回 4001。
 * 宿主据 4005 判"链到的是生产产物"，不再依赖链接期缺符号 fail-fast。
 */
#define MADINFER_ERROR_FEATURE_DISABLED 4005

/** 核内错误码（madinfer_failed.code）中本头文件消费者需要点名的两枚；全表见 MadInfer core-types error.rs。 */
#define MADINFER_CODE_CONSENT_DENIED 3007
#define MADINFER_CODE_CANCELLED 3008

/**
 * Rust 实装的留痕形态串（madai-w07 I-C-06；C 桩退役）。宿主留痕 `form=<串>` 时取本值——鸿蒙 NAPI 模块把
 * MADINFER_NAPI_FORM 定义成它（hilog `[madinfer-napi] … form=napi-rust`），装机自检断言值同步改。
 * 同一常量在 Rust 侧是 cabi::MADINFER_CABI_FORM（单测核对两处一致）。
 */
#define MADINFER_CABI_FORM "napi-rust"
/* ed25519 验签形态常量（madai-w07 总控合 I-C-06 / I-H-03 对齐：mad 侧头文件先有，事实源补入；宿主等待预算是宿主策略，不进契约） */
#define MADINFER_ED25519_SIGNATURE_LEN 64u
#define MADINFER_ED25519_PUBLIC_KEY_LEN 32u
#define MADINFER_PUBLIC_KEY_HEX_LEN 64u

/* ===================================================================================================================
 * §2 基础类型
 * =================================================================================================================== */

/** UTF-8 字节串（借用；不保证 NUL 结尾）。len = 0 时 ptr 可为 NULL。 */
typedef struct madinfer_str {
    const uint8_t* ptr;
    uint32_t len;
} madinfer_str;

/** 核站句柄（不透明；madinfer_core_create 产出，madinfer_core_destroy 回收）。 */
typedef struct madinfer_core* madinfer_core_handle;

/** 能力名上限（注册制词表 [a-z0-9._-]{1,64}）。 */
#define MADINFER_CAPABILITY_MAX 64u
/** 核版本串上限（semver + 预发布标记）。 */
#define MADINFER_VERSION_MAX 32u
/** KvStore 值上限（HostBridge 契约：值 ≤ 4 KB）。 */
#define MADINFER_KV_VALUE_MAX 4096u
/** 停止串条数上限（RequestRecord.options.stop ≤ 8）。 */
#define MADINFER_STOP_MAX 8u

/* --- 枚举（uint8_t 承载；取值 = uniffi 枚举声明序） --- */

/** TierRecord。 */
typedef enum madinfer_tier { MADINFER_TIER_LOW = 0, MADINFER_TIER_MID = 1, MADINFER_TIER_FLAGSHIP = 2, MADINFER_TIER_UNKNOWN = 3 } madinfer_tier;
/** OsRecord。 */
typedef enum madinfer_os { MADINFER_OS_IOS = 0, MADINFER_OS_ANDROID = 1, MADINFER_OS_OHOS = 2, MADINFER_OS_JVM = 3 } madinfer_os;
/** PrivacyRecord。 */
typedef enum madinfer_privacy {
    MADINFER_PRIVACY_PUBLIC = 0,
    MADINFER_PRIVACY_INTERNAL = 1,
    MADINFER_PRIVACY_E2EE_PLAINTEXT = 2,
    MADINFER_PRIVACY_REGULATED = 3
} madinfer_privacy;
/** QosRecord。 */
typedef enum madinfer_qos { MADINFER_QOS_INTERACTIVE = 0, MADINFER_QOS_ASYNC = 1, MADINFER_QOS_BACKGROUND = 2 } madinfer_qos;
/** RoleRecord。 */
typedef enum madinfer_role { MADINFER_ROLE_SYSTEM = 0, MADINFER_ROLE_USER = 1, MADINFER_ROLE_ASSISTANT = 2, MADINFER_ROLE_TOOL = 3 } madinfer_role;
/** SourceRecord。 */
typedef enum madinfer_source { MADINFER_SOURCE_SYSTEM_MODEL = 0, MADINFER_SOURCE_BUNDLED = 1, MADINFER_SOURCE_CLOUD = 2 } madinfer_source;
/** RouteReasonRecord。 */
typedef enum madinfer_route_reason {
    MADINFER_ROUTE_PRIVACY_FORCED_LOCAL = 0,
    MADINFER_ROUTE_STATIC_TIER = 1,
    MADINFER_ROUTE_AVAILABILITY = 2,
    MADINFER_ROUTE_SWITCH = 3,
    MADINFER_ROUTE_DEFAULT = 4,
    MADINFER_ROUTE_FALLBACK = 5
} madinfer_route_reason;
/** ConsentDecisionRecord。 */
typedef enum madinfer_consent_decision {
    MADINFER_CONSENT_ALLOW = 0,
    MADINFER_CONSENT_ALLOW_REMEMBER = 1,
    MADINFER_CONSENT_DENY = 2
} madinfer_consent_decision;
/** CapStateRecord。 */
typedef enum madinfer_cap_state {
    MADINFER_CAP_AVAILABLE = 0,
    MADINFER_CAP_NEEDS_DOWNLOAD = 1,
    MADINFER_CAP_DOWNLOADING = 2,
    MADINFER_CAP_UNSUPPORTED = 3,
    MADINFER_CAP_DISABLED = 4
} madinfer_cap_state;
/** LatencyRecord。 */
typedef enum madinfer_latency { MADINFER_LATENCY_FAST = 0, MADINFER_LATENCY_NORMAL = 1, MADINFER_LATENCY_SLOW = 2 } madinfer_latency;
/** 宿主请求种类（madinfer_host_reply.kind）；新端口按 kind 收编、不加符号（madkit 通用请求对同一教训）。 */
typedef enum madinfer_host_request_kind { MADINFER_HOST_REQUEST_CONSENT = 0 } madinfer_host_request_kind;

/* --- 资源信号枚举（madai-w07 F2 追加；madinfer_os_signal 的成员取值） --- */

/** ThermalRecord。 */
typedef enum madinfer_thermal {
    MADINFER_THERMAL_NOMINAL = 0,
    MADINFER_THERMAL_FAIR = 1,
    MADINFER_THERMAL_SERIOUS = 2,
    MADINFER_THERMAL_CRITICAL = 3
} madinfer_thermal;
/** MemoryPressureRecord。 */
typedef enum madinfer_memory_pressure {
    MADINFER_MEMORY_PRESSURE_NORMAL = 0,
    MADINFER_MEMORY_PRESSURE_WARNING = 1,
    MADINFER_MEMORY_PRESSURE_CRITICAL = 2
} madinfer_memory_pressure;
/** NetStateRecord。 */
typedef enum madinfer_net_state {
    MADINFER_NET_OFFLINE = 0,
    MADINFER_NET_METERED = 1,
    MADINFER_NET_UNMETERED = 2
} madinfer_net_state;
/** AppStateRecord。 */
typedef enum madinfer_app_state {
    MADINFER_APP_FOREGROUND = 0,
    MADINFER_APP_BACKGROUND = 1,
    MADINFER_APP_BACKGROUND_TASK = 2,
    MADINFER_APP_TERMINATING = 3
} madinfer_app_state;
/** OsSignalRecord 的变体（madinfer_os_signal.kind）；取值 = uniffi 枚举变体声明序。 */
typedef enum madinfer_os_signal_kind {
    MADINFER_SIGNAL_THERMAL = 0,
    MADINFER_SIGNAL_BATTERY = 1,
    MADINFER_SIGNAL_MEMORY = 2,
    MADINFER_SIGNAL_APP = 3,
    MADINFER_SIGNAL_NETWORK = 4,
    MADINFER_SIGNAL_IDLE = 5,
    MADINFER_SIGNAL_MAIN_LINK_BUSY = 6
} madinfer_os_signal_kind;

/**
 * WorkerClassRecord（madai-w08 G7；madinfer_host_vtable.on_worker_started 的 class 实参）。类别按槽位分派、不按请求 QoS；
 * 宿主 QoS 映射口径见 §5b（BACKGROUND → utility 档，禁 background 档；madai-w09 I-C-08 总控裁决，不拆、不升 ABI）。
 */
typedef enum madinfer_worker_class {
    MADINFER_WORKER_CONTROL = 0,     /* control 线程；授权专用线程 madinfer-ffi-consent 也以本类别上报（madai-w10 I-C-09） */
    MADINFER_WORKER_INTERACTIVE = 1, /* Cpu / Gpu / Npu 槽（内联引擎）worker；承接三档 QoS 的本地推理 */
    MADINFER_WORKER_BACKGROUND = 2,  /* External 槽（系统模型 / 云端适配器）worker；宿主映射 utility 档，禁 background 档（§5b） */
    MADINFER_WORKER_DELIVERY = 3 /* 预留：投递线程尚未拆出（合帧 / 回调仍在 control 线程），本批不回调此类 */
} madinfer_worker_class;

/* --- 结构化输出枚举（madai-w08 I-C-07 G4 追加） --- */

/** OutputSpecRecord / OutputRecord 的形态判别（madinfer_request.output_kind / madinfer_complete.output_kind）。 */
typedef enum madinfer_output_kind { MADINFER_OUTPUT_TEXT = 0, MADINFER_OUTPUT_STRUCTURED = 1 } madinfer_output_kind;
/** ConstraintSupportRecord（BackendCaps.constraint 镜像）：NONE 不支持 / EMULATED 仿真按语法生成（非真采样）/ NATIVE 原生。 */
typedef enum madinfer_constraint_support {
    MADINFER_CONSTRAINT_NONE = 0,
    MADINFER_CONSTRAINT_EMULATED = 1,
    MADINFER_CONSTRAINT_NATIVE = 2
} madinfer_constraint_support;

/* --- 配置应用枚举（madai-w08 I-C-07 追加；madinfer_apply_outcome 的成员取值） --- */

/** ApplyStageRecord：配置应用管线停在的阶段（应用成功 = APPLIED；拒绝时 = 走到哪一步被拒）。
 *  ABI 3 候选（madai-w12 I-C-11 契约注，不实施）：追加 MADINFER_APPLY_STAGE_VERIFY = 6 承接"验签超时 / 验签假"（今天落 RECEIVED，
 *  detail "verify=timeout" 区分）；追加即升 MADINFER_ABI_VERSION 3，本批取值与序不动。 */
typedef enum madinfer_apply_stage {
    MADINFER_APPLY_STAGE_RECEIVED = 0,
    MADINFER_APPLY_STAGE_VERIFIED = 1,
    MADINFER_APPLY_STAGE_PARSED = 2,
    MADINFER_APPLY_STAGE_VALIDATED = 3,
    MADINFER_APPLY_STAGE_APPLIED = 4,
    MADINFER_APPLY_STAGE_REJECTED = 5
} madinfer_apply_stage;
/** ApplyRejectReasonRecord：配置拒绝原因（注册制；telemetry config_rejected.reason 同表）。 */
typedef enum madinfer_apply_reject_reason {
    MADINFER_APPLY_REJECT_UNKNOWN_KEY = 0,
    MADINFER_APPLY_REJECT_SIGNATURE_INVALID = 1,
    MADINFER_APPLY_REJECT_SCHEMA_INVALID = 2,
    MADINFER_APPLY_REJECT_VERSION_NOT_NEWER = 3,
    MADINFER_APPLY_REJECT_MIN_SDK_UNSATISFIED = 4,
    MADINFER_APPLY_REJECT_OUT_OF_ROLLOUT = 5,
    MADINFER_APPLY_REJECT_SEMANTIC_INVALID = 6
} madinfer_apply_reject_reason;

/* ===================================================================================================================
 * §3 过界 record（uniffi Record 的 C 形态）
 * =================================================================================================================== */

/**
 * TrustedKeyRecord（madai-w07 F1 / F2）：签名文档里的 key_id ↔ ed25519 公钥（32 字节的 hex，64 字符）。
 * 非 hex / 长度不对 / key_id 为空 → madinfer_core_create 回 4001。
 */
typedef struct madinfer_trusted_key {
    madinfer_str key_id;
    madinfer_str public_key_hex;
} madinfer_trusted_key;

/** CoreOptions：建站参数。 */
typedef struct madinfer_core_options {
    /** 观测实例标识。 */
    madinfer_str instance_name;
    /** madinfer_tier。 */
    uint8_t tier;
    /** madinfer_os。 */
    uint8_t os;
    /** 合帧窗口毫秒（0 = 核缺省）。 */
    uint32_t frame_ms;
    /** 观测采样万分比。 */
    uint16_t sampling_bp;
    /** 注册能力词表（≥ 1 条；每条 ≤ MADINFER_CAPABILITY_MAX 字节）。 */
    const madinfer_str* capabilities;
    uint32_t capabilities_len;
    /** Option<String> consent_endpoint：授权链路演示位（Internal 隐私触发升云授权 → host authorize）。 */
    uint8_t has_consent_endpoint;
    madinfer_str consent_endpoint;
    /**
     * 受信公钥表（madai-w07 F2 追加 ↔ CoreOptions.trusted_keys）：非空即注入宿主验签器（验签经 host.verify_ed25519），
     * 生产构建自此可建站；trusted_keys_len = 0 沿旧口径（调试构建 TrustAllVerifier，生产构建 4003）。
     */
    const madinfer_trusted_key* trusted_keys;
    uint32_t trusted_keys_len;
} madinfer_core_options;

/** MessageRecord：一条消息（content 是业务已脱敏的载荷）。 */
typedef struct madinfer_message {
    /** madinfer_role。 */
    uint8_t role;
    madinfer_str content;
} madinfer_message;

/** OptionsRecord：采样参数（has_ 为 0 = 用核缺省）。 */
typedef struct madinfer_options {
    uint8_t has_temperature_x100;
    uint16_t temperature_x100;
    uint8_t has_top_p_bp;
    uint16_t top_p_bp;
    uint8_t has_max_tokens;
    uint32_t max_tokens;
    /** 停止串（≤ MADINFER_STOP_MAX 条）。 */
    const madinfer_str* stop;
    uint32_t stop_len;
    uint8_t has_seed;
    uint64_t seed;
} madinfer_options;

/** RequestRecord：一次流式生成请求（request.v1 op = stream 的过界形式）。 */
typedef struct madinfer_request {
    /** 能力名（注册制词表）。 */
    madinfer_str capability;
    /** 业务 id（1～65535）。 */
    uint16_t business;
    /** madinfer_privacy（必填）。 */
    uint8_t privacy;
    /** madinfer_qos。 */
    uint8_t qos;
    /** 截止毫秒。 */
    uint32_t deadline_ms;
    /** 消息历史（载荷）。 */
    const madinfer_message* messages;
    uint32_t messages_len;
    madinfer_options options;
    /** madinfer_output_kind（madai-w08 G4 追加）：0 Text / 1 Structured；其余值 4001。 */
    uint8_t output_kind;
    /**
     * Structured 时的 JSON Schema（draft 2020-12 子集）文本——载荷：核编成 GBNF 约束后端、终态前校验全文；不回传、不落日志。
     * 编不过 / 校验不过 → on_failed(2003 schema_invalid)，编不过发生在 Started 前。Text 时忽略（可为空）。
     */
    madinfer_str schema_json;
    /** Option<String> context：生成上下文键（1～128 字节业务稳定串，如 conv:<id>）；形式非法 4001。 */
    uint8_t has_context;
    madinfer_str context;
} madinfer_request;

/** CapabilityEntryRecord：一条能力状态（probe 出参，宿主提供存储）。 */
typedef struct madinfer_capability_entry {
    uint8_t capability[MADINFER_CAPABILITY_MAX];
    uint32_t capability_len;
    /** madinfer_cap_state。 */
    uint8_t state;
    /** Option<SourceRecord>。 */
    uint8_t has_source;
    uint8_t source;
    /** madinfer_latency。 */
    uint8_t latency;
    /** madinfer_constraint_support（madai-w08 G4 追加）：Available 时取路由到的后端能力，否则 NONE。 */
    uint8_t constraint;
} madinfer_capability_entry;

/**
 * ProbeRecord：探测快照。宿主填 entries / entries_capacity 后调 madinfer_probe，核回填其余字段：
 * entries_len = 实际写入条数（≤ capacity），entries_total = 快照全部条数——total > len 时宿主扩容重探；不算错误。
 */
typedef struct madinfer_probe_snapshot {
    uint32_t abi_version;
    uint8_t core_version[MADINFER_VERSION_MAX];
    uint32_t core_version_len;
    /** madinfer_tier。 */
    uint8_t tier;
    /** 快照单调版本。 */
    uint64_t snapshot_version;
    madinfer_capability_entry* entries;
    uint32_t entries_capacity;
    uint32_t entries_len;
    uint32_t entries_total;
} madinfer_probe_snapshot;

/** StartedRecord（K1：恰一次，先于任何 delta / complete；延迟到首个 delta / complete 前一刻发出）。 */
typedef struct madinfer_started {
    uint64_t request;
    /** madinfer_source。 */
    uint8_t source;
    /** Option<String> package（Bundled 时有）。 */
    uint8_t has_package;
    madinfer_str package;
    /** madinfer_route_reason。 */
    uint8_t route_reason;
    uint32_t queued_ms;
} madinfer_started;

/** DeltaRecord：一帧增量（text 是生成载荷；seq 自 0 严格递增）。 */
typedef struct madinfer_delta {
    uint64_t request;
    uint32_t seq;
    madinfer_str text;
    uint32_t tokens;
    /**
     * Option<String> structured_patch（madai-w08 G4 追加；event.v1 E6）：Structured 请求每帧 has_ = 1，内容是这一帧的
     * RFC 6902 JSON Patch 数组文本（可为 "[]"）；逐帧独立可应用，按 seq 顺序应用到 {} 恒等于该帧时刻已完整的前缀文档，
     * 应用到最后一帧 == complete.output。op 子集：add / replace / remove，数组只尾追加（/…/-）。Text 请求 has_ = 0。
     */
    uint8_t has_structured_patch;
    madinfer_str structured_patch;
} madinfer_delta;

/** StatsRecord：生成统计（event.v1 complete.stats 镜像）。 */
typedef struct madinfer_stats {
    uint32_t ttft_ms;
    uint32_t decode_ms;
    uint32_t prompt_tokens;
    uint32_t gen_tokens;
    uint32_t tps_x100;
    uint32_t peak_memory_mb;
    /** Option<SourceRecord>。 */
    uint8_t has_source;
    uint8_t source;
    uint8_t fallback_steps;
    /** 0 / 1。 */
    uint8_t prefix_hit;
} madinfer_stats;

/** CompleteRecord：终态（恰一次；失败路径不发）。 */
typedef struct madinfer_complete {
    uint64_t request;
    /** 完整文本（载荷；兼容字段——Structured 时与 output 同文）。 */
    madinfer_str text;
    madinfer_stats stats;
    /** madinfer_output_kind（madai-w08 G4 追加）：0 Text / 1 Structured。 */
    uint8_t output_kind;
    /** Text 的全文 / Structured 的已过 schema 校验 JSON（紧凑序列化；载荷）。 */
    madinfer_str output;
} madinfer_complete;

/** FailedRecord：终态（恰一次；含取消 3008）。code / wire 与 core-types error.rs 同表。 */
typedef struct madinfer_failed {
    uint64_t request;
    /** 稳定错误码 1xxx～3xxx。 */
    uint16_t code;
    /** 协议字符串（全拼）。 */
    madinfer_str wire;
    /** Option<u32>：建议等待毫秒（仅 Retryable）。 */
    uint8_t has_retry_after_ms;
    uint32_t retry_after_ms;
    /** Option<String>：已生成部分（已出 delta 后失败时有）。 */
    uint8_t has_partial_text;
    madinfer_str partial_text;
} madinfer_failed;

/** TelemetryBatchRecord：观测批次（类型化头 + 不透明载荷字节；宿主禁解析 payload）。 */
typedef struct madinfer_telemetry_batch {
    uint32_t schema_version;
    uint32_t event_count;
    const uint8_t* payload;
    uint32_t payload_len;
} madinfer_telemetry_batch;

/** ConsentRecord：授权上下文（升云前的一次性确认）；三字段都是注册制 id / 状态名，允许入日志。 */
typedef struct madinfer_consent {
    madinfer_str capability;
    /** madinfer_privacy。 */
    uint8_t privacy;
    madinfer_str endpoint_id;
} madinfer_consent;

/** 宿主请求回投（madinfer_host_reply 入参）：按 kind 取用对应成员。 */
typedef struct madinfer_host_reply_record {
    /** madinfer_host_request_kind。 */
    uint8_t kind;
    /** kind = CONSENT：madinfer_consent_decision。 */
    uint8_t consent_decision;
} madinfer_host_reply_record;

/**
 * OsSignalRecord（madai-w07 F2 追加；madinfer_signal 入参）：按 kind 取用对应成员，其余成员内容未定义。
 * 变体字段与 uniffi 枚举同名同序：Thermal{state} / Battery{level_pct, charging, low_power_mode} / Memory{level} /
 * App{state, budget_ms?} / Network{state} / Idle{idle} / MainLinkBusy{busy}。
 */
typedef struct madinfer_os_signal {
    /** madinfer_os_signal_kind。 */
    uint8_t kind;
    /** kind = THERMAL：madinfer_thermal。 */
    uint8_t thermal_state;
    /**
     * kind = BATTERY：电量 0～100；MADINFER_BATTERY_LEVEL_UNKNOWN (255) = 未知（madai-w08 G7 ↔ OsSignalRecord::Battery.level_pct
     * Option<u8>；用哨兵而非 has_ 位是为了不动 F2 布局、旧宿主取值语义不变）。101～254 → 4001。
     * 未知在核内原样是 None（madai-w09 I-C-08 起，不再以 100 代入）：不参与电量门控，充电 / 低电量模式两成员仍按实生效——
     * 宿主读不到电量时传 255，不得伪造 100。
     */
    uint8_t battery_level_pct;
    /** kind = BATTERY：充电中 0 / 1。 */
    uint8_t battery_charging;
    /** kind = BATTERY：低电量模式 0 / 1。 */
    uint8_t battery_low_power_mode;
    /** kind = MEMORY：madinfer_memory_pressure。 */
    uint8_t memory_level;
    /** kind = APP：madinfer_app_state。 */
    uint8_t app_state;
    /** kind = APP：Option<u32> budget_ms（后台预算毫秒）。 */
    uint8_t has_app_budget_ms;
    uint32_t app_budget_ms;
    /** kind = NETWORK：madinfer_net_state。 */
    uint8_t network_state;
    /** kind = IDLE：0 / 1。 */
    uint8_t idle;
    /** kind = MAIN_LINK_BUSY：0 / 1。 */
    uint8_t main_link_busy;
} madinfer_os_signal;

/** madinfer_os_signal.battery_level_pct 的"未知"哨兵（madai-w08 G7）。 */
#define MADINFER_BATTERY_LEVEL_UNKNOWN 255u

/** 配置应用诊断文本上限（madinfer_apply_outcome.detail；核截断在 UTF-8 字符边界）。 */
#define MADINFER_APPLY_DETAIL_MAX 128u

/**
 * ApplyOutcomeRecord ↔ madinfer_apply_config 的出参（madai-w08 I-C-07 追加）。核回填、宿主只读。
 * applied = 1 时 active_version 为新版本、has_reject_reason = 0、detail_len = 0；applied = 0 时 active_version 仍是旧版本
 * （原子：拒绝不动活动配置）、reject_reason 有值、detail 为诊断文本（key_id / 版本对 / 桶号 / 语义错误描述），宿主只记日志不解读。
 */
typedef struct madinfer_apply_outcome {
    /** 是否已切换为活动配置 0 / 1。 */
    uint8_t applied;
    /** 调用后的活动配置版本。 */
    uint64_t active_version;
    /** madinfer_apply_stage。 */
    uint8_t stage;
    /** Option<ApplyRejectReasonRecord> 的 has_ 位。 */
    uint8_t has_reject_reason;
    /** madinfer_apply_reject_reason。 */
    uint8_t reject_reason;
    /** detail 有效字节数（≤ MADINFER_APPLY_DETAIL_MAX）。 */
    uint32_t detail_len;
    /** 诊断文本 UTF-8 字节（不 NUL 结尾；超长截断）。 */
    uint8_t detail[MADINFER_APPLY_DETAIL_MAX];
} madinfer_apply_outcome;

/* ===================================================================================================================
 * §4 回调 vtable（函数指针 + user_data；宿主实现、核回调）
 *
 * 核在 madinfer_core_create / madinfer_stream_start 时把 vtable 结构体**按值拷走**（vtable 指针本身不必在调用后保持有效），
 * user_data 由宿主保证有效期：host vtable 到 madinfer_core_destroy 返回；sink vtable 到该请求的终态回调返回。
 * =================================================================================================================== */

/**
 * HostBridge 的 C 形态（madinfer-host 四端口）。
 *   now_ms          ↔ HostBridge::now_ms          单调毫秒（宿主保证不回退）；核内一切时间经它。
 *   kv_get          ↔ HostBridge::kv_get          返回 1 = 有值 / 0 = 无键；有值时 *out_len = 值全长并写入 min(全长, capacity) 字节
 *                                                  （全长 > capacity 即截断，宿主契约值 ≤ MADINFER_KV_VALUE_MAX 故核以该容量调用）。可阻塞 ≤ 50 ms。
 *   kv_put          ↔ HostBridge::kv_put          覆盖写。可阻塞 ≤ 50 ms。
 *   telemetry_send  ↔ HostBridge::telemetry_send  交付一批观测；宿主只入队。
 *   authorize       ↔ HostBridge::authorize       升云授权确认——uniffi 面是同步返回值（核在专用线程调它、允许阻塞等用户），
 *                                                  C 面拆成请求 / 回投对：本回调只入队立即返回，宿主弹窗收口后以同一 host_request
 *                                                  调 madinfer_host_reply(kind = CONSENT) 恰一次；Rust 实装在专用线程等待回投，
 *                                                  control 线程不等（ArkTS 引擎禁多线程进入，同步跨界不可行）。
 *                                                  两面等价关系（I-H-02 U-6）：同一专用线程、同一"恰一次"；宿主回 AllowRemember 时核侧
 *                                                  kv_put("madinfer.consent.<capability>.<endpoint>", 0x01) 记忆，此后同键请求不再问宿主
 *                                                  （madai-w07 F9，宿主零逻辑）；madinfer_core_destroy 时授权中的请求以 on_failed(3008) 收口。
 *   verify_ed25519  ↔ HostBridge::verify_ed25519  ed25519 验签（madai-w07 F2 追加）：验签算力在宿主（鸿蒙 cryptoFramework），核只持受信
 *                                                  公钥表；signature 64 字节、public_key 32 字节原始编码；返回 1 = 验真 / 0 = 验假，
 *                                                  任何异常一律回 0、禁越界抛出。可阻塞 ≤ 50 ms；在核的专用瞬时线程 madinfer-ffi-verify
 *                                                  上被调（madai-w11 I-C-10；control 线程最多等 MADINFER_VERIFY_DEADLINE_MS，见 §5）。
 *   on_worker_started ↔ HostBridge::on_worker_started 线程钩子（madai-w08 G7 追加）：核的每条线程启动时在**该线程上**调一次，
 *                                                  class = madinfer_worker_class；宿主据此给当前线程设优先级 / QoS / 名字后立即返回
 *                                                  （档位映射口径 §5b：BACKGROUND → utility 档，禁 background 档）。
 *                                                  **可为 NULL**（= 不设优先级）；前六个函数指针任一为 NULL → madinfer_core_create 回 4001。
 *                                                  授权专用线程 madinfer-ffi-consent（madai-w10 I-C-09）：每次授权一条，线程体第一行以
 *                                                  MADINFER_WORKER_CONTROL 调本钩子、随后才调 authorize——宿主的核线程守卫与 QoS 覆盖到授权回调体。
 */
typedef struct madinfer_host_vtable {
    void* user_data;
    uint64_t (*now_ms)(void* user_data);
    int32_t (*kv_get)(void* user_data, madinfer_str key, uint8_t* out_value, uint32_t capacity, uint32_t* out_len);
    void (*kv_put)(void* user_data, madinfer_str key, const uint8_t* value, uint32_t len);
    void (*telemetry_send)(void* user_data, const madinfer_telemetry_batch* batch);
    void (*authorize)(void* user_data, uint64_t host_request, const madinfer_consent* context);
    int32_t (*verify_ed25519)(void* user_data, const uint8_t* message, uint32_t message_len, const uint8_t* signature,
                              uint32_t signature_len, const uint8_t* public_key, uint32_t public_key_len);
    void (*on_worker_started)(void* user_data, uint8_t worker_class);
} madinfer_host_vtable;

/**
 * EventSink 的 C 形态。K1 时序：on_started 恰一次 → on_delta* → on_complete / on_failed 恰一次，之后无事件。
 * 实现体只准把事件塞进宿主侧队列后立即返回；允许在回调里反调 madinfer_stream_cancel（只入队，不死锁）。
 * 任一函数指针为 NULL → madinfer_stream_start 回 4001。
 */
typedef struct madinfer_sink_vtable {
    void* user_data;
    void (*on_started)(void* user_data, const madinfer_started* event);
    void (*on_delta)(void* user_data, const madinfer_delta* event);
    void (*on_complete)(void* user_data, const madinfer_complete* event);
    void (*on_failed)(void* user_data, const madinfer_failed* event);
} madinfer_sink_vtable;

/* ===================================================================================================================
 * 禁止在任何 host/sink 回调内调用 madinfer_core_destroy：会等待自己的生命周期票或线程。
 * destroy 等所有 host 回调（包括超时验签/线程钩子）返回；宿主永久阻塞意味着 destroy 无有限时限。
 * §5 线程规则（madinfer-host 纪律的 C ABI 投影；README §3 同款）
 *
 *   回调任意线程：host / sink vtable 的每个函数都可能在核的 control 线程、worker 线程或专用授权线程被调，核不承诺线程；
 *                宿主实现只准入队立即返回（鸿蒙：napi_threadsafe_function 投回 ArkTS 主循环后再分派——工作线程直接进
 *                ArkTS 引擎会 ecma_vm 多线程 abort）。回调内禁止阻塞等待宿主自己的主线程。
 *   入向任意线程：入口符号都可从任意线程调用（madinfer_core_create / destroy 除外：由宿主保证不与其余入口并发）；核侧只入队
 *                （madinfer_apply_config / madinfer_drop_contexts 例外：入队后阻塞等核回执，见 §6 第 9 / 第 10 符号说明；禁在回调里调）。
 *   上下文丢弃（madai-w14 I-C-12）：madinfer_drop_contexts 在 control 线程上执行 ContextPool::drop_prefix 后同步回条数——不回调宿主、
 *                不等 worker；在飞任务持有的上下文不被强拆，到该任务终态（on_complete / on_failed 发出）才结清 KV，但其键自本调用返回起
 *                即不可复用（同键新请求得到新上下文）。宿主关户流程：先 madinfer_stream_cancel 该账户在飞请求，再调本符号；两步顺序
 *                颠倒也正确，只是 KV 多占到终态。核已停回 4004，宿主据此判核站故障态（与第 9 符号同判据）。
 *   验签回投（madai-w10 I-C-09，I-H-05 反馈 ①；madai-w11 I-C-10 加 deadline）：madinfer_apply_config 的验签由核的 control 线程
 *                把 host.verify_ed25519 投到专用瞬时线程 madinfer-ffi-verify（每次验签一条；线程体第一行以 MADINFER_WORKER_CONTROL
 *                调 on_worker_started，随后才调 verify_ed25519），control 线程 recv_timeout(MADINFER_VERIFY_DEADLINE_MS) 等回执：
 *                  - 期限内回执：1 → 进解析，0 → signature_invalid；
 *                  - 超时：本次 apply 以 MADINFER_APPLY_REJECT_SIGNATURE_INVALID 拒（拒因表中语义最近者；stage = RECEIVED），
 *                    outcome.detail = "verify=timeout"；本次 apply 不再等该线程，但 destroy 必须等回调返回——宿主回调日后返回时核只按代数票哑火计数
 *                    （uniffi 面 verify_timeout_count / verify_late_reply_count），不再触碰宿主、不重投。
 *                每次 apply 至多一次 verify_ed25519（文档解析通过且 key_id 在受信表内才调；未知 key / 非 JSON 不进宿主），
 *                并发 apply 在核内按入邮箱序串行、按同序回执。禁在验签回投所依赖的线程上同步调本符号：宿主验签若经主循环 /
 *                消息队列回投（鸿蒙 TSF 投回 ArkTS 主线程再算），主循环直调 = 自己等自己 → 撞 deadline → 合法文档被
 *                signature_invalid（verify=timeout）拒（I-H-05 负面演示 wait_ms=202）。宿主须在与验签回投无依赖的线程调它
 *                （配置线程 / 工作线程）。宿主验签预算须 < MADINFER_VERIFY_DEADLINE_MS（§4 写的 ≤ 50 ms 仍是宿主承诺）。
 *                超时 / 验签假落 stage = RECEIVED 是 ABI 2 口径；stage = VERIFY 是 ABI 3 候选（文件头 madai-w12 契约注，不实施）。
 *   调试面（madai-w11 I-C-10）：madinfer_debug.h 的 madinfer_core_create_debug / madinfer_test_stop_control 在 debug / release 产物里
 *                都导出（build_ohos.sh 符号断言 = 14，两 profile 同数——madai-w14 前为 11；两 bindgen 脚本从任一 profile 生成同一份绑定），release 下
 *                函数体回 MADINFER_ERROR_FEATURE_DISABLED (4005)：create_debug → NULL + 4005（空指针 / 取值越界仍先 4001）、
 *                test_stop_control → 4005（core 为 NULL 仍 4001）。生产代码路径仍禁止引用 madinfer_debug.h；4005 是宿主
 *                自检"链到生产产物"的判据。madai-w12 I-C-11：uniffi 面多一个 debugSetTokenDelayMs（脚本后端站逐词元节拍，
 *                release 同样 4005），C 面无对应符号——鸿蒙壳消费须走调试面符号提案（文件头 madai-w12 契约注；madai-w14 起顺延为第 13 符号候选）。
 *   回调重入：在 sink 回调里调 madinfer_stream_cancel / madinfer_stream_start 合法；在 host 回调里调 madinfer_host_reply 合法。
 *   无同步跨界：符号面上不存在“调用后同步等待宿主返回”的形态；同步语义的 now_ms / kv_* 由宿主在本地（C / Rust / K-N 侧）
 *                自供，不跨 ArkTS。
 *   回投配对：每个 host_request 恰一次 madinfer_host_reply；重复 / 未知 id 回 4001 且无副作用。
 *   取消：cancel 后未完成请求恰一次 on_failed(3008)；已终态 / 未知 id 的 cancel 幂等 no-op（泳道规则 9）。
 *          取消先于回投时，请求已以 3008 收口；随后到达的首次回投仍算配对成功（回 0，决议落在已终态请求上被静默丢弃），
 *          宿主仍须回投恰一次以释放核侧等待线程（不回投的授权中请求由 madinfer_core_destroy 一并收口）。
 *   关停：madinfer_core_destroy 先关核（在飞请求含授权中的以 on_failed(3008) 收口、观测排水刷出），再等所有正在进行的
 *          所有宿主回调（含 authorize、超时 verify_ed25519、线程钩子）归还生命周期票，之后不再有任何回调；
 *          未回投的 host_request 随之失效。宿主须保证回调最终返回，否则 destroy 无法完成；禁止从回调内 destroy。
 * =================================================================================================================== */

/**
 * 验签 deadline 毫秒（madai-w11 I-C-10；§5 "验签回投" 条）：control 线程等 madinfer-ffi-verify 回执的上限，超时按
 * signature_invalid + detail "verify=timeout" 拒。Rust 侧同名常量 madinfer_ffi::VERIFY_DEADLINE_MS（单测核对两处一致）。
 * 取值论证：§4 宿主验签承诺 ≤ 50 ms；鸿蒙 TSF 回投最坏读数 202 ms（I-H-05）；2 000 ms 给主循环忙时 10 倍余量，
 * 又远小于宿主配置线程的任何合理等待预算。改值只改本常量与 Rust 常量，不升 ABI。
 */
#define MADINFER_VERIFY_DEADLINE_MS 2000u

/* ===================================================================================================================
 * §5b 行为契约注（madai-w08 总控收口，I-H-04 反馈 ③：三端壳此前只能从 Rust 源码读出的行为，写进事实源免各端自行试出）
 *
 *   档位缺省口径（G11）：候选未写 tier_min 时按 Tier::allowed_sources 判——Unknown / Low 档只许系统模型与云端，内联包
 *                （bundled）出链且 probe 该能力为 unsupported（3001）；候选显式写了 tier_min 以其为准（配置作者对该包的明示）。
 *                鸿蒙 3.9 GB 模拟器 / 2 GB AVD 都是 Low 档：仿真核在其上 probe 得 unsupported 是口径而非故障，自检载具用显式档位覆盖。
 *   Interactive 链过滤（R10 不变量）：madinfer_qos Interactive 的决策链剔除不支持 abort 的后端（今天：云端），因此云端目标不会在
 *                Interactive 请求里触发 authorize；privacy=internal + Interactive 直接落端侧包不问授权。要走云端授权链用 Async。
 *   观测批次策略：5 s / 50 条 / Shutdown 三者之一刷出一批到 telemetry_send（Telemetry::new 缺省值）；C 面无 flush 符号——自检类
 *                宿主想拿到批次须在 madinfer_core_destroy 之后读（destroy 的观测排水刷出）。
 *   仿真核切片（madai-w12 I-C-11，R-w11-5；替代 W08 "仿真核分帧" 条）：backend-llama 仿真后端仍按 max_tokens / 停止串截词元，
 *                随后把整轮输出文本按输出字节数确定性切片——每片 ≤ 32 字节（madinfer-backend-llama EMULATED_PIECE_BYTES）、只在
 *                完整 UTF-8 码点边界切（ASCII 恰 32、多字节码点不拆故可短于 32）、词元计数随片携带（complete.stats.gen_tokens
 *                仍是词元数）；片序列是输出文本的纯函数，与 worker / control 线程调度时序无关。核内合帧窗（frame_ms，缺省 80）仍在：
 *                  - 输出 ≤ 64 字节（≤ 2 片）：delta 帧数 = 片数（1 或 2），逐帧文本 = 逐片文本，可钉——自检 "self-check" ×
 *                    max_tokens 8 → 「self-check」16 字节 → 恰 1 帧；「总结一下今天的群消息」36 字节 → 恰 2 帧
 *                    （「总结一下今天的群消 / 息」）。W08 写的 "8 token 请求出 2 帧" 自本批起为 1 帧。
 *                  - 更长输出：首帧 = 首片（可钉），帧数 ∈ [2, 片数] 随合帧窗——宿主断言 K1 只准依赖 started:1 → delta ≥ 1 →
 *                    complete:1，不准钉帧数。要钉多帧用调试面 debugSetTokenDelayMs（节拍 > frame_ms 即一词元一帧）。
 *   结构化输出（G4）：structured_patch 每帧一份 RFC 6902 数组文本、逐帧独立可应用、seq 连续；根须为对象（schema 根为数组 / 标量
 *                时不产 patch，complete 仍带完整 json）；schema 编不过 → 2003 且无 started。
 *   apply 诊断种类（madai-w15 I-C-13）：uniffi 面自 0.13.0 以 ApplyOutcomeRecord.detail_kind 类型化 detail 串（VERIFY_TIMEOUT /
 *                VERIFY_REJECTED / UNKNOWN_KEY / PARSE / …），验签超时与验签假在 KMP 面据此分开；C 面 madinfer_apply_outcome 结构不动，
 *                detail 串不变——C 宿主仍靶 detail "verify=timeout"（ABI 2 口径；stage = VERIFY 仍是 ABI 3 候选，见文件头 madai-w12 注）。
 *                验签超时自 0.14.0 可在调试构建经 uniffi 面 debugSetVerifyDeadlineMs 注入复现（文件头 madai-w16 注）；C 面无对应符号。
 *   上下文驻留（madai-w16 I-C-14）：请求带 context（has_context = 1）时 worker 按键驻留后端生成上下文、同键命中即复用；
 *                驻留上限 = thresholds.kv_budget_mb[tier] ÷ 后端申报的每上下文 KV（仿真核 8 MB），超限按池 LRU 腾空闲上下文；
 *                预算未配置（0）= 本档不留 KV：请求照常完成、不进池、不驻留，madinfer_drop_contexts 回 0 条。后端并发席位
 *                （仿真核 1）之内才会有多个键同时驻留——席位 1 时换键即腾前一键，回到前一键是新建而非复用；宿主断言"同键命中"
 *                只准在连续同键请求上做。关户 madinfer_drop_contexts 后该账户的键在 worker 侧真释放（先于其后任何工单）。
 *                真实引擎站（madai-w17 I-C-16，只在 uniffi 面建得出）每上下文 KV 按模型参数算（smollm2-135m · n_ctx 4096 ≈ 90 MB），
 *                Flagship 缺省预算 128 MB 下同时驻留 1 键；首键按加载前估值 8 MB 记账、加载后的新键按真值记。
 *   线程类别与 QoS（madai-w09 I-C-08 总控裁决，I-S-04 反馈 U-3 销项）：madinfer_worker_class 按**槽位**分派、不按请求 QoS——
 *                INTERACTIVE = Cpu / Gpu / Npu 槽（内联引擎）worker，BACKGROUND = External 槽（系统模型 / 云端适配器）worker；
 *                三档 madinfer_qos 的请求都可能落在任一类 worker 上。Async 与 Background（批量）两档的区分在调度（门控准入 /
 *                配额 / 抢占），不在线程优先级；WorkerClass 不拆、不升 ABI（枚举取值与符号数不变）。宿主映射口径（三端同）：
 *                BACKGROUND worker → **utility 档，禁 background 档**（iOS QOS_CLASS_UTILITY 而非 QOS_CLASS_BACKGROUND、鸿蒙
 *                QOS_UTILITY 而非 QOS_BACKGROUND、Android 不用 THREAD_PRIORITY_BACKGROUND）——background 档在热 / 低电下会被系统
 *                压到零配额，把同一 worker 上的 Async 请求随批量一起饿死。CONTROL / INTERACTIVE / DELIVERY 沿 iOS 先例映射
 *                用户发起档（I-S-04），本裁决只钉 BACKGROUND。拆池（Async / Batch 各一类 worker）等真 llama 后端出现两档争用
 *                读数后再议。
 * =================================================================================================================== */

/* ===================================================================================================================
 * §6 十符号（W07 八符号 + madai-w08 I-C-07 第 9 符号 + madai-w14 I-C-12 第 10 符号）
 * =================================================================================================================== */

/** 1 ABI 版本握手（= MADINFER_ABI_VERSION）。 */
uint32_t madinfer_abi_version(void);

/**
 * 2 建站：注入宿主 vtable + 建站参数。成功返回非 NULL 句柄；失败返回 NULL 并把 4xxx 写入 *out_error_code（可为 NULL）。
 * 建站期核会经 kv_get / kv_put 读写 install 标记键（“madinfer.install_marker”），故 host vtable 从本调用起就必须可用。
 */
madinfer_core_handle madinfer_core_create(const madinfer_host_vtable* host, const madinfer_core_options* options, int32_t* out_error_code);

/** 3 关停：发 Shutdown 并 join；在飞请求以 on_failed 收口后返回；NULL 幂等。返回后不再有任何回调。 */
void madinfer_core_destroy(madinfer_core_handle core);

/** 4 探测：按 §3 madinfer_probe_snapshot 约定回填；core / out_buf 为 NULL 回 4001。
 *  后端形态（backend = llama | emulated）只在 uniffi 面 ProbeRecord.backend / CoreStatsRecord.backend_kind（madai-w17 I-C-16）；
 *  本结构不携该字段——C 面建的站恒仿真核。 */
int32_t madinfer_probe(madinfer_core_handle core, madinfer_probe_snapshot* out_buf);

/**
 * 5 流式生成：返回请求句柄（> 0），事件与取消都以它为锚。失败返回 0 并把 4xxx 写入 *out_error_code（可为 NULL）；
 * 形式非法（4001）在 Started 前拒绝，sink 不会收到任何回调。
 */
uint64_t madinfer_stream_start(madinfer_core_handle core, const madinfer_request* request, const madinfer_sink_vtable* sink, int32_t* out_error_code);

/** 6 取消：未完成请求恰一次 on_failed(3008)；已终态 / 未知句柄幂等 no-op。core 为 NULL 回 4001，其余恒 0。 */
int32_t madinfer_stream_cancel(madinfer_core_handle core, uint64_t request);

/** 7 宿主请求回投：以 host_request 配对恰一次；重复 / 未知 id / 空指针回 4001 且无副作用。 */
int32_t madinfer_host_reply(madinfer_core_handle core, uint64_t host_request, const madinfer_host_reply_record* reply);

/**
 * 8 资源信号（madai-w07 F2 追加）：热 / 电量 / 内存 / 前后台 / 网络 / 空闲 / 主链路忙进核调度门控。只入队不阻塞
 * （邮箱满则丢并计数，capability.rules 10）；core / signal 为 NULL 或 kind / 成员取值越界回 4001，其余恒 0。
 */
int32_t madinfer_signal(madinfer_core_handle core, const madinfer_os_signal* signal);

/**
 * 9 配置应用（madai-w08 I-C-07 G2；"核发起验签"的端到端）：送一份已签名 config.v1 文档字节进核，**同步等**应用结论并
 * 回填 *out。管线在核的 control 线程上走完：验签（trusted_keys 非空时经 host.verify_ed25519；为空时调试构建 TrustAll、
 * 生产构建建站已被 4003 拒）→ 解析 → 版本递增 / min_sdk / 设备分桶 / 语义 → 原子切换。被拒不动活动配置，结论同时进
 * 观测（config_apply / config_rejected）。
 * 返回值只表达调用本身：0 = 已拿到结论（applied 可为 0 / 1，看 *out）；core / document（len > 0 时）/ out 为 NULL 回 4001；
 * 核已停回 4004；核内 panic 收口回 4002。本符号与第 10 符号 madinfer_drop_contexts 是十符号中仅有的"调用方等核回执"形态（核不等宿主，
 * §5 无同步跨界不变：等的是核自己的 control 线程；control 线程对宿主验签的等待有上限 MADINFER_VERIFY_DEADLINE_MS，宿主须本地同步实现、≤ 50 ms）。
 * 宿主不得在任何 host / sink 回调里调本符号（会等自己）；建议在宿主的配置线程调用。
 * 线程契约（madai-w10 I-C-09 / madai-w11 I-C-10，§5 同文）：把 host.verify_ed25519 投到专用瞬时线程 madinfer-ffi-verify，
 * control 线程 recv_timeout(MADINFER_VERIFY_DEADLINE_MS) 等回执，超时 → signature_invalid + detail "verify=timeout"、迟到回执哑火计数；
 * 每次 apply 至多一次 verify_ed25519（解析通过且 key_id 受信才调）；并发 apply 在核内按入邮箱序串行、按同序回执；
 * 禁在验签回投所依赖的线程上同步调（宿主验签经主循环回投时，主循环直调即撞 deadline）。
 */
int32_t madinfer_apply_config(madinfer_core_handle core, const uint8_t* document, uint32_t document_len, madinfer_apply_outcome* out);

/**
 * 10 上下文丢弃（madai-w14 I-C-12；运行时契约设计稿 R-8 / T6 的核侧半边）：按键前缀丢弃生成上下文（KV 缓存 + 适配器亲和），
 * **同步等**条数并回填 *out_dropped（可为 NULL = 不要条数）。宿主租约关户时以账户前缀 acct:<accountId>/ 调本符号，之后同键
 * （同一会话 id）的预留必是新建、不复用前一账户的 KV。
 * 语义：键 starts_with(prefix) 的条目立即离池、不可复用；空闲条目 KV 立即结清；在飞任务持有的条目延迟到该任务终态才结清 KV
 * （不强拆在飞生成，也不写回）。空前缀不含账户段，按 0 条处理、不动池；无匹配回 0 条。
 * 返回值只表达调用本身：0 = 已拿到条数；core 为 NULL、prefix.len > 0 而 ptr 为 NULL、prefix 非 UTF-8 回 4001；核已停回 4004；
 * 核内 panic 收口回 4002。prefix 按 madinfer_str 按值传（同 §4 kv_get / kv_put），借用语义：只在调用期间有效。
 * 线程契约：在核的 control 线程上执行、不回调宿主、不等 worker；调用方阻塞等核回执，宿主不得在任何 host / sink 回调里调本符号
 * （会等自己）；与 madinfer_stream_cancel 无顺序要求。只加符号、不改任何结构 / 枚举 → MADINFER_ABI_VERSION 仍 2（文件头 madai-w14 契约注）。
 */
int32_t madinfer_drop_contexts(madinfer_core_handle core, madinfer_str prefix, uint32_t* out_dropped);

/** Explicit backend: 0 = test-only emulation; 1 = native llama (requires llama-sys).
 * No native-to-emulation fallback. model_path borrowed UTF-8, copied on call.
 * All other create pointer/lifetime restrictions still apply. */
madinfer_core_handle madinfer_core_create_with_backend(const madinfer_host_vtable* host, const madinfer_core_options* options, uint32_t backend, madinfer_str model_path, int32_t* out_error_code);
/** Actual selected backend, including explicit test-only classification. */
int32_t madinfer_backend_kind(madinfer_core_handle core, uint32_t* out_kind);

#ifdef __cplusplus
}
#endif

#endif /* MADINFER_H */
