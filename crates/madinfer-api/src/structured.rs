//! 结构化输出（madai-w08 I-C-07 G4；event.v1 E6 承诺 / I-E-04 §7-1）：请求侧的 [`OutputSpec`]，以及把逐帧文本变成
//! RFC 6902 JSON Patch 的 [`StructuredTracker`]。
//!
//! 管线：`Structured { schema_json }` 在请求受理时经 preproc `SchemaCompiler` 编成 GBNF 交后端（编不过 → 2003，Started 前拒）；
//! 后端产出的文本逐帧进 tracker——tracker 只看**已完整的前缀**（把未收尾的容器补闭、丢掉尚未写完的键 / 标量 / 字符串），
//! 与上一帧已提交的文档做结构 diff，产出这一帧的 op 列表；终态前对全文做 schema 校验（preproc `validate_instance`），
//! 不过即 2003 而不发 Complete。
//!
//! patch 子集（与 KMP `CatchupPatchDocument` 一致）：op 只 `add` / `replace` / `remove`；路径中间段只对象键；数组只准尾追加
//! （`/…/-`），数组前缀不一致时整段 `replace`；`replace` / `remove` 的目标必已存在（diff 保证）；根必须是对象（`path = ""`
//! 不在子集——根不是对象时不产 patch，Complete 仍带完整 JSON）。逐帧 op 按 seq 顺序应用于空对象 `{}`，结果恒等于
//! 该帧时刻的已完整前缀文档；应用到最后一帧即 Complete 的 JSON。
//!
//! schema 与生成文本都是**载荷**：本模块不落日志；错误只带注册制原因词。

use madinfer_preproc::{SchemaCompiler, SchemaError, ValidationError};
use serde_json::{Map, Value};

/// 请求的输出形态（request.v1 `output`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OutputSpec {
    /// 自由文本。
    #[default]
    Text,
    /// 结构化：按 JSON Schema（draft 2020-12 子集）约束并校验。schema 文本是载荷（核不解读语义、不回传）。
    Structured {
        /// schema JSON 文本。
        schema_json: String,
    },
}

/// 终态校验失败原因（映射 `SchemaInvalid 2003`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuredError {
    /// 生成文本不是合法 JSON。
    InvalidJson,
    /// JSON 不满足 schema。
    SchemaMismatch(ValidationError),
}

/// 一个结构化请求的逐帧状态：累积文本 + 上一帧已提交的文档。
#[derive(Debug)]
pub struct StructuredTracker {
    schema_json: String,
    text: String,
    committed: Value,
}

impl StructuredTracker {
    /// 编 schema：成功返回 (GBNF 约束, tracker)；失败返回编译错误（调用方映射 2003）。
    pub fn compile(schema_json: &str) -> Result<(madinfer_backend::Constraint, Self), SchemaError> {
        let grammar = SchemaCompiler::new().compile(schema_json)?;
        Ok((
            grammar.into_constraint(),
            Self {
                schema_json: schema_json.to_owned(),
                text: String::new(),
                committed: Value::Object(Map::new()),
            },
        ))
    }

    /// 一帧文本到达：返回这一帧的 op 列表（每个元素一条已序列化的 RFC 6902 op 对象；无变化为空）。
    pub fn push(&mut self, frame_text: &str) -> Vec<String> {
        self.text.push_str(frame_text);
        let Some(current) = complete_prefix(&self.text) else {
            return Vec::new();
        };
        let mut ops = Vec::new();
        if let (Value::Object(_), Value::Object(_)) = (&self.committed, &current) {
            diff("", &self.committed, &current, &mut ops);
            self.committed = current;
        }
        ops.into_iter()
            .map(|op| serde_json::to_string(&op).unwrap_or_else(|_| "{}".to_owned()))
            .collect()
    }

    /// 终态：全文解析 + schema 校验；通过返回紧凑序列化的 JSON（Complete 的 `output.structured`）。
    pub fn finish(&self) -> Result<String, StructuredError> {
        let value: Value =
            serde_json::from_str(&self.text).map_err(|_| StructuredError::InvalidJson)?;
        madinfer_preproc::validate_instance(&self.schema_json, &self.text)
            .map_err(StructuredError::SchemaMismatch)?;
        serde_json::to_string(&value).map_err(|_| StructuredError::InvalidJson)
    }

    /// 至今累积的文本（Failed 的 partial 用）。
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// 把一段 JSON 前缀截成"已完整"的文档：未收尾的容器补闭；写了一半的键 / 字符串 / 标量、以及有键无值的成员丢弃。
/// 前缀不像 JSON（结构字符乱序）返回 None——调用方这一帧不产 op，等下一帧。
pub fn complete_prefix(text: &str) -> Option<Value> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        /// 对象：等键（`{` 或 `,` 之后）。
        ObjectKey,
        /// 对象：键已完，等 `:`。
        ObjectColon,
        /// 对象：等值。
        ObjectValue,
        /// 对象：值已完，等 `,` 或 `}`。
        ObjectAfter,
        /// 数组：等值（`[` 或 `,` 之后）。
        ArrayValue,
        /// 数组：值已完，等 `,` 或 `]`。
        ArrayAfter,
    }
    let bytes = text.as_bytes();
    let mut stack: Vec<Phase> = Vec::new();
    // 已完整前缀的字节终点（紧跟容器开符或一个已完值之后）。
    let mut safe_end: Option<usize> = None;
    let mut i = 0;
    // 值已完：更新所在容器的相位；栈空即根值完整。
    fn value_done(stack: &mut [Phase]) -> Option<()> {
        match stack.last_mut() {
            Some(phase @ Phase::ObjectValue) => *phase = Phase::ObjectAfter,
            Some(phase @ Phase::ArrayValue) => *phase = Phase::ArrayAfter,
            None => {}
            Some(_) => return None,
        }
        Some(())
    }
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'{' | b'[' => {
                match stack.last() {
                    None | Some(Phase::ObjectValue | Phase::ArrayValue) => {}
                    Some(_) => return None,
                }
                stack.push(if c == b'{' {
                    Phase::ObjectKey
                } else {
                    Phase::ArrayValue
                });
                i += 1;
                safe_end = Some(i);
            }
            b'}' | b']' => {
                let closing_object = c == b'}';
                match stack.last() {
                    Some(Phase::ObjectKey | Phase::ObjectAfter) if closing_object => {}
                    Some(Phase::ArrayValue | Phase::ArrayAfter) if !closing_object => {}
                    _ => return None,
                }
                stack.pop();
                i += 1;
                value_done(&mut stack)?;
                if stack.is_empty() {
                    return serde_json::from_str(&text[..i]).ok();
                }
                safe_end = Some(i);
            }
            b'"' => {
                let Some(end) = string_end(bytes, i) else {
                    break; // 未收尾字符串：丢弃。
                };
                match stack.last_mut() {
                    Some(phase @ Phase::ObjectKey) => {
                        *phase = Phase::ObjectColon;
                        i = end;
                    }
                    Some(Phase::ObjectValue | Phase::ArrayValue) => {
                        i = end;
                        value_done(&mut stack)?;
                        safe_end = Some(i);
                    }
                    None => return serde_json::from_str(&text[..end]).ok(),
                    Some(_) => return None,
                }
            }
            b':' => {
                match stack.last_mut() {
                    Some(phase @ Phase::ObjectColon) => *phase = Phase::ObjectValue,
                    _ => return None,
                }
                i += 1;
            }
            b',' => {
                match stack.last_mut() {
                    Some(phase @ Phase::ObjectAfter) => *phase = Phase::ObjectKey,
                    Some(phase @ Phase::ArrayAfter) => *phase = Phase::ArrayValue,
                    _ => return None,
                }
                i += 1;
            }
            _ => {
                // 标量（数字 / true / false / null）：到分隔符为止才算完；文本末尾的标量视为未完。
                match stack.last() {
                    None | Some(Phase::ObjectValue | Phase::ArrayValue) => {}
                    Some(_) => return None,
                }
                let mut end = i;
                while end < bytes.len()
                    && !matches!(
                        bytes[end],
                        b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'
                    )
                {
                    end += 1;
                }
                if end == bytes.len() {
                    break;
                }
                if stack.is_empty() {
                    return serde_json::from_str(&text[i..end]).ok();
                }
                i = end;
                value_done(&mut stack)?;
                safe_end = Some(i);
            }
        }
    }
    let end = safe_end?;
    let mut closed = text[..end].to_owned();
    for phase in stack.iter().rev() {
        closed.push(match phase {
            Phase::ObjectKey | Phase::ObjectColon | Phase::ObjectValue | Phase::ObjectAfter => '}',
            Phase::ArrayValue | Phase::ArrayAfter => ']',
        });
    }
    serde_json::from_str(&closed).ok()
}

/// 自 `start`（开引号）起找字符串收尾引号，返回其后一字节的下标；未收尾返回 None。
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// RFC 6901 段转义。
fn escape_segment(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn op(kind: &str, path: &str, value: Option<&Value>) -> Value {
    let mut map = Map::new();
    map.insert("op".to_owned(), Value::String(kind.to_owned()));
    map.insert("path".to_owned(), Value::String(path.to_owned()));
    if let Some(value) = value {
        map.insert("value".to_owned(), value.clone());
    }
    Value::Object(map)
}

/// 结构 diff（子集口径见模块文档）。`path` 是当前节点的 JSON Pointer（根为空串）。
fn diff(path: &str, old: &Value, new: &Value, ops: &mut Vec<Value>) {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            for (key, value) in n {
                let child = format!("{path}/{}", escape_segment(key));
                match o.get(key) {
                    Some(previous) => diff(&child, previous, value, ops),
                    None => ops.push(op("add", &child, Some(value))),
                }
            }
            for key in o.keys() {
                if !n.contains_key(key) {
                    ops.push(op(
                        "remove",
                        &format!("{path}/{}", escape_segment(key)),
                        None,
                    ));
                }
            }
        }
        (Value::Array(o), Value::Array(n)) => {
            if n.len() >= o.len() && n[..o.len()] == o[..] {
                for item in &n[o.len()..] {
                    ops.push(op("add", &format!("{path}/-"), Some(item)));
                }
            } else if !path.is_empty() {
                ops.push(op("replace", path, Some(new)));
            }
        }
        _ => {
            if old != new && !path.is_empty() {
                ops.push(op("replace", path, Some(new)));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn prefix(text: &str) -> Value {
        complete_prefix(text).unwrap_or_else(|| panic!("前缀应可补闭：{text}"))
    }

    #[test]
    fn complete_prefix_closes_containers_and_drops_dangling_parts() {
        assert_eq!(prefix("{"), serde_json::json!({}));
        assert_eq!(prefix(r#"{"a":1,"#), serde_json::json!({"a":1}));
        assert_eq!(prefix(r#"{"a":1,"b"#), serde_json::json!({"a":1}));
        assert_eq!(prefix(r#"{"a":1,"b":"#), serde_json::json!({"a":1}));
        assert_eq!(prefix(r#"{"a":1,"b":"par"#), serde_json::json!({"a":1}));
        assert_eq!(prefix(r#"{"a":1,"b":tr"#), serde_json::json!({"a":1}));
        assert_eq!(
            prefix(r#"{"a":1,"b":12"#),
            serde_json::json!({"a":1}),
            "末尾数字可能还在长"
        );
        assert_eq!(
            prefix(r#"{"a":1,"b":12,"#),
            serde_json::json!({"a":1,"b":12})
        );
        assert_eq!(
            prefix(r#"{"a":{"b":[1,2"#),
            serde_json::json!({"a":{"b":[1]}})
        );
        assert_eq!(
            prefix(r#"{"a":{"b":[1,2]"#),
            serde_json::json!({"a":{"b":[1,2]}})
        );
        assert_eq!(prefix(r#"{"a":"x\"y","#), serde_json::json!({"a":"x\"y"}));
        assert_eq!(prefix(r#"  {"a":true} "#), serde_json::json!({"a":true}));
        assert_eq!(complete_prefix(""), None);
        assert_eq!(complete_prefix("   "), None);
        assert_eq!(complete_prefix(r#"{"a" 1"#), None, "结构乱序不猜");
    }

    #[test]
    fn diff_emits_subset_ops_only() {
        let mut ops = Vec::new();
        diff(
            "",
            &serde_json::json!({"keep":1,"gone":2,"arr":[1],"obj":{"x":1},"s":"a"}),
            &serde_json::json!({"keep":1,"arr":[1,2],"obj":{"x":1,"y":2},"s":"b","new/key":true}),
            &mut ops,
        );
        let ops: Vec<String> = ops.into_iter().map(|o| o.to_string()).collect();
        assert_eq!(
            ops,
            vec![
                r#"{"op":"add","path":"/arr/-","value":2}"#,
                r#"{"op":"add","path":"/new~1key","value":true}"#,
                r#"{"op":"add","path":"/obj/y","value":2}"#,
                r#"{"op":"replace","path":"/s","value":"b"}"#,
                r#"{"op":"remove","path":"/gone"}"#,
            ]
        );
        let mut ops = Vec::new();
        diff(
            "",
            &serde_json::json!({"arr":[1,2]}),
            &serde_json::json!({"arr":[2]}),
            &mut ops,
        );
        assert_eq!(
            ops[0].to_string(),
            r#"{"op":"replace","path":"/arr","value":[2]}"#,
            "数组前缀不一致整段 replace"
        );
    }

    /// 参考应用器（子集口径同 KMP CatchupPatchDocument）：逐 op 应用到文档。
    fn apply(doc: &mut Value, patch_ops: &[String]) {
        for text in patch_ops {
            let op: Value = serde_json::from_str(text).unwrap();
            let path = op["path"].as_str().unwrap();
            let segments: Vec<String> = path[1..]
                .split('/')
                .map(|s| s.replace("~1", "/").replace("~0", "~"))
                .collect();
            let (last, parents) = segments.split_last().unwrap();
            let mut container = &mut *doc;
            for segment in parents {
                container = container.get_mut(segment).expect("中间段必存在");
            }
            match op["op"].as_str().unwrap() {
                "add" if last == "-" => container.as_array_mut().unwrap().push(op["value"].clone()),
                "add" => {
                    container
                        .as_object_mut()
                        .unwrap()
                        .insert(last.clone(), op["value"].clone());
                }
                "replace" => {
                    let map = container.as_object_mut().unwrap();
                    assert!(map.contains_key(last), "replace 目标须存在");
                    map.insert(last.clone(), op["value"].clone());
                }
                "remove" => {
                    assert!(container.as_object_mut().unwrap().remove(last).is_some());
                }
                other => panic!("子集外 op {other}"),
            }
        }
    }

    #[test]
    fn tracker_patches_replay_to_the_final_document() {
        let schema = r#"{"type":"object","properties":{"title":{"type":"string"},"items":{"type":"array","items":{"type":"integer"}}},"required":["title","items"]}"#;
        let (constraint, mut tracker) = StructuredTracker::compile(schema).expect("compile");
        assert!(constraint.grammar.starts_with("root ::="));
        let frames = [
            r#"{"ti"#,
            r#"tle":"会议","it"#,
            r#"ems":[1"#,
            r#",2,3"#,
            r#"]}"#,
        ];
        let mut doc = serde_json::json!({});
        for frame in frames {
            let ops = tracker.push(frame);
            apply(&mut doc, &ops);
        }
        let json = tracker.finish().expect("过校验");
        assert_eq!(doc, serde_json::from_str::<Value>(&json).unwrap());
        assert_eq!(json, r#"{"items":[1,2,3],"title":"会议"}"#);
    }

    #[test]
    fn finish_rejects_invalid_json_and_schema_mismatch() {
        let schema = r#"{"type":"object","properties":{"n":{"type":"integer"}},"required":["n"]}"#;
        let (_, mut tracker) = StructuredTracker::compile(schema).expect("compile");
        tracker.push(r#"{"n":"#);
        assert_eq!(tracker.finish(), Err(StructuredError::InvalidJson));
        let (_, mut tracker) = StructuredTracker::compile(schema).expect("compile");
        tracker.push(r#"{"n":"x"}"#);
        assert!(matches!(
            tracker.finish(),
            Err(StructuredError::SchemaMismatch(ValidationError {
                reason: "type_mismatch",
                ..
            }))
        ));
        assert!(StructuredTracker::compile(r#"{"anyOf":[]}"#).is_err());
    }
}
