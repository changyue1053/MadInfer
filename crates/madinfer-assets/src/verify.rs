//! sha256 纯 Rust 手写实现（FIPS 180-4）+ blob 内容寻址校验入口。
//!
//! 依赖纪律：不引第三方 crate。实现以官方测试向量锁行为（FIPS 180-4 / NIST SHA-256 examples：
//! 空串、"abc"、两块消息、一百万字节 'a'），流式喂入与一次性结果等价另行单测。
//! [`Sha256`] 流式接口供下载器按块喂入；[`verify_blob`] 是入库前"先比 sha256 再入库"的校验入口
//! （C10 关键机制①）。hash 是内容寻址标识，不是用户内容，可进观测与错误详情。

use core::fmt;

use madinfer_core_types::AssetId;

/// FIPS 180-4 §4.2.2 常量表（前 64 个素数立方根小数部分）。
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// 初始 hash 值（FIPS 180-4 §5.3.3）。
const H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// 流式 SHA-256（FIPS 180-4）。
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    total_bytes: u64,
}

impl Sha256 {
    /// 初始状态。
    pub const fn new() -> Self {
        Self {
            state: H0,
            buffer: [0; 64],
            buffered: 0,
            total_bytes: 0,
        }
    }

    /// 喂入一段字节（可任意切块，与一次性结果等价）。
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.total_bytes = self
            .total_bytes
            .wrapping_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.buffered > 0 {
            let take = bytes.len().min(64 - self.buffered);
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&bytes[..take]);
            self.buffered += take;
            bytes = &bytes[take..];
            if self.buffered < 64 {
                return; // 未满一块：等下次喂入。
            }
            let block = self.buffer;
            compress(&mut self.state, &block);
            self.buffered = 0;
        }
        let mut chunks = bytes.chunks_exact(64);
        for block in &mut chunks {
            let mut owned = [0u8; 64];
            owned.copy_from_slice(block);
            compress(&mut self.state, &owned);
        }
        let rest = chunks.remainder();
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// 收尾并输出摘要（消费 self）。
    pub fn finalize(mut self) -> [u8; 32] {
        let bit_length = self.total_bytes.wrapping_mul(8);
        // 填充（§5.1.1）：0x80、补零到块尾留 8 字节、64 位大端比特长度。
        let pad_zeros = if self.buffered < 56 {
            55 - self.buffered
        } else {
            119 - self.buffered
        };
        let mut tail = Vec::with_capacity(pad_zeros + 9);
        tail.push(0x80u8);
        tail.resize(1 + pad_zeros, 0);
        tail.extend_from_slice(&bit_length.to_be_bytes());
        self.update(&tail);
        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Sha256(buffered={}, total={})",
            self.buffered, self.total_bytes
        )
    }
}

/// 压缩函数（FIPS 180-4 §6.2.2）。
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, chunk) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(big_s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = big_s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// 一次性摘要。
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// 一次性摘要的小写十六进制。
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha256(bytes))
}

fn hex(digest: &[u8; 32]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(TABLE[usize::from(byte >> 4)] as char);
        out.push(TABLE[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// blob 内容与其内容寻址 id 不符。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobMismatch {
    /// 期望（协议形态 `sha256:<hex>`）。
    pub expected: String,
    /// 实际（协议形态 `sha256:<hex>`）。
    pub actual: String,
}

impl fmt::Display for BlobMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blob hash mismatch: expected {}, actual {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for BlobMismatch {}

/// blob 内容寻址校验入口（C10 关键机制①：blob 到齐先比 sha256 再入库）。
pub fn verify_blob(bytes: &[u8], expected: &AssetId) -> Result<(), BlobMismatch> {
    let digest = sha256(bytes);
    if digest == *expected.bytes() {
        Ok(())
    } else {
        Err(BlobMismatch {
            expected: expected.to_wire(),
            actual: format!("sha256:{}", hex(&digest)),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// FIPS 180-4 / NIST SHA-256 官方向量。
    #[test]
    fn fips_180_4_official_vectors() {
        let cases: [(&[u8], &str); 4] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(sha256_hex(input), expected);
        }
    }

    /// FIPS 180-4 附录向量：一百万个 'a'。
    #[test]
    fn one_million_a_vector() {
        let input = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&input),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn streaming_in_odd_chunks_equals_one_shot() {
        let data: Vec<u8> = (0u32..1_027)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let one_shot = sha256(&data);
        for chunk in [1usize, 7, 13, 63, 64, 65, 200] {
            let mut hasher = Sha256::new();
            for part in data.chunks(chunk) {
                hasher.update(part);
            }
            assert_eq!(hasher.finalize(), one_shot, "块大小 {chunk}");
        }
    }

    #[test]
    fn verify_blob_accepts_matching_and_names_both_hashes_on_mismatch() {
        let bytes = b"backbone weights";
        let id = AssetId::parse(&format!("sha256:{}", sha256_hex(bytes))).expect("id");
        assert_eq!(verify_blob(bytes, &id), Ok(()));

        let wrong = AssetId::parse(&format!("sha256:{}", "0".repeat(64))).expect("id");
        let mismatch = verify_blob(bytes, &wrong).expect_err("mismatch");
        assert_eq!(mismatch.expected, wrong.to_wire());
        assert_eq!(mismatch.actual, format!("sha256:{}", sha256_hex(bytes)));
        assert!(mismatch.to_string().contains("mismatch"));
    }
}
