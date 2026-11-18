//! 执行槽类别（C1 底座；madai-w05 D5 自 madinfer-sched 下沉——调度的槽表与后端的探测声明
//! （`BackendCaps.slot`）引用同一枚举，避免 backend → sched 的反向依赖）。

/// 执行槽类别。NPU 槽串行且不可抢占。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotClass {
    /// CPU 推理。
    Cpu,
    /// GPU（Metal / Vulkan）。
    Gpu,
    /// NPU / 系统 AI 加速器。
    Npu,
    /// 外挂后端（系统模型 / 云端），按后端编号分槽。
    External(u8),
}

impl SlotClass {
    /// 是否可被交互档抢占。
    pub const fn preemptible(self) -> bool {
        !matches!(self, Self::Npu)
    }
}
