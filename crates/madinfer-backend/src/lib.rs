//! madinfer-backend —— L6 后端抽象（设计文档 C6 / §4.2）。
//!
//! 四个 trait 一条链：[`Backend`]（探测 / 可用性 / 加载）→ [`Model`]（元数据 / 占用 / 打开上下文 / 卸载）→
//! [`Context`]（一轮生成，`Send` 不 `Sync`，固定在打开它的 worker）→ [`TokenSink`]（逐片回调，可回 `Stop`）。
//! 取消经 [`CancelToken`] 翻旗，引擎每 token 查一次；错误是闭合枚举 [`BackendError`]，经 `From` 走 §7.3 固定映射表进
//! `InferError` 三分类。[`BackendRegistry`] 在注册时探测一次并缓存能力、连续 3 次崩溃隔离、以 [`RegistrySnapshot`] 跨线程。
//!
//! 后端实现者的六条（§4.2）：① `is_cancelled()` 为真后至多再产出一个 token 就返回 `Cancelled`；② `on_token` 回调内不持会影响
//! `cancel()` 的锁；③ `probe()` 不加载模型、不做 IO、≤ 10 ms；④ `footprint()` 在 `load` 返回后即准确；⑤ 崩溃用 `Crashed` 不 panic；
//! ⑥ 外挂后端的可用性变化主动通知。testkit 的 `backend_contract_tests!` 对任意实现验 ①②④ 与并发上限、约束声明、错误映射。
//!
//! 与设计文档 §4.2 签名的差异（本批口径）：`probe()` 不接设备画像（C11 未落，画像变化时由 owner 重新 `register`）；
//! `load(&LoadOpts)` 以 `package` / `weights` 指向已验签资产（`VerifiedAsset` 随 madinfer-assets 接入替换）；
//! `Model` 只保留 Generate 原语（Task 原语 `run(&TensorIo)` 随 C12 张量类型落地）；`BackendCaps` 只含调度与能力索引此刻消费的字段。
//!
//! 每上下文 KV 读数（madai-w16 I-C-14，I-C-13 §7 提案）：[`Backend::context_kv_mb`]（加载前口径）与 [`Model::context_kv_mb`]
//! （加载后口径）都是带缺省实现的只加方法——owner（madinfer-api 上下文池）以它对着 `thresholds.kv_budget_mb[tier]` 记账、腾 LRU；
//! 缺省 0 = 本后端不申报 KV（池只记亲和与命中）。此前池记账靠 owner 手里的常数估算，读数与后端实际驻留脱节。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod cancel;
mod error;
mod registry;
mod types;

use std::sync::Arc;

use madinfer_core_types::BackendId;

pub use cancel::{CancelReason, CancelToken};
pub use error::{
    BackendError, BUSY_RETRY_AFTER_MS, NETWORK_RETRY_AFTER_MS, OOM_RETRY_AFTER_MS,
    TRANSIENT_RETRY_AFTER_MS,
};
pub use registry::{
    BackendRegistry, RegisterError, RegistrySnapshot, RegistryStats, QUARANTINE_CRASH_THRESHOLD,
};
pub use types::{
    Availability, BackendCaps, Constraint, ConstraintSupport, ContextOptions, FinishReason,
    GenerateOptions, GenerateStats, LoadOpts, MemoryFootprint, Message, ModelFormat, ModelMeta,
    Quant, Role, SinkControl, TokenPiece, Turn, UnavailableReason,
};

/// 后端：一种引擎或一个外挂系统模型 / 云端端点。注册后不移除，quarantine 只是标记。
pub trait Backend: Send + Sync {
    /// 注册制 id（"llama.cpp"、"ios.foundation_models"、"cloud.enterprise" …）。
    fn id(&self) -> &BackendId;
    /// 在此设备上的能力声明；不加载模型、不做 IO。
    fn probe(&self) -> BackendCaps;
    /// 可用性（内联后端恒 `Available`）。
    fn availability(&self) -> Availability;
    /// 加载权重；返回的 Model 由运行时的注册表管生命周期。
    fn load(&self, opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError>;
    /// 每个生成上下文的 KV 驻留 MB——加载前即可知的口径（madai-w16 I-C-14）：仿真核回配置常数，真实引擎按 ctx 参数推算。
    /// owner 在模型加载前以它预留池预算；加载后以 [`Model::context_kv_mb`] 为准。缺省 0 = 不申报（池只记亲和与命中）。
    fn context_kv_mb(&self) -> u32 {
        0
    }
}

impl<B: Backend + ?Sized> Backend for Arc<B> {
    fn id(&self) -> &BackendId {
        (**self).id()
    }

    fn probe(&self) -> BackendCaps {
        (**self).probe()
    }

    fn availability(&self) -> Availability {
        (**self).availability()
    }

    fn load(&self, opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError> {
        (**self).load(opts)
    }

    fn context_kv_mb(&self) -> u32 {
        (**self).context_kv_mb()
    }
}

impl<B: Backend + ?Sized> Backend for Box<B> {
    fn id(&self) -> &BackendId {
        (**self).id()
    }

    fn probe(&self) -> BackendCaps {
        (**self).probe()
    }

    fn availability(&self) -> Availability {
        (**self).availability()
    }

    fn load(&self, opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError> {
        (**self).load(opts)
    }

    fn context_kv_mb(&self) -> u32 {
        (**self).context_kv_mb()
    }
}

/// 已加载的模型。同时打开的上下文数受 `caps.concurrency` 约束，超出返回 `Busy`。
pub trait Model: Send + Sync {
    /// 元数据。
    fn meta(&self) -> ModelMeta;
    /// 内存占用（`load` 返回后即准确）。
    fn footprint(&self) -> MemoryFootprint;
    /// 打开生成上下文；返回的 Context 固定到调用它的 worker。
    fn open_context(&self, options: &ContextOptions) -> Result<Box<dyn Context>, BackendError>;
    /// 卸载：释放权重与全部句柄（消费 self）。
    fn unload(self: Box<Self>);
    /// 每个打开的上下文的 KV 驻留 MB（madai-w16 I-C-14）：`load` 返回后即准确（与 [`Self::footprint`] 同口径——占用里的 KV 项
    /// = 本值 × 活跃上下文数）。owner 拿到它后替换加载前的 [`Backend::context_kv_mb`]。缺省 0 = 不申报。
    fn context_kv_mb(&self) -> u32 {
        0
    }
}

/// 生成上下文（KV 缓存 + 适配器亲和）。`Send` 不 `Sync`：只在亲和 worker 上调用。
pub trait Context: Send {
    /// 一轮生成：`turn` 含完整消息历史；token 逐片经 `sink` 回调；`cancel` 每 token 查一次。
    fn generate(
        &mut self,
        turn: &Turn,
        options: &GenerateOptions,
        constraint: Option<&Constraint>,
        sink: &mut dyn TokenSink,
        cancel: &CancelToken,
    ) -> Result<GenerateStats, BackendError>;
}

/// token 接收方（worker 侧把片回投核 actor 的邮箱）。
pub trait TokenSink {
    /// 一片到达；返回 `Stop` 让后端在下一 token 前结束。
    fn on_token(&mut self, piece: TokenPiece) -> SinkControl;
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use madinfer_host::LogLevel;

    struct Probe {
        id: BackendId,
        caps: BackendCaps,
        availability: Availability,
        probes: AtomicU32,
    }

    impl Probe {
        fn new(id: &str, supports_abort: bool, availability: Availability) -> Arc<Self> {
            Arc::new(Self {
                id: BackendId(id.to_owned()),
                caps: BackendCaps {
                    slot: madinfer_core_types::SlotClass::External(0),
                    max_context_tokens: 4_096,
                    concurrency: 1,
                    supports_abort,
                    constraint: ConstraintSupport::None,
                    tools: false,
                    image_input: false,
                    audio_input: false,
                },
                availability,
                probes: AtomicU32::new(0),
            })
        }
    }

    impl Backend for Probe {
        fn id(&self) -> &BackendId {
            &self.id
        }

        fn probe(&self) -> BackendCaps {
            self.probes.fetch_add(1, Ordering::SeqCst);
            self.caps
        }

        fn availability(&self) -> Availability {
            self.availability
        }

        fn load(&self, _opts: &LoadOpts) -> Result<Box<dyn Model>, BackendError> {
            Err(BackendError::ModelMissing)
        }
    }

    struct CountingLogger(AtomicU32);

    impl madinfer_host::Logger for CountingLogger {
        fn log(&self, level: LogLevel, _target: &'static str, facts: &str) {
            if level == LogLevel::Warn && facts.contains("registry=quarantine") {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn register_probes_once_and_rejects_duplicate_ids() {
        let mut reg = BackendRegistry::new();
        let llama = Probe::new("llama.cpp", true, Availability::Available);
        reg.register(llama.clone()).expect("first");
        assert_eq!(llama.probes.load(Ordering::SeqCst), 1);
        let dup = Probe::new("llama.cpp", true, Availability::Available);
        assert_eq!(
            reg.register(dup.clone()),
            Err(RegisterError::DuplicateId(BackendId("llama.cpp".into())))
        );
        assert_eq!(dup.probes.load(Ordering::SeqCst), 0, "被拒的不探测");
        assert!(reg.caps(&BackendId("llama.cpp".into())).is_some());
        let _ = reg.caps(&BackendId("llama.cpp".into()));
        let _ = reg.available_backends();
        assert_eq!(
            llama.probes.load(Ordering::SeqCst),
            1,
            "读能力走缓存，不再探测"
        );
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.stats().rejected_duplicates, 1);
    }

    #[test]
    fn three_consecutive_crashes_quarantine_and_success_resets_streak() {
        let logger = Arc::new(CountingLogger(AtomicU32::new(0)));
        let mut reg = BackendRegistry::with_logger(logger.clone());
        let id = BackendId("llama.cpp".into());
        reg.register(Probe::new("llama.cpp", true, Availability::Available))
            .expect("register");
        assert!(!reg.record_crash(&id));
        assert!(!reg.record_crash(&id));
        reg.record_success(&id);
        assert!(!reg.record_crash(&id), "成功后重新计数");
        assert!(!reg.record_crash(&id));
        assert!(reg.record_crash(&id), "第三次连续崩溃触发隔离");
        assert!(reg.is_quarantined(&id));
        assert!(!reg.record_crash(&id), "已隔离不重复触发");
        assert_eq!(reg.stats().quarantined, 1);
        assert_eq!(reg.stats().crashes, 6);
        assert_eq!(logger.0.load(Ordering::SeqCst), 1, "隔离留痕一次");
        assert!(
            !reg.record_crash(&BackendId("ghost".into())),
            "未注册的 id 忽略"
        );
    }

    #[test]
    fn available_backends_excludes_quarantined_and_unavailable() {
        let mut reg = BackendRegistry::new();
        reg.register(Probe::new("llama.cpp", true, Availability::Available))
            .expect("a");
        reg.register(Probe::new(
            "ios.foundation_models",
            true,
            Availability::Downloading { permille: 400 },
        ))
        .expect("b");
        reg.register(Probe::new("android.aicore", true, Availability::Available))
            .expect("c");
        let aicore = BackendId("android.aicore".into());
        for _ in 0..QUARANTINE_CRASH_THRESHOLD {
            reg.record_crash(&aicore);
        }
        let available = reg.available_backends();
        assert_eq!(
            available.into_iter().collect::<Vec<_>>(),
            vec![BackendId("llama.cpp".into())],
            "下载中与隔离中都不进决策链"
        );
        assert!(reg.get(&aicore).is_some(), "隔离只是标记，句柄仍可取");
        assert_eq!(reg.quarantined().len(), 1);
    }

    #[test]
    fn interactive_requires_abort_support_and_no_quarantine() {
        let mut reg = BackendRegistry::new();
        reg.register(Probe::new("llama.cpp", true, Availability::Available))
            .expect("a");
        reg.register(Probe::new("ohos.system_ai", false, Availability::Available))
            .expect("b");
        let llama = BackendId("llama.cpp".into());
        assert!(reg.accepts_interactive(&llama));
        assert!(
            !reg.accepts_interactive(&BackendId("ohos.system_ai".into())),
            "supports_abort=false 不接交互档"
        );
        assert!(!reg.accepts_interactive(&BackendId("unknown".into())));
        for _ in 0..QUARANTINE_CRASH_THRESHOLD {
            reg.record_crash(&llama);
        }
        assert!(!reg.accepts_interactive(&llama), "隔离后也不接");
    }

    #[test]
    fn snapshot_is_immutable_and_shareable() {
        let mut reg = BackendRegistry::new();
        reg.register(Probe::new("llama.cpp", true, Availability::Available))
            .expect("a");
        let snapshot = Arc::new(reg.snapshot());
        let llama = BackendId("llama.cpp".into());
        reg.register(Probe::new("android.aicore", true, Availability::Available))
            .expect("b");
        for _ in 0..QUARANTINE_CRASH_THRESHOLD {
            reg.record_crash(&llama);
        }
        assert_eq!(snapshot.len(), 1, "快照不反映之后的注册");
        assert!(!snapshot.is_quarantined(&llama), "快照不反映之后的隔离");
        assert!(reg.is_quarantined(&llama));
        let worker = std::thread::spawn({
            let snapshot = Arc::clone(&snapshot);
            move || {
                snapshot
                    .get(&BackendId("llama.cpp".into()))
                    .map(|b| b.probe())
            }
        });
        let caps = worker.join().expect("join").expect("backend in snapshot");
        assert_eq!(caps.concurrency, 1);
        assert_eq!(
            snapshot.caps(&llama).map(|c| c.max_context_tokens),
            Some(4_096)
        );
        let arc_backend: Arc<dyn Backend> = Probe::new("boxed", true, Availability::Available);
        assert_eq!(
            arc_backend.id().0,
            "boxed",
            "Arc<dyn Backend> 也实现 Backend"
        );
    }
}
