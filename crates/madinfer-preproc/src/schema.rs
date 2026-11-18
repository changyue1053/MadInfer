//! JSON Schema（draft 2020-12 子集）→ GBNF 编译器（C12 schema2gbnf）。
//!
//! 支持子集：object / array / string / number / integer / boolean / enum / const / required /
//! minItems / maxItems / 无环 `$ref`（仅 `#/$defs/<name>` 与 `#/definitions/<name>`，根层定义）。
//! 子集之外的一切形态（patternProperties、if-then-else、anyOf / oneOf / allOf、additionalProperties 非 false、
//! pattern、format、类型数组、null 类型、布尔 schema、循环 `$ref` …）返回
//! [`SchemaError::UnsupportedSchema`]，**禁 panic**；注解键（title / description / default /
//! examples / $comment / $schema / $id）忽略。
//!
//! 编译是纯函数：规则名从属性路径确定性派生（冲突加序号），同 schema 同输出；编译结果按
//! (schema hash, backend) 缓存归运行时（C5），本 crate 不缓存。schema 文本与错误里的指针是**载荷**，
//! 只回调用方，不进日志与观测。

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use crate::json::{parse_json, JsonValue};

/// GBNF 语法文本（编译产物；载荷）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Grammar {
    text: String,
}

impl Grammar {
    /// 语法文本。
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// 交给后端的约束形态。
    pub fn into_constraint(self) -> madinfer_backend::Constraint {
        madinfer_backend::Constraint { grammar: self.text }
    }
}

/// 编译错误（闭合枚举）。`pointer` 是 schema 内出错位置（JSON Pointer 形态）；连同 schema 文本都是载荷，
/// 只返回调用方，不进日志。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// schema 文本不是合法 JSON。
    InvalidJson {
        /// 出错字节偏移。
        offset: usize,
    },
    /// 子集之外的形态。
    UnsupportedSchema {
        /// 出错位置（如 `/properties/a/items`）。
        pointer: String,
        /// 注册制原因词（`unsupported_keyword` / `cyclic_ref` / `bounds_conflict` …）。
        reason: &'static str,
    },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson { offset } => write!(f, "schema json invalid at byte {offset}"),
            Self::UnsupportedSchema { pointer, reason } => {
                write!(f, "unsupported schema at {pointer}: {reason}")
            }
        }
    }
}

impl std::error::Error for SchemaError {}

/// 数组重复上界的展开上限（GBNF 无计数重复，逐次展开；超过判 `bounds_too_large`）。
pub const MAX_ITEM_BOUND: u32 = 32;

/// 编译器。`max_depth` 限制 schema 节点嵌套（含 `$ref` 展开），缺省 32。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaCompiler {
    max_depth: usize,
}

impl SchemaCompiler {
    /// 缺省深度上限（32）。
    pub const fn new() -> Self {
        Self { max_depth: 32 }
    }

    /// 自定义深度上限（0 视为 1）。
    pub const fn with_max_depth(max_depth: usize) -> Self {
        Self {
            max_depth: if max_depth == 0 { 1 } else { max_depth },
        }
    }

    /// 编译入口：schema 文本 → GBNF。
    pub fn compile(&self, schema_json: &str) -> Result<Grammar, SchemaError> {
        let root =
            parse_json(schema_json).map_err(|e| SchemaError::InvalidJson { offset: e.offset })?;
        let mut emit = Emit::default();
        let defs = collect_defs(&root);
        let mut state = Walk {
            compiler: *self,
            defs: &defs,
            emit: &mut emit,
            pointer: Vec::new(),
            names: Vec::new(),
            ref_stack: Vec::new(),
        };
        let root_expr = state.node(&root, 0, true)?;
        let mut text = String::new();
        text.push_str(&format!("root ::= ws {root_expr} ws\n"));
        for (name, body) in &emit.rules {
            text.push_str(&format!("{name} ::= {body}\n"));
        }
        for (name, body) in emit.primitive_lines() {
            text.push_str(&format!("{name} ::= {body}\n"));
        }
        Ok(Grammar { text })
    }
}

impl Default for SchemaCompiler {
    fn default() -> Self {
        Self::new()
    }
}

/// 根层 `$defs` / `definitions` 收集（仅根层；重名时 `$defs` 优先）。
fn collect_defs(root: &JsonValue) -> BTreeMap<String, &JsonValue> {
    let mut out = BTreeMap::new();
    for key in ["definitions", "$defs"] {
        if let Some(JsonValue::Object(members)) = root.get(key) {
            for (name, schema) in members {
                out.insert(name.clone(), schema);
            }
        }
    }
    out
}

/// 产出的规则与用到的基元。
#[derive(Default)]
struct Emit {
    rules: Vec<(String, String)>,
    rule_names: BTreeSet<String>,
    /// def 名 → 已编译表达式（同名 `$ref` 只编译一次）。
    def_cache: BTreeMap<String, String>,
    uses_boolean: bool,
    uses_integer: bool,
    uses_number: bool,
    uses_string: bool,
}

impl Emit {
    /// 从语义路径派生规则名：小写、非字母数字折叠为 `-`；冲突加序号。
    fn claim_name(&mut self, names: &[String], suffix: &str) -> String {
        let mut stem: String = names
            .iter()
            .map(|s| sanitize(s))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if stem.is_empty() {
            stem = "node".to_owned();
        }
        if !suffix.is_empty() {
            stem.push('-');
            stem.push_str(suffix);
        }
        let mut candidate = stem.clone();
        let mut counter = 2;
        while !self.rule_names.insert(candidate.clone()) {
            candidate = format!("{stem}-{counter}");
            counter += 1;
        }
        candidate
    }

    fn push_rule(&mut self, name: String, body: String) {
        self.rules.push((name, body));
    }

    fn primitive_lines(&self) -> Vec<(&'static str, &'static str)> {
        let mut out = Vec::new();
        if self.uses_boolean {
            out.push(("boolean", r#""true" | "false""#));
        }
        if self.uses_integer {
            out.push(("integer", r#""-"? ("0" | [1-9] [0-9]*)"#));
        }
        if self.uses_number {
            out.push((
                "number",
                r#""-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? ([eE] [-+]? [0-9]+)?"#,
            ));
        }
        if self.uses_string {
            out.push(("string", r#""\"" string-char* "\"""#));
            out.push((
                "string-char",
                r#"[^"\\\x00-\x1F] | "\\" (["\\/bfnrt] | "u" hex-digit hex-digit hex-digit hex-digit)"#,
            ));
            out.push(("hex-digit", "[0-9a-fA-F]"));
        }
        out.push(("ws", r#"[ \t\n\r]*"#));
        out
    }
}

fn sanitize(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut gap = false;
    for c in segment.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// RFC 6901 段转义。
fn pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// 结构性关键字（子集）；`$defs` / `definitions` 仅根层放行，注解键忽略。
const STRUCTURAL_KEYS: &[&str] = &[
    "type",
    "enum",
    "const",
    "properties",
    "required",
    "items",
    "minItems",
    "maxItems",
    "$ref",
    // madai-w08 G12（I-D-07 §7-2）：只接受 `additionalProperties: false`——本编译器产出的对象语法本就只含声明的属性（闭合），
    // 该关键字是对既有语法形态的显式声明；true / 子 schema 形态仍是子集之外（unsupported_keyword）。
    "additionalProperties",
];
const ANNOTATION_KEYS: &[&str] = &[
    "title",
    "description",
    "default",
    "examples",
    "$comment",
    "$schema",
    "$id",
];

struct Walk<'a> {
    compiler: SchemaCompiler,
    defs: &'a BTreeMap<String, &'a JsonValue>,
    emit: &'a mut Emit,
    /// JSON Pointer 段（报错用）。
    pointer: Vec<String>,
    /// 语义名段（规则命名用：属性名 / item / def 名）。
    names: Vec<String>,
    /// 展开中的 `$ref` 名（环检测）。
    ref_stack: Vec<String>,
}

impl Walk<'_> {
    fn fail(&self, reason: &'static str) -> SchemaError {
        SchemaError::UnsupportedSchema {
            pointer: self.pointer_text(),
            reason,
        }
    }

    fn fail_at(&self, key: &str, reason: &'static str) -> SchemaError {
        SchemaError::UnsupportedSchema {
            pointer: format!("{}/{}", self.pointer_text(), pointer_escape(key)),
            reason,
        }
    }

    fn pointer_text(&self) -> String {
        if self.pointer.is_empty() {
            return "/".to_owned();
        }
        let mut out = String::new();
        for segment in &self.pointer {
            out.push('/');
            out.push_str(&pointer_escape(segment));
        }
        out
    }

    /// 编译一个 schema 节点，返回可内联的 GBNF 表达式（object / array / `$ref` 会注册命名规则并返回规则名）。
    fn node(
        &mut self,
        schema: &JsonValue,
        depth: usize,
        at_root: bool,
    ) -> Result<String, SchemaError> {
        if depth >= self.compiler.max_depth {
            return Err(self.fail("too_deep"));
        }
        let JsonValue::Object(members) = schema else {
            return Err(self.fail("boolean_or_non_object_schema"));
        };
        // 关键字面检查：子集之外即错。
        for (key, _) in members {
            let structural = STRUCTURAL_KEYS.contains(&key.as_str());
            let annotation = ANNOTATION_KEYS.contains(&key.as_str());
            let root_defs = at_root && (key == "$defs" || key == "definitions");
            if !structural && !annotation && !root_defs {
                return Err(self.fail_at(key, "unsupported_keyword"));
            }
        }

        if let Some(target) = schema.get("$ref") {
            return self.reference(schema, target, depth);
        }

        let has_const = schema.get("const").is_some();
        let has_enum = schema.get("enum").is_some();
        if has_const && has_enum {
            return Err(self.fail_at("const", "const_with_enum"));
        }
        if has_const || has_enum {
            for key in ["properties", "required", "items", "minItems", "maxItems"] {
                if schema.get(key).is_some() {
                    return Err(self.fail_at(key, "keyword_wrong_type"));
                }
            }
        }
        if let Some(value) = schema.get("const") {
            return scalar_literal(value)
                .ok_or_else(|| self.fail_at("const", "non_scalar_literal"));
        }
        if let Some(values) = schema.get("enum") {
            return self.enumeration(values);
        }

        let Some(kind) = schema.get("type") else {
            return Err(self.fail("missing_type"));
        };
        let kind = match kind {
            JsonValue::String(kind) => kind,
            // madai-w08 G12（I-D-07 §7-2）：`type: [X, "null", …]`——原始类型联合（string / number / integer / boolean / null）
            // 编成 GBNF 选择；object / array 进联合仍是子集之外（联合里的复合类型无法对齐 properties / items 归属）。
            JsonValue::Array(kinds) => return self.type_union(schema, kinds),
            _ => return Err(self.fail_at("type", "type_not_string")),
        };
        // additionalProperties 只许 false 且只许出现在 object 上。
        self.check_additional_properties(schema, kind == "object")?;
        // 关键字与类型匹配面检查。
        if kind != "object" {
            for key in ["properties", "required"] {
                if schema.get(key).is_some() {
                    return Err(self.fail_at(key, "keyword_wrong_type"));
                }
            }
        }
        if kind != "array" {
            for key in ["items", "minItems", "maxItems"] {
                if schema.get(key).is_some() {
                    return Err(self.fail_at(key, "keyword_wrong_type"));
                }
            }
        }
        match kind.as_str() {
            "boolean" => {
                self.emit.uses_boolean = true;
                Ok("boolean".to_owned())
            }
            "integer" => {
                self.emit.uses_integer = true;
                Ok("integer".to_owned())
            }
            "number" => {
                self.emit.uses_number = true;
                Ok("number".to_owned())
            }
            "string" => {
                self.emit.uses_string = true;
                Ok("string".to_owned())
            }
            "null" => Ok(r#""null""#.to_owned()),
            "object" => {
                let expr = self.object(schema, depth)?;
                Ok(self.name_complex_node(expr, at_root))
            }
            "array" => {
                let expr = self.array(schema, depth)?;
                Ok(self.name_complex_node(expr, at_root))
            }
            _ => Err(self.fail_at("type", "unknown_type")),
        }
    }

    /// `additionalProperties`：只接受 `false`（闭合对象——本编译器产出的对象语法本就闭合），且只许出现在 object 节点上。
    fn check_additional_properties(
        &self,
        schema: &JsonValue,
        is_object: bool,
    ) -> Result<(), SchemaError> {
        match schema.get("additionalProperties") {
            None => Ok(()),
            Some(JsonValue::Bool(false)) if is_object => Ok(()),
            Some(JsonValue::Bool(false)) => {
                Err(self.fail_at("additionalProperties", "keyword_wrong_type"))
            }
            Some(_) => Err(self.fail_at("additionalProperties", "unsupported_keyword")),
        }
    }

    /// `type` 数组：原始类型联合（去重、保持声明顺序）→ `( a | b | "null" )`；空数组 / 非字符串 / 复合类型 / 重复出现皆拒。
    fn type_union(
        &mut self,
        schema: &JsonValue,
        kinds: &[JsonValue],
    ) -> Result<String, SchemaError> {
        if kinds.is_empty() {
            return Err(self.fail_at("type", "type_union_empty"));
        }
        for key in [
            "properties",
            "required",
            "items",
            "minItems",
            "maxItems",
            "additionalProperties",
        ] {
            if schema.get(key).is_some() {
                return Err(self.fail_at(key, "keyword_wrong_type"));
            }
        }
        let mut alternatives: Vec<String> = Vec::with_capacity(kinds.len());
        for kind in kinds {
            let JsonValue::String(kind) = kind else {
                return Err(self.fail_at("type", "type_not_string"));
            };
            let expr = match kind.as_str() {
                "boolean" => {
                    self.emit.uses_boolean = true;
                    "boolean"
                }
                "integer" => {
                    self.emit.uses_integer = true;
                    "integer"
                }
                "number" => {
                    self.emit.uses_number = true;
                    "number"
                }
                "string" => {
                    self.emit.uses_string = true;
                    "string"
                }
                "null" => r#""null""#,
                "object" | "array" => {
                    return Err(self.fail_at("type", "type_union_non_primitive"));
                }
                _ => return Err(self.fail_at("type", "unknown_type")),
            };
            if alternatives.iter().any(|a| a == expr) {
                return Err(self.fail_at("type", "type_union_duplicate"));
            }
            alternatives.push(expr.to_owned());
        }
        if alternatives.len() == 1 {
            return Ok(alternatives.remove(0));
        }
        Ok(format!("( {} )", alternatives.join(" | ")))
    }

    /// 根节点内联进 root 规则；其余 object / array 注册命名规则。
    fn name_complex_node(&mut self, expr: String, at_root: bool) -> String {
        if at_root {
            return expr;
        }
        let name = self.emit.claim_name(&self.names, "");
        self.emit.push_rule(name.clone(), expr);
        name
    }

    fn reference(
        &mut self,
        schema: &JsonValue,
        target: &JsonValue,
        depth: usize,
    ) -> Result<String, SchemaError> {
        for key in STRUCTURAL_KEYS {
            if *key != "$ref" && schema.get(key).is_some() {
                return Err(self.fail_at(key, "ref_with_siblings"));
            }
        }
        let JsonValue::String(target) = target else {
            return Err(self.fail_at("$ref", "ref_unsupported_target"));
        };
        let name = target
            .strip_prefix("#/$defs/")
            .or_else(|| target.strip_prefix("#/definitions/"))
            .ok_or_else(|| self.fail_at("$ref", "ref_unsupported_target"))?;
        if name.is_empty() || name.contains('/') || name.contains('~') {
            return Err(self.fail_at("$ref", "ref_unsupported_target"));
        }
        if self.ref_stack.iter().any(|active| active == name) {
            return Err(self.fail_at("$ref", "cyclic_ref"));
        }
        // 同名 def 只编译一次：缓存命中直接引用（object / array 缓存的是规则名，标量缓存基元名）。
        if let Some(cached) = self.emit.def_cache.get(name) {
            return Ok(cached.clone());
        }
        let Some(&def_schema) = self.defs.get(name) else {
            return Err(self.fail_at("$ref", "ref_not_found"));
        };
        self.ref_stack.push(name.to_owned());
        self.pointer.push("$defs".to_owned());
        self.pointer.push(name.to_owned());
        let saved_names = std::mem::take(&mut self.names);
        self.names = vec![format!("def-{name}")];
        let expr = self.node(def_schema, depth + 1, false)?;
        self.names = saved_names;
        self.pointer.pop();
        self.pointer.pop();
        self.ref_stack.pop();
        self.emit.def_cache.insert(name.to_owned(), expr.clone());
        Ok(expr)
    }

    fn enumeration(&mut self, values: &JsonValue) -> Result<String, SchemaError> {
        let JsonValue::Array(items) = values else {
            return Err(self.fail_at("enum", "enum_not_array"));
        };
        if items.is_empty() {
            return Err(self.fail_at("enum", "enum_empty"));
        }
        let mut literals = Vec::with_capacity(items.len());
        for item in items {
            let Some(literal) = scalar_literal(item) else {
                return Err(self.fail_at("enum", "non_scalar_literal"));
            };
            literals.push(literal);
        }
        Ok(format!("( {} )", literals.join(" | ")))
    }

    fn object(&mut self, schema: &JsonValue, depth: usize) -> Result<String, SchemaError> {
        let mut props: Vec<(&String, &JsonValue)> = Vec::new();
        match schema.get("properties") {
            Some(JsonValue::Object(members)) => {
                for (name, sub) in members {
                    props.push((name, sub));
                }
            }
            Some(_) => return Err(self.fail_at("properties", "properties_not_object")),
            None => {}
        }
        let mut required: BTreeSet<&str> = BTreeSet::new();
        match schema.get("required") {
            Some(JsonValue::Array(items)) => {
                for item in items {
                    let JsonValue::String(name) = item else {
                        return Err(self.fail_at("required", "required_not_strings"));
                    };
                    if !props.iter().any(|(p, _)| *p == name) {
                        return Err(self.fail_at("required", "required_unknown_property"));
                    }
                    required.insert(name);
                }
            }
            Some(_) => return Err(self.fail_at("required", "required_not_strings")),
            None => {}
        }

        // 每个属性一条 kv 规则：`"name" ws ":" ws <值表达式>`。
        let mut kv_names: Vec<(String, bool)> = Vec::with_capacity(props.len());
        for (prop, sub) in &props {
            self.pointer.push("properties".to_owned());
            self.pointer.push((*prop).clone());
            self.names.push((*prop).clone());
            let value_expr = self.node(sub, depth + 1, false)?;
            let kv_name = self.emit.claim_name(&self.names, "kv");
            self.emit.push_rule(
                kv_name.clone(),
                format!(
                    "{} ws \":\" ws {value_expr}",
                    gbnf_literal(&json_string(prop))
                ),
            );
            self.names.pop();
            self.pointer.pop();
            self.pointer.pop();
            kv_names.push((kv_name, required.contains(prop.as_str())));
        }

        if kv_names.is_empty() {
            return Ok(r#""{" ws "}""#.to_owned());
        }
        let first_required = kv_names.iter().position(|(_, req)| *req);
        let members = match first_required {
            Some(r0) => {
                let mut out = String::new();
                for (kv, _) in &kv_names[..r0] {
                    out.push_str(&format!(r#"( {kv} ws "," ws )? "#));
                }
                out.push_str(&kv_names[r0].0);
                for (kv, req) in &kv_names[r0 + 1..] {
                    if *req {
                        out.push_str(&format!(r#" ws "," ws {kv}"#));
                    } else {
                        out.push_str(&format!(r#" ( ws "," ws {kv} )?"#));
                    }
                }
                out
            }
            None => {
                // 全可选：按首个出现的成员分支，其后成员各自独立可选（保持声明顺序）。
                let mut alternatives = Vec::with_capacity(kv_names.len());
                for i in 0..kv_names.len() {
                    let mut alt = kv_names[i].0.clone();
                    for (kv, _) in &kv_names[i + 1..] {
                        alt.push_str(&format!(r#" ( ws "," ws {kv} )?"#));
                    }
                    alternatives.push(alt);
                }
                format!("( {} )?", alternatives.join(" | "))
            }
        };
        Ok(format!(r#""{{" ws {members} ws "}}""#))
    }

    fn array(&mut self, schema: &JsonValue, depth: usize) -> Result<String, SchemaError> {
        let Some(items) = schema.get("items") else {
            return Err(self.fail("missing_items"));
        };
        let min = match schema.get("minItems") {
            Some(value) => self.bound(value, "minItems")?,
            None => 0,
        };
        let max = match schema.get("maxItems") {
            Some(value) => Some(self.bound(value, "maxItems")?),
            None => None,
        };
        if let Some(max) = max {
            if min > max {
                return Err(self.fail_at("maxItems", "bounds_conflict"));
            }
        }
        self.pointer.push("items".to_owned());
        self.names.push("item".to_owned());
        let item = self.node(items, depth + 1, false)?;
        self.names.pop();
        self.pointer.pop();

        if max == Some(0) {
            return Ok(r#""[" ws "]""#.to_owned());
        }
        let members = if min == 0 {
            let inner_tail = match max {
                None => format!(r#" ( ws "," ws {item} )*"#),
                Some(m) => optional_tail(&item, m - 1),
            };
            format!("( {item}{inner_tail} )?")
        } else {
            let mut out = item.clone();
            for _ in 1..min {
                out.push_str(&format!(r#" ws "," ws {item}"#));
            }
            match max {
                None => out.push_str(&format!(r#" ( ws "," ws {item} )*"#)),
                Some(m) => out.push_str(&optional_tail(&item, m - min)),
            }
            out
        };
        Ok(format!(r#""[" ws {members} ws "]""#))
    }

    fn bound(&self, value: &JsonValue, key: &'static str) -> Result<u32, SchemaError> {
        let JsonValue::Number(lexeme) = value else {
            return Err(self.fail_at(key, "bounds_not_integer"));
        };
        let parsed: u32 = lexeme
            .parse()
            .map_err(|_| self.fail_at(key, "bounds_not_integer"))?;
        if parsed > MAX_ITEM_BOUND {
            return Err(self.fail_at(key, "bounds_too_large"));
        }
        Ok(parsed)
    }
}

/// 逐层嵌套的可选尾巴：`( ws "," ws item ( … )? )?`，共 `count` 层。
fn optional_tail(item: &str, count: u32) -> String {
    let mut acc = String::new();
    for _ in 0..count {
        acc = if acc.is_empty() {
            format!(r#" ( ws "," ws {item} )?"#)
        } else {
            format!(r#" ( ws "," ws {item}{acc} )?"#)
        };
    }
    acc
}

/// 标量 JSON 值 → GBNF 字面量（字符串带 JSON 引号；null 不在子集）。
fn scalar_literal(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(s) => Some(gbnf_literal(&json_string(s))),
        JsonValue::Number(lexeme) => Some(gbnf_literal(lexeme)),
        JsonValue::Bool(true) => Some(gbnf_literal("true")),
        JsonValue::Bool(false) => Some(gbnf_literal("false")),
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

/// 字符串的 JSON 文本形态（带引号与转义）。
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 任意文本 → GBNF 字面量（`"…"`，转义引号 / 反斜杠 / 控制符）。
fn gbnf_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn compile(schema: &str) -> Result<Grammar, SchemaError> {
        SchemaCompiler::new().compile(schema)
    }

    fn reason(result: Result<Grammar, SchemaError>) -> &'static str {
        match result {
            Err(SchemaError::UnsupportedSchema { reason, .. }) => reason,
            other => panic!("期望 UnsupportedSchema，实际 {other:?}"),
        }
    }

    #[test]
    fn golden_object_grammar() {
        let grammar = compile(
            r#"{ "type": "object",
                 "properties": { "name": { "type": "string" }, "count": { "type": "integer" } },
                 "required": ["name"] }"#,
        )
        .expect("compile");
        let golden = concat!(
            "root ::= ws \"{\" ws name-kv ( ws \",\" ws count-kv )? ws \"}\" ws\n",
            "name-kv ::= \"\\\"name\\\"\" ws \":\" ws string\n",
            "count-kv ::= \"\\\"count\\\"\" ws \":\" ws integer\n",
            "integer ::= \"-\"? (\"0\" | [1-9] [0-9]*)\n",
            "string ::= \"\\\"\" string-char* \"\\\"\"\n",
            "string-char ::= [^\"\\\\\\x00-\\x1F] | \"\\\\\" ([\"\\\\/bfnrt] | \"u\" hex-digit hex-digit hex-digit hex-digit)\n",
            "hex-digit ::= [0-9a-fA-F]\n",
            "ws ::= [ \\t\\n\\r]*\n",
        );
        assert_eq!(grammar.as_str(), golden);
    }

    #[test]
    fn compile_is_deterministic() {
        let schema = r#"{ "type": "object", "properties": { "a": { "type": "boolean" } } }"#;
        assert_eq!(
            compile(schema).expect("a").as_str(),
            compile(schema).expect("b").as_str()
        );
    }

    #[test]
    fn all_optional_object_branches_on_the_first_present_member() {
        let grammar = compile(
            r#"{ "type": "object",
                 "properties": { "a": { "type": "boolean" }, "b": { "type": "boolean" } } }"#,
        )
        .expect("compile");
        assert!(
            grammar
                .as_str()
                .contains(r#"( a-kv ( ws "," ws b-kv )? | b-kv )?"#),
            "实际：{}",
            grammar.as_str()
        );
        let empty = compile(r#"{ "type": "object" }"#).expect("compile");
        assert!(empty.as_str().starts_with(r#"root ::= ws "{" ws "}" ws"#));
    }

    #[test]
    fn mixed_required_optional_keeps_declaration_order() {
        let grammar = compile(
            r#"{ "type": "object",
                 "properties": { "o1": { "type": "boolean" }, "r1": { "type": "boolean" },
                                  "o2": { "type": "boolean" }, "r2": { "type": "boolean" } },
                 "required": ["r1", "r2"] }"#,
        )
        .expect("compile");
        assert!(
            grammar
                .as_str()
                .contains(r#"( o1-kv ws "," ws )? r1-kv ( ws "," ws o2-kv )? ws "," ws r2-kv"#),
            "实际：{}",
            grammar.as_str()
        );
    }

    #[test]
    fn array_bounds_expand_into_nested_optional_tails() {
        let grammar = compile(
            r#"{ "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 4 }"#,
        )
        .expect("compile");
        assert!(
            grammar.as_str().contains(
                r#""[" ws integer ws "," ws integer ( ws "," ws integer ( ws "," ws integer )? )? ws "]""#
            ),
            "实际：{}",
            grammar.as_str()
        );
        let unbounded =
            compile(r#"{ "type": "array", "items": { "type": "string" } }"#).expect("compile");
        assert!(unbounded
            .as_str()
            .contains(r#"( string ( ws "," ws string )* )?"#));
        let empty_only =
            compile(r#"{ "type": "array", "items": { "type": "string" }, "maxItems": 0 }"#)
                .expect("compile");
        assert!(empty_only.as_str().contains(r#""[" ws "]""#));
    }

    #[test]
    fn enum_and_const_serialize_exact_json_literals() {
        let grammar = compile(r#"{ "enum": ["red", "gr\"n", -1.5e3, true] }"#).expect("compile");
        assert!(
            grammar
                .as_str()
                .contains(r#"root ::= ws ( "\"red\"" | "\"gr\\\"n\"" | "-1.5e3" | "true" ) ws"#),
            "实际：{}",
            grammar.as_str()
        );
        let single = compile(r#"{ "const": "是" }"#).expect("compile");
        assert!(single.as_str().contains(r#"root ::= ws "\"是\"" ws"#));
    }

    #[test]
    fn refs_resolve_once_and_cycles_are_rejected() {
        let grammar = compile(
            r##"{ "type": "object",
                 "properties": { "a": { "$ref": "#/$defs/leaf" }, "b": { "$ref": "#/$defs/leaf" } },
                 "$defs": { "leaf": { "type": "object", "properties": { "v": { "type": "string" } } } } }"##,
        )
        .expect("compile");
        assert_eq!(
            grammar.as_str().matches("def-leaf ::=").count(),
            1,
            "同名 def 只编译一次：{}",
            grammar.as_str()
        );
        assert!(
            grammar
                .as_str()
                .contains(r#"a-kv ::= "\"a\"" ws ":" ws def-leaf"#),
            "两个 $ref 共用同一条 def 规则：{}",
            grammar.as_str()
        );

        let cyclic = compile(
            r##"{ "$ref": "#/$defs/a",
                 "$defs": { "a": { "$ref": "#/$defs/b" }, "b": { "$ref": "#/$defs/a" } } }"##,
        );
        assert_eq!(reason(cyclic), "cyclic_ref");

        assert_eq!(
            reason(compile(r##"{ "$ref": "#/$defs/nope", "$defs": {} }"##)),
            "ref_not_found"
        );
        assert_eq!(
            reason(compile(r#"{ "$ref": "https://example.com/x.json" }"#)),
            "ref_unsupported_target"
        );
        assert_eq!(
            reason(compile(
                r##"{ "$ref": "#/$defs/a", "type": "string", "$defs": { "a": { "type": "string" } } }"##
            )),
            "ref_with_siblings"
        );
    }

    #[test]
    fn unsupported_forms_return_errors_not_panics() {
        for (schema, expected) in [
            (
                r#"{ "type": "object", "patternProperties": {} }"#,
                "unsupported_keyword",
            ),
            (r#"{ "anyOf": [] }"#, "unsupported_keyword"),
            (r#"{ "if": {}, "then": {} }"#, "unsupported_keyword"),
            (
                r#"{ "type": "object", "additionalProperties": true }"#,
                "unsupported_keyword",
            ),
            (
                r#"{ "type": "object", "additionalProperties": { "type": "string" } }"#,
                "unsupported_keyword",
            ),
            (
                r#"{ "type": "string", "additionalProperties": false }"#,
                "keyword_wrong_type",
            ),
            (r#"{ "type": [] }"#, "type_union_empty"),
            (
                r#"{ "type": ["string", "object"] }"#,
                "type_union_non_primitive",
            ),
            (
                r#"{ "type": ["string", "string"] }"#,
                "type_union_duplicate",
            ),
            (
                r#"{ "type": ["string", "null"], "properties": {} }"#,
                "keyword_wrong_type",
            ),
            (r#"{ "title": "x" }"#, "missing_type"),
            (
                r#"{ "type": "string", "minItems": 1 }"#,
                "keyword_wrong_type",
            ),
            (
                r#"{ "enum": ["a"], "properties": {} }"#,
                "keyword_wrong_type",
            ),
            (r#"{ "enum": [] }"#, "enum_empty"),
            (r#"{ "enum": [{}] }"#, "non_scalar_literal"),
            (r#"{ "const": null }"#, "non_scalar_literal"),
            (
                r#"{ "type": "array", "items": { "type": "string" }, "maxItems": 100 }"#,
                "bounds_too_large",
            ),
            (
                r#"{ "type": "array", "items": { "type": "string" }, "minItems": 3, "maxItems": 1 }"#,
                "bounds_conflict",
            ),
            (r#"{ "type": "array" }"#, "missing_items"),
            (
                r#"{ "type": "object", "properties": { "a": { "type": "string" } }, "required": ["b"] }"#,
                "required_unknown_property",
            ),
            (r#"true"#, "boolean_or_non_object_schema"),
            (
                r#"{ "type": "object", "properties": { "a": true } }"#,
                "boolean_or_non_object_schema",
            ),
        ] {
            assert_eq!(reason(compile(schema)), expected, "schema: {schema}");
        }
    }

    #[test]
    fn invalid_json_reports_the_offset() {
        assert!(matches!(
            compile("{ \"type\": "),
            Err(SchemaError::InvalidJson { offset }) if offset >= 9
        ));
    }

    #[test]
    fn nesting_beyond_max_depth_is_rejected() {
        let mut schema = String::new();
        for _ in 0..40 {
            schema.push_str(r#"{ "type": "array", "items": "#);
        }
        schema.push_str(r#"{ "type": "string" }"#);
        schema.push_str(&"}".repeat(40));
        assert_eq!(reason(compile(&schema)), "too_deep");
        assert!(SchemaCompiler::with_max_depth(64).compile(&schema).is_ok());
    }

    #[test]
    fn colliding_rule_names_get_numeric_suffixes() {
        let grammar = compile(
            r#"{ "type": "object",
                 "properties": { "a-b": { "type": "object", "properties": {} },
                                  "a_b": { "type": "object", "properties": {} } } }"#,
        )
        .expect("compile");
        assert!(grammar.as_str().contains("a-b ::="));
        assert!(
            grammar.as_str().contains("a-b-2 ::="),
            "实际：{}",
            grammar.as_str()
        );
    }

    /// madai-w08 G12（I-D-07 §7-2）：mad catchup 的 SCHEMA_JSON 原文——`additionalProperties: false` ×2 与 `["string","null"]` ×2
    /// 此前是子集之外（核在 Started 前 2003），现在进子集：闭合对象不改语法形态，联合类型编成选择。
    #[test]
    fn closed_objects_and_nullable_unions_compile() {
        let catchup = r#"{
          "$schema": "https://json-schema.org/draft/2020-12/schema",
          "type": "object",
          "required": ["overview", "actions", "references"],
          "additionalProperties": false,
          "properties": {
            "overview": { "type": "string" },
            "actions": {
              "type": "array",
              "items": {
                "type": "object",
                "required": ["text"],
                "additionalProperties": false,
                "properties": {
                  "text": { "type": "string" },
                  "owner": { "type": ["string", "null"] },
                  "due_hint": { "type": ["string", "null"] }
                }
              }
            },
            "references": { "type": "array", "items": { "type": "integer" } }
          }
        }"#;
        let grammar = compile(catchup).expect("catchup schema compiles");
        let text = grammar.as_str();
        let owner_rule = text
            .lines()
            .find(|line| line.starts_with("actions-item-owner-kv ::= "))
            .expect("owner kv rule");
        assert!(
            owner_rule.ends_with(r#"ws ":" ws ( string | "null" )"#),
            "联合类型应编成选择：{owner_rule}"
        );
        // 闭合对象与不写 additionalProperties 的同形 schema 产同一份语法（编译器本就只产声明的属性）。
        let open = catchup.replace(r#""additionalProperties": false,"#, "");
        assert_eq!(compile(&open).expect("open").as_str(), text);
        // 独立 null 与三元联合。
        assert!(compile(r#"{ "type": "null" }"#)
            .expect("null")
            .as_str()
            .contains(r#"root ::= ws "null" ws"#));
        assert!(compile(r#"{ "type": ["integer", "boolean", "null"] }"#)
            .expect("union")
            .as_str()
            .contains(r#"root ::= ws ( integer | boolean | "null" ) ws"#));
    }

    #[test]
    fn grammar_converts_into_backend_constraint() {
        let grammar = compile(r#"{ "type": "boolean" }"#).expect("compile");
        let text = grammar.as_str().to_owned();
        assert_eq!(grammar.into_constraint().grammar, text);
        assert!(text.contains(r#"boolean ::= "true" | "false""#));
        assert!(text.ends_with("ws ::= [ \\t\\n\\r]*\n"));
    }
}
