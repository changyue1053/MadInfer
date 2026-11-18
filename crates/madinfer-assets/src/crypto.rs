//! 加密 blob 的解密契约：[`EncSpec`] 结构（manifest.v1 `blobs[].enc`）与解密端口 [`BlobDecryptor`]。
//!
//! 中间态（madai-w05 I-C-04）：真实 AES-256-GCM（专名口径）实装归 I4——依赖 crate 走 deny.toml 白名单、
//! 密钥经宿主 `SecureStore` 取、密钥内存 Zeroizing；本批只落契约与复核入口，测试用替身在 tests 模块。
//! 生产装配期断言拒绝任何非真实实现（同 `TrustAllVerifier` 口径，madinfer-ffi 装配时执行）。
//!
//! 口径（设计文档 C10 关键机制④）：加密 blob 以**密文** hash 为内容寻址 id；解密后必须比对
//! `plain_sha256`（[`decrypt_and_verify`]）；解密产物只写受保护目录，由 owner 按效果执行。

use core::fmt;

use madinfer_core_types::AssetId;

use crate::verify::verify_blob;

/// 加密算法（闭合枚举；manifest.v1 `enc.alg`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncAlg {
    /// AES-256-GCM（带认证的对称加密；算法专名）。
    Aes256Gcm,
}

impl EncAlg {
    /// 协议字符串。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Aes256Gcm => "aes-256-gcm",
        }
    }

    /// 从协议字符串解析。
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "aes-256-gcm" => Some(Self::Aes256Gcm),
            _ => None,
        }
    }
}

/// GCM 标准 nonce 长度（96 位）。
pub const GCM_NONCE_LEN: usize = 12;

/// 一个加密 blob 的解密说明（manifest.v1 `blobs[].enc`）。密钥本体不进本结构——只有 `key_id`，
/// 密钥由解密端口实现经宿主 `SecureStore` 取用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncSpec {
    /// 算法。
    pub alg: EncAlg,
    /// 内容密钥 id（宿主在登录后从租户密钥服务拉取并写入 Keychain / Keystore / HUKS）。
    pub key_id: String,
    /// 每 blob 独立 nonce（manifest 携带原始字节；GCM 口径 96 位）。
    pub nonce: Vec<u8>,
    /// 明文 hash（解密后复核；blob 自身的内容寻址 id 是密文 hash）。
    pub plain_sha256: AssetId,
}

/// 解密错误（闭合枚举；无自由文本）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecryptError {
    /// `key_id` 在 SecureStore 无对应密钥。
    KeyMissing,
    /// nonce 长度非法。
    NonceInvalid,
    /// 认证失败（tag 不符 / 密文被改）。
    AuthFailed,
    /// 算法不支持。
    Unsupported,
    /// 解密成功但明文 hash 与 `plain_sha256` 不符（仅 [`decrypt_and_verify`] 产出）。
    PlainMismatch,
}

impl DecryptError {
    /// 观测用原因码（注册制字符串）。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::KeyMissing => "key_missing",
            Self::NonceInvalid => "nonce_invalid",
            Self::AuthFailed => "auth_failed",
            Self::Unsupported => "unsupported",
            Self::PlainMismatch => "plain_mismatch",
        }
    }
}

impl fmt::Display for DecryptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "decrypt error {}", self.wire())
    }
}

impl std::error::Error for DecryptError {}

/// 解密端口：核只经 trait 调用；实现持有密钥获取途径（SecureStore）。
/// [`BlobDecryptor::decrypt`] 不做明文 hash 复核——复核统一走 [`decrypt_and_verify`]。
pub trait BlobDecryptor: Send + Sync {
    /// 解密一个 blob（密文 → 明文）。
    fn decrypt(&self, spec: &EncSpec, ciphertext: &[u8]) -> Result<Vec<u8>, DecryptError>;
}

/// 解密并复核明文 hash（C10 关键机制④）。任何失败都不产出明文。
pub fn decrypt_and_verify(
    decryptor: &dyn BlobDecryptor,
    spec: &EncSpec,
    ciphertext: &[u8],
) -> Result<Vec<u8>, DecryptError> {
    let plain = decryptor.decrypt(spec, ciphertext)?;
    match verify_blob(&plain, &spec.plain_sha256) {
        Ok(()) => Ok(plain),
        Err(_) => Err(DecryptError::PlainMismatch),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::verify::sha256_hex;
    use std::collections::BTreeMap;

    /// 测试替身：XOR 流 + 末字节校验和当 tag（只为锁契约形态，与真实 AES-256-GCM 无关）。
    struct FakeDecryptor {
        keys: BTreeMap<String, u8>,
    }

    impl FakeDecryptor {
        fn with_key(key_id: &str, key: u8) -> Self {
            let mut keys = BTreeMap::new();
            keys.insert(key_id.to_owned(), key);
            Self { keys }
        }

        /// 加密方向（测试内部造密文）：XOR 后追加校验和字节。
        fn seal(key: u8, nonce: &[u8], plain: &[u8]) -> Vec<u8> {
            let mut out: Vec<u8> = plain
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ key ^ nonce[i % nonce.len()])
                .collect();
            let tag = plain.iter().fold(key, |acc, b| acc.wrapping_add(*b));
            out.push(tag);
            out
        }
    }

    impl BlobDecryptor for FakeDecryptor {
        fn decrypt(&self, spec: &EncSpec, ciphertext: &[u8]) -> Result<Vec<u8>, DecryptError> {
            if spec.alg != EncAlg::Aes256Gcm {
                return Err(DecryptError::Unsupported);
            }
            if spec.nonce.len() != GCM_NONCE_LEN {
                return Err(DecryptError::NonceInvalid);
            }
            let Some(&key) = self.keys.get(&spec.key_id) else {
                return Err(DecryptError::KeyMissing);
            };
            let Some((tag, body)) = ciphertext.split_last() else {
                return Err(DecryptError::AuthFailed);
            };
            let plain: Vec<u8> = body
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ key ^ spec.nonce[i % spec.nonce.len()])
                .collect();
            let expected_tag = plain.iter().fold(key, |acc, b| acc.wrapping_add(*b));
            if *tag != expected_tag {
                return Err(DecryptError::AuthFailed);
            }
            Ok(plain)
        }
    }

    fn spec_for(plain: &[u8]) -> EncSpec {
        EncSpec {
            alg: EncAlg::Aes256Gcm,
            key_id: "tenant-k-2026-08".to_owned(),
            nonce: vec![7u8; GCM_NONCE_LEN],
            plain_sha256: AssetId::parse(&format!("sha256:{}", sha256_hex(plain))).expect("id"),
        }
    }

    #[test]
    fn roundtrip_decrypts_and_verifies_plain_hash() {
        let plain = b"adapter bytes";
        let spec = spec_for(plain);
        let decryptor = FakeDecryptor::with_key("tenant-k-2026-08", 0x5a);
        let sealed = FakeDecryptor::seal(0x5a, &spec.nonce, plain);
        let out = decrypt_and_verify(&decryptor, &spec, &sealed).expect("decrypt");
        assert_eq!(out, plain);
    }

    #[test]
    fn tampered_ciphertext_fails_auth_and_yields_no_plaintext() {
        let plain = b"adapter bytes";
        let spec = spec_for(plain);
        let decryptor = FakeDecryptor::with_key("tenant-k-2026-08", 0x5a);
        let mut sealed = FakeDecryptor::seal(0x5a, &spec.nonce, plain);
        sealed[0] ^= 0xff;
        assert_eq!(
            decrypt_and_verify(&decryptor, &spec, &sealed),
            Err(DecryptError::AuthFailed)
        );
    }

    #[test]
    fn plain_hash_mismatch_is_its_own_error() {
        let plain = b"adapter bytes";
        let mut spec = spec_for(plain);
        spec.plain_sha256 = AssetId::parse(&format!("sha256:{}", "0".repeat(64))).expect("id");
        let decryptor = FakeDecryptor::with_key("tenant-k-2026-08", 0x5a);
        let sealed = FakeDecryptor::seal(0x5a, &spec.nonce, plain);
        assert_eq!(
            decrypt_and_verify(&decryptor, &spec, &sealed),
            Err(DecryptError::PlainMismatch)
        );
    }

    #[test]
    fn key_and_nonce_faults_map_to_closed_errors() {
        let plain = b"x";
        let spec = spec_for(plain);
        let stranger = FakeDecryptor::with_key("other-key", 1);
        assert_eq!(
            stranger.decrypt(&spec, &[0, 0]),
            Err(DecryptError::KeyMissing)
        );
        let mut bad_nonce = spec.clone();
        bad_nonce.nonce = vec![1, 2, 3];
        let decryptor = FakeDecryptor::with_key("tenant-k-2026-08", 1);
        assert_eq!(
            decryptor.decrypt(&bad_nonce, &[0, 0]),
            Err(DecryptError::NonceInvalid)
        );
        assert_eq!(EncAlg::from_wire("aes-256-gcm"), Some(EncAlg::Aes256Gcm));
        assert_eq!(EncAlg::from_wire("rot13"), None);
        assert_eq!(EncAlg::Aes256Gcm.wire(), "aes-256-gcm");
        assert_eq!(DecryptError::PlainMismatch.wire(), "plain_mismatch");
    }
}
