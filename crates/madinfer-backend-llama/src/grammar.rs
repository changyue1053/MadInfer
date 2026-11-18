//! GBNF 解释器（madai-w08 I-C-07 G4：仿真核 **Emulated 约束模式**）。
//!
//! 真 llama.cpp 以 GBNF 做约束采样（Native）；仿真核没有采样器，于是反过来**按语法产出**：解析 GBNF 为规则表，
//! 从 `root` 起确定性地走一遍——分支与可选项由 seed 派生的旋转决定、重复次数有界、字符类里的字符优先从输入词元
//! （回显词汇）里挑。走完的文本必然被该语法接受，因而由 schema2gbnf 编译出的语法走出来的一定是合法且过 schema 的 JSON
//! （最小可行口径，I-E-04 §7-1 c）。后端只认语法不认 schema——约束的语义边界与 Native 一致。
//!
//! 支持的 GBNF 子集（覆盖 madinfer-preproc `SchemaCompiler` 的全部产出与 llama.cpp 常见手写语法）：
//! `name ::= body`、序列、`|`、`( … )`、后缀 `? * +`、`"字面量"`（转义 `\" \\ \n \r \t \xHH \uHHHH`）、
//! 字符类 `[a-z]` / `[^…]`（同款转义）、规则引用、`#` 注释。不支持 llama.cpp 的 `{m,n}` 计数重复与 `.`——
//! 出现即 [`GrammarError`]（后端报 `InvalidInput`，不 panic）。
//!
//! 产出按语法叶子切片（每个字面量 / 每段字符类文本一片）；`ws` 一类只含空白的字符类重复 0 次（不产随机空白）。

use std::collections::BTreeMap;

/// 语法错误（约束文本无法编译；后端映射为 `BackendError::InvalidInput`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrammarError {
    /// 出错行（1 起）。
    pub line: usize,
    /// 注册制原因词。
    pub reason: &'static str,
}

/// 单条表达式。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Expr {
    Seq(Vec<Expr>),
    Alt(Vec<Expr>),
    Repeat {
        inner: Box<Expr>,
        min: u32,
        max: Option<u32>,
    },
    Literal(String),
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Rule(String),
}

/// 已解析的语法。
#[derive(Clone, Debug)]
pub struct Grammar {
    rules: BTreeMap<String, Expr>,
}

/// 递归深度上限（规则引用嵌套；schema 编译器深度 32 + 基元）。
const MAX_DEPTH: usize = 96;
/// 一次产出的叶子片数上限（防语法自递归无界膨胀）。
const MAX_PIECES: usize = 4_096;
/// 非字符类重复的额外次数上限（min 之上再随机 0～2 次）。
const EXTRA_REPEATS: u64 = 3;
/// 字符类重复（字符串内容 / 数字）单段长度上限。
const MAX_RUN: usize = 24;

impl Grammar {
    /// 解析 GBNF 文本；必须含 `root` 规则，且引用的规则都要有定义。
    pub fn parse(text: &str) -> Result<Self, GrammarError> {
        let mut rules = BTreeMap::new();
        // 合并续行：不含 `::=` 的非空行归上一条规则。
        let mut logical: Vec<(usize, String)> = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = strip_comment(raw);
            if line.trim().is_empty() {
                continue;
            }
            if line.contains("::=") {
                logical.push((index + 1, line.to_owned()));
            } else if let Some(last) = logical.last_mut() {
                last.1.push(' ');
                last.1.push_str(line);
            } else {
                return Err(GrammarError {
                    line: index + 1,
                    reason: "body_before_rule",
                });
            }
        }
        for (line, text) in logical {
            let Some((name, body)) = text.split_once("::=") else {
                return Err(GrammarError {
                    line,
                    reason: "missing_assign",
                });
            };
            let name = name.trim();
            if name.is_empty() || !name.chars().all(is_name_char) {
                return Err(GrammarError {
                    line,
                    reason: "bad_rule_name",
                });
            }
            let mut parser = Parser {
                chars: body.chars().collect(),
                at: 0,
                line,
            };
            let expr = parser.alternatives()?;
            parser.skip_ws();
            if parser.at != parser.chars.len() {
                return Err(GrammarError {
                    line,
                    reason: "trailing_tokens",
                });
            }
            rules.insert(name.to_owned(), expr);
        }
        if !rules.contains_key("root") {
            return Err(GrammarError {
                line: 0,
                reason: "missing_root",
            });
        }
        let grammar = Self { rules };
        grammar.check_references()?;
        Ok(grammar)
    }

    fn check_references(&self) -> Result<(), GrammarError> {
        fn walk(expr: &Expr, rules: &BTreeMap<String, Expr>) -> Result<(), GrammarError> {
            match expr {
                Expr::Seq(items) | Expr::Alt(items) => {
                    items.iter().try_for_each(|item| walk(item, rules))
                }
                Expr::Repeat { inner, .. } => walk(inner, rules),
                Expr::Rule(name) if !rules.contains_key(name) => Err(GrammarError {
                    line: 0,
                    reason: "undefined_rule",
                }),
                Expr::Rule(_) | Expr::Literal(_) | Expr::Class { .. } => Ok(()),
            }
        }
        self.rules
            .values()
            .try_for_each(|expr| walk(expr, &self.rules))
    }

    /// 从 `root` 确定性走一遍语法：`state` 是 seed 派生的旋转，`pool` 是字符类取字优先使用的词汇（回显词元）。
    /// 返回按叶子切好的片；语法自递归超出深度 / 片数上限时报错（不 panic）。
    pub fn walk(&self, state: u64, pool: &[&str]) -> Result<Vec<String>, GrammarError> {
        let mut writer = Writer {
            grammar: self,
            state,
            pool,
            pool_at: 0,
            pieces: Vec::new(),
        };
        writer.expr(
            self.rules.get("root").ok_or(GrammarError {
                line: 0,
                reason: "missing_root",
            })?,
            0,
        )?;
        Ok(writer.pieces)
    }
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// 去掉行内 `#` 注释（字面量 / 字符类里的 `#` 不算）。
fn strip_comment(line: &str) -> &str {
    let mut in_literal = false;
    let mut in_class = false;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_literal || in_class => escaped = true,
            '"' if !in_class => in_literal = !in_literal,
            '[' if !in_literal => in_class = true,
            ']' if in_class => in_class = false,
            '#' if !in_literal && !in_class => return &line[..index],
            _ => {}
        }
    }
    line
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

struct Parser {
    chars: Vec<char>,
    at: usize,
    line: usize,
}

impl Parser {
    fn fail(&self, reason: &'static str) -> GrammarError {
        GrammarError {
            line: self.line,
            reason,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.at += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    /// alternatives ::= sequence ( "|" sequence )*
    fn alternatives(&mut self) -> Result<Expr, GrammarError> {
        let mut alternatives = vec![self.sequence()?];
        loop {
            self.skip_ws();
            if self.peek() == Some('|') {
                self.at += 1;
                alternatives.push(self.sequence()?);
            } else {
                break;
            }
        }
        Ok(if alternatives.len() == 1 {
            alternatives.pop().unwrap_or(Expr::Seq(Vec::new()))
        } else {
            Expr::Alt(alternatives)
        })
    }

    /// sequence ::= ( atom postfix? )*
    fn sequence(&mut self) -> Result<Expr, GrammarError> {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            let Some(c) = self.peek() else { break };
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            let item = match self.peek() {
                Some('?') => {
                    self.at += 1;
                    Expr::Repeat {
                        inner: Box::new(atom),
                        min: 0,
                        max: Some(1),
                    }
                }
                Some('*') => {
                    self.at += 1;
                    Expr::Repeat {
                        inner: Box::new(atom),
                        min: 0,
                        max: None,
                    }
                }
                Some('+') => {
                    self.at += 1;
                    Expr::Repeat {
                        inner: Box::new(atom),
                        min: 1,
                        max: None,
                    }
                }
                Some('{') => return Err(self.fail("counted_repeat_unsupported")),
                _ => atom,
            };
            items.push(item);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap_or(Expr::Seq(Vec::new()))
        } else {
            Expr::Seq(items)
        })
    }

    fn atom(&mut self) -> Result<Expr, GrammarError> {
        match self.peek() {
            Some('"') => self.literal(),
            Some('[') => self.class(),
            Some('(') => {
                self.at += 1;
                let inner = self.alternatives()?;
                self.skip_ws();
                if self.bump() != Some(')') {
                    return Err(self.fail("unclosed_group"));
                }
                Ok(inner)
            }
            Some(c) if is_name_char(c) => {
                let mut name = String::new();
                while let Some(c) = self.peek() {
                    if is_name_char(c) {
                        name.push(c);
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                Ok(Expr::Rule(name))
            }
            Some('.') => Err(self.fail("any_char_unsupported")),
            Some(_) => Err(self.fail("unexpected_char")),
            None => Err(self.fail("unexpected_end")),
        }
    }

    fn literal(&mut self) -> Result<Expr, GrammarError> {
        self.at += 1; // 开引号
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.fail("unclosed_literal")),
                Some('"') => break,
                Some('\\') => out.push(self.escape()?),
                Some(c) => out.push(c),
            }
        }
        Ok(Expr::Literal(out))
    }

    fn class(&mut self) -> Result<Expr, GrammarError> {
        self.at += 1; // [
        let negated = if self.peek() == Some('^') {
            self.at += 1;
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        loop {
            let low = match self.bump() {
                None => return Err(self.fail("unclosed_class")),
                Some(']') => break,
                Some('\\') => self.escape()?,
                Some(c) => c,
            };
            // 区间 `a-z`（`-` 后紧跟 `]` 时按字面 `-`）。
            if self.peek() == Some('-') && self.chars.get(self.at + 1).is_some_and(|c| *c != ']') {
                self.at += 1;
                let high = match self.bump() {
                    None => return Err(self.fail("unclosed_class")),
                    Some('\\') => self.escape()?,
                    Some(c) => c,
                };
                if high < low {
                    return Err(self.fail("inverted_range"));
                }
                ranges.push((low, high));
            } else {
                ranges.push((low, low));
            }
        }
        if ranges.is_empty() && !negated {
            return Err(self.fail("empty_class"));
        }
        Ok(Expr::Class { ranges, negated })
    }

    fn escape(&mut self) -> Result<char, GrammarError> {
        let c = match self.bump() {
            None => return Err(self.fail("dangling_escape")),
            Some('n') => '\n',
            Some('r') => '\r',
            Some('t') => '\t',
            Some('x') => self.hex(2)?,
            Some('u') => self.hex(4)?,
            Some('U') => self.hex(8)?,
            // `\"` `\\` `\]` `\[` `\-` `\/` 等按字面。
            Some(other) => other,
        };
        Ok(c)
    }

    fn hex(&mut self, digits: usize) -> Result<char, GrammarError> {
        let mut value: u32 = 0;
        for _ in 0..digits {
            let d = self
                .bump()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.fail("bad_hex_escape"))?;
            value = (value << 4) | d;
        }
        char::from_u32(value).ok_or_else(|| self.fail("bad_hex_escape"))
    }
}

// ---------------------------------------------------------------------------
// 产出
// ---------------------------------------------------------------------------

struct Writer<'a> {
    grammar: &'a Grammar,
    state: u64,
    pool: &'a [&'a str],
    pool_at: usize,
    pieces: Vec<String>,
}

impl Writer<'_> {
    /// 下一枚确定性随机数（splitmix64 步进）。
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn pick(&mut self, n: usize) -> usize {
        if n <= 1 {
            0
        } else {
            usize::try_from(self.next() % u64::try_from(n).unwrap_or(u64::MAX)).unwrap_or(0)
        }
    }

    fn push(&mut self, text: String) -> Result<(), GrammarError> {
        if text.is_empty() {
            return Ok(());
        }
        if self.pieces.len() >= MAX_PIECES {
            return Err(GrammarError {
                line: 0,
                reason: "output_too_long",
            });
        }
        self.pieces.push(text);
        Ok(())
    }

    fn expr(&mut self, expr: &Expr, depth: usize) -> Result<(), GrammarError> {
        if depth > MAX_DEPTH {
            return Err(GrammarError {
                line: 0,
                reason: "too_deep",
            });
        }
        match expr {
            Expr::Seq(items) => items.iter().try_for_each(|item| self.expr(item, depth)),
            Expr::Alt(items) => {
                let index = self.pick(items.len());
                self.expr(&items[index], depth)
            }
            Expr::Repeat { inner, min, max } => self.repeat(inner, *min, *max, depth),
            Expr::Literal(text) => self.push(text.clone()),
            Expr::Class { ranges, negated } => {
                let c = self.class_char(ranges, *negated, None);
                self.push(c.to_string())
            }
            Expr::Rule(name) => {
                let rule = self.grammar.rules.get(name).ok_or(GrammarError {
                    line: 0,
                    reason: "undefined_rule",
                })?;
                self.expr(rule, depth + 1)
            }
        }
    }

    fn repeat(
        &mut self,
        inner: &Expr,
        min: u32,
        max: Option<u32>,
        depth: usize,
    ) -> Result<(), GrammarError> {
        // 字符类重复 = 一段文本（字符串内容 / 数字位）：整段成一片，字符优先来自词汇池。
        // 经规则引用 / 分支能落到字符类的（`string-char ::= [^"\\…] | "\\" …`）同样按文本段处理，走字符类那一支。
        if let Some((ranges, negated)) = self.resolve_class(inner, 0) {
            let run = self.class_run(&ranges, negated, min, max);
            return self.push(run);
        }
        // 深度越深越保守（自递归语法收敛）。
        let extra_cap = if depth > MAX_DEPTH / 2 {
            0
        } else {
            EXTRA_REPEATS
        };
        let extra = match max {
            Some(m) => {
                let room = u64::from(m.saturating_sub(min));
                let bound = room.min(extra_cap);
                if bound == 0 {
                    0
                } else {
                    self.next() % (bound + 1)
                }
            }
            None => {
                if extra_cap == 0 {
                    0
                } else {
                    self.next() % (extra_cap + 1)
                }
            }
        };
        let count = u64::from(min) + extra;
        for _ in 0..count {
            self.expr(inner, depth + 1)?;
        }
        Ok(())
    }

    /// 表达式是否（经规则引用 / 分支）能落到单个字符类：是则返回该类（拷贝），重复它即"一段文本"。
    fn resolve_class(&self, expr: &Expr, depth: usize) -> Option<(Vec<(char, char)>, bool)> {
        if depth > 8 {
            return None;
        }
        match expr {
            Expr::Class { ranges, negated } => Some((ranges.clone(), *negated)),
            Expr::Rule(name) => self.resolve_class(self.grammar.rules.get(name)?, depth + 1),
            Expr::Alt(items) => items
                .iter()
                .find_map(|item| self.resolve_class(item, depth + 1)),
            Expr::Seq(_) | Expr::Repeat { .. } | Expr::Literal(_) => None,
        }
    }

    /// 一段字符类文本：只含空白的类（`ws`）取 0 次；否则先从词汇池挑匹配字符，池里挑不出再按区间造。
    fn class_run(
        &mut self,
        ranges: &[(char, char)],
        negated: bool,
        min: u32,
        max: Option<u32>,
    ) -> String {
        let whitespace_only = !negated
            && ranges
                .iter()
                .all(|(low, high)| (*low..=*high).all(char::is_whitespace));
        let min_len = usize::try_from(min).unwrap_or(usize::MAX);
        if whitespace_only && min == 0 {
            return String::new();
        }
        let cap = max.map_or(MAX_RUN, |m| {
            usize::try_from(m).unwrap_or(MAX_RUN).min(MAX_RUN)
        });
        let mut out = String::new();
        // 词汇池：最多试遍一轮，取首个有匹配字符的词。
        if !self.pool.is_empty() && !whitespace_only {
            for _ in 0..self.pool.len() {
                let word = self.pool[self.pool_at % self.pool.len()];
                self.pool_at += 1;
                let matched: String = word
                    .chars()
                    .filter(|c| class_accepts(ranges, negated, *c))
                    .take(cap)
                    .collect();
                if !matched.is_empty() {
                    out = matched;
                    break;
                }
            }
        }
        if out.chars().count() < min_len.max(1) && out.is_empty() {
            // 池里没有可用字符：造 min（至少 1，除非 max = 0）个区间字符。
            let want = min_len.max(1).min(cap);
            if want == 0 {
                return String::new();
            }
            let mut last = None;
            for _ in 0..want {
                let c = self.class_char(ranges, negated, last);
                out.push(c);
                last = Some(c);
            }
        }
        // 补齐到 min。
        let mut last = out.chars().last();
        while out.chars().count() < min_len {
            let c = self.class_char(ranges, negated, last);
            out.push(c);
            last = Some(c);
        }
        out
    }

    /// 字符类里挑一个字符：正类取区间内（按旋转），负类从常用可打印字符里挑一个不在区间的。
    fn class_char(
        &mut self,
        ranges: &[(char, char)],
        negated: bool,
        previous: Option<char>,
    ) -> char {
        if negated {
            const CANDIDATES: &[char] = &[
                'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'm', 'n', 'o', 'p', 'q',
                'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1', '2', '3', '4', '5', '6',
                '7', '8', '9', ' ', '.', ',', ':', ';', '-', '_', '!', '?', '(', ')',
            ];
            let start = self.pick(CANDIDATES.len());
            for offset in 0..CANDIDATES.len() {
                let c = CANDIDATES[(start + offset) % CANDIDATES.len()];
                if class_accepts(ranges, true, c) {
                    return c;
                }
            }
            return '\u{4E00}'; // 常用可打印字符全被排除：退到 CJK 首字（不在任何 ASCII 区间）。
        }
        let Some(&(low, high)) = ranges.get(self.pick(ranges.len())) else {
            return 'a';
        };
        let span = u32::from(high).saturating_sub(u32::from(low));
        let offset = if span == 0 {
            0
        } else {
            u32::try_from(self.next() % (u64::from(span) + 1)).unwrap_or(0)
        };
        let candidate = char::from_u32(u32::from(low) + offset).unwrap_or(low);
        // 数字段避免整段重复同一字符（可读性；不影响合法性）。
        if previous == Some(candidate) && span > 0 {
            char::from_u32(u32::from(low) + (offset + 1) % (span + 1)).unwrap_or(low)
        } else {
            candidate
        }
    }
}

fn class_accepts(ranges: &[(char, char)], negated: bool, c: char) -> bool {
    let inside = ranges.iter().any(|(low, high)| *low <= c && c <= *high);
    inside != negated
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // 测试模块放开（根 Cargo.toml lints 注）
mod tests {
    use super::*;

    const SAMPLE: &str = r#"root ::= ws "{" ws name-kv ( ws "," ws count-kv )? ws "}" ws
name-kv ::= "\"name\"" ws ":" ws string
count-kv ::= "\"count\"" ws ":" ws integer
integer ::= "-"? ("0" | [1-9] [0-9]*)
string ::= "\"" string-char* "\""
string-char ::= [^"\\\x00-\x1F] | "\\" (["\\/bfnrt] | "u" hex-digit hex-digit hex-digit hex-digit)
hex-digit ::= [0-9a-fA-F]
ws ::= [ \t\n\r]*
"#;

    #[test]
    fn parses_compiler_output_and_walks_to_json() {
        let grammar = Grammar::parse(SAMPLE).expect("parse");
        for seed in 0..16u64 {
            let pieces = grammar.walk(seed, &["alpha", "beta"]).expect("walk");
            let text: String = pieces.concat();
            assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
            assert!(text.contains("\"name\":"), "{text}");
            // 走出来的文本是合法 JSON（serde_json 在 dev-deps 之外：用括号 / 引号配对做形式核对）。
            assert_eq!(text.matches('"').count() % 2, 0, "{text}");
        }
    }

    #[test]
    fn literal_root_is_a_single_piece() {
        let grammar = Grammar::parse("root ::= \"ok\"").expect("parse");
        assert_eq!(grammar.walk(0, &[]).expect("walk"), vec!["ok".to_owned()]);
    }

    #[test]
    fn same_seed_same_output_and_pool_words_fill_strings() {
        let grammar = Grammar::parse(SAMPLE).expect("parse");
        let a = grammar.walk(7, &["会议", "纪要"]).expect("walk");
        let b = grammar.walk(7, &["会议", "纪要"]).expect("walk");
        assert_eq!(a, b);
        let text = a.concat();
        assert!(text.contains("会议") || text.contains("纪要"), "{text}");
    }

    #[test]
    fn rejects_unsupported_and_malformed_grammar() {
        assert_eq!(
            Grammar::parse("root ::= \"a\"{2,3}").unwrap_err().reason,
            "counted_repeat_unsupported"
        );
        assert_eq!(
            Grammar::parse("foo ::= \"a\"").unwrap_err().reason,
            "missing_root"
        );
        assert_eq!(
            Grammar::parse("root ::= bar").unwrap_err().reason,
            "undefined_rule"
        );
        assert_eq!(
            Grammar::parse("root ::= \"unclosed").unwrap_err().reason,
            "unclosed_literal"
        );
        assert_eq!(
            Grammar::parse("root ::= ( \"a\"").unwrap_err().reason,
            "unclosed_group"
        );
    }

    #[test]
    fn comments_and_continuation_lines_are_handled() {
        let text = "# 注释\nroot ::= \"a\" # 尾注\n  | \"b\"\n";
        let grammar = Grammar::parse(text).expect("parse");
        let out = grammar.walk(0, &[]).expect("walk").concat();
        assert!(out == "a" || out == "b");
    }

    #[test]
    fn self_recursive_grammar_terminates() {
        let grammar = Grammar::parse("root ::= \"x\" root | \"y\"").expect("parse");
        let out = grammar.walk(3, &[]).expect("walk").concat();
        assert!(out.ends_with('y'));
    }
}
