//! 标识与枚举。全部 `Copy` 或廉价 `Clone`；`wire()` 返回协议里的 snake_case 字符串（schemas/*.v1.json 的 enum 值）。

use core::fmt;

/// 能力标识：注册制词表（"summarize"、"embed.text"、"classify.priority"）。
///
/// 长度 1～64，只含 `[a-z0-9._-]`；注册表校验在 madinfer-api，这里只做形式校验。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// 形式校验后构造；非法返回 `None`。
    pub fn new(value: &str) -> Option<Self> {
        let ok = !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            });
        ok.then(|| Self(value.to_owned()))
    }

    /// 协议字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CapabilityId({})", self.0)
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 业务标识：注册制（registry.toml），1～65535；调度配额与观测归因用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BusinessId(u16);

impl BusinessId {
    /// 0 保留为"未注册"，返回 `None`。
    pub const fn new(value: u16) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    /// 原始值。
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// 后端标识：注册制（"llama.cpp"、"ios.foundation_models"、"android.aicore"、"ohos.system_ai"、"cloud.enterprise"）。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BackendId(pub String);

/// 资产标识：blob 内容的 sha256（协议形式 `sha256:<64 hex>`）。`Ord` 为字节序（供集合键与确定性排序）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetId([u8; 32]);

impl AssetId {
    /// 解析 `sha256:<64 hex>`；大小写不敏感。
    pub fn parse(value: &str) -> Option<Self> {
        let hex = value.strip_prefix("sha256:")?;
        if hex.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16)?;
            let lo = (chunk[1] as char).to_digit(16)?;
            out[i] = ((hi << 4) | lo) as u8;
        }
        Some(Self(out))
    }

    /// 原始 32 字节。
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 协议形式 `sha256:<64 hex>`（小写）。
    pub fn to_wire(&self) -> String {
        let mut s = String::with_capacity(7 + 64);
        s.push_str("sha256:");
        for b in self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        s
    }

    /// 本地内容寻址库的相对路径 `blobs/sha256/<前两位>/<其余 62 位>`。
    pub fn blob_relative_path(&self) -> String {
        let wire = self.to_wire();
        let hex = &wire[7..];
        format!("blobs/sha256/{}/{}", &hex[..2], &hex[2..])
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl fmt::Debug for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetId({})", self.to_wire())
    }
}

/// 算法包引用 `name@version`；version 是 semver 字符串，比较与解析推迟到 madinfer-assets（那里引 semver crate）。
/// `Ord` 只是字典序（供集合键用），不是版本序。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PackageRef {
    name: String,
    version: String,
}

impl PackageRef {
    /// 解析 `name@version`；name 非空且不含 `@`，version 非空。
    pub fn parse(value: &str) -> Option<Self> {
        let (name, version) = value.split_once('@')?;
        if name.is_empty() || version.is_empty() || name.contains('@') {
            return None;
        }
        Some(Self {
            name: name.to_owned(),
            version: version.to_owned(),
        })
    }

    /// 包名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 版本字符串。
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 协议形式。
    pub fn to_wire(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// 请求标识：核分配的进程内单调 id，事件与取消都以它为锚。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// 上下文键：业务给出的稳定字符串（UTF-8 字节 1～128，按字节不按字符）；核内 hash 后作为生成上下文（KV、适配器）复用的键。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ContextKey(String);

impl ContextKey {
    /// 形式校验后构造：`value` 的 **UTF-8 字节数**须在 1～128（不是字符数），否则 `None`（ffi 入口回 4001）。
    ///
    /// 口径按字节不按字符（request.v1 `context` 描述同文；madai-w09 I-C-08 / I-D-08 与 KMP `ContextKey` 校验同口径）：
    /// 一个 CJK 字符占 3 字节，42 个 CJK 字符（126 字节）合法、43 个（129 字节）超限；业务拼键时以 `str::len()`（字节）预判，
    /// 128 字节的 ASCII 键与 42 个 CJK 字符是同一条上限。
    pub fn new(value: &str) -> Option<Self> {
        (!value.is_empty() && value.len() <= 128).then(|| Self(value.to_owned()))
    }

    /// 原始字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 隐私标签（公理 A4）。判别值进入观测与云端协议，不得改动；路由用 `Ord` 比较而不是匹配。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum PrivacyTag {
    /// 可出端、可进日志。
    Public = 0,
    /// 可出端到企业自有云，不进第三方。
    Internal = 1,
    /// E2EE 明文：只准端侧执行，路由器无权改判。
    E2eePlaintext = 2,
    /// 受监管数据：只准端侧执行。
    Regulated = 3,
}

impl PrivacyTag {
    /// 是否只准端侧执行。
    pub const fn local_only(self) -> bool {
        matches!(self, Self::E2eePlaintext | Self::Regulated)
    }

    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::E2eePlaintext => "e2ee_plaintext",
            Self::Regulated => "regulated",
        }
    }

    /// 从协议字符串解析。
    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "public" => Self::Public,
            "internal" => Self::Internal,
            "e2ee_plaintext" => Self::E2eePlaintext,
            "regulated" => Self::Regulated,
            _ => return None,
        })
    }
}

/// 实现来源：这次推理真正发生在哪。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Source {
    /// 系统模型（Foundation Models / AICore / 鸿蒙系统 AI）。
    SystemModel = 0,
    /// 内联引擎 + 算法包。
    Bundled = 1,
    /// 云端适配器。
    Cloud = 2,
}

impl Source {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::SystemModel => "system_model",
            Self::Bundled => "bundled",
            Self::Cloud => "cloud",
        }
    }
}

/// 机型档位；`Ord` 用于 `tier_min` 比较（Low < Mid < Flagship）；Unknown 单独判定，不参与比较语义。缺省 Unknown。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum Tier {
    /// 不满足任何内联模型；只走系统模型或云。
    Low = 0,
    /// 小模型 / 量化包。
    Mid = 1,
    /// 全部能力。
    Flagship = 2,
    /// 设备画像未完成。
    #[default]
    Unknown = 255,
}

impl Tier {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Mid => "mid",
            Self::Flagship => "flagship",
            Self::Unknown => "unknown",
        }
    }

    /// 本档**缺省**允许的实现来源（纯函数；madai-w08 G11 自 madinfer-device 上提为唯一事实源）：`Unknown` / `Low`
    /// 只许系统模型与云端，`Mid` / `Flagship` 三源全开。口径：路由与能力索引在候选**未写** `tier_min` 时按本表判；
    /// 候选显式写了 `tier_min` 即配置作者对该包的明示（例如 `embed.text` 的 `tier_min = low`），优先于本表。
    pub const fn allowed_sources(self) -> &'static [Source] {
        match self {
            Self::Low | Self::Unknown => SYSTEM_MODEL_OR_CLOUD,
            Self::Mid | Self::Flagship => ALL_SOURCES,
        }
    }

    /// 缺省口径下本档是否允许内联引擎 + 算法包（`Source::Bundled`）。
    pub const fn allows_bundled_by_default(self) -> bool {
        matches!(self, Self::Mid | Self::Flagship)
    }
}

/// Unknown / Low 档缺省允许的来源：系统模型与云端。
const SYSTEM_MODEL_OR_CLOUD: &[Source] = &[Source::SystemModel, Source::Cloud];
/// Mid / Flagship 档缺省允许的全部来源。
const ALL_SOURCES: &[Source] = &[Source::SystemModel, Source::Bundled, Source::Cloud];

/// 操作系统。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Os {
    /// iOS / iPadOS。
    Ios = 0,
    /// Android。
    Android = 1,
    /// HarmonyOS（OpenHarmony 系）。
    Ohos = 2,
    /// 桌面 JVM（预览与测试）。
    Jvm = 3,
}

impl Os {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
            Self::Ohos => "ohos",
            Self::Jvm => "jvm",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContextKey;

    /// madai-w09 I-C-08：ContextKey 上限是 UTF-8 字节 128，不是字符 128——CJK 三字节字符在 42 / 43 个之间翻转。
    #[test]
    fn context_key_limit_counts_utf8_bytes_not_chars() {
        assert!(ContextKey::new("").is_none(), "空串非法");
        assert!(
            ContextKey::new(&"x".repeat(128)).is_some(),
            "128 ASCII 字节合法"
        );
        assert!(ContextKey::new(&"x".repeat(129)).is_none(), "129 字节超限");
        let cjk_42 = "会".repeat(42);
        assert_eq!(cjk_42.len(), 126, "42 个 CJK 字符 = 126 字节");
        assert!(ContextKey::new(&cjk_42).is_some(), "126 字节合法");
        let cjk_43 = "会".repeat(43);
        assert_eq!(cjk_43.len(), 129, "43 个 CJK 字符 = 129 字节");
        assert_eq!(
            cjk_43.chars().count(),
            43,
            "字符数远小于 128 仍被拒——口径是字节"
        );
        assert!(
            ContextKey::new(&cjk_43).is_none(),
            "129 字节超限（4001 的来源）"
        );
    }
}
