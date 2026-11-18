//! 安装 FSM（11 条迁移）与安装器（owner）。
//!
//! ```text
//!  Discovered ──ManifestAccepted──▶ ManifestVerified ──DownloadsStarted──▶ Downloading ──AllBlobsArrived──▶ Verifying ──BlobsVerified──▶ Installed ──Activated──▶ Active
//!      │                                  │                                   │  ▲ BlobArrived                │                            ▲          │
//!      └──ManifestRejected──▶ Quarantined ◀┘（跳过下载：AllBlobsPresent → Verifying）└──DownloadFailed──▶ Quarantined ◀┘ HashMismatch     └──Superseded──┘
//! ```
//! 验签先于一切；blob 到齐先比 sha256 再入库；激活是指针切换。进度（剩余 blob 数 / 字节）是 owner 附属字段。

use std::collections::{BTreeMap, BTreeSet};

use madinfer_core_types::{AssetId, PackageRef};
use madinfer_fsm::{Fsm, FsmBuilder, FsmEvent};

use crate::store::{BlobRef, InstallPlan, StoreView};

/// 安装状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InstallState {
    /// 云端 / 配置里出现了这个包。
    Discovered,
    /// manifest 验签 + 兼容校验通过。
    ManifestVerified,
    /// blob 下载中。
    Downloading,
    /// 全部 blob 到齐，校验 sha256 中。
    Verifying,
    /// 已入库（manifest 落盘、注册表登记），未激活。
    Installed,
    /// ACTIVE 指向本版本。
    Active,
    /// 隔离：验签失败 / hash 不符 / 永久下载失败；直到新的 manifest 到来。
    Quarantined,
}

/// 隔离原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuarantineReason {
    /// manifest 签名无效。
    SignatureInvalid,
    /// 兼容矩阵不满足。
    Incompatible,
    /// blob 内容 hash 不符。
    HashMismatch,
    /// 下载永久失败（错误码 ≥ 1000）。
    DownloadFailed,
}

/// manifest 检查结论（验签 + 兼容；由 verify.rs / compat.rs 算出后传入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestCheck {
    /// 通过。
    Valid,
    /// 签名无效。
    SignatureInvalid,
    /// 不兼容（SDK / 引擎 / 格式 / 内存 / 档位 / OS）。
    Incompatible,
}

/// 安装事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallEvent {
    /// manifest 通过。
    ManifestAccepted,
    /// manifest 拒绝。
    ManifestRejected(QuarantineReason),
    /// 有 blob 要下载，已发起。
    DownloadsStarted,
    /// 全部 blob 已在库（共享去重）。
    AllBlobsPresent,
    /// 一个 blob 到达。
    BlobArrived,
    /// 全部到达。
    AllBlobsArrived,
    /// 永久失败。
    DownloadFailed,
    /// sha256 全部相符。
    BlobsVerified,
    /// 有 blob 不符。
    HashMismatch,
    /// 指针切到本版本。
    Activated,
    /// 另一版本被激活。
    Superseded,
}

/// 安装事件种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InstallEventKind {
    /// 见 [`InstallEvent::ManifestAccepted`]。
    ManifestAccepted,
    /// 见 [`InstallEvent::ManifestRejected`]。
    ManifestRejected,
    /// 见 [`InstallEvent::DownloadsStarted`]。
    DownloadsStarted,
    /// 见 [`InstallEvent::AllBlobsPresent`]。
    AllBlobsPresent,
    /// 见 [`InstallEvent::BlobArrived`]。
    BlobArrived,
    /// 见 [`InstallEvent::AllBlobsArrived`]。
    AllBlobsArrived,
    /// 见 [`InstallEvent::DownloadFailed`]。
    DownloadFailed,
    /// 见 [`InstallEvent::BlobsVerified`]。
    BlobsVerified,
    /// 见 [`InstallEvent::HashMismatch`]。
    HashMismatch,
    /// 见 [`InstallEvent::Activated`]。
    Activated,
    /// 见 [`InstallEvent::Superseded`]。
    Superseded,
}

impl FsmEvent for InstallEvent {
    type Kind = InstallEventKind;

    fn kind(&self) -> InstallEventKind {
        match self {
            InstallEvent::ManifestAccepted => InstallEventKind::ManifestAccepted,
            InstallEvent::ManifestRejected(_) => InstallEventKind::ManifestRejected,
            InstallEvent::DownloadsStarted => InstallEventKind::DownloadsStarted,
            InstallEvent::AllBlobsPresent => InstallEventKind::AllBlobsPresent,
            InstallEvent::BlobArrived => InstallEventKind::BlobArrived,
            InstallEvent::AllBlobsArrived => InstallEventKind::AllBlobsArrived,
            InstallEvent::DownloadFailed => InstallEventKind::DownloadFailed,
            InstallEvent::BlobsVerified => InstallEventKind::BlobsVerified,
            InstallEvent::HashMismatch => InstallEventKind::HashMismatch,
            InstallEvent::Activated => InstallEventKind::Activated,
            InstallEvent::Superseded => InstallEventKind::Superseded,
        }
    }
}

/// 安装效果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallEffect {
    /// 计算安装计划并发起缺失 blob 的下载（owner 调 Downloader）。
    StartDownloads,
    /// 取消在途下载。
    CancelDownloads,
    /// 对全部 blob 比 sha256（owner 读盘算 hash）。
    VerifyBlobs,
    /// 写 manifest 到 packages/<name>/<ver>/、登记注册表。
    WriteManifest,
    /// 指针切换（PackageRegistry::activate）。
    SwitchActive,
    /// 隔离并打点。
    Quarantine(QuarantineReason),
    /// 删已下载的 .part 与未入库的 blob。
    DeleteStaged,
}

fn start_downloads(_: &InstallEvent) -> Vec<InstallEffect> {
    vec![InstallEffect::StartDownloads]
}

fn verify(_: &InstallEvent) -> Vec<InstallEffect> {
    vec![InstallEffect::VerifyBlobs]
}

fn write_manifest(_: &InstallEvent) -> Vec<InstallEffect> {
    vec![InstallEffect::WriteManifest]
}

fn switch_active(_: &InstallEvent) -> Vec<InstallEffect> {
    vec![InstallEffect::SwitchActive]
}

fn quarantine_manifest(e: &InstallEvent) -> Vec<InstallEffect> {
    match e {
        InstallEvent::ManifestRejected(reason) => vec![InstallEffect::Quarantine(*reason)],
        _ => Vec::new(),
    }
}

fn quarantine_download(_: &InstallEvent) -> Vec<InstallEffect> {
    vec![
        InstallEffect::CancelDownloads,
        InstallEffect::DeleteStaged,
        InstallEffect::Quarantine(QuarantineReason::DownloadFailed),
    ]
}

fn quarantine_hash(_: &InstallEvent) -> Vec<InstallEffect> {
    vec![
        InstallEffect::DeleteStaged,
        InstallEffect::Quarantine(QuarantineReason::HashMismatch),
    ]
}

/// 迁移表条目数。
pub const INSTALL_FSM_TABLE_SIZE: usize = 11;

/// 建一台安装 FSM（初始 Discovered）。
pub fn build_install_fsm() -> Fsm<InstallState, InstallEvent, InstallEffect> {
    use InstallEventKind as K;
    use InstallState as S;
    let table = Ok(FsmBuilder::<S, InstallEvent, InstallEffect>::new(
        S::Discovered,
    ))
    .and_then(|b| b.on(S::Discovered, K::ManifestAccepted, S::ManifestVerified))
    .and_then(|b| {
        b.on_with(
            S::Discovered,
            K::ManifestRejected,
            S::Quarantined,
            quarantine_manifest,
        )
    })
    .and_then(|b| {
        b.on_with(
            S::ManifestVerified,
            K::DownloadsStarted,
            S::Downloading,
            start_downloads,
        )
    })
    .and_then(|b| {
        b.on_with(
            S::ManifestVerified,
            K::AllBlobsPresent,
            S::Verifying,
            verify,
        )
    })
    .and_then(|b| b.on(S::Downloading, K::BlobArrived, S::Downloading))
    .and_then(|b| b.on_with(S::Downloading, K::AllBlobsArrived, S::Verifying, verify))
    .and_then(|b| {
        b.on_with(
            S::Downloading,
            K::DownloadFailed,
            S::Quarantined,
            quarantine_download,
        )
    })
    .and_then(|b| b.on_with(S::Verifying, K::BlobsVerified, S::Installed, write_manifest))
    .and_then(|b| {
        b.on_with(
            S::Verifying,
            K::HashMismatch,
            S::Quarantined,
            quarantine_hash,
        )
    })
    .and_then(|b| b.on_with(S::Installed, K::Activated, S::Active, switch_active))
    .and_then(|b| b.on(S::Active, K::Superseded, S::Installed));
    match table {
        Ok(builder) => builder.build(),
        Err(_) => FsmBuilder::new(S::Discovered).build(),
    }
}

struct Entry {
    fsm: Fsm<InstallState, InstallEvent, InstallEffect>,
    blobs: Vec<BlobRef>,
    remaining: BTreeSet<AssetId>,
    downloaded_bytes: u64,
    total_bytes: u64,
}

/// 安装器（owner）：每包一台 FSM + 进度附属字段。
#[derive(Default)]
pub struct Installer {
    entries: BTreeMap<PackageRef, Entry>,
}

/// 安装器对外的进度（进能力索引 Downloading{permille}）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// 状态。
    pub state: InstallState,
    /// 千分比（Downloading 期有效）。
    pub permille: u16,
    /// 剩余字节。
    pub remaining_bytes: u64,
}

impl Installer {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 某包状态。
    pub fn state(&self, package: &PackageRef) -> Option<InstallState> {
        self.entries.get(package).map(|e| e.fsm.state())
    }

    /// 进度。
    pub fn progress(&self, package: &PackageRef) -> Option<Progress> {
        self.entries.get(package).map(|e| Progress {
            state: e.fsm.state(),
            permille: e
                .downloaded_bytes
                .saturating_mul(1000)
                .checked_div(e.total_bytes)
                .and_then(|value| u16::try_from(value).ok())
                .unwrap_or(1000),
            remaining_bytes: e.total_bytes.saturating_sub(e.downloaded_bytes),
        })
    }

    /// 发现一个包：先看 manifest 检查结论，再按库快照定计划。返回效果（含 [`InstallEffect::StartDownloads`] 时，
    /// owner 用 [`Installer::plan`] 取要下载的 blob）。
    pub fn begin(
        &mut self,
        package: PackageRef,
        blobs: Vec<BlobRef>,
        check: ManifestCheck,
        view: &StoreView,
        now_ms: u64,
    ) -> Vec<InstallEffect> {
        let mut fsm = build_install_fsm();
        let mut effects = Vec::new();
        let event = match check {
            ManifestCheck::Valid => InstallEvent::ManifestAccepted,
            ManifestCheck::SignatureInvalid => {
                InstallEvent::ManifestRejected(QuarantineReason::SignatureInvalid)
            }
            ManifestCheck::Incompatible => {
                InstallEvent::ManifestRejected(QuarantineReason::Incompatible)
            }
        };
        effects.extend(fsm.fire(&event, now_ms).effects);
        let plan = InstallPlan::compute(&blobs, view);
        let remaining: BTreeSet<AssetId> = plan.to_download.iter().map(|b| b.id).collect();
        let total_bytes = plan.bytes_to_download;
        if fsm.state() == InstallState::ManifestVerified {
            let next = if remaining.is_empty() {
                InstallEvent::AllBlobsPresent
            } else {
                InstallEvent::DownloadsStarted
            };
            effects.extend(fsm.fire(&next, now_ms).effects);
        }
        self.entries.insert(
            package,
            Entry {
                fsm,
                blobs,
                remaining,
                downloaded_bytes: 0,
                total_bytes,
            },
        );
        effects
    }

    /// 待下载 blob（Downloading 期）。
    pub fn plan(&self, package: &PackageRef) -> Vec<BlobRef> {
        self.entries.get(package).map_or_else(Vec::new, |e| {
            e.blobs
                .iter()
                .filter(|b| e.remaining.contains(&b.id))
                .cloned()
                .collect()
        })
    }

    /// 一个 blob 下载完成（owner 已确认落点存在；hash 校验在 Verifying 阶段统一做）。
    pub fn on_blob_downloaded(
        &mut self,
        package: &PackageRef,
        blob: AssetId,
        now_ms: u64,
    ) -> Vec<InstallEffect> {
        let Some(e) = self.entries.get_mut(package) else {
            return Vec::new();
        };
        if !e.remaining.remove(&blob) {
            return Vec::new();
        }
        if let Some(b) = e.blobs.iter().find(|b| b.id == blob) {
            e.downloaded_bytes = e.downloaded_bytes.saturating_add(b.size);
        }
        let mut effects = e.fsm.fire(&InstallEvent::BlobArrived, now_ms).effects;
        if e.remaining.is_empty() {
            effects.extend(e.fsm.fire(&InstallEvent::AllBlobsArrived, now_ms).effects);
        }
        effects
    }

    /// 下载永久失败。
    pub fn on_download_failed(&mut self, package: &PackageRef, now_ms: u64) -> Vec<InstallEffect> {
        self.fire(package, &InstallEvent::DownloadFailed, now_ms)
    }

    /// 校验结论。
    pub fn on_verified(
        &mut self,
        package: &PackageRef,
        all_match: bool,
        now_ms: u64,
    ) -> Vec<InstallEffect> {
        let event = if all_match {
            InstallEvent::BlobsVerified
        } else {
            InstallEvent::HashMismatch
        };
        self.fire(package, &event, now_ms)
    }

    /// 激活：本版本 Installed → Active，同名其他 Active 版本 → Installed（Superseded）。
    pub fn activate(&mut self, package: &PackageRef, now_ms: u64) -> Vec<InstallEffect> {
        let others: Vec<PackageRef> = self
            .entries
            .iter()
            .filter(|(p, e)| {
                p.name() == package.name() && *p != package && e.fsm.state() == InstallState::Active
            })
            .map(|(p, _)| p.clone())
            .collect();
        let mut effects = Vec::new();
        for other in others {
            effects.extend(self.fire(&other, &InstallEvent::Superseded, now_ms));
        }
        effects.extend(self.fire(package, &InstallEvent::Activated, now_ms));
        effects
    }

    /// 已进隔离的包（观测 quarantine）。
    pub fn quarantined(&self) -> Vec<PackageRef> {
        self.entries
            .iter()
            .filter(|(_, e)| e.fsm.state() == InstallState::Quarantined)
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// 迁移轨迹（describe 格式）。
    pub fn trace(&self, package: &PackageRef) -> Vec<String> {
        self.entries
            .get(package)
            .map_or_else(Vec::new, |e| e.fsm.trace().map(|r| r.describe()).collect())
    }

    fn fire(
        &mut self,
        package: &PackageRef,
        event: &InstallEvent,
        now_ms: u64,
    ) -> Vec<InstallEffect> {
        self.entries
            .get_mut(package)
            .map_or_else(Vec::new, |e| e.fsm.fire(event, now_ms).effects)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::store::BlobKind;

    fn id(byte: &str) -> AssetId {
        AssetId::parse(&format!("sha256:{}", byte.repeat(32))).expect("id")
    }

    fn pkg(s: &str) -> PackageRef {
        PackageRef::parse(s).expect("pkg")
    }

    fn blobs() -> Vec<BlobRef> {
        vec![
            BlobRef {
                id: id("aa"),
                size: 1_000,
                kind: BlobKind::BackboneWeights,
                shared: true,
                url: Some("u".into()),
            },
            BlobRef {
                id: id("bb"),
                size: 200,
                kind: BlobKind::Adapter,
                shared: false,
                url: Some("u".into()),
            },
            BlobRef {
                id: id("cc"),
                size: 4,
                kind: BlobKind::PromptTemplate,
                shared: false,
                url: Some("u".into()),
            },
        ]
    }

    #[test]
    fn table_is_locked() {
        assert_eq!(build_install_fsm().table_size(), INSTALL_FSM_TABLE_SIZE);
    }

    #[test]
    fn full_install_path_with_shared_base_already_present() {
        let mut inst = Installer::new();
        let mut view = StoreView::default();
        view.blobs.insert(id("aa"));
        let p = pkg("catchup-summary@1.4.2");
        let fx = inst.begin(p.clone(), blobs(), ManifestCheck::Valid, &view, 0);
        assert_eq!(fx, vec![InstallEffect::StartDownloads]);
        assert_eq!(inst.state(&p), Some(InstallState::Downloading));
        assert_eq!(inst.plan(&p).len(), 2, "基座已在库，只下 adapter + 模板");
        assert_eq!(inst.progress(&p).expect("p").permille, 0);

        assert!(
            inst.on_blob_downloaded(&p, id("bb"), 1).is_empty(),
            "进度事件无效果"
        );
        assert_eq!(inst.progress(&p).expect("p").permille, 980, "200 / 204");
        assert!(
            inst.on_blob_downloaded(&p, id("bb"), 2).is_empty(),
            "重复到达忽略"
        );
        let fx = inst.on_blob_downloaded(&p, id("cc"), 3);
        assert_eq!(fx, vec![InstallEffect::VerifyBlobs]);
        assert_eq!(inst.state(&p), Some(InstallState::Verifying));

        let fx = inst.on_verified(&p, true, 4);
        assert_eq!(fx, vec![InstallEffect::WriteManifest]);
        let fx = inst.activate(&p, 5);
        assert_eq!(fx, vec![InstallEffect::SwitchActive]);
        assert_eq!(inst.state(&p), Some(InstallState::Active));
        assert_eq!(
            inst.trace(&p),
            [
                "Discovered --ManifestAccepted--> ManifestVerified @0ms",
                "ManifestVerified --DownloadsStarted--> Downloading @0ms",
                "Downloading --BlobArrived--> Downloading @1ms",
                "Downloading --BlobArrived--> Downloading @3ms",
                "Downloading --AllBlobsArrived--> Verifying @3ms",
                "Verifying --BlobsVerified--> Installed @4ms",
                "Installed --Activated--> Active @5ms",
            ]
        );

        // 新版本激活：旧版本 Superseded。
        let p2 = pkg("catchup-summary@1.4.3");
        let mut view2 = StoreView::default();
        view2.blobs.extend([id("aa"), id("bb"), id("cc")]);
        let fx = inst.begin(p2.clone(), blobs(), ManifestCheck::Valid, &view2, 6);
        assert_eq!(fx, vec![InstallEffect::VerifyBlobs], "全部在库直接校验");
        inst.on_verified(&p2, true, 7);
        let fx = inst.activate(&p2, 8);
        assert_eq!(fx, vec![InstallEffect::SwitchActive]);
        assert_eq!(
            inst.state(&p),
            Some(InstallState::Installed),
            "旧版本退回 Installed（PREVIOUS）"
        );
        assert_eq!(inst.state(&p2), Some(InstallState::Active));
    }

    #[test]
    fn signature_failure_never_starts_downloads_and_hash_mismatch_quarantines() {
        let mut inst = Installer::new();
        let p = pkg("evil@1.0.0");
        let fx = inst.begin(
            p.clone(),
            blobs(),
            ManifestCheck::SignatureInvalid,
            &StoreView::default(),
            0,
        );
        assert_eq!(
            fx,
            vec![InstallEffect::Quarantine(
                QuarantineReason::SignatureInvalid
            )]
        );
        assert!(inst.plan(&p).is_empty() || inst.state(&p) == Some(InstallState::Quarantined));
        assert_eq!(inst.quarantined(), vec![p.clone()]);

        let p2 = pkg("corrupt@1.0.0");
        inst.begin(
            p2.clone(),
            blobs(),
            ManifestCheck::Valid,
            &StoreView::default(),
            1,
        );
        for b in ["aa", "bb", "cc"] {
            inst.on_blob_downloaded(&p2, id(b), 2);
        }
        let fx = inst.on_verified(&p2, false, 3);
        assert_eq!(
            fx,
            vec![
                InstallEffect::DeleteStaged,
                InstallEffect::Quarantine(QuarantineReason::HashMismatch)
            ]
        );
        assert!(
            inst.activate(&p2, 4).is_empty(),
            "隔离的包不可激活（无出边）"
        );

        let p3 = pkg("flaky@1.0.0");
        inst.begin(
            p3.clone(),
            blobs(),
            ManifestCheck::Valid,
            &StoreView::default(),
            5,
        );
        let fx = inst.on_download_failed(&p3, 6);
        assert_eq!(fx[0], InstallEffect::CancelDownloads);
        assert_eq!(inst.quarantined().len(), 3);
    }
}
