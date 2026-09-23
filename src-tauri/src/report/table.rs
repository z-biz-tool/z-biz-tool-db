// 报表引擎：内存表与关系算子
//
// 跨库联邦在这里发生：每张表由自己的连接拉回内存，join 在本地做，
// 不把 SQL 下推到对端（终端工具没有分布式查询引擎，下推只会带来方言
// 与权限两类不可控风险）。代价由 DatasetSpec 的 max_rows 闸门兜住。
//
// 语义对齐 SQL 而非"看起来能用"：
//   - join 键任一侧为 NULL 永不匹配（LEFT 时补 NULL 出行）
//   - 右侧重名输出列自动改名为 col_2 / col_3，避免静默覆盖
//   - GROUP BY 里 NULL 自成一组（与 join 的 ON 语义相反，这是标准行为）
//   - COUNT(col) 只数非 NULL；SUM/AVG/MIN/MAX 忽略 NULL

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::expr::{bucket_of, group_eq, join_eq, join_key_of, ord_key, Expr, Value};

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<HashMap<String, Value>>,
}

/// 列名一律按 ASCII 大小写不敏感匹配（MySQL 的列名就是这个规则）
fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn get_ci(row: &HashMap<String, Value>, name: &str) -> Value {
    if let Some(v) = row.get(name) {
        return v.clone();
    }
    row.iter()
        .find(|(k, _)| same_name(k, name))
        .map(|(_, v)| v.clone())
        .unwrap_or(Value::Null)
}

impl Table {
    /// 以 columns 为准归一化：缺失列补 NULL，多余列丢弃。
    pub fn new(columns: Vec<String>, rows: Vec<HashMap<String, Value>>) -> Self {
        let mut out_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let mut norm = HashMap::with_capacity(columns.len());
            for c in &columns {
                norm.insert(c.clone(), get_ci(&row, c));
            }
            out_rows.push(norm);
        }
        Table {
            columns,
            rows: out_rows,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn has_column(&self, name: &str) -> bool {
        self.resolve_column(name).is_some()
    }

    /// 列名不区分大小写地解析为表中的规范写法
    pub fn resolve_column(&self, name: &str) -> Option<&String> {
        self.columns.iter().find(|c| same_name(c, name))
    }

    pub fn rename_column(&mut self, from: &str, to: &str) -> Result<(), String> {
        let src = self
            .resolve_column(from)
            .cloned()
            .ok_or_else(|| format!("列 {} 不存在，无法重命名", from))?;
        if self.has_column(to) {
            return Err(format!("列 {} 已存在，{} 无法重命名为它", to, from));
        }
        for c in self.columns.iter_mut() {
            if *c == src {
                *c = to.to_string();
            }
        }
        for row in self.rows.iter_mut() {
            if let Some(v) = row.remove(&src) {
                row.insert(to.to_string(), v);
            }
        }
        Ok(())
    }

    pub fn project(&self, cols: &[String]) -> Result<Table, String> {
        let mut picked = Vec::with_capacity(cols.len());
        for c in cols {
            let real = self
                .resolve_column(c)
                .ok_or_else(|| format!("投影失败：列 {} 不存在", c))?
                .clone();
            if picked.iter().any(|p: &String| same_name(p, &real)) {
                return Err(format!("投影列 {} 重复", c));
            }
            picked.push(real);
        }
        let rows = self
            .rows
            .iter()
            .map(|row| {
                let mut m = HashMap::with_capacity(picked.len());
                for c in &picked {
                    m.insert(c.clone(), get_ci(row, c));
                }
                m
            })
            .collect();
        Ok(Table {
            columns: picked,
            rows,
        })
    }

    /// WHERE 语义：只有求值为 true 才保留（NULL 与 false 都过滤掉）
    pub fn filter(&self, pred: &Expr) -> Result<Table, String> {
        let mut rows = Vec::new();
        for row in &self.rows {
            if pred.eval(row)?.truthy().unwrap_or(false) {
                rows.push(row.clone());
            }
        }
        Ok(Table {
            columns: self.columns.clone(),
            rows,
        })
    }

    pub fn add_computed(&self, name: &str, expr: &Expr) -> Result<Table, String> {
        if self.has_column(name) {
            return Err(format!("计算列 {} 与已有列冲突", name));
        }
        let mut columns = self.columns.clone();
        columns.push(name.to_string());
        let rows = self
            .rows
            .iter()
            .map(|row| {
                let mut m = row.clone();
                m.insert(name.to_string(), expr.eval(row)?);
                Ok::<_, String>(m)
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Table { columns, rows })
    }

    pub fn limit(&self, n: usize) -> Table {
        if self.rows.len() <= n {
            return self.clone();
        }
        Table {
            columns: self.columns.clone(),
            rows: self.rows[..n].to_vec(),
        }
    }

    /// 取某列全部值（用于调试与 AI 上下文回显）
    pub fn column_values(&self, name: &str) -> Vec<Value> {
        match self.resolve_column(name) {
            Some(c) => self.rows.iter().map(|r| get_ci(r, c)).collect(),
            None => Vec::new(),
        }
    }
}

// ==================== Join ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinType {
    Inner,
    Left,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinKey {
    pub left: String,
    pub right: String,
}

impl JoinKey {
    pub fn new(left: &str, right: &str) -> Self {
        JoinKey {
            left: left.to_string(),
            right: right.to_string(),
        }
    }
}

fn key_values(row: &HashMap<String, Value>, cols: &[String]) -> Vec<Value> {
    cols.iter().map(|c| get_ci(row, c)).collect()
}

fn key_buckets(vals: &[Value]) -> String {
    let mut s = String::new();
    for v in vals {
        s.push_str(&bucket_of(v));
        s.push('\u{1}');
    }
    s
}

/// 复合连接键的桶：走规范化键（[`join_key_of`]），所以 MySQL 的 bigint 1001
/// 能落进 SQLite 文本键 "1001" 所在的桶。任一侧含 NULL / NaN 时返回 None——
/// 那一行不可能匹配，既不建索引也不探测。
fn join_key_bucket(vals: &[Value]) -> Option<String> {
    let mut s = String::new();
    for v in vals {
        s.push_str(&join_key_of(v)?);
        s.push('\u{1}');
    }
    Some(s)
}

fn ensure_free_name(columns: &[String], name: &str) -> String {
    let dup = |cand: &str| columns.iter().any(|c| same_name(c, cand));
    if !dup(name) {
        return name.to_string();
    }
    let mut i = 2;
    loop {
        let cand = format!("{}_{}", name, i);
        if !dup(&cand) {
            return cand;
        }
        i += 1;
    }
}

/// join 输出列的唯一规则：右列与已有列重名时依次加 _2 / _3 后缀。
/// 计划期（validate）与执行期共用它，避免列名推断和实际结果对不上。
/// 返回 (完整输出列, 右表原列名→输出列名)
pub fn plan_join_columns(
    left: &[String],
    right: &[String],
) -> (Vec<String>, Vec<(String, String)>) {
    let mut out = left.to_vec();
    let mut alias = Vec::with_capacity(right.len());
    for c in right {
        let a = ensure_free_name(&out, c);
        out.push(a.clone());
        alias.push((c.clone(), a));
    }
    (out, alias)
}

/// 多键 hash join。右表按连接键建桶，左表逐行探测。
///
/// 复合键任一侧含 NULL 就不可能匹配；右表命中多行时按插入顺序扇出。
/// 连接键按规范化值比对：bigint 1001 配得上 varchar "1001"，但 "007" 配不上 7。
pub fn hash_join(
    left: &Table,
    right: &Table,
    keys: &[JoinKey],
    jt: JoinType,
) -> Result<Table, String> {
    if keys.is_empty() {
        return Err("join 至少需要一个连接键".into());
    }
    let mut left_keys = Vec::with_capacity(keys.len());
    let mut right_keys = Vec::with_capacity(keys.len());
    for k in keys {
        left_keys.push(
            left.resolve_column(&k.left)
                .ok_or_else(|| format!("左表没有连接列 {}", k.left))?
                .clone(),
        );
        right_keys.push(
            right
                .resolve_column(&k.right)
                .ok_or_else(|| format!("右表没有连接列 {}", k.right))?
                .clone(),
        );
    }

    let (out_columns, right_alias) = plan_join_columns(&left.columns, &right.columns);

    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, row) in right.rows.iter().enumerate() {
        let vals = key_values(row, &right_keys);
        if let Some(bk) = join_key_bucket(&vals) {
            index.entry(bk).or_default().push(idx);
        }
    }

    let mut rows: Vec<HashMap<String, Value>> = Vec::new();
    for lrow in &left.rows {
        let lvals = key_values(lrow, &left_keys);
        let mut matched: Vec<&HashMap<String, Value>> = Vec::new();
        if let Some(bk) = join_key_bucket(&lvals) {
            if let Some(bucket) = index.get(&bk) {
                for &ridx in bucket {
                    let rrow = &right.rows[ridx];
                    let rvals = key_values(rrow, &right_keys);
                    // 桶键已经等价于判等，这一层精判只挡一件事：
                    // 文本里出现分隔符 \u{1} 时，复合键的拼接会有歧义（["a\u{1}b", ""] 与 ["a", "\u{1}b"] 同串）
                    if lvals
                        .iter()
                        .zip(rvals.iter())
                        .all(|(a, b)| join_eq(a, b))
                    {
                        matched.push(rrow);
                    }
                }
            }
        }
        if matched.is_empty() {
            if jt == JoinType::Left {
                let padded: HashMap<String, Value> = out_columns
                    .iter()
                    .map(|c| {
                        let v = match left.resolve_column(c) {
                            Some(lc) => get_ci(lrow, lc),
                            None => Value::Null,
                        };
                        (c.clone(), v)
                    })
                    .collect();
                rows.push(padded);
            }
            continue;
        }
        for m in matched {
            let mut merged: HashMap<String, Value> = HashMap::with_capacity(out_columns.len());
            for c in &left.columns {
                merged.insert(c.clone(), get_ci(lrow, c));
            }
            for (src, alias) in &right_alias {
                merged.insert(alias.clone(), get_ci(m, src));
            }
            rows.push(merged);
        }
    }

    Ok(Table {
        columns: out_columns,
        rows,
    })
}

// ==================== Aggregate ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggFunc {
    Count,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggFunc {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.trim().to_ascii_uppercase().as_str() {
            "COUNT" => Ok(AggFunc::Count),
            "COUNTD" | "COUNT_DISTINCT" | "COUNTDISTINCT" => Ok(AggFunc::CountDistinct),
            "SUM" => Ok(AggFunc::Sum),
            "AVG" | "MEAN" => Ok(AggFunc::Avg),
            "MIN" => Ok(AggFunc::Min),
            "MAX" => Ok(AggFunc::Max),
            other => Err(format!("不支持的聚合函数 {}", other)),
        }
    }

    /// 无列聚合只允许 COUNT（即 COUNT(*)）
    fn requires_column(self) -> bool {
        !matches!(self, AggFunc::Count)
    }
}

/// JSON 里聚合函数名交给 [`parse`]，所以 "SUM" / "sum" / "countd" 都能落回同一个算子。
impl std::str::FromStr for AggFunc {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AggFunc::parse(s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggSpec {
    pub output: String,
    #[serde(deserialize_with = "super::de_ci")]
    pub func: AggFunc,
    /// None 表示 `COUNT(*)`
    #[serde(default)]
    pub column: Option<String>,
}

impl AggSpec {
    pub fn new(output: &str, func: AggFunc, column: Option<&str>) -> Self {
        AggSpec {
            output: output.to_string(),
            func,
            column: column.map(|s| s.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Acc {
    n: i64,
    non_null: i64,
    /// 真正参与数值求和的个数：SUM/AVG 以它为准，纯文本列得到 NULL 而不是 0
    numeric: i64,
    int_sum: i64,
    float_sum: f64,
    float_mode: bool,
    distinct: Vec<Value>,
    min_of: Option<Value>,
    max_of: Option<Value>,
}

impl Acc {
    fn add_number(&mut self, f: f64) {
        self.numeric += 1;
        if !self.float_mode {
            self.float_mode = true;
            self.float_sum = self.int_sum as f64;
        }
        self.float_sum += f;
    }

    fn push(&mut self, v: &Value) {
        self.non_null += 1;
        match v {
            Value::Int(i) => {
                self.numeric += 1;
                if self.float_mode {
                    self.float_sum += *i as f64;
                } else {
                    match self.int_sum.checked_add(*i) {
                        Some(s) => self.int_sum = s,
                        // i64 溢出：转浮点继续，绝不悄悄回绕成负数
                        None => {
                            self.float_mode = true;
                            self.float_sum = self.int_sum as f64 + *i as f64;
                        }
                    }
                }
            }
            Value::Float(f) => self.add_number(*f),
            Value::Text(s) => {
                if let Ok(f) = s.trim().parse::<f64>() {
                    self.add_number(f);
                }
            }
            Value::Bool(_) | Value::Null => {}
        }
        if !self.distinct.iter().any(|d| group_eq(d, v)) {
            self.distinct.push(v.clone());
        }
        let take_min = match (&self.min_of, v) {
            (None, _) => true,
            (Some(cur), _) => matches!(ord_key(v, cur), Some(k) if k < 0),
        };
        let take_max = match (&self.max_of, v) {
            (None, _) => true,
            (Some(cur), _) => matches!(ord_key(v, cur), Some(k) if k > 0),
        };
        if take_min {
            self.min_of = Some(v.clone());
        }
        if take_max {
            self.max_of = Some(v.clone());
        }
    }
}

fn finish(func: AggFunc, acc: &Acc, has_column: bool) -> Value {
    match func {
        // COUNT(*) 数行；COUNT(col) 只数非 NULL
        AggFunc::Count => Value::Int(if has_column {
            acc.non_null
        } else {
            acc.n
        }),
        AggFunc::CountDistinct => Value::Int(acc.distinct.len() as i64),
        AggFunc::Sum => {
            if acc.numeric == 0 {
                Value::Null
            } else if acc.float_mode {
                Value::Float(acc.float_sum)
            } else {
                Value::Int(acc.int_sum)
            }
        }
        AggFunc::Avg => {
            if acc.numeric == 0 {
                Value::Null
            } else {
                let total = if acc.float_mode {
                    acc.float_sum
                } else {
                    acc.int_sum as f64
                };
                Value::Float(total / acc.numeric as f64)
            }
        }
        AggFunc::Min => acc.min_of.clone().unwrap_or(Value::Null),
        AggFunc::Max => acc.max_of.clone().unwrap_or(Value::Null),
    }
}

#[derive(Default)]
struct Group {
    keys: Vec<Value>,
    accs: Vec<Acc>,
}

/// group by + 聚合。group_cols 为空时退化为全表单行聚合。
/// 输出顺序 = 各分组首次出现的顺序，保证报表可复现。
pub fn aggregate(
    table: &Table,
    group_cols: &[String],
    aggs: &[AggSpec],
) -> Result<Table, String> {
    let mut resolved_groups: Vec<String> = Vec::with_capacity(group_cols.len());
    for g in group_cols {
        resolved_groups.push(
            table
                .resolve_column(g)
                .ok_or_else(|| format!("分组列 {} 不存在", g))?
                .clone(),
        );
    }
    for spec in aggs {
        if spec.column.is_none() && spec.func.requires_column() {
            return Err(format!("{:?} 必须指定聚合列", spec.func));
        }
        if let Some(c) = &spec.column {
            if !table.has_column(c) {
                return Err(format!("聚合列 {} 不存在", c));
            }
        }
        if aggs.iter().filter(|o| o.output == spec.output).count() > 1 {
            return Err(format!("聚合输出列 {} 重复", spec.output));
        }
    }

    let out_columns: Vec<String> = resolved_groups
        .iter()
        .cloned()
        .chain(aggs.iter().map(|a| a.output.clone()))
        .collect();

    let mut groups: Vec<Group> = Vec::new();
    let mut buckets: HashMap<String, Vec<usize>> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();

    for row in &table.rows {
        let kv: Vec<Value> = resolved_groups.iter().map(|c| get_ci(row, c)).collect();
        let bk = key_buckets(&kv);
        let hit = buckets.get(&bk).and_then(|list| {
            list.iter().rev().find(|&&gi| {
                groups[gi]
                    .keys
                    .iter()
                    .zip(kv.iter())
                    .all(|(a, b)| group_eq(a, b))
            })
        }).copied();
        let slot = match hit {
            Some(gi) => gi,
            None => {
                groups.push(Group {
                    keys: kv,
                    accs: aggs.iter().map(|_| Acc::default()).collect(),
                });
                let gi = groups.len() - 1;
                buckets.entry(bk).or_default().push(gi);
                order.push(gi);
                gi
            }
        };

        let g = &mut groups[slot];
        for (ai, spec) in aggs.iter().enumerate() {
            g.accs[ai].n += 1;
            if let Some(c) = &spec.column {
                let v = get_ci(row, c);
                if !v.is_null() {
                    g.accs[ai].push(&v);
                }
            }
        }
    }

    // 全表聚合且零行：SQL 仍返回一行（COUNT=0 / SUM=NULL）
    if groups.is_empty() {
        let mut m: HashMap<String, Value> = HashMap::new();
        for spec in aggs {
            m.insert(
                spec.output.clone(),
                finish(spec.func, &Acc::default(), spec.column.is_some()),
            );
        }
        return Ok(Table {
            columns: out_columns,
            rows: if resolved_groups.is_empty() { vec![m] } else { Vec::new() },
        });
    }

    let rows = order
        .into_iter()
        .map(|gi| {
            let g = &groups[gi];
            let mut m: HashMap<String, Value> = HashMap::with_capacity(out_columns.len());
            for (c, v) in resolved_groups.iter().zip(g.keys.iter()) {
                m.insert(c.clone(), v.clone());
            }
            for (spec, acc) in aggs.iter().zip(g.accs.iter()) {
                m.insert(
                    spec.output.clone(),
                    finish(spec.func, acc, spec.column.is_some()),
                );
            }
            m
        })
        .collect();

    Ok(Table {
        columns: out_columns,
        rows,
    })
}

// ==================== Sort ====================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortSpec {
    pub column: String,
    pub desc: bool,
}

fn to_ord(k: i32) -> Ordering {
    match k {
        k if k < 0 => Ordering::Less,
        k if k > 0 => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

/// 稳定排序；NULL 恒排最后。不可比（如文本对数值）视为相等，交给下一级或原序。
pub fn sort_table(table: &Table, specs: &[SortSpec]) -> Result<Table, String> {
    let mut resolved = Vec::with_capacity(specs.len());
    for s in specs {
        resolved.push((
            table
                .resolve_column(&s.column)
                .ok_or_else(|| format!("排序列 {} 不存在", s.column))?
                .clone(),
            s.desc,
        ));
    }
    let mut rows = table.rows.clone();
    rows.sort_by(|a, b| {
        for (col, desc) in &resolved {
            let (va, vb) = (get_ci(a, col), get_ci(b, col));
            let ord = match (va.is_null(), vb.is_null()) {
                (true, true) => Ordering::Equal,
                (true, _) => return Ordering::Greater,
                (_, true) => return Ordering::Less,
                _ => match ord_key(&va, &vb) {
                    Some(k) => to_ord(k),
                    None => Ordering::Equal,
                },
            };
            if ord != Ordering::Equal {
                return if *desc { ord.reverse() } else { ord };
            }
        }
        Ordering::Equal
    });
    Ok(Table {
        columns: table.columns.clone(),
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::expr::{parse_expr, AllowedColumns};

    fn v(cols: &[&str], rows: &[Vec<(&str, Value)>]) -> Table {
        Table::new(
            cols.iter().map(|c| c.to_string()).collect(),
            rows.iter()
                .map(|r| r.iter().cloned().map(|(k, val)| (k.to_string(), val)).collect())
                .collect(),
        )
    }

    fn iv<'a>(pairs: &'a [(&'a str, i64)]) -> Vec<(&'a str, Value)> {
        pairs.iter().map(|(k, i)| (*k, Value::Int(*i))).collect()
    }

    fn cell(t: &Table, row: usize, col: &str) -> Value {
        t.rows[row].get(col).cloned().unwrap_or(Value::Null)
    }

    fn col_names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn expr(src: &str, cols: &[&str]) -> Expr {
        let allow = AllowedColumns(col_names(cols));
        parse_expr(src, &allow).unwrap_or_else(|e| panic!("解析 {} 失败: {}", src, e))
    }

    // 模拟"跨库"：两张表来自不同连接、不同驱动，列类型宽窄不一
    fn orders() -> Table {
        v(
            &["id", "user_id", "amount", "day"],
            &[
                iv(&[("id", 1), ("user_id", 10), ("amount", 100), ("day", 1)]),
                iv(&[("id", 2), ("user_id", 10), ("amount", 50), ("day", 2)]),
                iv(&[("id", 3), ("user_id", 20), ("amount", 70), ("day", 2)]),
                // user_id 为 NULL：孤儿订单，任何 join 都不应匹配
                vec![
                    ("id", Value::Int(4)),
                    ("user_id", Value::Null),
                    ("amount", Value::Int(999)),
                    ("day", Value::Int(3)),
                ],
            ],
        )
    }

    fn users() -> Table {
        v(
            &["uid", "name", "amount"],
            &[
                vec![
                    ("uid", Value::Int(10)),
                    ("name", Value::Text("ann".into())),
                    ("amount", Value::Int(7)),
                ],
                vec![
                    ("uid", Value::Int(20)),
                    ("name", Value::Text("bob".into())),
                    ("amount", Value::Int(7)),
                ],
                // 右表 NULL 键同样被排除在索引外
                vec![
                    ("uid", Value::Null),
                    ("name", Value::Text("ghost".into())),
                    ("amount", Value::Int(0)),
                ],
            ],
        )
    }

    #[test]
    fn project_filter_limit_compose() {
        let o = orders();
        let f = o.filter(&expr("amount > 60", &["id", "user_id", "amount", "day"])).unwrap();
        assert_eq!(f.len(), 3, "100 / 70 / 999 三行过阈");
        let p = f.project(&col_names(&["amount"])).unwrap();
        assert_eq!(p.columns, col_names(&["amount"]));
        assert_eq!(cell(&p, 0, "amount"), Value::Int(100));
        assert_eq!(o.limit(3).len(), 3);
        assert_eq!(o.limit(99).len(), 4);
        // NULL 参与 WHERE 一律被过滤掉（三值逻辑）
        let n = o.filter(&expr("user_id = 10", &["id", "user_id", "amount", "day"])).unwrap();
        assert_eq!(n.len(), 2);
    }

    #[test]
    fn computed_column_materializes_and_conflicts() {
        let o = orders();
        let c = o
            .add_computed(
                "net",
                &expr("amount * 9 / 10", &["id", "user_id", "amount", "day"]),
            )
            .unwrap();
        assert_eq!(cell(&c, 0, "net"), Value::Int(90));
        assert!(c.columns.contains(&"net".to_string()));
        assert!(o.add_computed("amount", &expr("1", &[])).is_err(), "重名必须报错");
    }

    #[test]
    fn inner_join_fans_out_and_renames_duplicates() {
        let joined = hash_join(&orders(), &users(), &[JoinKey::new("user_id", "uid")], JoinType::Inner)
            .unwrap();
        // amount 两边都有 → 右侧改名 amount_2，左列原样保留
        assert_eq!(
            joined.columns,
            col_names(&["id", "user_id", "amount", "day", "uid", "name", "amount_2"])
        );
        // 订单 1/2 → ann，订单 3 → bob，订单 4 的 user_id 为 NULL 被丢弃
        assert_eq!(joined.len(), 3);
        assert_eq!(cell(&joined, 0, "name"), Value::Text("ann".into()));
        assert_eq!(cell(&joined, 0, "amount"), Value::Int(100), "左侧 amount 不能被覆盖");
        assert_eq!(cell(&joined, 0, "amount_2"), Value::Int(7), "右侧 amount 以 _2 呈现");
        assert_eq!(cell(&joined, 2, "name"), Value::Text("bob".into()));
    }

    #[test]
    fn multi_key_join_requires_every_component_to_match() {
        // 只有 (user_id, day) 同时相等才配对
        let j = hash_join(
            &orders(),
            &users(),
            &[JoinKey::new("user_id", "uid"), JoinKey::new("day", "amount")],
            JoinType::Inner,
        )
        .unwrap();
        // order1: (10,1) vs users(10,7) 不等；order3: (20,2) vs (20,7) 不等 → 全空
        assert!(j.is_empty());

        // 右表补一行让 (20,2) 命中
        let mut u = users();
        u.rows.push(
            [("uid", Value::Int(20)), ("name", Value::Text("bob2".into())), ("amount", Value::Int(2))]
                .into_iter()
                .map(|(k, val)| (k.to_string(), val))
                .collect(),
        );
        let j2 = hash_join(
            &orders(),
            &u,
            &[JoinKey::new("user_id", "uid"), JoinKey::new("day", "amount")],
            JoinType::Inner,
        )
        .unwrap();
        // 复合键任一侧 NULL → 永不匹配；(10,2) 与 (20,2) 各命中
        assert_eq!(j2.len(), 1);
        assert_eq!(cell(&j2, 0, "name"), Value::Text("bob2".into()));
    }

    #[test]
    fn left_join_keeps_unmatched_rows_padded() {
        let j = hash_join(&orders(), &users(), &[JoinKey::new("user_id", "uid")], JoinType::Left)
            .unwrap();
        assert_eq!(j.len(), 4, "LEFT 必须保留孤儿订单");
        let orphan = j
            .rows
            .iter()
            .position(|r| r.get("id") == Some(&Value::Int(4)))
            .unwrap();
        assert_eq!(cell(&j, orphan, "name"), Value::Null);
        assert_eq!(cell(&j, orphan, "uid"), Value::Null);
        assert_eq!(cell(&j, orphan, "amount"), Value::Int(999), "左列不受补 NULL 影响");
    }

    #[test]
    fn join_across_different_numeric_widths() {
        // MySQL 给 INT、SQLite 给 BIGINT、PG 给 NUMERIC：10 与 10.0 必须能配上
        let left = v(&["k", "x"], &[
            vec![("k", Value::Int(10)), ("x", Value::Int(1))],
            vec![("k", Value::Float(20.0)), ("x", Value::Int(2))],
        ]);
        let right = v(&["k2", "y"], &[
            vec![("k2", Value::Float(10.0)), ("y", Value::Text("a".into()))],
            vec![("k2", Value::Int(20)), ("y", Value::Text("b".into()))],
        ]);
        let j = hash_join(&left, &right, &[JoinKey::new("k", "k2")], JoinType::Inner).unwrap();
        assert_eq!(j.len(), 2);
        assert_eq!(cell(&j, 0, "y"), Value::Text("a".into()));
        assert_eq!(cell(&j, 1, "y"), Value::Text("b".into()));

        // 文本 "10" 与数值 10 现在必须配上：跨库时同一根键一边是 bigint、
        // 一边是 TEXT 亲和的 varchar，按旧口径拒绝会让跨库报表静默空表
        let left_txt = v(&["k"], &[vec![("k", Value::Text("10".into()))]]);
        let j2 = hash_join(&left_txt, &right, &[JoinKey::new("k", "k2")], JoinType::Inner).unwrap();
        assert_eq!(j2.len(), 1, "整数写法的文本键要能配上数值键");
        assert_eq!(cell(&j2, 0, "y"), Value::Text("a".into()));
    }

    #[test]
    fn join_key_tolerance_covers_only_exact_integer_writings() {
        let num = |vals: Vec<(Value, &str)>| {
            v(
                &["k", "tag"],
                &vals.into_iter()
                    .map(|(key, tag)| vec![("k", key), ("tag", Value::Text(tag.into()))])
                    .collect::<Vec<_>>(),
            )
        };
        // 左：文本键的各种写法；右：数值键。只有纯整数写法能配上。
        let right = num(vec![
            (Value::Int(7), "int7"),
            (Value::Int(0), "int0"),
            (Value::Float(1.5), "float1.5"),
        ]);

        let cases = [
            ("7", 1, "bigint 7 要配得上 varchar 7"),
            ("07", 0, "前导零是真实键，不能当成 7"),
            ("0", 1, "文本 0 配 Int(0)"),
            ("-0", 1, "负零与 0 同值"),
            ("7.0", 0, "带小数点的文本不算整数写法"),
            ("1.5", 0, "小数不做跨族容忍"),
            ("", 0, "空串不是 0"),
            (" 7", 0, "带空格的文本就是另一个键"),
            ("+7", 0, "正号写法不认"),
            ("seven", 0, "纯文本键不与数值配"),
        ];
        for (txt, want, why) in cases {
            let left = v(&["k"], &[vec![("k", Value::Text(txt.into()))]]);
            let j = hash_join(&left, &right, &[JoinKey::new("k", "k")], JoinType::Inner).unwrap();
            assert_eq!(j.len(), want, "文本 {:?} 期望 {} 行：{}", txt, want, why);
        }

        // Bool 仍与 1/0 同族（旧口径下 true 落 #1.0，与 Int(1) 同桶）
        let bt = v(&["k"], &[vec![("k", Value::Bool(true))]]);
        let bn = v(&["k"], &[vec![("k", Value::Int(1))]]);
        let j = hash_join(&bt, &bn, &[JoinKey::new("k", "k")], JoinType::Inner).unwrap();
        assert_eq!(j.len(), 1, "Bool true 仍要配得上 Int 1");
        let btxt = v(&["k"], &[vec![("k", Value::Text("1".into()))]]);
        let j = hash_join(&bt, &btxt, &[JoinKey::new("k", "k")], JoinType::Inner).unwrap();
        assert_eq!(j.len(), 1, "Bool true 也配得上文本 1");

        // NaN / ±inf 永不匹配，与 NULL 同义：SQL 里 NaN = NaN 不为真
        for bad in [Value::Float(f64::NAN), Value::Float(f64::INFINITY)] {
            let l = v(&["k"], &[vec![("k", bad.clone())]]);
            let r = v(&["k"], &[vec![("k", bad.clone())]]);
            let j = hash_join(&l, &r, &[JoinKey::new("k", "k")], JoinType::Inner).unwrap();
            assert!(j.is_empty(), "{:?} 与自己都不该匹配", bad);
        }

        // 超过 2^53 的两个不同整数不再撞成一桶（旧口径按 f64 投影会错配）
        let big_l = v(&["k"], &[vec![("k", Value::Int(9_007_199_254_740_993))]]);
        let big_r = v(&["k"], &[vec![("k", Value::Int(9_007_199_254_740_992))]]);
        let j = hash_join(&big_l, &big_r, &[JoinKey::new("k", "k")], JoinType::Inner).unwrap();
        assert!(j.is_empty(), "两个不同的大整数不能配");

        // 复合键里只要有一侧是 NULL / NaN，整行不参与
        let mut u = users();
        u.rows.push(
            [("uid", Value::Null), ("name", Value::Text("ghost".into())), ("amount", Value::Int(1))]
                .into_iter()
                .map(|(k, val)| (k.to_string(), val))
                .collect(),
        );
        let j = hash_join(
            &orders(),
            &u,
            &[JoinKey::new("user_id", "uid"), JoinKey::new("day", "amount")],
            JoinType::Left,
        )
        .unwrap();
        assert!(
            !j.rows.iter().any(|r| r.get("name") == Some(&Value::Text("ghost".into()))),
            "NULL 键的行不该出现在任何一侧"
        );
    }

    #[test]
    fn join_rejects_unknown_or_missing_keys() {
        assert!(hash_join(&orders(), &users(), &[], JoinType::Inner).is_err());
        assert!(hash_join(
            &orders(),
            &users(),
            &[JoinKey::new("nope", "uid")],
            JoinType::Inner
        )
        .is_err());
    }

    #[test]
    fn group_by_null_forms_its_own_group() {
        let grouped = aggregate(
            &orders(),
            &col_names(&["user_id"]),
            &[AggSpec::new("n", AggFunc::Count, None)],
        )
        .unwrap();
        // 10 / 20 / NULL 各自成组（GROUP BY 与 ON 的 NULL 语义相反）
        assert_eq!(grouped.len(), 3);
        let null_row = grouped
            .rows
            .iter()
            .position(|r| r.get("user_id") == Some(&Value::Null))
            .expect("NULL 应自成一组");
        assert_eq!(cell(&grouped, null_row, "n"), Value::Int(1));
        let ann = grouped
            .rows
            .iter()
            .position(|r| r.get("user_id") == Some(&Value::Int(10)))
            .unwrap();
        assert_eq!(cell(&grouped, ann, "n"), Value::Int(2));
    }

    #[test]
    fn count_star_differs_from_count_column() {
        // 有一行 amount 为 NULL：COUNT(*) = 3，COUNT(amount) = 2
        let t = v(&["g", "amount"], &[
            vec![("g", Value::Int(1)), ("amount", Value::Int(5))],
            vec![("g", Value::Int(1)), ("amount", Value::Null)],
            vec![("g", Value::Int(1)), ("amount", Value::Int(7))],
        ]);
        let a = aggregate(
            &t,
            &col_names(&["g"]),
            &[
                AggSpec::new("all_rows", AggFunc::Count, None),
                AggSpec::new("with_amount", AggFunc::Count, Some("amount")),
                AggSpec::new("total", AggFunc::Sum, Some("amount")),
                AggSpec::new("avg", AggFunc::Avg, Some("amount")),
                AggSpec::new("mx", AggFunc::Max, Some("amount")),
                AggSpec::new("mn", AggFunc::Min, Some("amount")),
            ],
        )
        .unwrap();
        assert_eq!(cell(&a, 0, "all_rows"), Value::Int(3));
        assert_eq!(cell(&a, 0, "with_amount"), Value::Int(2));
        assert_eq!(cell(&a, 0, "total"), Value::Int(12));
        assert_eq!(cell(&a, 0, "avg"), Value::Float(6.0), "AVG 按非 NULL 个数除");
        assert_eq!(cell(&a, 0, "mx"), Value::Int(7));
        assert_eq!(cell(&a, 0, "mn"), Value::Int(5));
    }

    #[test]
    fn count_distinct_and_min_max_over_text() {
        let t = v(&["city"], &[
            vec![("city", Value::Text("hz".into()))],
            vec![("city", Value::Text("hz".into()))],
            vec![("city", Value::Text("nb".into()))],
            vec![("city", Value::Null)],
        ]);
        let a = aggregate(
            &t,
            &[],
            &[
                AggSpec::new("cities", AggFunc::CountDistinct, Some("city")),
                AggSpec::new("lo", AggFunc::Min, Some("city")),
                AggSpec::new("hi", AggFunc::Max, Some("city")),
                AggSpec::new("s", AggFunc::Sum, Some("city")),
            ],
        )
        .unwrap();
        assert_eq!(cell(&a, 0, "cities"), Value::Int(2), "NULL 不计入 DISTINCT");
        assert_eq!(cell(&a, 0, "lo"), Value::Text("hz".into()));
        assert_eq!(cell(&a, 0, "hi"), Value::Text("nb".into()));
        assert_eq!(cell(&a, 0, "s"), Value::Null, "文本求和为 NULL");
    }

    #[test]
    fn sum_promotes_instead_of_wrapping() {
        // 整数溢出必须转浮点，不能回绕成负数
        let t = v(&["n"], &[
            vec![("n", Value::Int(i64::MAX))],
            vec![("n", Value::Int(1))],
        ]);
        let a = aggregate(&t, &[], &[AggSpec::new("s", AggFunc::Sum, Some("n"))]).unwrap();
        match cell(&a, 0, "s") {
            Value::Float(f) => assert_eq!(f, i64::MAX as f64 + 1.0),
            other => panic!("应提升为 Float，实际 {:?}", other),
        }
        // 混入 Float 也要提升
        let t2 = v(&["n"], &[
            vec![("n", Value::Int(2))],
            vec![("n", Value::Float(0.5))],
        ]);
        let a2 = aggregate(&t2, &[], &[AggSpec::new("s", AggFunc::Sum, Some("n"))]).unwrap();
        assert_eq!(cell(&a2, 0, "s"), Value::Float(2.5));
        // 纯整数保持整数
        let a3 = aggregate(
            &orders(),
            &[],
            &[AggSpec::new("s", AggFunc::Sum, Some("amount"))],
        )
        .unwrap();
        assert_eq!(cell(&a3, 0, "s"), Value::Int(1219), "100+50+70+999");
    }

    #[test]
    fn empty_input_still_yields_one_row_only_for_grand_total() {
        let empty = Table::new(col_names(&["a"]), Vec::new());
        let grand = aggregate(
            &empty,
            &[],
            &[
                AggSpec::new("n", AggFunc::Count, None),
                AggSpec::new("s", AggFunc::Sum, Some("a")),
            ],
        )
        .unwrap();
        assert_eq!(grand.len(), 1, "COUNT(*) over 空表在 SQL 里返回一行");
        assert_eq!(cell(&grand, 0, "n"), Value::Int(0));
        assert_eq!(cell(&grand, 0, "s"), Value::Null);

        let grouped = aggregate(&empty, &col_names(&["a"]), &[AggSpec::new("n", AggFunc::Count, None)]).unwrap();
        assert!(grouped.is_empty(), "带 GROUP BY 时空表返回零行");
    }

    #[test]
    fn aggregate_validates_specs() {
        let o = orders();
        assert!(aggregate(&o, &col_names(&["ghost"]), &[]).is_err());
        assert!(aggregate(&o, &[], &[AggSpec::new("s", AggFunc::Sum, Some("ghost"))]).is_err());
        assert!(aggregate(&o, &[], &[AggSpec::new("s", AggFunc::Sum, None)]).is_err(),
                "SUM 不给列没有意义");
        assert!(aggregate(
            &o,
            &[],
            &[AggSpec::new("x", AggFunc::Count, None), AggSpec::new("x", AggFunc::Sum, Some("amount"))]
        )
        .is_err(),
        "输出列重名必须报错");
    }

    #[test]
    fn sort_puts_null_last_and_honours_desc() {
        let t = v(&["g", "amount"], &[
            vec![("g", Value::Int(2)), ("amount", Value::Int(50))],
            vec![("g", Value::Int(1)), ("amount", Value::Int(100))],
            vec![("g", Value::Int(1)), ("amount", Value::Int(50))],
            vec![("g", Value::Int(1)), ("amount", Value::Null)],
        ]);
        let s = sort_table(&t, &[SortSpec { column: "g".into(), desc: false }, SortSpec { column: "amount".into(), desc: true }]).unwrap();
        assert_eq!(
            (cell(&s, 0, "g"), cell(&s, 0, "amount")),
            (Value::Int(1), Value::Int(100))
        );
        assert_eq!(
            (cell(&s, 1, "g"), cell(&s, 1, "amount")),
            (Value::Int(1), Value::Int(50))
        );
        assert_eq!(cell(&s, 2, "amount"), Value::Null, "NULL 排最后");
        assert_eq!(cell(&s, 3, "g"), Value::Int(2));
        assert!(sort_table(&t, &[SortSpec { column: "ghost".into(), desc: false }]).is_err());
    }

    #[test]
    fn rename_and_normalisation() {
        let mut o = orders();
        o.rename_column("day", "d").unwrap();
        assert!(o.has_column("D"), "列名查找不区分大小写");
        assert_eq!(cell(&o.limit(1), 0, "d"), Value::Int(1));
        assert!(o.rename_column("ghost", "x").is_err());
        assert!(o.rename_column("id", "amount").is_err());

        // 驱动返回的行宽不一致时按 columns 归一：缺的补 NULL，多的丢掉
        let ragged = Table::new(
            col_names(&["a", "b"]),
            vec![
                [("a", Value::Int(1)), ("c", Value::Int(9))].into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            ],
        );
        assert_eq!(ragged.columns, col_names(&["a", "b"]));
        assert_eq!(cell(&ragged, 0, "a"), Value::Int(1));
        assert_eq!(cell(&ragged, 0, "b"), Value::Null);
        assert!(!ragged.rows[0].contains_key("c"));
    }

    #[test]
    fn sum_and_avg_count_only_numeric_values() {
        // 修复点：纯文本列的 SUM 曾返回 Int(0)，把"没有可加的值"伪装成"和为 0"
        let t = v(&["mix"], &[
            vec![("mix", Value::Text("hz".into()))],
            vec![("mix", Value::Text("7".into()))],
            vec![("mix", Value::Bool(true))],
            vec![("mix", Value::Null)],
        ]);
        let a = aggregate(
            &t,
            &[],
            &[
                AggSpec::new("s", AggFunc::Sum, Some("mix")),
                AggSpec::new("avg", AggFunc::Avg, Some("mix")),
                AggSpec::new("cnt", AggFunc::Count, Some("mix")),
                AggSpec::new("all", AggFunc::Count, None),
            ],
        )
        .unwrap();
        assert_eq!(cell(&a, 0, "s"), Value::Float(7.0), "只有 '7' 可数值化");
        assert_eq!(cell(&a, 0, "avg"), Value::Float(7.0), "AVG 除以可数值化个数，不是非 NULL 个数");
        assert_eq!(cell(&a, 0, "cnt"), Value::Int(3), "COUNT(col) 数非 NULL：含 Bool 与文本");
        assert_eq!(cell(&a, 0, "all"), Value::Int(4));

        // 全文本 → SUM 必须是 NULL
        let t2 = v(&["c"], &[vec![("c", Value::Text("x".into()))]]);
        let a2 = aggregate(
            &t2,
            &[],
            &[AggSpec::new("s", AggFunc::Sum, Some("c")), AggSpec::new("avg", AggFunc::Avg, Some("c"))],
        )
        .unwrap();
        assert_eq!(cell(&a2, 0, "s"), Value::Null);
        assert_eq!(cell(&a2, 0, "avg"), Value::Null);
    }
}
