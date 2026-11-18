//! 追踪上下文（W3C Trace Context 的 hex 形式；request.v1 `trace`、cloud.v1 `X-Infer-Trace`）。

/// 追踪上下文。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TraceCtx {
    /// 16 字节 trace id。
    pub trace_id: [u8; 16],
    /// 8 字节 span id。
    pub span_id: [u8; 8],
    /// 是否采样。
    pub sampled: bool,
}

impl TraceCtx {
    /// 从 32 位 / 16 位 hex 解析。
    pub fn parse(trace_hex: &str, span_hex: &str, sampled: bool) -> Option<Self> {
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];
        decode_hex(trace_hex, &mut trace_id)?;
        decode_hex(span_hex, &mut span_id)?;
        Some(Self {
            trace_id,
            span_id,
            sampled,
        })
    }

    /// `traceparent` 头（版本 00）。
    pub fn traceparent(&self) -> String {
        let mut s = String::with_capacity(55);
        s.push_str("00-");
        push_hex(&mut s, &self.trace_id);
        s.push('-');
        push_hex(&mut s, &self.span_id);
        s.push_str(if self.sampled { "-01" } else { "-00" });
        s
    }

    /// trace id 的 hex。
    pub fn trace_id_hex(&self) -> String {
        let mut s = String::with_capacity(32);
        push_hex(&mut s, &self.trace_id);
        s
    }
}

fn decode_hex(hex: &str, out: &mut [u8]) -> Option<()> {
    if hex.len() != out.len() * 2 {
        return None;
    }
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(())
}

fn push_hex(s: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
}
