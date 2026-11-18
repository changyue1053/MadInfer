//! 内容寻址布局、安装计划、包注册表指针、GC 计划。全部纯计算；真实 IO 由 owner 按效果执行。

use std::collections::{BTreeMap, BTreeSet};

use madinfer_core_types::{AssetId, PackageRef};

/// blob 种类（manifest.v1 blobs[].kind）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlobKind {
    /// 基座权重。
    BackboneWeights,
    /// 适配器。
    Adapter,
    /// tokenizer。
    Tokenizer,
    /// 提示模板。
    PromptTemplate,
    /// JSON Schema。
    JsonSchema,
    /// 语法。
    Grammar,
    /// 评估集。
    EvalSet,
    /// 其他。
    Aux,
}

/// manifest 里对一个 blob 的引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRef {
    /// 内容 hash（加密 blob 即密文 hash）。
    pub id: AssetId,
    /// 字节。
    pub size: u64,
    /// 种类。
    pub kind: BlobKind,
    /// 允许跨包共享存储。
    pub shared: bool,
    /// 下载源。
    pub url: Option<String>,
}

/// 本地库快照（owner 扫盘得到；sans-io）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreView {
    /// 库中已有且校验过的 blob。
    pub blobs: BTreeSet<AssetId>,
    /// blob 最近被引用 / 安装时刻。
    pub blob_last_referenced_ms: BTreeMap<AssetId, u64>,
}

/// 安装计划：哪些 blob 要下载、哪些已在库（共享去重）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    /// 需下载。
    pub to_download: Vec<BlobRef>,
    /// 已在库。
    pub present: Vec<AssetId>,
    /// 需下载总字节。
    pub bytes_to_download: u64,
}

impl InstallPlan {
    /// 按库快照算计划。
    pub fn compute(blobs: &[BlobRef], view: &StoreView) -> Self {
        let mut to_download = Vec::new();
        let mut present = Vec::new();
        let mut bytes = 0u64;
        for b in blobs {
            if view.blobs.contains(&b.id) {
                present.push(b.id);
            } else {
                bytes = bytes.saturating_add(b.size);
                to_download.push(b.clone());
            }
        }
        Self {
            to_download,
            present,
            bytes_to_download: bytes,
        }
    }
}

/// 本地路径约定（相对 cache_dir / protected_dir）。
pub mod layout {
    use madinfer_core_types::{AssetId, PackageRef};

    /// blob 路径：`blobs/sha256/<前两位>/<其余>`。
    pub fn blob_path(id: &AssetId) -> String {
        id.blob_relative_path()
    }

    /// 下载临时落点：`<tmp>/<hex>.part`。
    pub fn part_path(id: &AssetId) -> String {
        format!("{}.part", &id.to_wire()[7..])
    }

    /// manifest 路径：`packages/<name>/<version>/manifest.json`。
    pub fn manifest_path(package: &PackageRef) -> String {
        format!(
            "packages/{}/{}/manifest.json",
            package.name(),
            package.version()
        )
    }

    /// ACTIVE 指针文件。
    pub fn active_pointer(name: &str) -> String {
        format!("packages/{name}/ACTIVE")
    }

    /// PREVIOUS 指针文件。
    pub fn previous_pointer(name: &str) -> String {
        format!("packages/{name}/PREVIOUS")
    }
}

/// 已安装包。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPackage {
    /// 包。
    pub package: PackageRef,
    /// 引用的 blob。
    pub blobs: Vec<AssetId>,
    /// 安装时刻。
    pub installed_at_ms: u64,
}

/// 包注册表：每个包名的 ACTIVE / PREVIOUS 指针（内容为版本号，落盘用临时文件 + rename 原子化）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageRegistry {
    installed: BTreeMap<PackageRef, InstalledPackage>,
    active: BTreeMap<String, String>,
    previous: BTreeMap<String, String>,
}

/// 指针效果（owner 写文件）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PointerEffect {
    /// 写 ACTIVE。
    WriteActive {
        /// 包名。
        name: String,
        /// 版本。
        version: String,
    },
    /// 写 PREVIOUS。
    WritePrevious {
        /// 包名。
        name: String,
        /// 版本。
        version: String,
    },
    /// 删 PREVIOUS。
    ClearPrevious {
        /// 包名。
        name: String,
    },
}

impl PackageRegistry {
    /// 登记已安装（blob 全部在库、manifest 已落盘）。
    pub fn install(&mut self, package: InstalledPackage) {
        self.installed.insert(package.package.clone(), package);
    }

    /// 已安装集合。
    pub fn installed(&self) -> impl Iterator<Item = &InstalledPackage> {
        self.installed.values()
    }

    /// 是否已安装。
    pub fn is_installed(&self, package: &PackageRef) -> bool {
        self.installed.contains_key(package)
    }

    /// 激活版本。
    pub fn active(&self, name: &str) -> Option<PackageRef> {
        self.active
            .get(name)
            .and_then(|v| PackageRef::parse(&format!("{name}@{v}")))
    }

    /// 上一版。
    pub fn previous(&self, name: &str) -> Option<PackageRef> {
        self.previous
            .get(name)
            .and_then(|v| PackageRef::parse(&format!("{name}@{v}")))
    }

    /// 激活：指针切换；旧 ACTIVE 进 PREVIOUS。未安装的版本拒绝。
    pub fn activate(&mut self, package: &PackageRef) -> Option<Vec<PointerEffect>> {
        if !self.installed.contains_key(package) {
            return None;
        }
        let name = package.name().to_owned();
        let mut effects = Vec::new();
        if let Some(old) = self
            .active
            .insert(name.clone(), package.version().to_owned())
        {
            if old != package.version() {
                self.previous.insert(name.clone(), old.clone());
                effects.push(PointerEffect::WritePrevious {
                    name: name.clone(),
                    version: old,
                });
            }
        }
        effects.push(PointerEffect::WriteActive {
            name,
            version: package.version().to_owned(),
        });
        Some(effects)
    }

    /// 回滚：PREVIOUS 切回 ACTIVE（PREVIOUS 必须仍已安装——不变量"任一时刻 PREVIOUS 可激活"）。
    pub fn rollback(&mut self, name: &str) -> Option<Vec<PointerEffect>> {
        let prev = self.previous.remove(name)?;
        let prev_ref = PackageRef::parse(&format!("{name}@{prev}"))?;
        if !self.installed.contains_key(&prev_ref) {
            return None;
        }
        let demoted = self.active.insert(name.to_owned(), prev.clone());
        let mut effects = vec![PointerEffect::WriteActive {
            name: name.to_owned(),
            version: prev,
        }];
        match demoted {
            Some(v) => {
                self.previous.insert(name.to_owned(), v.clone());
                effects.push(PointerEffect::WritePrevious {
                    name: name.to_owned(),
                    version: v,
                });
            }
            None => effects.push(PointerEffect::ClearPrevious {
                name: name.to_owned(),
            }),
        }
        Some(effects)
    }

    /// 被 ACTIVE / PREVIOUS 引用的全部 blob（GC 的保留集）。
    pub fn referenced_blobs(&self) -> BTreeSet<AssetId> {
        let mut set = BTreeSet::new();
        for (name, version) in self.active.iter().chain(self.previous.iter()) {
            if let Some(p) = PackageRef::parse(&format!("{name}@{version}")) {
                if let Some(installed) = self.installed.get(&p) {
                    set.extend(installed.blobs.iter().copied());
                }
            }
        }
        set
    }

    /// 卸载一个既非 ACTIVE 也非 PREVIOUS 的版本。
    pub fn remove(&mut self, package: &PackageRef) -> bool {
        let name = package.name();
        let pinned = self.active.get(name) == Some(&package.version().to_owned())
            || self.previous.get(name) == Some(&package.version().to_owned());
        if pinned {
            return false;
        }
        self.installed.remove(package).is_some()
    }
}

/// GC 策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcPolicy {
    /// 引用为零后的保留时长（缺省 7 天）。
    pub keep_ms: u64,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            keep_ms: 7 * 24 * 3_600 * 1_000,
        }
    }
}

impl GcPolicy {
    /// 计划：库中不被任何 ACTIVE / PREVIOUS 引用、且最近引用时刻早于保留期的 blob。`force` 时忽略保留期（用户触发全清）。
    pub fn plan(
        &self,
        view: &StoreView,
        referenced: &BTreeSet<AssetId>,
        now_ms: u64,
        force: bool,
    ) -> Vec<AssetId> {
        let mut out: Vec<AssetId> = view
            .blobs
            .iter()
            .filter(|id| !referenced.contains(id))
            .filter(|id| {
                force
                    || view
                        .blob_last_referenced_ms
                        .get(id)
                        .is_none_or(|last| now_ms.saturating_sub(*last) >= self.keep_ms)
            })
            .copied()
            .collect();
        out.sort_by(|a, b| a.bytes().cmp(b.bytes()));
        out
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn id(byte: &str) -> AssetId {
        AssetId::parse(&format!("sha256:{}", byte.repeat(32))).expect("id")
    }

    fn pkg(s: &str) -> PackageRef {
        PackageRef::parse(s).expect("pkg")
    }

    #[test]
    fn install_plan_dedupes_shared_blobs() {
        let blobs = vec![
            BlobRef {
                id: id("aa"),
                size: 1_000,
                kind: BlobKind::BackboneWeights,
                shared: true,
                url: Some("u".into()),
            },
            BlobRef {
                id: id("bb"),
                size: 30,
                kind: BlobKind::Adapter,
                shared: false,
                url: Some("u".into()),
            },
        ];
        let mut view = StoreView::default();
        view.blobs.insert(id("aa"));
        let plan = InstallPlan::compute(&blobs, &view);
        assert_eq!(plan.present, vec![id("aa")]);
        assert_eq!(plan.to_download.len(), 1);
        assert_eq!(plan.bytes_to_download, 30);
        assert_eq!(
            layout::blob_path(&id("aa")),
            format!("blobs/sha256/aa/{}", "aa".repeat(31))
        );
        assert_eq!(
            layout::manifest_path(&pkg("catchup-summary@1.4.2")),
            "packages/catchup-summary/1.4.2/manifest.json"
        );
    }

    #[test]
    fn activate_rollback_and_referenced_blobs() {
        let mut reg = PackageRegistry::default();
        reg.install(InstalledPackage {
            package: pkg("catchup-summary@1.4.1"),
            blobs: vec![id("aa"), id("b1")],
            installed_at_ms: 1,
        });
        reg.install(InstalledPackage {
            package: pkg("catchup-summary@1.4.2"),
            blobs: vec![id("aa"), id("b2")],
            installed_at_ms: 2,
        });
        assert!(
            reg.activate(&pkg("catchup-summary@9.9.9")).is_none(),
            "未安装不可激活"
        );
        let fx = reg.activate(&pkg("catchup-summary@1.4.1")).expect("ok");
        assert_eq!(
            fx,
            vec![PointerEffect::WriteActive {
                name: "catchup-summary".into(),
                version: "1.4.1".into()
            }]
        );
        let fx = reg.activate(&pkg("catchup-summary@1.4.2")).expect("ok");
        assert_eq!(fx.len(), 2, "写 PREVIOUS 再写 ACTIVE");
        assert_eq!(
            reg.active("catchup-summary"),
            Some(pkg("catchup-summary@1.4.2"))
        );
        assert_eq!(
            reg.previous("catchup-summary"),
            Some(pkg("catchup-summary@1.4.1"))
        );
        assert_eq!(
            reg.referenced_blobs(),
            [id("aa"), id("b1"), id("b2")].into_iter().collect()
        );
        assert!(
            !reg.remove(&pkg("catchup-summary@1.4.1")),
            "PREVIOUS 不可卸载（任一时刻可回滚）"
        );

        let fx = reg.rollback("catchup-summary").expect("ok");
        assert_eq!(
            reg.active("catchup-summary"),
            Some(pkg("catchup-summary@1.4.1"))
        );
        assert_eq!(
            reg.previous("catchup-summary"),
            Some(pkg("catchup-summary@1.4.2"))
        );
        assert!(matches!(fx[0], PointerEffect::WriteActive { .. }));
        assert!(reg.rollback("nope").is_none());
    }

    #[test]
    fn gc_keeps_referenced_and_recent() {
        let mut view = StoreView::default();
        for b in ["aa", "bb", "cc"] {
            view.blobs.insert(id(b));
        }
        let day = 24 * 3_600 * 1_000;
        view.blob_last_referenced_ms.insert(id("bb"), 10 * day);
        view.blob_last_referenced_ms.insert(id("cc"), day);
        let referenced: BTreeSet<AssetId> = [id("aa")].into_iter().collect();
        let plan = GcPolicy::default().plan(&view, &referenced, 10 * day + 1, false);
        assert_eq!(plan, vec![id("cc")], "aa 被引用、bb 刚引用过、cc 超 7 天");
        let all = GcPolicy::default().plan(&view, &referenced, 10 * day + 1, true);
        assert_eq!(
            all,
            vec![id("bb"), id("cc")],
            "全清忽略保留期但仍保留被引用的"
        );
    }
}
