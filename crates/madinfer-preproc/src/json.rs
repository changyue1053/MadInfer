//! 极小 JSON 解析器（纯 std；仅供 [`crate::SchemaCompiler`] 读 schema 文本，不做通用 JSON 库）。
//!
//! §9 解析器纪律：递归深度上限（防栈溢出）、一切非法输入返回带偏移的错误、**禁 panic**。
//! 数字保留词素原文（不转浮点）——enum / const 的字面量要逐字节回写进语法；
//! 对象成员保持声明顺序（属性顺序决定语法里的成员顺序），键重复时取末个（查找语义）。

/// 解析深度上限（数组 / 对象嵌套层数）。
pub(crate) const JSON_MAX_DEPTH: usize = 128;

/// JSON 值（schema 文本的内存形态）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JsonValue {
    /// null。
    Null,
    /// true / false。
    Bool(bool),
    /// 数字：保留词素（如 `-1.5e3`）。
    Number(String),
    /// 字符串（已解转义）。
    String(String),
    /// 数组。
    Array(Vec<JsonValue>),
    /// 对象：成员按声明顺序。
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// 对象成员查找（键重复取末个）；非对象返回 None。
    pub(crate) fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            Self::Object(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// 解析错误：`offset` 是出错处的字节偏移。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JsonError {
    pub(crate) offset: usize,
}

/// 解析入口：整段文本必须恰是一个 JSON 值（尾随非空白判错）。
pub(crate) fn parse_json(text: &str) -> Result<JsonValue, JsonError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.value(JSON_MAX_DEPTH)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.err());
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn err(&self) -> JsonError {
        JsonError { offset: self.pos }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> Result<(), JsonError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err())
        }
    }

    fn value(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        if depth == 0 {
            return Err(self.err());
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(JsonValue::String(self.string()?)),
            Some(b't') => self.literal("true", JsonValue::Bool(true)),
            Some(b'f') => self.literal("false", JsonValue::Bool(false)),
            Some(b'n') => self.literal("null", JsonValue::Null),
            Some(b'-' | b'0'..=b'9') => Ok(JsonValue::Number(self.number()?)),
            _ => Err(self.err()),
        }
    }

    fn literal(&mut self, word: &str, value: JsonValue) -> Result<JsonValue, JsonError> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.err())
        }
    }

    fn object(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.eat(b'{')?;
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.eat(b':')?;
            self.skip_ws();
            let value = self.value(depth - 1)?;
            members.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(members));
                }
                _ => return Err(self.err()),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth - 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(items));
                }
                _ => return Err(self.err()),
            }
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.eat(b'"')?;
        let mut out = String::new();
        let mut run_start = self.pos;
        loop {
            match self.peek() {
                None => return Err(self.err()),
                Some(b'"') => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    self.escape(&mut out)?;
                    run_start = self.pos;
                }
                Some(byte) if byte < 0x20 => return Err(self.err()),
                Some(_) => self.pos += 1,
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Result<(), JsonError> {
        let Some(byte) = self.peek() else {
            return Err(self.err());
        };
        self.pos += 1;
        match byte {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000C}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let unit = self.hex4()?;
                let code = if (0xD800..=0xDBFF).contains(&unit) {
                    // 代理对：必须紧跟 \u 低代理。
                    if self.peek() != Some(b'\\') {
                        return Err(self.err());
                    }
                    self.pos += 1;
                    if self.peek() != Some(b'u') {
                        return Err(self.err());
                    }
                    self.pos += 1;
                    let low = self.hex4()?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(self.err());
                    }
                    0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                } else {
                    unit
                };
                match char::from_u32(code) {
                    Some(c) => out.push(c),
                    None => return Err(self.err()),
                }
            }
            _ => return Err(self.err()),
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut value: u32 = 0;
        for _ in 0..4 {
            let Some(byte) = self.peek() else {
                return Err(self.err());
            };
            let digit = match byte {
                b'0'..=b'9' => u32::from(byte - b'0'),
                b'a'..=b'f' => u32::from(byte - b'a') + 10,
                b'A'..=b'F' => u32::from(byte - b'A') + 10,
                _ => return Err(self.err()),
            };
            value = (value << 4) | digit;
            self.pos += 1;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<String, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err(self.err()),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            self.digits()?;
        }
        Ok(self.text[start..self.pos].to_owned())
    }

    fn digits(&mut self) -> Result<(), JsonError> {
        let mut seen = false;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
            seen = true;
        }
        if seen {
            Ok(())
        } else {
            Err(self.err())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_values_and_keeps_member_order() {
        let value =
            parse_json(r#"{ "b": [1, -2.5e3, "x"], "a": { "k": true, "n": null }, "b": false }"#)
                .expect("parse");
        let JsonValue::Object(members) = &value else {
            panic!("object");
        };
        assert_eq!(
            members.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["b", "a", "b"],
            "声明顺序保留"
        );
        assert_eq!(
            value.get("b"),
            Some(&JsonValue::Bool(false)),
            "重复键取末个"
        );
        let JsonValue::Array(items) = &members[0].1 else {
            panic!("array");
        };
        assert_eq!(items[1], JsonValue::Number("-2.5e3".to_owned()), "词素原文");
    }

    #[test]
    fn string_escapes_include_surrogate_pairs() {
        let value = parse_json(r#""a\"b\\c\n\u4e2d\uD83D\uDE00""#).expect("parse");
        assert_eq!(value, JsonValue::String("a\"b\\c\n中😀".to_owned()));
    }

    #[test]
    fn malformed_inputs_return_offsets_not_panics() {
        for (text, offset_at_least) in [
            ("", 0),
            ("{", 1),
            ("01", 1),
            ("[1,]", 3),
            ("\"unterminated", 13),
            ("\"ctrl\u{0001}\"", 5),
            ("true false", 5),
            ("{\"a\" 1}", 5),
            ("\"\\uD800x\"", 6),
        ] {
            let error = parse_json(text).expect_err(text);
            assert!(error.offset >= offset_at_least, "{text}: {error:?}");
        }
    }

    #[test]
    fn depth_cap_rejects_pathological_nesting() {
        let deep = "[".repeat(JSON_MAX_DEPTH + 1) + &"]".repeat(JSON_MAX_DEPTH + 1);
        assert!(parse_json(&deep).is_err(), "越深即错，不 panic");
        let ok = "[".repeat(8) + "1" + &"]".repeat(8);
        assert!(parse_json(&ok).is_ok());
    }
}
