// 报表引擎：受限表达式语言
//
// 设计对齐 z-report 的 ComputedField.expr：AI 或用户只提交"声明式规格"，
// 由本地引擎求值，而不是把自由文本 SQL 直接打到数据库上。
// 这样做的收益：
//   1. 计算列不可能触发注入（表达式没有语句边界，也不触达驱动）
//   2. 同一条表达式可跨 MySQL/PG/SQLite 复用（不依赖方言函数）
//   3. 幻觉可在求值前被解析器挡掉（未知列/未知函数直接报错）
//
// 支持：字面量、列引用、算术、比较、逻辑、|| 串接、若干白名单函数、CASE WHEN。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 引擎内部标量。边界处与 tagged-cell JSON 互转。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
}

impl Default for Value {
    fn default() -> Self {
        Value::Null
    }
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// 数值化：Int/Float/可解析的 Text/Bool → f64；NULL → None
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Null => None,
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Text(s) => s.trim().parse::<f64>().ok(),
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => Some(*f as i64),
            Value::Bool(b) => Some(*b as i64),
            Value::Text(s) => s.trim().parse::<i64>().ok().or_else(|| {
                s.trim().parse::<f64>().ok().map(|f| f as i64)
            }),
            Value::Null => None,
        }
    }

    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::Text(s) => Some(s.clone()),
            Value::Int(i) => Some(i.to_string()),
            Value::Float(f) => Some(fmt_f64(*f)),
            Value::Bool(b) => Some(b.to_string()),
        }
    }

    /// 真值判定（NULL 传播时由调用方决定）
    pub fn truthy(&self) -> Option<bool> {
        match self {
            Value::Null => None,
            Value::Bool(b) => Some(*b),
            Value::Int(i) => Some(*i != 0),
            Value::Float(f) => Some(*f != 0.0),
            Value::Text(s) => Some(!s.is_empty()),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            // 保持整数精度：超出 f64 精确范围时降级为字符串，避免前端丢位
            // 2^53 = 9007199254740992 是 JS 能精确表示的最大整数，+1 就已经不精确了
            Value::Int(i) => {
                if i.unsigned_abs() > 9007199254740992 {
                    serde_json::Value::String(i.to_string())
                } else {
                    serde_json::json!(*i)
                }
            }
            Value::Float(f) => serde_json::json!(*f),
            Value::Text(s) => serde_json::Value::String(s.clone()),
            Value::Bool(b) => serde_json::json!(*b),
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::Text(n.to_string())
                }
            }
            serde_json::Value::String(s) => Value::Text(s.clone()),
            // 对象/数组视为文本（tagged cell 在入口已拆包）
            other => Value::Text(other.to_string()),
        }
    }
}

/// 浮点输出：整数值不带小数点，避免 "100.0" 这类脏显示
fn fmt_f64(f: f64) -> String {
    if f.is_finite() && f == f.trunc() && f.abs() < 9.007199254740992e15 {
        format!("{}", f as i64)
    } else {
        format!("{}", f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Concat,
    /// IS NULL：SQL 里它是唯一能对 NULL 返回非 NULL 的比较，不能折叠成 Eq
    IsNull,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    Column(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Func(String, Vec<Expr>),
    Case {
        whens: Vec<(Expr, Expr)>,
        els: Option<Box<Expr>>,
    },
}

// ==================== Tokenizer ====================

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    /// 整数字面量单独保留，避免 9007199254740993 走 f64 丢精度
    IntNum(i64),
    Str(String),
    Ident(String),
    Op(String),
    Eof,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // 行注释
        if c == '-' && i + 1 < b.len() && b[i + 1] == b'-' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // 字符串：单引号，'' 转义
        if c == '\'' {
            i += 1;
            let mut s = String::new();
            loop {
                if i >= b.len() {
                    return Err("字符串字面量未闭合".into());
                }
                if b[i] == b'\'' {
                    if i + 1 < b.len() && b[i + 1] == b'\'' {
                        s.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                s.push(b[i] as char);
                i += 1;
            }
            out.push(Tok::Str(s));
            continue;
        }
        // 反引号/双引号标识符
        if c == '`' || c == '"' {
            let quote = c;
            i += 1;
            let start = i;
            while i < b.len() && (b[i] as char) != quote {
                i += 1;
            }
            if i >= b.len() {
                return Err(format!("{} 标识符未闭合", quote));
            }
            out.push(Tok::Ident(src[start..i].to_string()));
            i += 1;
            continue;
        }
        // 数字
        if c.is_ascii_digit() || (c == '.' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            let mut is_float = false;
            while i < b.len() {
                let ch = b[i] as char;
                if ch.is_ascii_digit() {
                    i += 1;
                } else if ch == '.' && !is_float {
                    is_float = true;
                    i += 1;
                } else if ch == 'e' || ch == 'E' {
                    is_float = true;
                    i += 1;
                    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let lex = &src[start..i];
            if !is_float {
                if let Ok(v) = lex.parse::<i64>() {
                    out.push(Tok::IntNum(v));
                    continue;
                }
            }
            let v: f64 = lex.parse().map_err(|_| format!("非法数字: {}", lex))?;
            out.push(Tok::Num(v));
            continue;
        }
        // 标识符 / 关键字
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() {
                let ch = b[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(Tok::Ident(src[start..i].to_string()));
            continue;
        }
        // 运算符（含双字符）
        let two: String = src[i..].chars().take(2).collect();
        let op2 = match two.as_str() {
            "<=" | ">=" | "<>" | "!=" | "||" | "&&" => Some(two),
            _ => None,
        };
        if let Some(o) = op2 {
            i += o.len();
            out.push(Tok::Op(o));
            continue;
        }
        let one = src[i..].chars().next().unwrap();
        match one {
            '+' | '-' | '*' | '/' | '%' | '=' | '<' | '>' | '(' | ')' | ',' | '.' => {
                out.push(Tok::Op(one.to_string()));
                i += one.len_utf8();
            }
            other => return Err(format!("无法识别的字符: '{}'", other)),
        }
    }
    out.push(Tok::Eof);
    Ok(out)
}

// ==================== Parser ====================

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

/// 表达式允许引用的列名集合；解析器用它区分"列引用"与函数名。
pub struct AllowedColumns(pub Vec<String>);

impl AllowedColumns {
    pub fn contains(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.0.iter().any(|c| c.to_ascii_lowercase() == lower)
    }
}

impl Parser {
    fn peek(&self) -> &Tok {
        self.toks.get(self.pos).unwrap_or(&Tok::Eof)
    }
    fn bump(&mut self) -> Tok {
        let t = self.peek().clone();
        self.pos += 1;
        t
    }
    fn eat_ident_kw(&mut self, kw: &str) -> bool {
        if let Tok::Ident(s) = self.peek() {
            if s.eq_ignore_ascii_case(kw) {
                self.pos += 1;
                return true;
            }
        }
        false
    }
    fn peek_ident_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
    fn eat_op(&mut self, op: &str) -> bool {
        if let Tok::Op(s) = self.peek() {
            if s == op {
                self.pos += 1;
                return true;
            }
        }
        false
    }
    fn expect_op(&mut self, op: &str) -> Result<(), String> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(format!("期望 '{}'，实际 {:?}", op, self.peek()))
        }
    }

    fn parse_primary(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        match self.peek().clone() {
            Tok::IntNum(v) => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Int(v)))
            }
            Tok::Num(v) => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Float(v)))
            }
            Tok::Str(s) => {
                self.pos += 1;
                Ok(Expr::Literal(Value::Text(s)))
            }
            Tok::Op(o) if o == "(" => {
                self.pos += 1;
                let e = self.parse_or(cols)?;
                self.expect_op(")")?;
                Ok(e)
            }
            Tok::Op(o) if o == "-" => {
                self.pos += 1;
                let inner = self.parse_primary(cols)?;
                Ok(Expr::Neg(Box::new(inner)))
            }
            Tok::Ident(name) => {
                self.pos += 1;
                let upper = name.to_ascii_uppercase();
                match upper.as_str() {
                    "TRUE" => return Ok(Expr::Literal(Value::Bool(true))),
                    "FALSE" => return Ok(Expr::Literal(Value::Bool(false))),
                    "NULL" => return Ok(Expr::Literal(Value::Null)),
                    "NOT" => {
                        let inner = self.parse_primary(cols)?;
                        return Ok(Expr::Not(Box::new(inner)));
                    }
                    "CASE" => return self.parse_case(cols),
                    _ => {}
                }
                // 函数调用
                if matches!(self.peek(), Tok::Op(s) if s == "(") {
                    self.pos += 1;
                    let fname = name.to_ascii_uppercase();
                    check_fn_allowed(&fname)?;
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Tok::Op(s) if s == ")") {
                        loop {
                            args.push(self.parse_or(cols)?);
                            if self.eat_op(",") {
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect_op(")")?;
                    return Ok(Expr::Func(fname, args));
                }
                // 列引用：必须在允许集合内
                if !cols.contains(&name) {
                    return Err(format!("未知列: {}", name));
                }
                Ok(Expr::Column(name))
            }
            other => Err(format!("意外的记号: {:?}", other)),
        }
    }

    fn parse_case(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        // CASE [operand] WHEN x THEN y ... [ELSE z] END
        let operand = if self.peek_ident_kw("WHEN") {
            None
        } else {
            Some(self.parse_or(cols)?)
        };
        let mut whens = Vec::new();
        loop {
            if let Some(op) = &operand {
                // 简单 CASE：比较 op 与 when 表达式
                if !self.eat_ident_kw("WHEN") {
                    break;
                }
                let cond_expr = self.parse_or(cols)?;
                let cond = Expr::Bin(BinOp::Eq, Box::new(op.clone()), Box::new(cond_expr));
                if !self.eat_ident_kw("THEN") {
                    return Err("CASE 缺少 THEN".into());
                }
                let then = self.parse_or(cols)?;
                whens.push((cond, then));
            } else {
                if !self.eat_ident_kw("WHEN") {
                    break;
                }
                let cond = self.parse_or(cols)?;
                if !self.eat_ident_kw("THEN") {
                    return Err("CASE 缺少 THEN".into());
                }
                let then = self.parse_or(cols)?;
                whens.push((cond, then));
            }
        }
        if whens.is_empty() {
            return Err("CASE 至少需要一个 WHEN".into());
        }
        let els = if self.eat_ident_kw("ELSE") {
            Some(Box::new(self.parse_or(cols)?))
        } else {
            None
        };
        if !self.eat_ident_kw("END") {
            return Err("CASE 缺少 END".into());
        }
        Ok(Expr::Case { whens, els })
    }

    fn parse_mul(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        let mut lhs = self.parse_primary(cols)?;
        loop {
            let op = match self.peek() {
                Tok::Op(s)
                    if s == "*" || s == "/" || s == "%" || s == "||" =>
                {
                    s.clone()
                }
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_primary(cols)?;
            let bin = match op.as_str() {
                "*" => BinOp::Mul,
                "/" => BinOp::Div,
                "%" => BinOp::Mod,
                "||" => BinOp::Concat,
                _ => unreachable!(),
            };
            lhs = Expr::Bin(bin, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_add(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        let mut lhs = self.parse_mul(cols)?;
        loop {
            let op = match self.peek() {
                Tok::Op(s) if s == "+" || s == "-" => s.clone(),
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_mul(cols)?;
            let bin = if op == "+" { BinOp::Add } else { BinOp::Sub };
            lhs = Expr::Bin(bin, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        let mut lhs = self.parse_add(cols)?;
        loop {
            let op = match self.peek() {
                Tok::Op(s) if matches!(s.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") => {
                    s.clone()
                }
                _ => {
                    // IS NULL / IS NOT NULL / IN (...)
                    if self.eat_ident_kw("IS") {
                        let negated = self.eat_ident_kw("NOT");
                        if !self.eat_ident_kw("NULL") {
                            return Err("IS 之后只支持 NULL".into());
                        }
                        let is_null =
                            Expr::Bin(BinOp::IsNull, Box::new(lhs), Box::new(Expr::Literal(Value::Null)));
                        lhs = if negated { Expr::Not(Box::new(is_null)) } else { is_null };
                        continue;
                    }
                    break;
                }
            };
            self.pos += 1;
            let rhs = self.parse_add(cols)?;
            let bin = match op.as_str() {
                "=" => BinOp::Eq,
                "<>" | "!=" => BinOp::Ne,
                "<" => BinOp::Lt,
                "<=" => BinOp::Le,
                ">" => BinOp::Gt,
                ">=" => BinOp::Ge,
                _ => unreachable!(),
            };
            lhs = Expr::Bin(bin, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        let mut lhs = self.parse_cmp(cols)?;
        while self.eat_ident_kw("AND") || self.eat_op("&&") {
            let rhs = self.parse_cmp(cols)?;
            lhs = Expr::Bin(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_or(&mut self, cols: &AllowedColumns) -> Result<Expr, String> {
        let mut lhs = self.parse_and(cols)?;
        while self.eat_ident_kw("OR") {
            let rhs = self.parse_and(cols)?;
            lhs = Expr::Bin(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
}

/// 白名单函数：避免把 SQL 全量函数面暴露给 AI 或用户
pub const ALLOWED_FUNCTIONS: &[&str] = &[
    "ABS", "ROUND", "CEIL", "FLOOR", "SQRT", "POW", "POWER", "MOD", "GREATEST", "LEAST",
    "COALESCE", "NULLIF", "IF", "CONCAT", "UPPER", "LOWER", "TRIM", "LENGTH", "SUBSTR",
    "SUBSTRING", "REPLACE", "CAST_NUM", "CAST_INT",
];

fn check_fn_allowed(name: &str) -> Result<(), String> {
    if ALLOWED_FUNCTIONS.contains(&name) {
        Ok(())
    } else {
        Err(format!("函数 {} 不在允许列表内", name))
    }
}

/// 解析表达式。未知列、未知函数、语法错误均返回 Err（幻觉在此被挡下）。
pub fn parse_expr(src: &str, cols: &AllowedColumns) -> Result<Expr, String> {
    if src.trim().is_empty() {
        return Err("表达式为空".into());
    }
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_or(&cols)?;
    if !matches!(p.peek(), Tok::Eof) {
        return Err(format!("表达式尾部有多余内容: {:?}", p.peek()));
    }
    Ok(e)
}

// ==================== Evaluator ====================

impl Expr {
    pub fn eval(&self, row: &HashMap<String, Value>) -> Result<Value, String> {
        match self {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::Column(name) => {
                // 列名大小写不敏感匹配
                if let Some(v) = row.get(name) {
                    return Ok(v.clone());
                }
                let lower = name.to_ascii_lowercase();
                for (k, v) in row {
                    if k.to_ascii_lowercase() == lower {
                        return Ok(v.clone());
                    }
                }
                Ok(Value::Null)
            }
            Expr::Neg(e) => {
                let v = e.eval(row)?;
                Ok(match v.as_f64() {
                    None => Value::Null,
                    Some(f) => {
                        if let Value::Int(i) = v {
                            Value::Int(-i)
                        } else {
                            Value::Float(-f)
                        }
                    }
                })
            }
            Expr::Not(e) => {
                let v = e.eval(row)?;
                Ok(match v.truthy() {
                    None => Value::Null,
                    Some(b) => Value::Bool(!b),
                })
            }
            Expr::Bin(op, l, r) => eval_bin(*op, l, r, row),
            Expr::Case { whens, els } => {
                for (c, t) in whens {
                    match c.eval(row)?.truthy() {
                        Some(true) => return t.eval(row),
                        Some(false) | None => continue,
                    }
                }
                match els {
                    Some(e) => e.eval(row),
                    None => Ok(Value::Null),
                }
            }
            Expr::Func(name, args) => eval_fn(name, args, row),
        }
    }

    /// 表达式中引用到的列名（用于校验）
    pub fn referenced_columns(&self, out: &mut Vec<String>) {
        match self {
            Expr::Column(c) => out.push(c.clone()),
            Expr::Neg(a) | Expr::Not(a) => a.referenced_columns(out),
            Expr::Bin(_, a, b) => {
                a.referenced_columns(out);
                b.referenced_columns(out);
            }
            Expr::Func(_, args) => {
                for a in args {
                    a.referenced_columns(out);
                }
            }
            Expr::Case { whens, els } => {
                for (c, t) in whens {
                    c.referenced_columns(out);
                    t.referenced_columns(out);
                }
                if let Some(e) = els {
                    e.referenced_columns(out);
                }
            }
            Expr::Literal(_) => {}
        }
    }
}

/// SQL 语义：NULL 参与算术 → NULL；字符串比较按文本。
fn eval_bin(
    op: BinOp,
    l: &Expr,
    r: &Expr,
    row: &HashMap<String, Value>,
) -> Result<Value, String> {
    use BinOp::*;
    // 逻辑算子短路（并把 NULL 当 false 处理，符合 SQL WHERE 语义）
    match op {
        And => {
            let a = l.eval(row)?;
            if a.truthy() == Some(false) {
                return Ok(Value::Bool(false));
            }
            let b = r.eval(row)?;
            if b.truthy() == Some(false) {
                return Ok(Value::Bool(false));
            }
            return Ok(Value::Bool(a.truthy() == Some(true) && b.truthy() == Some(true)));
        }
        Or => {
            let a = l.eval(row)?;
            if a.truthy() == Some(true) {
                return Ok(Value::Bool(true));
            }
            let b = r.eval(row)?;
            return Ok(Value::Bool(a.truthy() == Some(true) || b.truthy() == Some(true)));
        }
        Concat => {
            let a = l.eval(row)?;
            let b = r.eval(row)?;
            return Ok(Value::Text(format!(
                "{}{}",
                a.as_text().unwrap_or_default(),
                b.as_text().unwrap_or_default()
            )));
        }
        IsNull => {
            return Ok(Value::Bool(l.eval(row)?.is_null()));
        }
        _ => {}
    }

    let a = l.eval(row)?;
    let b = r.eval(row)?;

    match op {
        Eq => Ok(cmp_eq(&a, &b)),
        Ne => Ok(inv(cmp_eq(&a, &b))),
        Lt | Le | Gt | Ge => {
            // 四则序关系统一由 ord_key 的 -1/0/1 编码派生；
            // 不能把 Gt 复用一个"小于"判定函数（那会把 > 判成 <）
            Ok(match ord_key(&a, &b) {
                None => Value::Null,
                Some(k) => Value::Bool(match op {
                    Lt => k < 0,
                    Le => k <= 0,
                    Gt => k > 0,
                    Ge => k >= 0,
                    _ => unreachable!(),
                }),
            })
        }
        Add | Sub | Mul | Div | Mod => {
            // 任一侧 NULL → NULL
            if a.is_null() || b.is_null() {
                return Ok(Value::Null);
            }
            // 整数 + 整数 → 整数（避免 1+2=3.0）
            if let (Value::Int(x), Value::Int(y)) = (&a, &b) {
                let r = match op {
                    Add => x.checked_add(*y),
                    Sub => x.checked_sub(*y),
                    Mul => x.checked_mul(*y),
                    Div => {
                        if *y == 0 {
                            return Ok(Value::Null);
                        }
                        x.checked_div(*y)
                    }
                    Mod => {
                        if *y == 0 {
                            return Ok(Value::Null);
                        }
                        x.checked_rem(*y)
                    }
                    _ => None,
                };
                return Ok(r.map(Value::Int).unwrap_or(Value::Null));
            }
            let (x, y) = match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => (x, y),
                _ => return Ok(Value::Null),
            };
            let out = match op {
                Add => x + y,
                Sub => x - y,
                Mul => x * y,
                Div => {
                    if y == 0.0 {
                        return Ok(Value::Null);
                    }
                    x / y
                }
                Mod => {
                    if y == 0.0 {
                        return Ok(Value::Null);
                    }
                    x % y
                }
                _ => return Ok(Value::Null),
            };
            Ok(Value::Float(out))
        }
        _ => Ok(Value::Null),
    }
}

fn inv(v: Value) -> Value {
    match v {
        Value::Bool(b) => Value::Bool(!b),
        other => other,
    }
}

/// 比较归类：只有"同为文本"或"同可数值化"才可比，其余不可比（求值为 NULL）。
/// 把 Text("apple") 与 Int(5) 退化成字符串比较是错的，SQL 在这里给 NULL。
fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if a.is_null() || b.is_null() {
        return None;
    }
    if let (Value::Text(x), Value::Text(y)) = (a, b) {
        return Some(x.cmp(y));
    }
    if matches!(a, Value::Text(_)) || matches!(b, Value::Text(_)) {
        return None;
    }
    let (x, y) = (a.as_f64()?, b.as_f64()?);
    x.partial_cmp(&y)
}

fn cmp_eq(a: &Value, b: &Value) -> Value {
    // SQL：NULL = NULL 为 NULL，不是 TRUE
    match compare(a, b) {
        None => Value::Null,
        Some(o) => Value::Bool(o == std::cmp::Ordering::Equal),
    }
}

/// -1 / 0 / 1；None 表示不可比（含任一侧 NULL）
fn ord_key(a: &Value, b: &Value) -> Option<i32> {
    compare(a, b).map(|o| match o {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

fn eval_fn(
    name: &str,
    args: &[Expr],
    row: &HashMap<String, Value>,
) -> Result<Value, String> {
    let arg = |i: usize| -> Result<Value, String> {
        args.get(i)
            .ok_or_else(|| format!("函数 {} 缺少第 {} 个参数", name, i + 1))
            .and_then(|e| e.eval(row))
    };
    match name {
        "ABS" => {
            let v = arg(0)?;
            Ok(match v {
                Value::Int(i) => Value::Int(i.abs()),
                Value::Float(f) => Value::Float(f.abs()),
                _ => Value::Null,
            })
        }
        "ROUND" => {
            let v = match arg(0)?.as_f64() {
                Some(v) => v,
                None => return Ok(Value::Null),
            };
            let digits = arg(1).ok().and_then(|x| x.as_i64()).unwrap_or(0);
            let d = digits.max(0).min(12) as u32;
            let p = 10f64.powi(d as i32);
            let scaled = v * p;
            // half-up（与 SQL 习惯一致，不用 Rust 的 half-even）
            let r = if scaled >= 0.0 {
                (scaled + 0.5).floor()
            } else {
                (scaled - 0.5).ceil()
            };
            let out = r / p;
            Ok(if d == 0 { Value::Int(out as i64) } else { Value::Float(out) })
        }
        "CEIL" => Ok(arg(0)?.as_f64().map(|f| Value::Int(f.ceil() as i64)).unwrap_or(Value::Null)),
        "FLOOR" => Ok(arg(0)?.as_f64().map(|f| Value::Int(f.floor() as i64)).unwrap_or(Value::Null)),
        "SQRT" => Ok(arg(0)?.as_f64().map(|f| Value::Float(f.sqrt())).unwrap_or(Value::Null)),
        "POW" | "POWER" => {
            let a = arg(0)?.as_f64();
            let b = arg(1)?.as_f64();
            Ok(match (a, b) {
                (Some(x), Some(y)) => Value::Float(x.powf(y)),
                _ => Value::Null,
            })
        }
        "MOD" => {
            let a = arg(0)?.as_i64();
            let b = arg(1)?.as_i64();
            Ok(match (a, b) {
                (Some(x), Some(y)) if y != 0 => Value::Int(x.rem_euclid(y)),
                _ => Value::Null,
            })
        }
        "GREATEST" | "LEAST" => {
            let mut best: Option<Value> = None;
            for a in args {
                let v = a.eval(row)?;
                if v.is_null() {
                    continue;
                }
                best = Some(match best {
                    None => v,
                    Some(cur) => {
                        let take_new = if name == "GREATEST" {
                            ord_key(&v, &cur).map(|k| k > 0).unwrap_or(false)
                        } else {
                            ord_key(&v, &cur).map(|k| k < 0).unwrap_or(false)
                        };
                        if take_new { v } else { cur }
                    }
                });
            }
            Ok(best.unwrap_or(Value::Null))
        }
        "COALESCE" => {
            for a in args {
                let v = a.eval(row)?;
                if !v.is_null() {
                    return Ok(v);
                }
            }
            Ok(Value::Null)
        }
        "NULLIF" => {
            let a = arg(0)?;
            let b = arg(1)?;
            Ok(if matches!(cmp_eq(&a, &b), Value::Bool(true)) {
                Value::Null
            } else {
                a
            })
        }
        "IF" => {
            let c = args
                .first()
                .ok_or_else(|| "IF 需要 3 个参数".to_string())?
                .eval(row)?;
            let t = arg(1)?;
            let f = arg(2)?;
            Ok(if c.truthy() == Some(true) { t } else { f })
        }
        "CONCAT" => {
            let mut s = String::new();
            for a in args {
                let v = a.eval(row)?;
                s.push_str(&v.as_text().unwrap_or_default());
            }
            Ok(Value::Text(s))
        }
        "UPPER" | "LOWER" | "TRIM" => {
            let s = arg(0)?.as_text().ok_or_else(|| "参数须为文本".to_string())?;
            let _ = name;
            Ok(Value::Text(match name {
                "UPPER" => s.to_uppercase(),
                "LOWER" => s.to_lowercase(),
                _ => s.trim().to_string(),
            }))
        }
        "LENGTH" => Ok(arg(0)?.as_text().map(|s| Value::Int(s.chars().count() as i64)).unwrap_or(Value::Null)),
        "SUBSTR" | "SUBSTRING" => {
            let s = match arg(0)?.as_text() {
                Some(s) => s,
                None => return Ok(Value::Null),
            };
            // SQL SUBSTR 是 1-based
            let start = arg(1)?.as_i64().unwrap_or(1);
            let chars: Vec<char> = s.chars().collect();
            let from = if start > 0 {
                (start as usize - 1).min(chars.len())
            } else if start < 0 {
                (chars.len() as i64 + start).max(0) as usize
            } else {
                0
            };
            let take = match args.get(2) {
                Some(e) => e.eval(row)?.as_i64().unwrap_or(0).max(0) as usize,
                None => chars.len() - from,
            };
            Ok(Value::Text(chars[from..(from + take).min(chars.len())].iter().collect()))
        }
        "REPLACE" => {
            let s = arg(0)?.as_text().unwrap_or_default();
            let from = arg(1)?.as_text().unwrap_or_default();
            let to = arg(2)?.as_text().unwrap_or_default();
            if from.is_empty() {
                return Ok(Value::Text(s));
            }
            Ok(Value::Text(s.replace(&from, &to)))
        }
        "CAST_NUM" => Ok(arg(0)?.as_f64().map(Value::Float).unwrap_or(Value::Null)),
        "CAST_INT" => Ok(arg(0)?.as_i64().map(Value::Int).unwrap_or(Value::Null)),
        other => Err(format!("函数 {} 未实现", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn cols(names: &[&str]) -> AllowedColumns {
        AllowedColumns(names.iter().map(|s| s.to_string()).collect())
    }

    fn ev(src: &str, names: &[&str], r: &HashMap<String, Value>) -> Value {
        let e = parse_expr(src, &cols(names)).unwrap_or_else(|err| panic!("解析 {} 失败: {}", src, err));
        e.eval(r).unwrap_or_else(|err| panic!("求值 {} 失败: {}", src, err))
    }

    #[test]
    fn arithmetic_int_stays_int() {
        let r = row(&[("a", Value::Int(7)), ("b", Value::Int(2))]);
        assert_eq!(ev("a + b * 2", &["a", "b"], &r), Value::Int(11));
        assert_eq!(ev("a - b", &["a", "b"], &r), Value::Int(5));
        assert_eq!(ev("a / b", &["a", "b"], &r), Value::Int(3)); // 整除
    }

    #[test]
    fn arithmetic_float_and_null_propagation() {
        let r = row(&[("a", Value::Float(7.5)), ("b", Value::Null)]);
        assert_eq!(ev("a * 2", &["a", "b"], &r), Value::Float(15.0));
        assert_eq!(ev("a + b", &["a", "b"], &r), Value::Null);
        // 除零 → NULL，不 panic
        let z = row(&[("a", Value::Int(1)), ("z", Value::Int(0))]);
        assert_eq!(ev("a / z", &["a", "z"], &z), Value::Null);
    }

    #[test]
    fn string_concat_and_compare() {
        let r = row(&[("first", Value::Text("张".into())), ("last", Value::Text("三".into()))]);
        assert_eq!(
            ev("last || first", &["first", "last"], &r),
            Value::Text("三张".into())
        );
        let n = row(&[("s", Value::Text("abc".into()))]);
        assert_eq!(ev("LENGTH(s)", &["s"], &n), Value::Int(3));
        assert_eq!(ev("UPPER(s)", &["s"], &n), Value::Text("ABC".into()));
    }

    #[test]
    fn case_when_both_forms() {
        let r = row(&[("score", Value::Int(73)), ("grade", Value::Text("B".into()))]);
        assert_eq!(
            ev(
                "CASE WHEN score >= 90 THEN 'A' WHEN score >= 60 THEN 'B' ELSE 'C' END",
                &["score", "grade"],
                &r
            ),
            Value::Text("B".into())
        );
        // 简单 CASE
        assert_eq!(
            ev("CASE grade WHEN 'A' THEN 1 WHEN 'B' THEN 2 ELSE 0 END", &["grade"], &row(&[("grade", Value::Text("B".into()))])),
            Value::Int(2)
        );
    }

    #[test]
    fn coalesce_nullif_round() {
        let r = row(&[("a", Value::Null), ("b", Value::Int(5)), ("f", Value::Float(3.14159))]);
        assert_eq!(ev("COALESCE(a, b, 0)", &["a", "b", "f"], &r), Value::Int(5));
        assert_eq!(ev("NULLIF(b, 5)", &["a", "b", "f"], &r), Value::Null);
        assert_eq!(ev("ROUND(f, 2)", &["a", "b", "f"], &r), Value::Float(3.14));
        assert_eq!(ev("ROUND(2.5, 0)", &[], &r), Value::Int(3)); // half-up
    }

    #[test]
    fn logic_operators_sql_three_valued() {
        let r = row(&[("a", Value::Int(1)), ("n", Value::Null)]);
        assert_eq!(ev("a = 1 AND a < 5", &["a", "n"], &r), Value::Bool(true));
        assert_eq!(ev("a = 2 OR a = 1", &["a", "n"], &r), Value::Bool(true));
        // NULL 比较不是 true，WHERE 语义下被过滤掉
        assert!(!ev("n = 1", &["a", "n"], &r).truthy().unwrap_or(false));
    }

    #[test]
    fn is_null_and_not() {
        let r = row(&[("a", Value::Null), ("b", Value::Int(1))]);
        assert_eq!(ev("a IS NULL", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("b IS NOT NULL", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("NOT (a IS NULL)", &["a", "b"], &r), Value::Bool(false));
    }

    #[test]
    fn rejects_unknown_column() {
        let err = parse_expr("ghost_col + 1", &cols(&["a"])).unwrap_err();
        assert!(err.contains("未知列"), "应报未知列，实际: {}", err);
    }

    #[test]
    fn rejects_non_whitelisted_function() {
        // 关键安全点：AI 若幻觉出 DROP/PG_SLEEP 之类，这里直接挡下
        let err = parse_expr("PG_SLEEP(10)", &cols(&["a"])).unwrap_err();
        assert!(err.contains("不允许") || err.contains("不在允许"), "实际: {}", err);
    }

    #[test]
    fn string_literal_with_doubled_quote() {
        let r = row(&[]);
        let v = ev("'it''s ok'", &[], &r);
        assert_eq!(v, Value::Text("it's ok".into()));
    }

    #[test]
    fn big_int_literal_keeps_precision() {
        let r = row(&[]);
        let v = parse_expr("9007199254740993", &cols(&[])).unwrap();
        assert_eq!(v.eval(&r).unwrap(), Value::Int(9007199254740993));
    }

    #[test]
    fn big_int_to_json_downgrades_to_string() {
        // 超出 2^53 的整数不能以 JSON number 交给 JS，否则末位被抹平
        let j = Value::Int(9007199254740993).to_json();
        assert!(j.is_string(), "实际: {}", j);
    }

    #[test]
    fn substr_is_one_based_and_negative() {
        let r = row(&[("s", Value::Text("abcdef".into()))]);
        assert_eq!(ev("SUBSTR(s, 2, 3)", &["s"], &r), Value::Text("bcd".into()));
        assert_eq!(ev("SUBSTR(s, -2)", &["s"], &r), Value::Text("ef".into()));
    }

    #[test]
    fn unbalanced_parens_rejected() {
        assert!(parse_expr("(a + b", &cols(&["a", "b"])).is_err());
        assert!(parse_expr("a + ", &cols(&["a"])).is_err());
        assert!(parse_expr("", &cols(&["a"])).is_err());
    }

    #[test]
    fn referenced_columns_extracted() {
        let e = parse_expr("ROUND(a * b + LENGTH(c), 2)", &cols(&["a", "b", "c"])).unwrap();
        let mut v = Vec::new();
        e.referenced_columns(&mut v);
        v.sort();
        assert_eq!(v, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    // ==================== 回归：修复过的语义缺陷 ====================

    #[test]
    fn is_null_is_not_collapsed_into_eq() {
        // 修复点：IS NULL 曾被折叠成 `= NULL`，因 NULL 传播恒为 NULL 而永不成立
        let r = row(&[("n", Value::Null), ("z", Value::Int(0))]);
        assert_eq!(ev("n IS NULL", &["n", "z"], &r), Value::Bool(true));
        assert_eq!(ev("z IS NULL", &["n", "z"], &r), Value::Bool(false));
        assert_eq!(ev("z IS NOT NULL", &["n", "z"], &r), Value::Bool(true));
        assert_eq!(ev("n IS NOT NULL", &["n", "z"], &r), Value::Bool(false));
        // 而真正的 `= NULL` 必须保持 NULL（三值逻辑），两者不能混为一谈
        assert_eq!(ev("n = NULL", &["n", "z"], &r), Value::Null);
    }

    #[test]
    fn searched_case_without_operand() {
        // 修复点：解析操作数时用 eat 吞掉了首个 WHEN，导致 searched 形式直接空转报错
        let r = row(&[("x", Value::Int(5))]);
        assert_eq!(
            ev("CASE WHEN x > 10 THEN 'big' WHEN x > 1 THEN 'mid' ELSE 'small' END", &["x"], &r),
            Value::Text("mid".into())
        );
        // 无 ELSE 且不命中 → NULL
        assert_eq!(
            ev("CASE WHEN x > 10 THEN 'big' END", &["x"], &r),
            Value::Null
        );
    }

    #[test]
    fn json_boundary_at_2_pow_53() {
        // 修复点：阈值写成了 2^53+1，导致刚好越界的值仍以 number 下发
        let ok = Value::Int(9007199254740992).to_json(); // 2^53，JS 精确
        assert!(ok.is_number(), "2^53 应保持 number，实际 {}", ok);
        for over in [9007199254740993i64, i64::MAX, i64::MIN, -9007199254740993] {
            let j = Value::Int(over).to_json();
            assert!(j.is_string(), "{} 应降级为 string，实际 {}", over, j);
        }
    }

    #[test]
    fn nested_disallowed_function_is_rejected() {
        // 白名单必须递归生效：合法函数里套非法函数也要挡下
        assert!(parse_expr("ABS(PG_SLEEP(1))", &cols(&["a"])).is_err());
        assert!(parse_expr("COALESCE(a, DROP_TABLE(1))", &cols(&["a"])).is_err());
        assert!(parse_expr("ROUND(a, 2)", &cols(&["a"])).is_ok());
    }

    #[test]
    fn two_char_operators_advance_two() {
        // 修复点：双字符运算符曾在前移游标前被 move 进 token
        let r = row(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(ev("a <= b", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("a >= b", &["a", "b"], &r), Value::Bool(false));
        assert_eq!(ev("a <> b", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("a != b", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("a = b", &["a", "b"], &r), Value::Bool(false));
    }

    #[test]
    fn text_ordering_and_div_zero() {
        let r = row(&[("s", Value::Text("apple".into())), ("z", Value::Int(0)), ("n", Value::Int(9))]);
        assert_eq!(ev("s < 'b'", &["s", "z", "n"], &r), Value::Bool(true));
        assert_eq!(ev("n / z", &["s", "z", "n"], &r), Value::Null);
        assert_eq!(ev("n % z", &["s", "z", "n"], &r), Value::Null);
        assert_eq!(ev("n % 4", &["s", "z", "n"], &r), Value::Int(1));
        // 与 NULL 的序比较同样是 NULL
        assert_eq!(ev("NULL < 1", &["s", "z", "n"], &r), Value::Null);
    }

    #[test]
    fn ordering_operators_are_not_inverted() {
        // 修复点：Gt 曾复用"小于"判定，导致 5 > 10 为 true
        let r = row(&[("a", Value::Int(5)), ("b", Value::Int(10))]);
        assert_eq!(ev("a > b", &["a", "b"], &r), Value::Bool(false));
        assert_eq!(ev("a < b", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("a >= b", &["a", "b"], &r), Value::Bool(false));
        assert_eq!(ev("a <= b", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("a >= 5", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("a <= 5", &["a", "b"], &r), Value::Bool(true));
        assert_eq!(ev("b / 2 > a", &["a", "b"], &r), Value::Bool(false));
        assert_eq!(ev("b / 2 >= a", &["a", "b"], &r), Value::Bool(true));
    }

    #[test]
    fn text_and_number_are_incomparable() {
        // 修复点：Text 与数值曾被一起 as_text() 后按字典序硬比
        let r = row(&[("s", Value::Text("apple".into())), ("n", Value::Int(5))]);
        assert_eq!(ev("s > n", &["s", "n"], &r), Value::Null);
        assert_eq!(ev("s = n", &["s", "n"], &r), Value::Null);
        // 同为文本仍按字典序
        assert_eq!(ev("s = 'apple'", &["s", "n"], &r), Value::Bool(true));
        assert_eq!(ev("s < 'b'", &["s", "n"], &r), Value::Bool(true));
        // 同为文本的数字串不能被当成数值（'10' < '9' 是字典序）
        assert_eq!(ev("'10' < '9'", &["s", "n"], &r), Value::Bool(true));
        // 数值优先：两侧都能数值化才按数值比
        assert_eq!(ev("n = 5", &["s", "n"], &r), Value::Bool(true));
    }
}
