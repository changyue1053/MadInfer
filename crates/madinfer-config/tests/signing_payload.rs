//! 签名载荷金样（madai-w10 I-C-09 J2；I-H-05 反馈 ③ / I-S-05 U-7；madai-w11 I-C-10 改读仓根 `fixtures/signing_payload.golden.json`）：
//! 固定 config.v1 文档 → `ConfigDoc::signing_payload` 的固定字节串，hex 与 sha256 钉死。三端壳（I-D JDK 签名 fixture /
//! I-S `ConfigV1Fixture` / I-H cryptoFramework fixture）与云端签发侧读**同一份文件**断言即可，不必各自手写规范形、不必起 Rust
//! 复核（此前三端各手写一份，serde 一变三端同红——I-C-09 §7）；规范形口径见 `src/doc.rs` 头注，三端消费法见 README。
//! 任何改变载荷字节的变更（serde 行为、字段增删、换 JCS）本测即红——那是同批替换 fixture 并在报告 §7 通知三端的信号，
//! 不是本测该被放宽的信号。

#![allow(clippy::unwrap_used, clippy::expect_used)] // 测试放开（根 Cargo.toml lints 注）

use madinfer_config::ConfigDoc;
use serde::Deserialize;

/// 仓根金样文件（编译期内嵌：测试只认事实源，路径相对本 crate 的 Cargo.toml）。
const GOLDEN_FILE: &str = include_str!("../../../fixtures/signing_payload.golden.json");

/// 金样文件形态（README "签名载荷金样" 节同文）。
#[derive(Deserialize)]
struct GoldenFile {
    schema: String,
    documents: Vec<GoldenDocument>,
}

#[derive(Deserialize)]
struct GoldenDocument {
    name: String,
    /// 文档原文（故意非规范形）。
    document: String,
    /// `signing_payload` 输出字节的小写 hex。
    canonical_hex: String,
    /// 上述字节串的 sha256 小写 hex。
    sha256: String,
}

fn golden() -> GoldenFile {
    let file: GoldenFile = serde_json::from_str(GOLDEN_FILE).expect("金样文件合法 JSON");
    assert_eq!(file.schema, "madinfer signing_payload golden v1");
    assert!(file.documents.len() >= 3, "金样至少三份文档");
    file
}

#[test]
fn signing_payload_golden_file_is_byte_stable() {
    for sample in golden().documents {
        let doc: ConfigDoc = serde_json::from_str(&sample.document)
            .unwrap_or_else(|e| panic!("{}：文档合法 {e}", sample.name));
        let payload = doc.signing_payload();
        assert_eq!(
            hex(&payload),
            sample.canonical_hex,
            "{}：签名载荷规范形变了——三端 fixture 须同批替换\n实际载荷：{}",
            sample.name,
            String::from_utf8_lossy(&payload)
        );
        assert_eq!(
            sha256_hex(&payload),
            sample.sha256,
            "{}：载荷 sha256",
            sample.name
        );
        // 载荷不含签名；文档里的 signature 三字段不影响载荷（换 sig / key_id 载荷不变）。
        let mut resigned = doc.clone();
        resigned.signature.sig = "b3RoZXI=".to_owned();
        resigned.signature.key_id = "k-other".to_owned();
        assert_eq!(resigned.signing_payload(), payload, "{}", sample.name);
        // 规范形是幂等的：把载荷再补上 signature 反序列化，二次载荷逐字节相同。
        let mut value: serde_json::Value = serde_json::from_slice(&payload).expect("载荷是 JSON");
        value["signature"] = serde_json::json!({ "alg": "ed25519", "key_id": "k1", "sig": "" });
        let again: ConfigDoc =
            serde_json::from_value(value).expect("载荷 + signature 仍是合法文档");
        assert_eq!(
            again.signing_payload(),
            payload,
            "{}：规范形幂等",
            sample.name
        );
    }
}

#[test]
fn signing_payload_golden_keeps_madai_w10_sample() {
    // 连续性：madai-w10 I-C-09 金样（813 字节、sha256 795de2eb…7814d9）原样是本文件第一份文档——三端已按它写的 fixture 不必动。
    let file = golden();
    let first = &file.documents[0];
    assert_eq!(first.name, "non_canonical_shuffled");
    assert_eq!(first.canonical_hex.len(), 813 * 2, "813 字节");
    assert_eq!(
        first.sha256,
        "795de2ebc7186b26f70c18846ffd4d34005f3ad55d6e26b2450e9ec4057814d9"
    );
    let payload = hex_decode(&first.canonical_hex);
    assert!(std::str::from_utf8(&payload)
        .expect("UTF-8")
        .starts_with(r#"{"issued_at":"#));
}

#[test]
fn signing_payload_hash_helper_matches_known_vector() {
    // FIPS 180-4 已知向量：sha256("abc")。
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn hex_decode(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// SHA-256（FIPS 180-4；测试内自含实现——工作区第三方只有 serde / serde_json / semver，deny.toml 白名单不扩）
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

#[rustfmt::skip]
const H0: [u32; 8] = [
    0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19,
];

fn sha256_hex(message: &[u8]) -> String {
    let mut h = H0;
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    let bit_len = u64::try_from(message.len()).unwrap() * 8;
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    hex(&h.iter().flat_map(|w| w.to_be_bytes()).collect::<Vec<u8>>())
}
