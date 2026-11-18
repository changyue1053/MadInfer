//! JSON 实例对 JSON Schema（与 [`crate::SchemaCompiler`] 同一子集）的校验（madai-w08 I-C-07 G4；I-E-04 §7-1 f）。
//!
//! 结构化输出终态前的守门：Emulated / 系统模型产出的文本先解析、再逐节点对 schema——不通过即 `SchemaInvalid 2003`，
//! `Complete` 只在通过时发出。子集：object（properties / required；`additionalProperties: false` 时未声明的属性判
//! `additional_property`，未写时放行——与编译器同口径，madai-w08 G12）/ array（items / minItems / maxItems）/ string / number /
//! integer / boolean / null / 原始类型联合 `type: [X, "null", …]`（任一匹配即通过）/ enum / const / 无环 `$ref`
//! （`#/$defs/<name>` / `#/definitions/<name>`）。schema 子集之外的形态在编译期已被拒，
//! 这里遇到即视为不通过（保守）。schema 与实例都是**载荷**：错误里只带 JSON Pointer 与注册制原因词。

use std::collections::BTreeMap;

use crate::json::{parse_json, JsonValue};

/// 校验失败（首个不通过的节点）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// 实例内出错位置（JSON Pointer；根为 `/`）。
    pub pointer: String,
    /// 注册制原因词（`invalid_json` / `type_mismatch` / `missing_required` / `additional_property` / `enum_mismatch` /
    /// `const_mismatch` / `too_few_items` / `too_many_items` / `schema_unsupported`）。
    pub reason: &'static str,
}

/// 校验 `instance_json` 是否满足 `schema_json`。schema 非法 JSON 或子集之外一律 `schema_unsupported`；实例非法 JSON 为 `invalid_json`。
pub fn validate_instance(schema_json: &str, instance_json: &str) -> Result<(), ValidationError> {
    let schema = parse_json(schema_json).map_err(|_| ValidationError {
        pointer: "/".to_owned(),
        reason: "schema_unsupported",
    })?;
    let instance = parse_json(instance_json).map_err(|_| ValidationError {
        pointer: "/".to_owned(),
        reason: "invalid_json",
    })?;
    let mut defs = BTreeMap::new();
    for key in ["definitions", "$defs"] {
        if let Some(JsonValue::Object(members)) = schema.get(key) {
            for (name, sub) in members {
                defs.insert(name.as_str(), sub);
            }
        }
    }
    let mut walker = Validator {
        defs: &defs,
        path: Vec::new(),
    };
    walker.node(&schema, &instance, 0)
}

struct Validator<'a> {
    defs: &'a BTreeMap<&'a str, &'a JsonValue>,
    path: Vec<String>,
}

const MAX_DEPTH: usize = 64;

impl Validator<'_> {
    fn fail(&self, reason: &'static str) -> ValidationError {
        let pointer = if self.path.is_empty() {
            "/".to_owned()
        } else {
            let mut out = String::new();
            for segment in &self.path {
                out.push('/');
                out.push_str(&segment.replace('~', "~0").replace('/', "~1"));
            }
            out
        };
        ValidationError { pointer, reason }
    }

    fn node(
        &mut self,
        schema: &JsonValue,
        instance: &JsonValue,
        depth: usize,
    ) -> Result<(), ValidationError> {
        if depth > MAX_DEPTH {
            return Err(self.fail("schema_unsupported"));
        }
        let JsonValue::Object(_) = schema else {
            return Err(self.fail("schema_unsupported"));
        };
        if let Some(JsonValue::String(target)) = schema.get("$ref") {
            let name = target
                .strip_prefix("#/$defs/")
                .or_else(|| target.strip_prefix("#/definitions/"))
                .ok_or_else(|| self.fail("schema_unsupported"))?;
            let Some(&def) = self.defs.get(name) else {
                return Err(self.fail("schema_unsupported"));
            };
            return self.node(def, instance, depth + 1);
        }
        if let Some(expected) = schema.get("const") {
            return if scalar_eq(expected, instance) {
                Ok(())
            } else {
                Err(self.fail("const_mismatch"))
            };
        }
        if let Some(JsonValue::Array(options)) = schema.get("enum") {
            return if options.iter().any(|o| scalar_eq(o, instance)) {
                Ok(())
            } else {
                Err(self.fail("enum_mismatch"))
            };
        }
        let kind = match schema.get("type") {
            Some(JsonValue::String(kind)) => kind,
            // 原始类型联合：任一分支匹配即通过（复合类型进联合在编译期已拒；这里保守：分支非原始类型 → schema_unsupported）。
            Some(JsonValue::Array(kinds)) => {
                if kinds.is_empty() {
                    return Err(self.fail("schema_unsupported"));
                }
                let mut names: Vec<&str> = Vec::with_capacity(kinds.len());
                for kind in kinds {
                    let JsonValue::String(kind) = kind else {
                        return Err(self.fail("schema_unsupported"));
                    };
                    names.push(kind.as_str());
                }
                // 先整体判子集（任一分支非原始类型名即 schema_unsupported），再看实例是否命中任一分支。
                let mut hit = false;
                for name in names {
                    match primitive_matches(name, instance) {
                        None => return Err(self.fail("schema_unsupported")),
                        Some(true) => hit = true,
                        Some(false) => {}
                    }
                }
                return if hit {
                    Ok(())
                } else {
                    Err(self.fail("type_mismatch"))
                };
            }
            _ => return Err(self.fail("schema_unsupported")),
        };
        match (kind.as_str(), instance) {
            ("boolean", JsonValue::Bool(_)) | ("string", JsonValue::String(_)) => Ok(()),
            ("null", JsonValue::Null) => Ok(()),
            ("number", JsonValue::Number(_)) => Ok(()),
            ("integer", JsonValue::Number(lexeme)) => {
                if lexeme.contains(['.', 'e', 'E']) {
                    Err(self.fail("type_mismatch"))
                } else {
                    Ok(())
                }
            }
            ("object", JsonValue::Object(members)) => self.object(schema, members, depth),
            ("array", JsonValue::Array(items)) => self.array(schema, items, depth),
            ("boolean" | "string" | "number" | "integer" | "null" | "object" | "array", _) => {
                Err(self.fail("type_mismatch"))
            }
            _ => Err(self.fail("schema_unsupported")),
        }
    }

    fn object(
        &mut self,
        schema: &JsonValue,
        members: &[(String, JsonValue)],
        depth: usize,
    ) -> Result<(), ValidationError> {
        if let Some(JsonValue::Array(required)) = schema.get("required") {
            for name in required {
                let JsonValue::String(name) = name else {
                    return Err(self.fail("schema_unsupported"));
                };
                if !members.iter().any(|(k, _)| k == name) {
                    self.path.push(name.clone());
                    let error = self.fail("missing_required");
                    self.path.pop();
                    return Err(error);
                }
            }
        }
        let props = match schema.get("properties") {
            Some(JsonValue::Object(props)) => Some(props),
            _ => None,
        };
        if let Some(props) = props {
            for (name, sub) in props {
                // 键重复取末个（与解析器 get 口径一致）。
                if let Some((_, value)) = members.iter().rev().find(|(k, _)| k == name) {
                    self.path.push(name.clone());
                    self.node(sub, value, depth + 1)?;
                    self.path.pop();
                }
            }
        }
        // madai-w08 G12：`additionalProperties: false` → 未声明的属性判不通过（首个越界键进 pointer）；未写则放行。
        if let Some(JsonValue::Bool(false)) = schema.get("additionalProperties") {
            for (key, _) in members {
                let declared = props.is_some_and(|p| p.iter().any(|(n, _)| n == key));
                if !declared {
                    self.path.push(key.clone());
                    let error = self.fail("additional_property");
                    self.path.pop();
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn array(
        &mut self,
        schema: &JsonValue,
        items: &[JsonValue],
        depth: usize,
    ) -> Result<(), ValidationError> {
        if let Some(JsonValue::Number(min)) = schema.get("minItems") {
            if min.parse::<usize>().is_ok_and(|min| items.len() < min) {
                return Err(self.fail("too_few_items"));
            }
        }
        if let Some(JsonValue::Number(max)) = schema.get("maxItems") {
            if max.parse::<usize>().is_ok_and(|max| items.len() > max) {
                return Err(self.fail("too_many_items"));
            }
        }
        let Some(item_schema) = schema.get("items") else {
            return Err(self.fail("schema_unsupported"));
        };
        for (index, item) in items.iter().enumerate() {
            self.path.push(index.to_string());
            self.node(item_schema, item, depth + 1)?;
            self.path.pop();
        }
        Ok(())
    }
}

/// 原始类型名对实例的匹配：`Some(true)` 匹配、`Some(false)` 不匹配、`None` 非原始类型名。
fn primitive_matches(kind: &str, instance: &JsonValue) -> Option<bool> {
    Some(match (kind, instance) {
        ("boolean", JsonValue::Bool(_))
        | ("string", JsonValue::String(_))
        | ("number", JsonValue::Number(_))
        | ("null", JsonValue::Null) => true,
        ("integer", JsonValue::Number(lexeme)) => !lexeme.contains(['.', 'e', 'E']),
        ("boolean" | "string" | "number" | "integer" | "null", _) => false,
        _ => return None,
    })
}

/// 标量相等（数字按词素比较——编译器把 enum / const 的数字按词素写进语法，产出词素与 schema 同文）。
fn scalar_eq(expected: &JsonValue, actual: &JsonValue) -> bool {
    match (expected, actual) {
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Number(a), JsonValue::Number(b)) => a == b,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::Null, JsonValue::Null) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r##"{
      "type": "object",
      "properties": {
        "title": { "type": "string" },
        "count": { "type": "integer" },
        "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 3 },
        "level": { "enum": ["low", "high"] },
        "nested": { "$ref": "#/$defs/leaf" }
      },
      "required": ["title", "tags"],
      "$defs": { "leaf": { "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"] } }
    }"##;

    #[test]
    fn accepts_conforming_instance_and_ignores_undeclared_properties() {
        let instance =
            r#"{"title":"t","count":3,"tags":["a"],"level":"low","nested":{"ok":true},"extra":1}"#;
        assert_eq!(validate_instance(SCHEMA, instance), Ok(()));
    }

    #[test]
    fn reports_first_failure_with_pointer_and_reason() {
        let cases = [
            (r#"{"tags":["a"]}"#, "/title", "missing_required"),
            (r#"{"title":1,"tags":["a"]}"#, "/title", "type_mismatch"),
            (r#"{"title":"t","tags":[]}"#, "/tags", "too_few_items"),
            (
                r#"{"title":"t","tags":["a","b","c","d"]}"#,
                "/tags",
                "too_many_items",
            ),
            (r#"{"title":"t","tags":[1]}"#, "/tags/0", "type_mismatch"),
            (
                r#"{"title":"t","tags":["a"],"count":1.5}"#,
                "/count",
                "type_mismatch",
            ),
            (
                r#"{"title":"t","tags":["a"],"level":"mid"}"#,
                "/level",
                "enum_mismatch",
            ),
            (
                r#"{"title":"t","tags":["a"],"nested":{}}"#,
                "/nested/ok",
                "missing_required",
            ),
            ("not json", "/", "invalid_json"),
            ("[1]", "/", "type_mismatch"),
        ];
        for (instance, pointer, reason) in cases {
            assert_eq!(
                validate_instance(SCHEMA, instance),
                Err(ValidationError {
                    pointer: pointer.to_owned(),
                    reason,
                }),
                "{instance}"
            );
        }
    }

    /// madai-w08 G12：闭合对象与原始类型联合（catchup schema 的两个特性）。
    #[test]
    fn closed_objects_and_nullable_unions() {
        let schema = r#"{"type":"object","additionalProperties":false,"required":["text"],
            "properties":{"text":{"type":"string"},"owner":{"type":["string","null"]}}}"#;
        assert_eq!(
            validate_instance(schema, r#"{"text":"a","owner":null}"#),
            Ok(())
        );
        assert_eq!(
            validate_instance(schema, r#"{"text":"a","owner":"b"}"#),
            Ok(())
        );
        assert_eq!(validate_instance(schema, r#"{"text":"a"}"#), Ok(()));
        assert_eq!(
            validate_instance(schema, r#"{"text":"a","owner":1}"#),
            Err(ValidationError {
                pointer: "/owner".to_owned(),
                reason: "type_mismatch"
            })
        );
        assert_eq!(
            validate_instance(schema, r#"{"text":"a","extra":true}"#),
            Err(ValidationError {
                pointer: "/extra".to_owned(),
                reason: "additional_property"
            })
        );
        // 未写 additionalProperties 仍放行未声明属性（与编译器口径一致）。
        let open = schema.replace(r#""additionalProperties":false,"#, "");
        assert_eq!(
            validate_instance(&open, r#"{"text":"a","extra":true}"#),
            Ok(())
        );
        assert_eq!(validate_instance(r#"{"type":"null"}"#, "null"), Ok(()));
        assert_eq!(
            validate_instance(r#"{"type":["integer","null"]}"#, "1.5"),
            Err(ValidationError {
                pointer: "/".to_owned(),
                reason: "type_mismatch"
            })
        );
        assert_eq!(
            validate_instance(r#"{"type":["string","object"]}"#, r#""a""#),
            Err(ValidationError {
                pointer: "/".to_owned(),
                reason: "schema_unsupported"
            })
        );
    }

    #[test]
    fn const_and_unsupported_schema() {
        assert_eq!(validate_instance(r#"{"const":"x"}"#, r#""x""#), Ok(()));
        assert_eq!(
            validate_instance(r#"{"const":"x"}"#, r#""y""#).map_err(|e| e.reason),
            Err("const_mismatch")
        );
        assert_eq!(
            validate_instance(r#"{"anyOf":[]}"#, r#"1"#).map_err(|e| e.reason),
            Err("schema_unsupported")
        );
        assert_eq!(
            validate_instance("{", r#"1"#).map_err(|e| e.reason),
            Err("schema_unsupported")
        );
    }
}
