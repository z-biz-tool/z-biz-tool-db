// AI 辅助写 SQL：自然语言 → 一条真正打得住这张库的 SQL。
//
// 报表链路刻意不让模型写 SQL；但用户在 SQL 编辑器里要的就是 SQL 本身，
// 所以这里单独搭一条链，并把报表链路那套防线照搬过来：
//   1. 模型只看得见本机目录里的表与列（连 DBConfig 都不进参数，凭据无从泄漏）
//   2. 回复先剥掉 ``` 围栏、"好的，下面是…"这类开场白和结尾那句中文说明
//   3. 本机校验：FROM/JOIN 的表必须命中目录，别名.列 必须在该表列清单里，
//      多语句直接拒——编出来的字段进不了编辑器
//   4. 校验失败把错误原文回喂，模型自己改，重试次数有上限
// 写操作不在这里拦：归类成 warning 报给 UI，真正执行仍走 T-006/T-024 的审批门禁。

use serde::{Deserialize, Serialize};

use crate::db::sql_classify;
use crate::report::ai::{strip_fences, column_list, CatalogTable, Model, HttpModel, MAX_REPAIRS};
use crate::AIConfig;

/// 进提示词的表数上限：SQL 编辑器对着一个连接，几十张表模型也抓不住重点
pub const MAX_TABLES: usize = 20;

/// 一条生成的结果
#[derive(Debug, Clone, Serialize)]
pub struct SqlDraft {
    pub sql: String,
    /// 本机认出这条 SQL 用了目录里的哪几张表——UI 用它说明"依据是什么"
    pub tables: Vec<String>,
    /// 本次用的方言（由目录里的连接配置给出，模型改不动）
    pub dialect: String,
    /// 不算错但用户该知道的事：写操作、没引用任何目录内的表
    pub warnings: Vec<String>,
    /// 模型被本机校验打回了几次
    pub repairs: u8,
}

/// 上一稿：让"再按月份聚合一下""把金额换成含税的"这种追问能在已有语句上改，
/// 而不是每次从零重写。带着当时那句需求，模型才知道旧稿是为了什么写的。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PriorDraft {
    pub question: String,
    pub sql: String,
}

/// 目录里出现过的表名（含 schema 限定的写法），以及每张表的列清单
struct Index {
    /// 小写表名 → 目录项
    tables: Vec<(String, usize)>,
}

impl Index {
    fn new(catalog: &[CatalogTable]) -> Self {
        let tables = catalog
            .iter()
            .enumerate()
            .map(|(i, t)| (qualified(t).to_ascii_lowercase(), i))
            .collect();
        Self { tables }
    }

    fn find(&self, name: &str) -> Option<usize> {
        let key = name.to_ascii_lowercase();
        self.tables
            .iter()
            .find(|(k, _)| *k == key || k.ends_with(&format!(".{}", key)))
            .map(|(_, i)| *i)
        // 允许裸表名命中 schema.table；命中不上就是真没有这张表
    }
}

fn qualified(t: &CatalogTable) -> String {
    if t.schema.trim().is_empty() {
        t.table.clone()
    } else {
        format!("{}.{}", t.schema, t.table)
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

// ==================== 词法 ====================

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 裸词（关键字、表名、列名）
    Word(String),
    /// 引号包起来的标识符：`x` / "x" / [x]
    Quoted(String),
    /// 数字字面量。留着是为了分得出"只写了半个 SELECT"和"SELECT 1"
    Num(String),
    Dot,
    Other(char),
}

fn tokenize(sql: &str) -> Vec<Tok> {
    let ch: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        // 行注释
        if c == '-' && i + 1 < ch.len() && ch[i + 1] == '-' {
            while i < ch.len() && ch[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // 块注释
        if c == '/' && i + 1 < ch.len() && ch[i + 1] == '*' {
            i += 2;
            while i + 1 < ch.len() && !(ch[i] == '*' && ch[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(ch.len());
            continue;
        }
        // 字符串字面量：整段跳过，里面的点号与词都不算标识符
        if c == '\'' {
            i += 1;
            while i < ch.len() {
                if ch[i] == '\'' {
                    if i + 1 < ch.len() && ch[i + 1] == '\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 引用标识符
        let close = match c {
            '`' => Some('`'),
            '"' => Some('"'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(end) = close {
            let start = i + 1;
            i += 1;
            while i < ch.len() && ch[i] != end {
                i += 1;
            }
            out.push(Tok::Quoted(ch[start..i.min(ch.len())].iter().collect()));
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < ch.len() && (ch[i].is_alphanumeric() || matches!(ch[i], '_' | '$')) {
                i += 1;
            }
            out.push(Tok::Word(ch[start..i].iter().collect()));
            continue;
        }
        // 数字字面量：只作为一个"有内容"的记号留着，不参与标识符判断
        if c.is_ascii_digit() {
            let start = i;
            while i < ch.len() && (ch[i].is_ascii_digit() || ch[i] == '.') {
                i += 1;
            }
            out.push(Tok::Num(ch[start..i].iter().collect()));
            continue;
        }
        if c == '.' {
            out.push(Tok::Dot);
        } else if !c.is_whitespace() {
            out.push(Tok::Other(c));
        }
        i += 1;
    }
    out
}

/// 不该被当成表别名的词：`FROM orders WHERE` 里的 WHERE 不是别名
const AFTER_TABLE: &[&str] = &[
    "where", "group", "order", "having", "limit", "offset", "on", "using", "join", "inner",
    "left", "right", "full", "cross", "outer", "union", "except", "intersect", "set", "values",
    "into", "as", "for", "window", "fetch", "with", "returning",
];

/// 取某个位置的裸词（引号标识符也算），越界或非词返回 None
fn word_at(toks: &[Tok], i: usize) -> Option<&str> {
    match toks.get(i)? {
        Tok::Word(w) => Some(w.as_str()),
        Tok::Quoted(w) => Some(w.as_str()),
        _ => None,
    }
}

fn is_word(t: &Tok, kw: &str) -> bool {
    matches!(t, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
}

// ==================== 提示词 ====================

/// 生成给模型的完整提示词。prior 是上一稿（追问式改稿时才带），
/// feedback 是上一稿被本机校验拒绝的原因。
pub fn prompt(
    question: &str,
    catalog: &[CatalogTable],
    dialect: &str,
    prior: Option<&PriorDraft>,
    feedback: Option<&str>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "你是数据库工程师。用 {} 方言写一条 SQL，只回 SQL 本身，不要解释、不要 markdown 围栏。\n\n\
         表与列（表名和列名只能从这里取，编不出来的一律会被本机挡下；括号里是「列名 类型」）：\n",
        dialect
    ));
    for t in catalog {
        if t.columns.is_empty() {
            s.push_str(&format!("  · {}（本机没读到列清单）\n", qualified(t)));
        } else {
            s.push_str(&format!(
                "  · {}({})\n",
                qualified(t),
                column_list(t)
            ));
        }
    }
    s.push_str(
        "\n规则：\n\
         1. 只能一条语句；不要写 INSERT/UPDATE/DELETE/DROP，除非需求明确要改数据。\n\
         2. 表起了别名就用别名引用列，不要用库名以外的前缀。\n\
         3. 不确定的列宁可不选，也不要用 SELECT * 蒙；时间比较写成字符串比较。\n\
         4. 需要限制行数时用 LIMIT。\n\
         5. 按类型写条件：文本列别拿去求和，日期/数值列比较前先看清它是 int、decimal 还是字符串，\n\
         \x20  类型不一致就先 CAST 对齐。\n",
    );
    s.push_str("\n需求：");
    s.push_str(question.trim());
    if let Some(p) = prior {
        s.push_str(&format!(
            "\n\n上一稿是这么写的（当时需求：{}）：\n{}\n\
             请在它基础上按新需求改：新需求没提到的部分保持原样，只回改好的完整 SQL，\n\
             不要回 diff、不要只回改动的那几行。\n",
            p.question.trim(),
            p.sql.trim()
        ));
    }
    if let Some(fb) = feedback {
        s.push_str("\n\n上一稿没有通过本机校验，原因：\n");
        s.push_str(fb.trim());
        s.push_str("\n请只修正这个问题，重新输出完整 SQL。");
    }
    s
}

// ==================== 从回复里挖 SQL ====================

/// 语句开头关键字：用来跳过"好的，这是你要的 SQL："这类开场白
const HEADS: &[&str] = &[
    "select", "with", "insert", "update", "delete", "create", "alter", "drop", "truncate",
    "replace", "merge", "show", "describe", "desc", "explain", "pragma", "values", "call",
];

/// 模型讲道理时才会出现的标点（半角的那些 SQL 里到处都是，不能列进来）
const PROSE_PUNCT: &[char] = &['。', '！', '？', '；', '：', '、', '，', '（'];

/// 全角标点不会出现在合法 SQL 里（字符串/注释里的已被跳过），看见就是模型在讲道理。
/// 返回这段说明的起始字节：从标点往回退到 contiguous 的非 ASCII 词首，
/// 这样 `orders，不需要 join` 切在逗号前、`…订单号。` 切在整个句子前。
fn prose_cut_start(body: &str) -> Option<usize> {
    let ch: Vec<char> = body.chars().collect();
    let mut off = Vec::with_capacity(ch.len() + 1);
    let mut acc = 0usize;
    for c in &ch {
        off.push(acc);
        acc += c.len_utf8();
    }
    let at = |i: usize| if i >= off.len() { acc } else { off[i] };

    let mut i = 0usize;
    while i < ch.len() {
        let c = ch[i];
        let quote = match c {
            '\'' => Some('\''),
            '`' => Some('`'),
            '"' => Some('"'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(close) = quote {
            let mut j = i + 1;
            while j < ch.len() {
                if ch[j] == close {
                    // '' 是 SQL 里的转义引号，不算收尾
                    if close == '\'' && j + 1 < ch.len() && ch[j + 1] == '\'' {
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            i = j;
            continue;
        }
        if c == '-' && i + 1 < ch.len() && ch[i + 1] == '-' {
            let mut j = i;
            while j < ch.len() && ch[j] != '\n' {
                j += 1;
            }
            i = j;
            continue;
        }
        if c == '/' && i + 1 < ch.len() && ch[i + 1] == '*' {
            let mut j = i + 2;
            while j + 1 < ch.len() && !(ch[j] == '*' && ch[j + 1] == '/') {
                j += 1;
            }
            i = (j + 2).min(ch.len());
            continue;
        }
        if PROSE_PUNCT.contains(&c) {
            let mut j = i;
            while j > 0 && !ch[j - 1].is_ascii() && ch[j - 1].is_alphabetic() {
                j -= 1;
            }
            return Some(at(j));
        }
        i += 1;
    }
    None
}

/// 按顶层分号切出来的非空语句数。classify 的 other_statements 是"第一个分号之后
/// 还有几个分号"，用它会把 `SELECT 1; DROP TABLE x` 判成一条——所以自己数。
fn statement_count(toks: &[Tok]) -> usize {
    let mut n = 0usize;
    let mut dirty = false;
    for t in toks {
        match t {
            Tok::Other(';') => {
                if dirty {
                    n += 1;
                    dirty = false;
                }
            }
            Tok::Word(_) | Tok::Quoted(_) | Tok::Num(_) => dirty = true,
            _ => {}
        }
    }
    if dirty {
        n += 1;
    }
    n.max(1)
}

/// 把模型回复收成一条可直接进编辑器的 SQL
pub fn extract_sql(raw: &str) -> Result<String, String> {
    let body = strip_fences(raw);
    // 只折 ASCII 大小写：to_lowercase 会改变字节长度，切不起 body
    let lower = body.to_ascii_lowercase();
    let start = HEADS
        .iter()
        .filter_map(|kw| word_start_at(&lower, kw))
        .min()
        .ok_or_else(|| "模型没给出 SQL，只回了一段说明".to_string())?;
    let mut sql = body[start..].trim();
    if let Some(cut) = prose_cut_start(sql) {
        sql = sql[..cut].trim_end();
    }
    if sql.is_empty() {
        return Err("模型给出的 SQL 是空的".into());
    }
    let toks = tokenize(sql);
    let n = statement_count(&toks);
    if n > 1 {
        return Err(format!("一次只要一条语句，模型给了 {n} 条"));
    }
    let content = toks
        .iter()
        .filter(|t| matches!(t, Tok::Word(_) | Tok::Quoted(_) | Tok::Num(_)))
        .count();
    if content < 2 {
        return Err(format!("只写了 `{sql}`，不像一条完整 SQL"));
    }
    let sql = sql.trim_end_matches(';').trim_end().to_string();
    let c = sql_classify::classify(&sql);
    if c.kind == sql_classify::StatementKind::Unknown {
        return Err("看不出这是一条完整 SQL（开头不是常见的语句关键字）".into());
    }
    Ok(sql)
}

/// 在已小写的正文里找某个关键字作为独立单词的起始字节位置
fn word_start_at(hay: &str, kw: &str) -> Option<usize> {
    let b = hay.as_bytes();
    let word_char = |c: &u8| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$';
    let mut from = 0;
    while let Some(rel) = hay[from..].find(kw) {
        let at = from + rel;
        let before_ok = at == 0 || !word_char(&b[at - 1]);
        let end = at + kw.len();
        let after_ok = end >= b.len() || !word_char(&b[end]);
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

// ==================== 本机校验 ====================

/// 后面跟着"库里真实存在的表"的关键字。CREATE 不在其中：新建的表当然不在
/// 目录里，拦下来等于不让 AI 写 DDL；DROP/TRUNCATE/ALTER 拦的却是既有的表。
fn takes_table(kw: &str) -> bool {
    matches!(
        kw,
        "from" | "join" | "update" | "into" | "drop" | "truncate" | "alter"
    )
}

/// `DROP TABLE IF EXISTS x` 里挡在表名前面的那些词
const TABLE_NOISE: &[&str] = &[
    "only",
    "if",
    "exists",
    "table",
    "view",
    "cascade",
    "restrict",
    "ignore",
    "concurrently",
    "temporary",
    "temp",
];

/// 跳过别名位上不该出现的前缀词，返回真正的表名起始位置
fn skip_table_noise(toks: &[Tok], mut i: usize) -> usize {
    while let Some(w) = word_at(toks, i) {
        if TABLE_NOISE.iter().any(|n| same(w, n)) {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// SQL 里引用到的表（FROM/JOIN/UPDATE/INSERT INTO/DROP TABLE 后面的那个名字）
fn referenced_tables(toks: &[Tok]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let kw = match &toks[i] {
            Tok::Word(w) => w.to_ascii_lowercase(),
            _ => {
                i += 1;
                continue;
            }
        };
        if takes_table(&kw) {
            let j = skip_table_noise(toks, i + 1);
            if let Some((n, next)) = dotted_name(toks, j) {
                out.push(n);
                i = next;
                continue;
            }
        }
        i += 1;
    }
    out.sort();
    out.dedup();
    out
}

/// 从 i 起读一个可能带 schema/database 前缀的名字，取末段，返回（名字, 结束位置）
fn dotted_name(toks: &[Tok], i: usize) -> Option<(String, usize)> {
    let mut pos = i;
    let mut last = word_at(toks, pos)?.to_string();
    while matches!(toks.get(pos + 1), Some(Tok::Dot)) {
        match word_at(toks, pos + 2) {
            Some(w) => {
                last = w.to_string();
                pos += 2;
            }
            None => break,
        }
    }
    Some((last, pos + 1))
}

/// 模型自己起的名字（CTE、派生表别名）不该拿去和目录比对
fn local_names(toks: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..toks.len() {
        if i > 0 && is_word(&toks[i], "as") && matches!(toks.get(i + 1), Some(Tok::Other('('))) {
            // WITH recent AS (SELECT …)：recent 是这一句里临时起的名字
            if let Some(w) = word_at(toks, i - 1) {
                out.push(w.to_ascii_lowercase());
            }
        }
        if matches!(&toks[i], Tok::Other(')')) {
            // FROM (SELECT …) t / (…) AS t：闭合括号后面那个词是派生表别名
            let next = i + 1;
            let at = if matches!(toks.get(next), Some(t) if is_word(t, "as")) {
                next + 1
            } else {
                next
            };
            if let Some(w) = word_at(toks, at) {
                out.push(w.to_ascii_lowercase());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 别名 → 目录里的表下标
fn alias_map(toks: &[Tok], index: &Index) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let kw = match &toks[i] {
            Tok::Word(w) => w.to_ascii_lowercase(),
            _ => {
                i += 1;
                continue;
            }
        };
        if !takes_table(kw.as_str()) {
            i += 1;
            continue;
        }
        let Some((name, next)) = dotted_name(toks, skip_table_noise(toks, i + 1)) else {
            i += 1;
            continue;
        };
        let Some(idx) = index.find(&name) else {
            i = next;
            continue;
        };
        // 紧跟其后的裸词就是别名（可能中间夹一个 AS）
        let mut j = next;
        if matches!(toks.get(j), Some(t) if is_word(t, "as")) {
            j += 1;
        }
        if let Some(w) = word_at(toks, j) {
            if !AFTER_TABLE.iter().any(|k| same(w, k)) {
                out.push((w.to_ascii_lowercase(), idx));
            }
        }
        i = next;
    }
    out
}

/// 校验这条 SQL 是否只用得了目录里真实存在的表与列。
/// Err 里带上"有哪些可选"，模型看到才有机会自己改对。
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    /// 这条 SQL 命中的目录内表（带 schema 前缀的原样）
    pub tables: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn check_sql(sql: &str, catalog: &[CatalogTable]) -> Result<Checked, String> {
    if catalog.is_empty() {
        return Err("本机没有可用表目录：先选一张表再让 AI 写".into());
    }
    let toks = tokenize(sql);
    let index = Index::new(catalog);
    let local = local_names(&toks);
    let refs = referenced_tables(&toks);
    let mut used: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for r in &refs {
        if local.iter().any(|l| l == &r.to_ascii_lowercase()) {
            continue;
        }
        match index.find(r) {
            Some(i) => used.push(qualified(&catalog[i])),
            None => unknown.push(r.clone()),
        }
    }
    if !unknown.is_empty() {
        let mut avail: Vec<String> = catalog.iter().map(qualified).collect();
        avail.sort();
        return Err(format!(
            "SQL 里的表 {} 不在本次目录里（可用：{}）",
            unknown.join("、"),
            avail.join(", ")
        ));
    }

    // 限定引用 a.b：a 能定位到目录里的表时，b 必须是那张表的列
    let aliases = alias_map(&toks, &index);
    let mut bad: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 2 < toks.len() {
        let (Some(q), Some(col)) = (word_at(&toks, i), word_at(&toks, i + 2)) else {
            i += 1;
            continue;
        };
        if !matches!(toks.get(i + 1), Some(Tok::Dot)) {
            i += 1;
            continue;
        }
        let idx = aliases
            .iter()
            .find(|(a, _)| a == &q.to_ascii_lowercase())
            .map(|(_, t)| *t)
            .or_else(|| {
                index
                    .find(q)
                    .filter(|t| refs.iter().any(|r| same(r, &catalog[*t].table)))
            });
        if let Some(t) = idx {
            let cols = &catalog[t].columns;
            if !cols.is_empty() && !cols.iter().any(|c| same(c, col)) {
                bad.push(format!("{}.{}", qualified(&catalog[t]), col));
            }
        }
        i += 3;
    }
    if !bad.is_empty() {
        bad.sort();
        bad.dedup();
        return Err(format!(
            "SQL 用到了目录里不存在的列：{}。这些表的真实列是：{}",
            bad.join("、"),
            catalog
                .iter()
                .map(|t| format!("{} = [{}]", qualified(t), t.columns.join(", ")))
                .collect::<Vec<_>>()
                .join("；")
        ));
    }

    let mut warnings: Vec<String> = Vec::new();
    if used.is_empty() {
        warnings.push("这条 SQL 没有引用目录里的任何表".into());
    }
    let c = sql_classify::classify(sql);
    if c.safety == sql_classify::SafetyClass::Write {
        warnings.push("这是写操作，执行时仍需通过审批门禁".into());
    }
    used.sort();
    used.dedup();
    Ok(Checked {
        tables: used,
        warnings,
    })
}

// ==================== 生成回路 ====================

fn dialect_of(catalog: &[CatalogTable]) -> String {
    let mut d: Vec<&str> = catalog.iter().map(|t| t.database_type.as_str()).collect();
    d.sort();
    d.dedup();
    match d.as_slice() {
        [] => "标准".to_string(),
        [one] => (*one).to_string(),
        many => many.join("/").to_string(),
    }
}

/// 生成：问一次 → 本机校验 → 不通过就把错误原文回喂，最多 repairs 次。
/// prior 是上一稿，追问式改稿时带上；改出来的稿子照样过本机校验。
pub async fn generate(
    model: &dyn Model,
    question: &str,
    catalog: &[CatalogTable],
    repairs: u8,
    prior: Option<&PriorDraft>,
) -> Result<SqlDraft, String> {
    if question.trim().is_empty() {
        return Err("先描述你想查什么".into());
    }
    // 空白的上一稿只会往提示词里塞噪音，当作没带
    let prior = prior.filter(|p| !p.sql.trim().is_empty());
    if catalog.is_empty() {
        return Err("先选至少一张表，模型没有列清单就只能编字段".into());
    }
    let mut kept = catalog;
    let mut trim: Option<String> = None;
    if catalog.len() > MAX_TABLES {
        kept = &catalog[..MAX_TABLES];
        trim = Some(format!(
            "目录里有 {} 张表，只带了前 {} 张进提示词",
            catalog.len(),
            MAX_TABLES
        ));
    }
    let dialect = dialect_of(kept);
    let rounds = repairs.min(MAX_REPAIRS);
    let mut feedback: Option<String> = None;
    let mut last = String::new();
    for round in 0..=rounds {
        let raw = model
            .complete(prompt(question, kept, &dialect, prior, feedback.as_deref()))
            .await?;
        let sql = match extract_sql(&raw) {
            Ok(s) => s,
            Err(e) => {
                last = e;
                feedback = Some(last.clone());
                continue;
            }
        };
        // 校验用整份目录：截断只是为了提示词不臃肿，真实存在的表不该被当成幻觉
        let checked = match check_sql(&sql, catalog) {
            Ok(v) => v,
            Err(e) => {
                last = e;
                feedback = Some(last.clone());
                continue;
            }
        };
        let mut warnings = checked.warnings;
        if let Some(t) = trim {
            warnings.push(t);
        }
        return Ok(SqlDraft {
            sql,
            tables: checked.tables,
            dialect,
            warnings,
            repairs: round,
        });
    }
    Err(format!(
        "重试 {} 次后仍未通过本机校验：{}",
        rounds + 1,
        last
    ))
}

/// 自然语言 → 一条 SQL（已过本机校验）。执行仍由用户自己点，且写操作走审批门禁。
#[tauri::command]
pub async fn ai_sql_generate(
    question: String,
    catalog: Vec<CatalogTable>,
    config: AIConfig,
    max_repairs: Option<u8>,
    prior: Option<PriorDraft>,
) -> Result<SqlDraft, String> {
    let model = HttpModel::new(config)?;
    generate(
        &model,
        &question,
        &catalog,
        max_repairs.unwrap_or(2),
        prior.as_ref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;

    /// `cols` 写 "name TYPE"（如 `amount decimal(12,2)`）就能带上类型，
    /// 只写名字则类型留空——和提示词里的渲染格式互为逆运算。
    fn cat_table(conn: &str, db: &str, schema: &str, name: &str, cols: &[&str]) -> CatalogTable {
        let mut column_types = HashMap::new();
        let mut columns = Vec::new();
        for c in cols {
            match c.split_once(' ') {
                Some((n, ty)) if !ty.trim().is_empty() => {
                    columns.push(n.to_string());
                    column_types.insert(n.to_string(), ty.trim().to_string());
                }
                _ => columns.push(c.to_string()),
            }
        }
        CatalogTable {
            connection_id: conn.into(),
            connection_name: conn.into(),
            database_type: db.into(),
            schema: schema.into(),
            table: name.into(),
            columns,
            column_types,
        }
    }

    fn catalog() -> Vec<CatalogTable> {
        vec![
            cat_table(
                "shop",
                "mysql",
                "",
                "orders",
                &["id", "city", "amount", "status", "created_at"],
            ),
            cat_table(
                "shop",
                "mysql",
                "",
                "order_items",
                &["id", "order_id", "sku", "qty", "price"],
            ),
        ]
    }

    /// 脚本化模型：记下每次收到的提示词，按脚本回答，用完就说"写不出来"
    struct Scripted {
        answers: Vec<String>,
        prompts: std::sync::Mutex<Vec<String>>,
    }

    impl Model for Scripted {
        fn complete(&self, prompt: String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
            self.prompts.lock().unwrap().push(prompt);
            let n = self.prompts.lock().unwrap().len();
            let answer = self
                .answers
                .get(n - 1)
                .cloned()
                .unwrap_or_else(|| "抱歉，这个需求我写不出来。".into());
            Box::pin(async move { Ok(answer) })
        }
    }

    fn scripted(answers: Vec<String>) -> Scripted {
        Scripted { answers, prompts: std::sync::Mutex::new(Vec::new()) }
    }

    // ==================== 提示词 ====================

    #[test]
    fn prompt_feeds_dialect_and_the_real_column_lists() {
        let p = prompt("各城市成交额", &catalog(), "mysql", None, None);
        assert!(p.contains("mysql 方言"), "{p}");
        assert!(
            p.contains("orders(id, city, amount, status, created_at)"),
            "{p}"
        );
        assert!(!p.contains("上一稿"), "{p}");

        let with_fb = prompt("各城市成交额", &catalog(), "mysql", None, Some("表 invoicez 不在本次目录里"));
        assert!(with_fb.contains("上一稿没有通过本机校验"), "{with_fb}");
        assert!(with_fb.contains("invoicez"), "{with_fb}");

        let no_cols = prompt("随便", &[cat_table("crm", "postgresql", "", "customers", &[])], "postgresql", None, None);
        assert!(no_cols.contains("customers（本机没读到列清单）"), "{no_cols}");    }

    /// 追问式改稿：上一稿和它当时的需求都要送进提示词，否则模型只能从零重写
    #[test]
    fn prompt_shows_the_prior_draft_and_its_question() {
        let prior = PriorDraft {
            question: "各城市成交额".into(),
            sql: "SELECT city, SUM(amount) AS total FROM orders GROUP BY city".into(),
        };
        let p = prompt("再按月拆开", &catalog(), "mysql", Some(&prior), None);
        assert!(p.contains("上一稿是这么写的"), "{p}");
        // 这两串只可能来自 prior：新需求里没有，目录里也没有
        assert!(p.contains("当时需求：各城市成交额"), "{p}");
        assert!(p.contains("SUM(amount) AS total"), "{p}");
        assert!(p.contains("请在它基础上"), "{p}");
        assert!(p.contains("需求：再按月拆开"), "{p}");
        // 改稿不能把"只回完整 SQL"这条弄丢，否则模型会回 diff
        assert!(p.contains("不要回 diff"), "{p}");
    }

    /// 目录带类型时提示词要写成 `amount decimal(12,2)`，但列校验的口径不变：
    /// 类型是给模型少犯错的，不是给校验用的（校验只认 columns 里的名字）。
    #[test]
    fn prompt_shows_types_while_validation_still_uses_names() {
        let c = vec![cat_table(
            "shop",
            "mysql",
            "",
            "orders",
            &["id int(11)", "amount decimal(12,2)", "status varchar(20)"],
        )];
        let p = prompt("按金额汇总", &c, "mysql", None, None);
        assert!(p.contains("orders(id int(11), amount decimal(12,2), status varchar(20))"), "{p}");
        assert!(p.contains("列名 类型"), "{p}");
        let out = check_sql(
            "SELECT status, SUM(amount) AS total FROM orders GROUP BY status",
            &c,
        )
        .unwrap();
        assert_eq!(out.tables, vec!["orders".to_string()]);
        // 校验口径没变：带类型的目录仍按裸列名判存在与否，
        // 报错里回给模型的真实列清单也不能被类型污染（否则模型照着抄就写进 SQL 了）
        let e = check_sql("SELECT orders.idz FROM orders", &c).unwrap_err();
        assert!(e.contains("orders.idz"), "{e}");
        assert!(e.contains("id, amount, status"), "{e}");
        assert!(!e.contains("decimal(12,2)"), "校验拒因里不该出现类型：{e}");
    }

    // ==================== 从回复里挖 SQL ====================

    #[test]
    fn extract_sql_peels_fence_prose_and_trailing_semicolon() {
        let raw = "好的，下面是你要的 SQL：\n```sql\nSELECT city, SUM(amount) AS total\nFROM orders\nGROUP BY city;\n```";
        assert_eq!(
            extract_sql(raw).unwrap(),
            "SELECT city, SUM(amount) AS total\nFROM orders\nGROUP BY city"
        );
        // 没有围栏、直接就是 SQL
        assert_eq!(extract_sql("SELECT id FROM orders").unwrap(), "SELECT id FROM orders");
        // 结尾带中文说明：全角句号之前才是 SQL
        assert_eq!(
            extract_sql("SELECT id FROM orders\n\n这样就能拿到全部订单号。").unwrap(),
            "SELECT id FROM orders"
        );
        // 说明也可能以逗号开头
        assert_eq!(
            extract_sql("SELECT id FROM orders，不需要 join").unwrap(),
            "SELECT id FROM orders"
        );
        assert_eq!(extract_sql("SELECT 1").unwrap(), "SELECT 1");
    }

    #[test]
    fn extract_sql_keeps_fullwidth_punctuation_inside_literals_and_comments() {
        // 字符串里的全角标点不是"模型开始讲道理"，砍了就把 SQL 砍坏了
        assert_eq!(
            extract_sql("SELECT id FROM orders WHERE city = '北京。路'").unwrap(),
            "SELECT id FROM orders WHERE city = '北京。路'"
        );
        assert_eq!(
            extract_sql("SELECT id FROM orders -- 备注：先这么写着\n").unwrap(),
            "SELECT id FROM orders -- 备注：先这么写着"
        );
        // 反引号标识符里也有冒号时同样不能当切点
        assert_eq!(
            extract_sql("SELECT `时间：创建` FROM orders").unwrap(),
            "SELECT `时间：创建` FROM orders"
        );
    }

    #[test]
    fn extract_sql_refuses_more_than_one_statement() {
        let e = extract_sql("SELECT id FROM orders; DROP TABLE orders;").unwrap_err();
        assert!(e.contains("一次只要一条语句"), "{e}");
        assert!(e.contains('2'), "{e}");
        // 尾部分号被吃掉之后仍然只剩一条
        assert!(extract_sql("SELECT id FROM orders;").is_ok());
        // 分号后面还有真内容，就是两条
        let e2 = extract_sql("SELECT id FROM orders; SELECT 1").unwrap_err();
        assert!(e2.contains("2 条"), "{e2}");
    }

    #[test]
    fn extract_sql_refuses_replies_without_sql() {
        let e = extract_sql("抱歉，这个需求我实现不了。").unwrap_err();
        assert!(e.contains("没给出 SQL"), "{e}");
        // 光一个关键字不成句
        let e2 = extract_sql("SELECT").unwrap_err();
        assert!(e2.contains("不像一条完整 SQL"), "{e2}");
        assert!(extract_sql("").is_err());
    }

    // ==================== 本机校验 ====================

    #[test]
    fn check_accepts_an_ordinary_group_by() {
        let out = check_sql(
            "SELECT city, SUM(amount) AS total FROM orders WHERE status = 'paid' GROUP BY city ORDER BY total DESC LIMIT 5",
            &catalog(),
        )
        .unwrap();
        assert_eq!(out.tables, vec!["orders".to_string()]);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn check_rejects_tables_that_are_not_in_the_catalog() {
        let e = check_sql("SELECT * FROM invoicez", &catalog()).unwrap_err();
        assert!(e.contains("invoicez"), "{e}");
        assert!(e.contains("orders"), "{e}");
    }

    #[test]
    fn check_rejects_hallucinated_columns_via_alias_and_via_table_name() {
        let e = check_sql("SELECT o.amountz FROM orders o", &catalog()).unwrap_err();
        assert!(e.contains("orders.amountz"), "{e}");
        assert!(e.contains("created_at"), "{e}");

        let e2 = check_sql("SELECT order_items.total FROM order_items", &catalog()).unwrap_err();
        assert!(e2.contains("order_items.total"), "{e2}");

        // 同一列名在另一张表里存在，也不能因此放过
        check_sql("SELECT o.sku FROM order_items o", &catalog()).unwrap();
        let e3 = check_sql("SELECT o.sku FROM orders o", &catalog()).unwrap_err();
        assert!(e3.contains("orders.sku"), "{e3}");
    }

    #[test]
    fn check_ignores_dots_inside_string_literals() {
        let out = check_sql(
            "SELECT id FROM orders WHERE city = 'a.b' AND status <> 'x.y'",
            &catalog(),
        )
        .unwrap();
        assert_eq!(out.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn check_ignores_dots_inside_comments() {
        let out = check_sql(
            "-- ghost.column 只是注释\n/* SELECT * FROM phantom */\nSELECT id FROM orders",
            &catalog(),
        )
        .unwrap();
        assert_eq!(out.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn check_accepts_cte_and_derived_tables() {
        let out = check_sql(
            "WITH recent AS (SELECT city, amount FROM orders WHERE id > 10) \
             SELECT city FROM recent JOIN (SELECT order_id, SUM(qty) AS q FROM order_items GROUP BY order_id) s \
             ON s.order_id = recent.id",
            &catalog(),
        )
        .unwrap();
        assert_eq!(out.tables, vec!["order_items".to_string(), "orders".to_string()]);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn check_catches_ddl_pointing_at_a_made_up_table() {
        // 光看 FROM/JOIN 的话，DROP TABLE ghost 会一路放过
        let e = check_sql("DROP TABLE IF EXISTS ghost", &catalog()).unwrap_err();
        assert!(e.contains("ghost"), "{e}");
        let out = check_sql("DELETE FROM orders WHERE id = 1", &catalog()).unwrap();
        assert_eq!(out.tables, vec!["orders".to_string()]);
        assert!(
            out.warnings.iter().any(|w| w.contains("审批门禁")),
            "{:?}",
            out.warnings
        );
        let alt = check_sql("ALTER TABLE orders ADD COLUMN note VARCHAR(64)", &catalog()).unwrap();
        assert_eq!(alt.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn check_rejects_an_empty_catalog() {
        let e = check_sql("SELECT 1", &[]).unwrap_err();
        assert!(e.contains("没有可用表目录"), "{e}");
    }

    #[test]
    fn check_matches_bare_names_against_schema_qualified_entries() {
        let cat = vec![cat_table(
            "crm",
            "postgresql",
            "public",
            "customers",
            &["id", "city", "level"],
        )];
        let out = check_sql("SELECT c.city FROM customers c", &cat).unwrap();
        assert_eq!(out.tables, vec!["public.customers".to_string()]);
        assert_eq!(out.warnings, Vec::<String>::new());
        assert!(check_sql("SELECT c.cityz FROM public.customers c", &cat)
            .unwrap_err()
            .contains("public.customers.cityz"));
        // 跨库时方言写在提示词里，SQL 里出现库名也认
        let two = vec![
            cat_table("shop", "mysql", "shopdb", "orders", &["id", "city"]),
            cat_table("crm", "postgresql", "", "customers", &["id", "city"]),
        ];
        assert!(check_sql("SELECT o.id FROM shopdb.orders o", &two).is_ok());
    }

    #[test]
    fn check_reads_quoted_identifiers() {
        let out = check_sql("SELECT o.`amount` FROM `orders` o", &catalog()).unwrap();
        assert_eq!(out.tables, vec!["orders".to_string()]);
        assert!(check_sql("SELECT o.`amountz` FROM `orders` o", &catalog())
            .unwrap_err()
            .contains("orders.amountz"));
    }

    #[test]
    fn check_warns_when_no_catalog_table_is_touched() {
        let out = check_sql("SELECT 1 AS one", &catalog()).unwrap();
        assert!(out.tables.is_empty(), "{:?}", out.tables);
        assert!(
            out.warnings.iter().any(|w| w.contains("没有引用目录")),
            "{:?}",
            out.warnings
        );
    }

    // ==================== 生成回路 ====================

    #[tokio::test]
    async fn generate_repairs_itself_from_the_local_verdict() {
        let model = scripted(vec![
            "SELECT city, total FROM invoicez".into(),
            "```sql\nSELECT city, SUM(amount) AS total FROM orders GROUP BY city;\n```".into(),
        ]);
        let out = generate(&model, "各城市成交额", &catalog(), 3, None).await.unwrap();
        assert_eq!(out.repairs, 1);
        assert_eq!(out.sql, "SELECT city, SUM(amount) AS total FROM orders GROUP BY city");
        assert_eq!(out.dialect, "mysql");
        assert_eq!(out.tables, vec!["orders".to_string()]);
        let prompts = model.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].contains("不在本次目录里"), "{}", prompts[1]);
        assert!(prompts[1].contains("invoicez"), "{}", prompts[1]);
    }

    #[tokio::test]
    async fn generate_gives_up_after_the_last_round_and_says_why() {
        let model = scripted(vec!["SELECT * FROM ghost_a".into(), "SELECT * FROM ghost_b".into()]);
        let e = generate(&model, "随便查查", &catalog(), 1, None).await.unwrap_err();
        assert!(e.contains("重试 2 次"), "{e}");
        assert!(e.contains("ghost_b"), "{e}");
        assert_eq!(model.prompts.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn generate_trims_the_prompt_but_still_validates_against_everything() {
        let mut big = catalog();
        for i in 0..25 {
            big.push(cat_table("shop", "mysql", "", &format!("t{i}"), &["id"]));
        }
        let model = scripted(vec!["SELECT id FROM t24".into()]);
        let out = generate(&model, "查 t24", &big, 2, None).await.unwrap();
        assert_eq!(out.sql, "SELECT id FROM t24");
        assert!(
            out.warnings.iter().any(|w| w.contains("只带了前 20 张")),
            "{:?}",
            out.warnings
        );
        // 第 21 张之后的表没进提示词，所以首稿里不该出现它的列清单
        let p = model.prompts.lock().unwrap()[0].clone();
        assert!(!p.contains("t24("), "{p}");
    }

    #[tokio::test]
    async fn generate_refuses_before_bothering_the_model() {
        let model = scripted(vec![]);
        assert!(generate(&model, "  ", &catalog(), 1, None).await.is_err());
        assert!(generate(&model, "查订单", &[], 1, None).await.is_err());
        assert!(model.prompts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn generate_reports_a_model_error_instead_of_looping() {
        struct Down;
        impl Model for Down {
            fn complete(&self, _p: String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
                Box::pin(async { Err("AI 服务请求失败: 连接超时".into()) })
            }
        }
        let e = generate(&Down, "查订单", &catalog(), 3, None).await.unwrap_err();
        assert_eq!(e, "AI 服务请求失败: 连接超时");
    }

    fn prior_draft(question: &str, sql: &str) -> PriorDraft {
        PriorDraft { question: question.into(), sql: sql.into() }
    }

    /// 空白的上一稿（用户新开会话、或上一轮什么都没写）不该往提示词里塞噪音
    #[tokio::test]
    async fn generate_ignores_a_blank_prior_draft() {
        let model = scripted(vec!["SELECT id FROM orders".into()]);
        let out = generate(
            &model,
            "查订单",
            &catalog(),
            1,
            Some(&prior_draft("上次的需求串", "   \n  ")),
        )
        .await
        .unwrap();
        assert_eq!(out.sql, "SELECT id FROM orders");
        let ps = model.prompts.lock().unwrap();
        assert_eq!(ps.len(), 1);
        assert!(!ps[0].contains("上一稿是这么写的"), "{}", ps[0]);
        assert!(!ps[0].contains("上次的需求串"), "{}", ps[0]);
    }

    /// 改稿回路：重试轮次也不能把上一稿弄丢，否则第二轮模型是在凭空重写
    #[tokio::test]
    async fn generate_keeps_the_prior_in_every_round() {
        let model = scripted(vec![
            "SELECT city, total FROM invoicez".into(),
            "SELECT city, SUM(amount) AS total FROM orders GROUP BY city".into(),
        ]);
        let p = prior_draft("各城市成交额", "SELECT id, city FROM orders WHERE status = 'paid'");
        let out = generate(&model, "再按月拆开", &catalog(), 3, Some(&p)).await.unwrap();
        assert_eq!(out.repairs, 1);
        assert_eq!(out.sql, "SELECT city, SUM(amount) AS total FROM orders GROUP BY city");
        let ps = model.prompts.lock().unwrap();
        assert_eq!(ps.len(), 2);
        for round in ps.iter() {
            assert!(round.contains("SELECT id, city FROM orders WHERE status = 'paid'"), "{round}");
            assert!(round.contains("再按月拆开"), "{round}");
        }
        // 改稿提示和校验拒因同时在场，模型才知道既要在旧稿上改、又要修哪个错
        assert!(ps[1].contains("上一稿没有通过本机校验"), "{}", ps[1]);
        assert!(ps[1].contains("invoicez"), "{}", ps[1]);
    }

    /// 上一稿是从编辑器/历史里带过来的，里面的列可能是编的：
    /// 模型照抄也不能放行，改出来的稿子过的还是同一套本机校验。
    #[tokio::test]
    async fn generate_still_rejects_a_hallucinated_column_from_the_prior() {
        let bad = "SELECT o.city, o.net_amount FROM orders o";
        let model = scripted(vec![bad.into(), bad.into()]);
        let p = prior_draft("各城市成交额", bad);
        let e = generate(&model, "把金额换成税前的", &catalog(), 1, Some(&p))
            .await
            .unwrap_err();
        assert!(e.contains("重试 2 次"), "{e}");
        assert!(e.contains("orders.net_amount"), "{e}");
        assert_eq!(model.prompts.lock().unwrap().len(), 2);
    }

    #[test]
    fn prior_deserializes_from_the_frontend_shape() {
        // 前端传 {question, sql}；少一个字段的半截对象不能解成"sql 为空"混过去
        let p: PriorDraft =
            serde_json::from_str(r#"{"question":"按月份","sql":"SELECT 1"}"#).unwrap();
        assert_eq!(p.question, "按月份");
        assert_eq!(p.sql, "SELECT 1");
        assert!(serde_json::from_str::<PriorDraft>(r#"{"question":"按月份"}"#).is_err());
    }
}
