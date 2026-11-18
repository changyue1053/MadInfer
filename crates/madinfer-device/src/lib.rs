//! madinfer-device —— 设备画像（设计文档 C11）：SoC / NPU 静态识别表、档位（Tier）判定、来源门。
//!
//! 全部纯函数：IO 事实来自宿主（[`madinfer_host::DeviceFacts`]），微基准（带宽 / int8 实测）归后续批次——
//! 本批带宽取识别表静态估值，实测落地后以参数覆盖（[`DeviceProfile::from_facts_with_bandwidth`]）。
//! 档位线缺省为编译内值，可由 config.v1 `thresholds.tier_lines` 经参数传入（[`TierLines`]）。
//! 判定取 RAM 与带宽两维的低档；任一维缺失即 `Tier::Unknown`。Unknown / Low 档只允许系统模型与云端
//! （[`allowed_sources`]），内联模型不进决策链。画像不含任何设备标识符（device.v1 不变量）。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod profile;
mod soc;
mod tier;

pub use profile::DeviceProfile;
pub use soc::{identify_soc, NpuApi, SocSpec, SOC_SPECS};
pub use tier::{allowed_sources, judge_tier, TierLine, TierLines};
