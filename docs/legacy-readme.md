# MadInfer

端智能通用 SDK 的 Rust 内核。mad 的同级仓（形态同 imcore），经 `mad-bom.toml` 的 `[[sibling_repo]] name = "MadInfer"` 以 tag（`v<semver>-mad.<n>`，同五库）进入 mad 班次；mad 形态下本仓是 im-core-ffi 的 path 依赖（同 imdb-core），门 `infer_door` 在 im-core-ffi 里——一个进程只准加载一个核心二进制（imcore 规则第 10 条）；mad 侧的 KMP 投影在 `capability/capability-infer-*`，产品是 `products/mad-ai`。

设计依据：《端智能通用 SDK 架构设计分析》《端智能通用 SDK 架构设计（Rust 核心）》；接入与里程碑：《MadAI 产品接入计划》。

## 目录

```
Cargo.toml                 工作区：只产 rlib（mad 形态不自产 cdylib）、panic = unwind、统一 lints、依赖白名单（deny.toml）、禁墙钟 / 禁 Mutex（clippy.toml）
crates/madinfer-core-types    C1  底座类型
crates/madinfer-fsm           横切 表驱动 FSM 内核（沿 mad capability-im-engine fsm/Fsm.kt 五条规约）
crates/madinfer-host          C16 宿主端口 trait；MonotonicClock 是全仓唯一 Instant::now 豁免点
crates/madinfer-testkit       C19 替身：ManualClock / RecordingLogger / MemoryKv / RecordingTelemetrySink / ScriptedAuthorizer / NoopBackend / FakeBackend + backend_contract_tests! 六条契约宏
crates/madinfer-router        C3  路由纯函数：privacy_gate → 静态过滤 → priority；advance 降级推进
crates/madinfer-sched         C4  调度：Job FSM(23) + 门控表 + 配额台账 + 槽表 + 抢占 + 代数票定时器 + 排水；SchedCore sans-io + SchedulerActor
crates/madinfer-api           C2  能力面：Framer 合帧、请求 FSM(20，Running 态延迟发布 Started、AwaitingConsent 授权态)、CapabilityIndex；
                                 核 actor（core.rs CoreActor 有界邮箱泵 + workers.rs WorkerPool + pipeline.rs GenRequest / Event）；Infer 门面随 I3 ffi
crates/madinfer-backend       C6  后端抽象：Backend / Model / Context / TokenSink trait、CancelToken、BackendError → InferError（§7.3 表）、BackendRegistry（探测缓存、连续 3 次崩溃隔离、不可变快照）
crates/madinfer-config        C14 config.v1 类型 + 应用管线 FSM(7)：验签 / 解析 / 版本 / min_sdk / 分桶 / 语义 → 原子切换 / 回滚
crates/madinfer-runtime       C5  模型驻留 FSM(12，Loading 态 + begin_load / on_loaded / on_load_failed 加载协议) + 注册表（一基座、压力逐出顺序）+ 上下文池（KV 预算、LRU）
crates/madinfer-assets        C10 安装 FSM(11) + 内容寻址布局 + ACTIVE / PREVIOUS 指针 + GC 计划
crates/madinfer-observe       C13 闭合事件枚举 + 注册制 id + 采样批量；单测逐键核对 schemas/telemetry.v1.json 白名单
crates/madinfer-backend-llama C7  llama.cpp 后端适配器（W05 骨架；真实引擎 feature llama-sys 门控，I-C-04 填充）
crates/madinfer-device        C11 设备探测与分档（W05 骨架，I-C-04 填充）
crates/madinfer-preproc       C12 预处理：模板 / 截断 / schema→GBNF（W05 骨架，I-C-04 填充）
crates/madinfer-ffi           C17 FFI 门：UniFFI 出口 + panic 收口（W05 骨架，I-S-01 spike）；include/madinfer.h 是 C ABI 事实源，include/madinfer_debug.h 是调试面
fixtures/                  跨端金样事实源：signing_payload.golden.json（config.v1 签名载荷规范形，三端与云端签发侧读同一份断言；见下"签名载荷金样"）
schemas/                   协议 schema v1 事实源（mad-repo/schema/infer/ 是镜像，两仓门禁双向判红）
tools/check_schemas.py     schema 形式校验
deny.toml                  cargo-deny：第三方 crate 白名单 + 禁用清单
COMPAT.toml                与 mad BOM / bcv / ABI / schema 的兼容矩阵
third_party/PINS.toml      引擎钉版（llama.cpp 于 I2b 引入）
```

泳道规则：`Mad/madai-lane-rules.md`（15 条，mad rules 之外的 MadAI 增补）。

设计范式（全部 crate 共用，来源 mad-repo 既有工程）：表驱动 FSM + `table_size` 锁拓扑 + 非法事件计数不 panic；sans-io（迁移只产效果，IO 在 owner / actor）；
单消费者邮箱泵（`recv` / `recv_timeout`，不轮询）；注入时钟（测试手拨）；定时器代数票（陈旧票哑火）；合帧窗口折叠 + 终态强制放行；
计数台账 + `key=value` 留痕；进度 / 引用计数不进 FSM 状态；命名：状态裸名、事件过去式、效果动宾。

crate 落地顺序 = 设计文档 §4 的 C1～C19，按里程碑逐个加入 `[workspace].members`：

| 里程碑 | crate | 状态 |
|---|---|---|
| I1 | madinfer-core-types、schemas | 完成（madai-w01） |
| I2a | madinfer-fsm、madinfer-host、madinfer-testkit、madinfer-router、madinfer-sched、madinfer-api（三块纯逻辑）、madinfer-config、madinfer-runtime、madinfer-assets、madinfer-observe | 完成（madai-w02）；共 70 个单测 |
| I2b | madinfer-backend（trait + BackendRegistry）、testkit NoopBackend / FakeBackend + `backend_contract_tests!`、三处 FSM 补边（Job 23 / 驻留 12 / 请求 20）、madinfer-api 核 actor + worker 池 + 端到端管线（`Core::builder().spawn()` → `CoreHandle::stream`） | 完成（madai-w04 I-C-03）；共 116 个测试。剩余 I2b 项：madinfer-backend-llama（llama-sys、PINS 钉版）、madinfer-preproc、madinfer-device、assets verify.rs（ed25519 + JCS）/ crypto.rs、config 换 xxh3 分桶 |
| I3 | madinfer-ffi（UniFFI UDL + C ABI 表；mad 形态经 im-core-ffi 的 infer_door 导出，非 mad 宿主才自产 cdylib）、madinfer-api 的 Infer 门面（由 `Core::builder` 收口）、delivery 线程（Framer 与回调移出 control 线程）、madinfer-backend-system（外挂后端桥） | — |
| I4 | madinfer-eval、madinfer-evalrun | — |
| I5～I7 | madinfer-backend-cloud、size / 性能门、真机矩阵 | — |

## 本地

```
cargo check --offline --workspace
cargo test  --offline --workspace
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo fmt --check
python3 tools/check_schemas.py
bash tools/check_naming.sh            # 复用 mad-repo 禁词扫描器（词表全栈同名）
```

第三方只有 serde / serde_json / semver（及其传递依赖，见 deny.toml 白名单），全部在本地 registry 缓存内，无网可编。

## llama-sys 宿主真链接（spike，madai-w16 I-C-15）

缺省 feature 关：`cargo test --workspace` 不编 `third_party/`、不跑 cmake、`madinfer-backend-llama` 仍是仿真核。开 `llama-sys` 后
`build.rs` 以**系统 cmake**（无 cmake / cc crate）经 `crates/madinfer-backend-llama/csrc/CMakeLists.txt` 编 `third_party/llama.cpp`
（钉版 `third_party/PINS.toml`：tag `b10883`，裁剪子树 373 文件，MIT 随行）的 CPU 静态库 + C 桥，链进 `sys` 模块；
`LlamaBackend::native(options, gguf_path)` 走真实路径（greedy、每词元查取消旗、约束声明 `None`），`LlamaBackend::new` 不变。

```
bash tools/models/fetch.sh smollm2-135m-instruct-q4_k_m          # 按 tools/models/manifest.toml 拉到 ~/.cache/madinfer/models/，核 sha256
cargo build -p madinfer-backend-llama --features llama-sys --offline # 宿主 macOS arm64：cmake Release，Metal / BLAS / Accelerate 关
MADINFER_LLAMA_MODEL=~/.cache/madinfer/models/smollm2-135m-instruct-q4_k_m.gguf \
  cargo test -p madinfer-backend-llama --features llama-sys --offline # 六条契约 + 金样 tests/golden/llama_host.txt + 确定性 + 读数
cargo test -p madinfer-backend-llama --features llama-sys --offline --test native -- --nocapture   # 模型缺席：6 例 skip 留痕，不红
```

`MADINFER_LLAMA_CMAKE_JOBS` 控 cmake 并行度、`CMAKE` 指可执行；金样只对宿主 arm64 CPU 路径负责，Metal / Vulkan
回填归 I4d（`PINS.toml` 记同一 tarball 的 sha256，补目录可复现）。

## llama 三端交叉编译与预编静态库（madai-w17 I-C-17）

`build.rs` 按 `TARGET` 给 cmake 传工具链（CPU 路径，Metal / Accelerate 关）：iOS 模拟器 `CMAKE_SYSTEM_NAME=iOS` + `iphonesimulator` +
arm64 + 部署 16.0；Android NDK 27.2 `android.toolchain.cmake`（`arm64-v8a` / `android-24` / `c++_static`，`ANDROID_NDK_HOME` 覆盖）；
鸿蒙 DevEco `ohos.toolchain.cmake`（`OHOS_ARCH=arm64-v8a`，`OHOS_SDK_NATIVE` 覆盖；三元组 target_os 是 `linux`，按 `-linux-ohos`
后缀判）。Android / 鸿蒙显式 `GGML_CPU_ARM_ARCH=armv8-a`（`MADINFER_LLAMA_ARM_ARCH` 覆盖）。configure 参数变即清 cmake 目录重配。

```
bash tools/llama/build_targets.sh [--targets a,b] [--tag b10883] [--with-ffi] [--log-dir DIR]   # 缺省四目标：宿主 + iOS sim + Android + 鸿蒙
#   → target/madinfer-llama/<tag>/<triple>/{libmadinfer_llama_bridge,libllama,libggml,libggml-cpu,libggml-base}.a + MANIFEST.txt
#     （sha256 / 字节 / rustc / cc / cmake 版本 / 构建时长；--with-ffi 另记 libmadinfer_ffi.{a,dylib,so} 读数）；末行 RESULT build_targets.sh exit=<n> targets=<n> ok=<n>
MADINFER_LLAMA_LIB_DIR=target/madinfer-llama/b10883/aarch64-apple-darwin cargo build -p madinfer-backend-llama --features llama-sys --offline
#   目录在场且 5 个 .a 齐 → build.rs 不调 cmake 直接链（I4-01 预编缓存形态）；缺任一 .a 即构建红并点名，不回退
bash tools/llama/check_prebuilt.sh [--triple <t>]        # 上面两条的用例：CMAKE=/usr/bin/false 下正例绿 + 缺 libggml-cpu.a 反例红
```

鸿蒙 rust 目标只在 `stable` 工具链装了（`rust-toolchain.toml` 钉 1.85 未装），脚本以 `RUSTUP_TOOLCHAIN=stable`（`MADINFER_OHOS_RUSTUP_TOOLCHAIN`
覆盖）跑该目标；Android / 鸿蒙的 rustc 链接器由脚本以 `CARGO_TARGET_<T>_LINKER` 给 NDK / DevEco clang，不改 `.cargo/config.toml`。
`madinfer-ffi` 自身无 `llama-sys` feature，三端带引擎链接用 `cargo build -p madinfer-ffi --features madinfer-backend-llama/llama-sys --target <t> --release`。
四目标体积表与 mad w37 三端对照见收口报告 `Mad/madai-w17-report-i-c-17.md` §3（批次 Mad 目录，不入本仓）。

核 actor 的线程形态（I2b）：一个 control 线程（`madinfer-core`）独占调度核、请求 FSM、合帧器、配置、观测、后端 / 模型注册表；
worker 池按 `SlotTable` 每个槽类别起 N 个线程（`madinfer-worker-<n>`），持后端注册表的不可变快照，token 与回执经同一有界邮箱回投；
业务回调本批在 control 线程执行（`EventSink::on_event` 只入队即返回），I3 移到 delivery 线程。
工具链版本见 `rust-toolchain.toml`（与 imcore 同 1.85）。

## 签名载荷金样（三端消费法）

`fixtures/signing_payload.golden.json` 是 config.v1 签名载荷规范形的**唯一事实源**（madai-w11 I-C-10；此前三端各手写一份，serde 一变三端同红）。
规范形口径在 `crates/madinfer-config/src/doc.rs` 头注；`crates/madinfer-config/tests/signing_payload.rs` 读本文件断言，任何改变载荷字节的变更
本文件同批替换并在报告 §7 通知三端。

文件形态：`schema`（恒 `"madinfer signing_payload golden v1"`）、`note`、`documents[]`；每份文档五字段——

| 字段 | 含义 |
|---|---|
| `name` | 用例名（`non_canonical_shuffled` / `minimal_defaults` / `escapes_and_unicode`；第一份与 madai-w10 金样同值：813 字节、sha256 `795de2eb…7814d9`） |
| `document` | 文档原文（字符串；故意非规范形：键序打乱、带空白、缺省省略、未知键、转义与非 ASCII） |
| `canonical_hex` | `ConfigDoc::signing_payload(document)` 输出字节的小写 hex——**这就是要签 / 要验的那串字节** |
| `sha256` | 上述字节串的 sha256 小写 hex（壳侧不想比对长 hex 时只比它） |
| `note` | 这份文档钉住的规范形要点 |

三端消费法（都是"读文件 → 逐份断言"，不再手写规范形常量）：

- **I-D（Android / JVM 测试）**：把本文件作 test resource（Gradle `testFixtures` 或 `src/test/resources/`，构建脚本从 `../MadInfer/fixtures/` 拷入不手改），
  JUnit 逐份：`hexDecode(canonical_hex)` 即签名输入——用 JDK `Signature.getInstance("Ed25519")` 对它签出 `sig`，再把 `document` 原文塞上该 `sig`
  组成送核文档；同时断言 `sha256(hexDecode(canonical_hex)) == sha256`。
- **I-S（iOS / Swift 测试）**：`ConfigV1Fixture` 改为 `Bundle.module` 加载本文件（SPM resource，构建期从 MadInfer 拷入），`XCTAssertEqual(SHA256.hash(data: Data(hex:)), sha256)`；
  签名输入同样是 `canonical_hex` 解出的字节（CryptoKit `Curve25519.Signing`）。
- **I-H（鸿蒙 / ArkTS 测试）**：`rawfile` 收本文件，`cryptoFramework` 的 ed25519 `sign` 输入 = `canonical_hex` 解出的字节；hilog 留痕只准 `name=… len=… sha256_ok=1`，不打载荷。
- **云端签发侧**：对 `document` 原文按 doc.rs 口径规范化后 hex 须与 `canonical_hex` 逐字节相同——不同即签发侧规范化实现有偏差，不是核的问题。

## 调试面与绑定（两 profile 同一份）

`InferCore.createDebug` / `testStopControl`（uniffi）与 `madinfer_core_create_debug` / `madinfer_test_stop_control`（C，`include/madinfer_debug.h`）自 madai-w11 I-C-10 起
**导出无条件、函数体门控**：debug / release 产物符号面同数（`build_ohos.sh` 断言 `madinfer_*` = 11，debug 与 release 清单 diff 为空），release 下函数体恒
4005 `FeatureDisabled`（形式错误仍先 4001）。因此 `crates/madinfer-ffi/bindgen_swift.sh` / `bindgen_kotlin.sh` 不再按 profile 分叉（`MADINFER_BINDGEN_PROFILE` 退役、
设了只打一行忽略）：脚本每次从 debug 与 release 两枚宿主 cdylib 各生成一份、`diff -r` 为空才落 `bindings/`。宿主生产代码路径仍禁止引用调试面；自检据 4005 判"链到生产产物"。

验签 deadline：`madinfer_apply_config` / `InferCore.applyConfig` 的宿主验签在瞬时线程 `madinfer-ffi-verify` 上被调，control 线程最多等 `MADINFER_VERIFY_DEADLINE_MS`
（2 000 ms，`madinfer.h` §5 / `madinfer_ffi::VERIFY_DEADLINE_MS`），超时按 `signature_invalid` + `detail = "verify=timeout"` 拒，迟到回执哑火计数
（`verifyTimeoutCount()` / `verifyLateReplyCount()`）。宿主验签卡死不再拖死核站。

## 纪律

- `#![forbid(unsafe_code)]` 是所有非 FFI crate 的缺省；unsafe 只准出现在 madinfer-ffi 与 madinfer-backend-llama 的 sys 边界，逐处注释不变量。
- 禁墙钟、禁锁自旋、禁 tokio、禁 HTTP 客户端：时间、线程 QoS、网络全部经 madinfer-host 端口进来（deny.toml 的 deny 段是机器可读形式）。
- 用户内容（prompt、token 增量、图像 / 音频字节）禁入日志与 telemetry；telemetry.v1 的 `additionalProperties: false` 是白名单门。
- 分支与 tag 随 mad 班次编号：分支 `madai-w<N>/<子泳道>-<任务编号>`，tag `v<semver>-mad.<n>`（同五库；批次号写在 COMPAT.toml milestones）；schema 改动与 mad-repo `schema/infer/` 同一批合入。
