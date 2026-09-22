// 报表引擎：声明式数据集（跨库联邦的最小单元）
//
// 这是"AI 加持"的落点：AI 不写 SQL，只产出一份 DatasetSpec JSON。
// 引擎把 spec 变成 N 条结构受控的单表 SELECT（每源一条），拉回内存后
// 由 table.rs 做 join / filter / aggregate。因此：
//   1. AI 能影响的文本只有标识符（assert_ident + 按方言加引号）和受限表达式
//      （expr.rs 解析器，未知列 / 非白名单函数直接拒绝）
//   2. 一条 spec 可以同时挂 MySQL 的订单表和 SQLite 的汇率表，join 在本地做
//   3. 幻觉在 validate_dataset 阶段就报错，不会变成打到生产库的奇怪查询
//   4. 每个源被 max_rows 硬闸住，跨库全表拉取不会静默吃满桌面进程内存

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use super::expr::{parse_expr, AllowedColumns, Value};
use super::table::{
    aggregate, hash_join, plan_join_columns, sort_table, AggFunc, AggSpec, JoinKey, JoinType,
    SortSpec, Table,
};

/// 单个源默认最多拉回的行数
pub const DEFAULT_MAX_ROWS: usize = 50_000;
/// 允许 spec 自行抬高的上限，防止把桌面进程 OOM 当成"可配置"
pub const HARD_MAX_ROWS: usize = 500_000;

pub const DIALECTS: &[&str] = &["mysql", "postgresql", "sqlite"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    /// spec 内的唯一别名，join 与字段引用都以它定位
    pub alias: String,
    pub connection_id: String,
    /// mysql | postgresql | sqlite。可以省略：报表链路会按连接实际方言回填，
    /// 直接调用 report_dataset_* 时缺省会由 source_sql 报"方言不受支持"。
    #[serde(default)]
    pub database_type: String,
    #[serde(default)]
    pub schema: String,
    pub table: String,
    /// 为空表示 SELECT *；validate 时需要 schema 缓存提供列清单
    #[serde(default)]
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinPair {
    /// 左 = 到当前为止累计的输出列名（重命名后的写法）
    pub left: String,
    /// 右 = 本次接入表的原始列名
    pub right: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum JoinKind {
    #[default]
    Inner,
    Left,
}

/// 模型会写 "inner" / "LEFT OUTER" / "Join"，全部按同一种连接收下来。
impl std::str::FromStr for JoinKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().replace(' ', "").as_str() {
            "INNER" | "JOIN" | "INNERJOIN" => Ok(JoinKind::Inner),
            "LEFT" | "LEFTJOIN" | "LEFTOUTER" | "OUTERLEFT" => Ok(JoinKind::Left),
            other => Err(format!("不支持的连接类型 {}（可选 INNER / LEFT）", other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinSpec {
    pub source: String,
    pub on: Vec<JoinPair>,
    #[serde(default, deserialize_with = "super::de_ci")]
    pub kind: JoinKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputedSpec {
    pub name: String,
    pub expr: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortDir {
    pub column: String,
    #[serde(default)]
    pub desc: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetSpec {
    pub id: String,
    pub name: String,
    pub base: String,
    pub sources: Vec<SourceRef>,
    #[serde(default)]
    pub joins: Vec<JoinSpec>,
    /// 逐条 AND 的布尔表达式，作用域是 join 之后的全部列
    #[serde(default)]
    pub filters: Vec<String>,
    /// 聚合前计算列
    #[serde(default)]
    pub computed: Vec<ComputedSpec>,
    #[serde(default)]
    pub group_by: Vec<String>,
    #[serde(default)]
    pub aggregates: Vec<AggSpec>,
    /// 聚合后计算列，可引用分组列与聚合输出列
    #[serde(default)]
    pub post_computed: Vec<ComputedSpec>,
    /// 最终投影；空表示全列
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub sort: Vec<SortDir>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub max_rows: Option<usize>,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn pick(cols: &[String], name: &str) -> Option<String> {
    cols.iter().find(|c| same(c, name)).cloned()
}

impl DatasetSpec {
    pub fn new(id: &str, name: &str, base: &str) -> Self {
        DatasetSpec {
            id: id.to_string(),
            name: name.to_string(),
            base: base.to_string(),
            sources: Vec::new(),
            joins: Vec::new(),
            filters: Vec::new(),
            computed: Vec::new(),
            group_by: Vec::new(),
            aggregates: Vec::new(),
            post_computed: Vec::new(),
            fields: Vec::new(),
            sort: Vec::new(),
            limit: None,
            max_rows: None,
        }
    }

    pub fn row_cap(&self) -> usize {
        self.max_rows.unwrap_or(DEFAULT_MAX_ROWS).clamp(1, HARD_MAX_ROWS)
    }

    pub fn source(&self, alias: &str) -> Option<&SourceRef> {
        self.sources.iter().find(|s| s.alias == alias)
    }

    fn join_type(kind: JoinKind) -> JoinType {
        match kind {
            JoinKind::Inner => JoinType::Inner,
            JoinKind::Left => JoinType::Left,
        }
    }
}

// ==================== 标识符加固 ====================

/// 标识符白名单：字母 / 数字 / 下划线 / $（`is_alphanumeric` 含 CJK，
/// 所以中文库表名可用），长度 1..=64。引号、分号、空白、`-`、`/` 全部拒绝，
/// 因此注释与拼接逃逸在语法层面就不可能成立。
pub fn assert_ident(raw: &str, what: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(format!("{} 标识符为空", what));
    }
    if s.chars().count() > 64 {
        return Err(format!("{} 标识符过长: {}", what, s));
    }
    let first = s.chars().next().unwrap();
    if !(first == '_' || first.is_alphabetic()) {
        return Err(format!("{} 标识符必须以字母或下划线开头: {}", what, s));
    }
    for c in s.chars() {
        if !(c == '_' || c == '$' || c.is_alphanumeric()) {
            return Err(format!("{} 标识符含非法字符 '{}': {}", what, c, s));
        }
    }
    Ok(s.to_string())
}

/// 按方言加引号。先过白名单，再对内部引号双写——纵深防御。
pub fn quote_ident(dialect: &str, raw: &str, what: &str) -> Result<String, String> {
    let s = assert_ident(raw, what)?;
    Ok(match dialect {
        "mysql" => format!("`{}`", s.replace('`', "``")),
        "postgresql" | "sqlite" => format!("\"{}\"", s.replace('"', "\"\"")),
        other => return Err(format!("不支持的方言 {}", other)),
    })
}

/// 为某个源生成结构受控的单表 SELECT。整条报表链路里只有这里会打到数据库，
/// 且形状恒定：`SELECT <cols> FROM <tbl> LIMIT <cap+1>`。
/// 多取的 1 行只用来判定"是否被截断"，不会进入结果。
pub fn source_sql(src: &SourceRef, cap: usize) -> Result<String, String> {
    if !DIALECTS.contains(&src.database_type.as_str()) {
        return Err(format!("源 {} 方言不受支持: {}", src.alias, src.database_type));
    }
    let mut tbl = quote_ident(&src.database_type, &src.table, "表")?;
    if !src.schema.trim().is_empty() {
        tbl = format!(
            "{}.{}",
            quote_ident(&src.database_type, &src.schema, "库")?,
            tbl
        );
    }
    let cols = if src.columns.is_empty() {
        "*".to_string()
    } else {
        let mut out = Vec::with_capacity(src.columns.len());
        for c in &src.columns {
            out.push(quote_ident(&src.database_type, c, "列")?);
        }
        out.join(", ")
    };
    Ok(format!("SELECT {} FROM {} LIMIT {}", cols, tbl, cap + 1))
}

// ==================== 数据源接缝 ====================

/// 与驱动层解耦的取数接缝：真实实现按 connection_id 找连接池执行 source_sql；
/// 测试实现直接吐内存表。
///
/// `Send + Sync` 是硬要求：跨库联邦会为每个源各起一次 await，调用方又是 Tauri 的
/// 多线程 IPC future，没有这两条就编不过（不是风格问题）。
pub trait RowSource: Send + Sync {
    fn fetch<'a>(
        &'a self,
        src: &'a SourceRef,
        cap: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Table, String>> + Send + 'a>>;
}

// ==================== 校验 ====================

/// alias → 该源可用的列（来自 schema 缓存）。缺项视为无法校验。
pub type SchemaCache = HashMap<String, Vec<String>>;

fn allowed(cols: &[String]) -> AllowedColumns {
    AllowedColumns(cols.to_vec())
}

/// 计划产物：人类可读的算子链 + 数据集最终输出的列。
/// 两者必须同源：曾经出过"计划里推断的列名和实际结果对不上"的问题，
/// 所以列的累积直接复用校验过程，不再另写一份推导。
#[derive(Debug, Clone, Default)]
pub struct DatasetPlan {
    pub steps: Vec<String>,
    pub columns: Vec<String>,
}

/// 静态校验 + 生成计划文本。任何一处不成立都返回 Err，绝不"先跑跑看"。
/// 只要步骤文本的调用方用它；需要输出列清单的用 plan_dataset。
pub fn validate_dataset(spec: &DatasetSpec, schemas: &SchemaCache) -> Result<Vec<String>, String> {
    plan_dataset(spec, schemas).map(|p| p.steps)
}

pub fn plan_dataset(spec: &DatasetSpec, schemas: &SchemaCache) -> Result<DatasetPlan, String> {
    if spec.id.trim().is_empty() {
        return Err("数据集缺少 id".into());
    }
    let mut steps: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for s in &spec.sources {
        assert_ident(&s.alias, "源别名")?;
        if seen.contains(&s.alias) {
            return Err(format!("源别名 {} 重复", s.alias));
        }
        seen.push(s.alias.clone());
        if s.connection_id.trim().is_empty() {
            return Err(format!("源 {} 没有 connection_id", s.alias));
        }
        // 标识符提前按该方言验一遍，别等到拼 SQL 才报错
        source_sql(s, spec.row_cap())?;
    }
    if spec.source(&spec.base).is_none() {
        return Err(format!("基表 {} 不在 sources 里", spec.base));
    }
    for j in &spec.joins {
        if spec.source(&j.source).is_none() {
            return Err(format!("join 目标 {} 不在 sources 里", j.source));
        }
        if j.on.is_empty() {
            return Err(format!("join {} 缺少 ON 条件", j.source));
        }
    }
    if let Some(n) = spec.limit {
        if n == 0 {
            return Err("limit 为 0 会返回空结果，请去掉".into());
        }
    }

    let mut cols = source_columns(spec, schemas, &spec.base)?;
    steps.push(format!(
        "FROM {} ({} {})",
        spec.base,
        spec.source(&spec.base).unwrap().table,
        cols.len()
    ));

    for j in &spec.joins {
        let right = source_columns(spec, schemas, &j.source)?;
        let (out, _) = plan_join_columns(&cols, &right);
        for pair in &j.on {
            if pick(&cols, &pair.left).is_none() {
                return Err(format!(
                    "join {} 的左侧列 {} 不在已累计的列里",
                    j.source, pair.left
                ));
            }
            if pick(&right, &pair.right).is_none() {
                return Err(format!("join {} 的右侧列 {} 不存在", j.source, pair.right));
            }
        }
        steps.push(format!(
            "{} JOIN {} ON {}",
            match j.kind {
                JoinKind::Inner => "INNER",
                JoinKind::Left => "LEFT",
            },
            j.source,
            j.on.iter()
                .map(|p| format!("{} = {}", p.left, p.right))
                .collect::<Vec<_>>()
                .join(" AND ")
        ));
        cols = out;
    }

    for f in &spec.filters {
        parse_expr(f, &allowed(&cols)).map_err(|e| format!("过滤条件 [{}]: {}", f, e))?;
        steps.push(format!("WHERE {}", f));
    }
    for c in &spec.computed {
        assert_ident(&c.name, "计算列")?;
        if pick(&cols, &c.name).is_some() {
            return Err(format!("计算列 {} 与已有列重名", c.name));
        }
        parse_expr(&c.expr, &allowed(&cols))
            .map_err(|e| format!("计算列 [{}]: {}", c.name, e))?;
        cols.push(c.name.clone());
        steps.push(format!("COMPUTE {} = {}", c.name, c.expr));
    }

    let aggregating = !spec.group_by.is_empty() || !spec.aggregates.is_empty();
    if aggregating {
        let mut g: Vec<String> = Vec::new();
        for gb in &spec.group_by {
            g.push(pick(&cols, gb).ok_or_else(|| format!("分组列 {} 不存在", gb))?);
        }
        let mut agg_out: Vec<String> = Vec::new();
        for a in &spec.aggregates {
            assert_ident(&a.output, "聚合输出列")?;
            if pick(&g, &a.output).is_some() {
                return Err(format!("聚合输出 {} 与分组列重名", a.output));
            }
            if agg_out.iter().any(|x| same(x, &a.output)) {
                return Err(format!("聚合输出列 {} 重复", a.output));
            }
            agg_out.push(a.output.clone());
            match &a.column {
                Some(col) => {
                    if pick(&cols, col).is_none() {
                        return Err(format!("聚合列 {} 不存在", col));
                    }
                }
                None => {
                    if a.func != AggFunc::Count {
                        return Err(format!("聚合 {} 必须指定列", a.output));
                    }
                }
            }
        }
        cols = g;
        cols.extend(spec.aggregates.iter().map(|a| a.output.clone()));
        steps.push(format!(
            "GROUP BY [{}] AGG [{}]",
            if spec.group_by.is_empty() { "-".to_string() } else { spec.group_by.join(", ") },
            spec.aggregates
                .iter()
                .map(|a| format!("{}={:?}", a.output, a.func))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if !spec.post_computed.is_empty() {
        return Err("没有聚合时 post_computed 无意义，请改用 computed".into());
    }

    for c in &spec.post_computed {
        assert_ident(&c.name, "聚合后计算列")?;
        if pick(&cols, &c.name).is_some() {
            return Err(format!("聚合后计算列 {} 与已有列重名", c.name));
        }
        parse_expr(&c.expr, &allowed(&cols))
            .map_err(|e| format!("聚合后计算列 [{}]: {}", c.name, e))?;
        cols.push(c.name.clone());
        steps.push(format!("POST {} = {}", c.name, c.expr));
    }

    let projected: Option<Vec<String>> = if spec.fields.is_empty() {
        steps.push(format!("SELECT * ({} 列)", cols.len()));
        None
    } else {
        let mut picked: Vec<String> = Vec::with_capacity(spec.fields.len());
        for f in &spec.fields {
            let real = pick(&cols, f).ok_or_else(|| format!("输出字段 {} 不存在", f))?;
            if picked.contains(&real) {
                return Err(format!("输出字段 {} 重复", f));
            }
            picked.push(real);
        }
        steps.push(format!("SELECT {}", picked.join(", ")));
        Some(picked)
    };

    for s in &spec.sort {
        if pick(&cols, &s.column).is_none() {
            return Err(format!("排序列 {} 不存在", s.column));
        }
        steps.push(format!(
            "ORDER BY {} {}",
            s.column,
            if s.desc { "DESC" } else { "ASC" }
        ));
    }
    if let Some(n) = spec.limit {
        steps.push(format!("LIMIT {}", n));
    }
    Ok(DatasetPlan {
        columns: projected.unwrap_or(cols),
        steps,
    })
}

fn source_columns(
    spec: &DatasetSpec,
    schemas: &SchemaCache,
    alias: &str,
) -> Result<Vec<String>, String> {
    let src = spec
        .source(alias)
        .ok_or_else(|| format!("源 {} 不存在", alias))?;
    if !src.columns.is_empty() {
        return Ok(src.columns.clone());
    }
    match schemas.get(alias) {
        Some(c) if !c.is_empty() => Ok(c.clone()),
        _ => Err(format!("源 {} 未声明列清单，也无法从 schema 缓存取到", alias)),
    }
}

// ==================== 执行 ====================

#[derive(Debug, Clone, Default)]
pub struct DatasetResult {
    pub table: Table,
    /// 每个源实际拉回的行数（截断前的真实计数）
    pub source_rows: Vec<(String, usize)>,
    /// 撞上 max_rows 硬闸的源；UI 必须显式提示"结果不完整"
    pub truncated: Vec<String>,
    /// 每条源 SQL，供"查看生成的 SQL"面板回显
    pub generated_sql: Vec<(String, String)>,
}

impl DatasetResult {
    pub fn is_partial(&self) -> bool {
        !self.truncated.is_empty()
    }
}

async fn pull(
    spec: &DatasetSpec,
    source: &dyn RowSource,
    cache: &mut HashMap<String, Table>,
    report: &mut Vec<(String, usize)>,
    truncated: &mut Vec<String>,
    sqls: &mut Vec<(String, String)>,
    alias: &str,
) -> Result<Table, String> {
    let src = spec
        .source(alias)
        .ok_or_else(|| format!("源 {} 不在 sources 里", alias))?
        .clone();
    let cap = spec.row_cap();
    sqls.push((alias.to_string(), source_sql(&src, cap)?));
    let t = source.fetch(&src, cap).await?;
    let n = t.len();
    let kept = if n > cap {
        truncated.push(alias.to_string());
        t.limit(cap)
    } else {
        t
    };
    report.push((alias.to_string(), n));
    cache.insert(alias.to_string(), kept.clone());
    Ok(kept)
}

/// 按 spec 拉数并跑完整算子链。执行顺序固定为
/// join → WHERE → 计算列 → 聚合 → 聚合后计算列 → 排序 → 投影 → LIMIT，
/// 与 SQL 的逻辑次序一致，避免"过滤发生在补 NULL 之前"这类惊喜。
pub async fn execute_dataset(
    spec: &DatasetSpec,
    source: &dyn RowSource,
) -> Result<DatasetResult, String> {
    let mut cache: HashMap<String, Table> = HashMap::new();
    let mut out = DatasetResult::default();

    let mut cur = pull(
        spec,
        source,
        &mut cache,
        &mut out.source_rows,
        &mut out.truncated,
        &mut out.generated_sql,
        &spec.base,
    )
    .await?;

    for j in &spec.joins {
        if !cache.contains_key(&j.source) {
            pull(
                spec,
                source,
                &mut cache,
                &mut out.source_rows,
                &mut out.truncated,
                &mut out.generated_sql,
                &j.source,
            )
            .await?;
        }
        let right = cache
            .get(&j.source)
            .ok_or_else(|| format!("源 {} 取数失败", j.source))?;
        let keys: Vec<JoinKey> = j
            .on
            .iter()
            .map(|p| JoinKey::new(&p.left, &p.right))
            .collect();
        cur = hash_join(&cur, right, &keys, DatasetSpec::join_type(j.kind))?;
    }

    for f in &spec.filters {
        let e = parse_expr(f, &allowed(&cur.columns)).map_err(|err| {
            format!("过滤条件 [{}]: {}", f, err)
        })?;
        cur = cur.filter(&e)?;
    }
    for c in &spec.computed {
        let e = parse_expr(&c.expr, &allowed(&cur.columns))?;
        cur = cur.add_computed(&c.name, &e)?;
    }

    if !spec.group_by.is_empty() || !spec.aggregates.is_empty() {
        let gb: Vec<String> = spec
            .group_by
            .iter()
            .map(|g| pick(&cur.columns, g).ok_or_else(|| format!("分组列 {} 不存在", g)))
            .collect::<Result<_, String>>()?;
        cur = aggregate(&cur, &gb, &spec.aggregates)?;
    }

    for c in &spec.post_computed {
        let e = parse_expr(&c.expr, &allowed(&cur.columns))?;
        cur = cur.add_computed(&c.name, &e)?;
    }

    if !spec.sort.is_empty() {
        let specs: Vec<SortSpec> = spec
            .sort
            .iter()
            .map(|s| SortSpec {
                column: s.column.clone(),
                desc: s.desc,
            })
            .collect();
        cur = sort_table(&cur, &specs)?;
    }
    if !spec.fields.is_empty() {
        cur = cur.project(&spec.fields)?;
    }
    if let Some(n) = spec.limit {
        cur = cur.limit(n);
    }
    out.table = cur;
    Ok(out)
}

// ==================== 与驱动层的类型桥 ====================

/// tagged-cell JSON（`{"__kind":..,"value":..}`）→ 引擎标量。
///
/// `__kind` 的字面值由 lib.rs 的 `kind_to_str` 决定，是小写（`text` / `integer` /
/// `decode_error`）。这里必须按那份契约匹配：拼错一个大小写不会报错，只会让整列
/// 静默变成 NULL——报表算出来"全是空"比直接崩更难查。
///
/// Binary / Unsupported / DecodeError 一律 NULL：报表宁可少一个点，
/// 也不能把解码失败当成 0 或空串混进聚合结果。
pub fn tagged_to_value(cell: &serde_json::Value) -> Value {
    let (kind, val): (String, serde_json::Value) = match cell {
        serde_json::Value::Null => return Value::Null,
        serde_json::Value::Object(m) => match m.get("__kind") {
            Some(k) => (
                k.as_str().unwrap_or("raw").to_ascii_lowercase(),
                m.get("value").cloned().unwrap_or(serde_json::Value::Null),
            ),
            None => ("raw".to_string(), cell.clone()),
        },
        other => ("raw".to_string(), other.clone()),
    };
    let num = |v: &serde_json::Value| -> Option<f64> {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    };
    match kind.as_str() {
        "null" | "binary" | "unsupported" | "decode_error" => Value::Null,
        // T-038：大整数在过 IPC 前会被降级成字符串，所以 integer 也可能是文本。
        // 超出 i64（MySQL BIGINT UNSIGNED）时退到 f64 而不是丢掉这一行。
        "integer" => match &val {
            serde_json::Value::Null => Value::Null,
            _ => val
                .as_i64()
                .or_else(|| val.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                .map(Value::Int)
                .or_else(|| num(&val).map(Value::Float))
                .unwrap_or_else(|| Value::Text(val.to_string().trim_matches('"').to_string())),
        },
        "float" | "decimal" => match num(&val) {
            Some(f) => Value::Float(f),
            None => Value::Null,
        },
        "text" | "uuid" | "json" | "date" | "time" | "datetime" | "timestamp" => match val {
            serde_json::Value::String(s) => Value::Text(s),
            serde_json::Value::Null => Value::Null,
            other => Value::Text(other.to_string()),
        },
        "raw" => match val {
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => n.as_f64().map(Value::Float).unwrap_or(Value::Null),
            },
            serde_json::Value::String(s) => Value::Text(s),
            serde_json::Value::Null => Value::Null,
            other => Value::Text(other.to_string()),
        },
        // 未知标签宁可报错也不要静默 NULL：出现它说明驱动侧加了新 kind 而这里没跟上
        other => Value::Text(format!("<unknown-kind:{}>", other)),
    }
}

/// QueryResult 形状（列名 + tagged 行）→ 引擎表。真实 RowSource 适配器用它。
pub fn table_from_tagged(columns: &[String], rows: &[Vec<serde_json::Value>]) -> Table {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let mut m = HashMap::with_capacity(columns.len());
        for (i, c) in columns.iter().enumerate() {
            m.insert(c.clone(), r.get(i).map(tagged_to_value).unwrap_or(Value::Null));
        }
        out.push(m);
    }
    Table::new(columns.to_vec(), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        tables: HashMap<String, Table>,
    }

    impl Mock {
        fn new(pairs: Vec<(&str, Table)>) -> Mock {
            Mock {
                tables: pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            }
        }
    }

    impl RowSource for Mock {
        fn fetch<'a>(
            &'a self,
            src: &'a SourceRef,
            _cap: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Table, String>> + Send + 'a>> {
            match self.tables.get(&src.alias) {
                Some(t) => {
                    let t = t.clone();
                    Box::pin(async move { Ok(t) })
                }
                None => Box::pin(async move { Err(format!("mock 里没有源 {}", src.alias)) }),
            }
        }
    }

    fn tbl(cols: &[&str], rows: &[Vec<(&str, Value)>]) -> Table {
        Table::new(
            cols.iter().map(|c| c.to_string()).collect(),
            rows.iter()
                .map(|r| r.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
                .collect(),
        )
    }

    fn src(alias: &str, dialect: &str, table: &str, cols: &[&str]) -> SourceRef {
        SourceRef {
            alias: alias.to_string(),
            connection_id: format!("conn-{}", alias),
            database_type: dialect.to_string(),
            schema: String::new(),
            table: table.to_string(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn schemas(pairs: &[(&str, &[&str])]) -> SchemaCache {
        pairs
            .iter()
            .map(|(a, c)| (a.to_string(), c.iter().map(|x| x.to_string()).collect()))
            .collect()
    }

    fn cell(t: &Table, row: usize, col: &str) -> Value {
        t.rows[row].get(col).cloned().unwrap_or(Value::Null)
    }

    /// 订单在 MySQL，汇率在 SQLite——典型的多业务库场景
    fn cross_spec() -> DatasetSpec {
        let mut spec = DatasetSpec::new("ds-1", "每日净额", "orders");
        spec.sources = vec![
            src("orders", "mysql", "t_order", &["id", "day", "amount", "cur"]),
            src("rates", "sqlite", "fx_rate", &["cur", "rate"]),
        ];
        spec.joins = vec![JoinSpec {
            source: "rates".into(),
            on: vec![JoinPair { left: "cur".into(), right: "cur".into() }],
            kind: JoinKind::Left,
        }];
        spec.filters = vec!["amount > 0".into()];
        spec.computed = vec![ComputedSpec {
            name: "net".into(),
            expr: "amount * rate".into(),
        }];
        spec.group_by = vec!["day".into()];
        spec.aggregates = vec![
            AggSpec::new("total", AggFunc::Sum, Some("net")),
            AggSpec::new("n", AggFunc::Count, None),
        ];
        spec.post_computed = vec![ComputedSpec {
            name: "per_order".into(),
            expr: "total / n".into(),
        }];
        spec.sort = vec![SortDir { column: "total".into(), desc: true }];
        spec
    }

    #[test]
    fn identifier_whitelist_blocks_injection() {
        for bad in [
            "t; DROP TABLE x",
            "tbl --",
            "a b",
            "`x`",
            "\"x\"",
            "1abc",
            "",
            "tbl/*x*/",
            &"x".repeat(65),
        ] {
            assert!(assert_ident(bad, "表").is_err(), "应拒绝 {:?}", bad);
        }
        assert_eq!(assert_ident(" 用户表 ", "表").unwrap(), "用户表");
        assert_eq!(assert_ident("_t$1", "列").unwrap(), "_t$1");
    }

    #[test]
    fn source_sql_shape_is_fixed_per_dialect() {
        let s = src("o", "mysql", "t_order", &["id", "amount"]);
        assert_eq!(
            source_sql(&s, 100).unwrap(),
            "SELECT `id`, `amount` FROM `t_order` LIMIT 101"
        );
        let mut p = src("o", "postgresql", "orders", &[]);
        p.schema = "public".into();
        assert_eq!(
            source_sql(&p, 50).unwrap(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 51"
        );
        let q = src("o", "sqlite", "fx_rate", &["cur"]);
        assert_eq!(source_sql(&q, 10).unwrap(), "SELECT \"cur\" FROM \"fx_rate\" LIMIT 11");
        // 方言不认识就直接拒绝，绝不退化成"原样拼进去"
        let bad = src("o", "oracle", "t", &[]);
        assert!(source_sql(&bad, 10).is_err());
        // 标识符不合法时，SQL 生成阶段同样失败
        let evil = src("o", "mysql", "t`; DROP TABLE u; --", &[]);
        assert!(source_sql(&evil, 10).is_err());
    }

    #[test]
    fn max_rows_gate_has_a_floor_and_ceiling() {
        let mut spec = DatasetSpec::new("d", "n", "o");
        assert_eq!(spec.row_cap(), DEFAULT_MAX_ROWS);
        spec.max_rows = Some(0);
        assert_eq!(spec.row_cap(), 1);
        spec.max_rows = Some(usize::MAX);
        assert_eq!(spec.row_cap(), HARD_MAX_ROWS);
    }

    #[test]
    fn validate_catches_hallucinated_columns_and_functions() {
        let spec = cross_spec();
        let cache = schemas(&[
            ("orders", &["id", "day", "amount", "cur"]),
            ("rates", &["cur", "rate", "day"]),
        ]);
        assert!(validate_dataset(&spec, &cache).is_ok());

        let mut ghost = spec.clone();
        ghost.filters = vec!["missing_col > 1".into()];
        let e = validate_dataset(&ghost, &cache).unwrap_err();
        assert!(e.contains("missing_col"), "实际: {}", e);

        let mut fn_hallu = spec.clone();
        fn_hallu.computed = vec![ComputedSpec { name: "x".into(), expr: "PG_SLEEP(30)".into() }];
        assert!(validate_dataset(&fn_hallu, &cache).is_err(), "非白名单函数必须挡下");

        let mut no_join = spec.clone();
        no_join.joins = vec![JoinSpec { source: "rates".into(), on: vec![], kind: JoinKind::Inner }];
        assert!(validate_dataset(&no_join, &cache).is_err());

        let mut unknown_src = spec.clone();
        unknown_src.base = "ghost".into();
        assert!(validate_dataset(&unknown_src, &cache).is_err());

        // 源未声明列时回落到 schema 缓存；缓存也没有就报错，绝不放行
        let mut naked = spec.clone();
        naked.sources[0].columns.clear();
        assert!(validate_dataset(&naked, &schemas(&[("rates", &["cur", "rate"])])).is_err());
        assert!(validate_dataset(&naked, &cache).is_ok(), "orders 的列应从缓存补齐");
        assert!(naked.filters[0].contains("amount"), "过滤引用的是缓存里的列");
    }

    #[test]
    fn validate_dialect_and_alias_are_strict() {
        let cache = schemas(&[("o", &["a"])]);
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![src("o", "mysql", "o", &["a"])];
        assert!(validate_dataset(&spec, &cache).is_ok());

        spec.sources.push(src("o", "sqlite", "r", &["b"]));
        assert!(validate_dataset(&spec, &cache).unwrap_err().contains("重复"));

        let mut d = DatasetSpec::new("d", "n", "o");
        d.sources = vec![src("o", "sqlserver", "o", &["a"])];
        assert!(validate_dataset(&d, &cache).is_err());

        let mut cid = DatasetSpec::new("d", "n", "o");
        let mut s0 = src("o", "mysql", "o", &["a"]);
        s0.connection_id = "  ".into();
        cid.sources = vec![s0];
        assert!(validate_dataset(&cid, &cache).is_err(), "没有连接信息就没法拉数");
    }

    #[test]
    fn validate_knows_the_renamed_right_columns() {
        // 两侧都有 amount：join 之后右侧叫 amount_2，计划期必须按这个名字放行
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![
            src("o", "mysql", "t_order", &["id", "amount"]),
            src("r", "sqlite", "fx", &["id", "amount"]),
        ];
        spec.joins = vec![JoinSpec {
            source: "r".into(),
            on: vec![JoinPair { left: "id".into(), right: "id".into() }],
            kind: JoinKind::Inner,
        }];
        spec.filters = vec!["amount_2 > 1".into()];
        let cache = schemas(&[]);
        assert!(validate_dataset(&spec, &cache).is_ok());

        let mut wrong = spec.clone();
        wrong.fields = vec!["amount_r".into()];
        assert!(validate_dataset(&wrong, &cache).is_err(), "臆造的改名规则要报错");
    }

    #[test]
    fn validate_rejects_senseless_aggregate_shapes() {
        let cache = schemas(&[("o", &["a", "b"])]);
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![src("o", "mysql", "o", &["a", "b"])];
        spec.aggregates = vec![AggSpec::new("s", AggFunc::Sum, None)];
        assert!(validate_dataset(&spec, &cache).is_err(), "SUM 不给列没有意义");

        spec.aggregates = vec![AggSpec::new("s", AggFunc::Sum, Some("ghost"))];
        assert!(validate_dataset(&spec, &cache).is_err());

        spec.aggregates = vec![
            AggSpec::new("s", AggFunc::Sum, Some("b")),
            AggSpec::new("s", AggFunc::Count, None),
        ];
        spec.group_by = vec!["a".into()];
        assert!(validate_dataset(&spec, &cache).is_err(), "输出列重名");

        spec.aggregates = vec![AggSpec::new("s", AggFunc::Sum, Some("b"))];
        spec.group_by = vec!["s".into()];
        assert!(validate_dataset(&spec, &cache).is_err(), "分组列撞上聚合输出名");

        spec.group_by = vec![];
        spec.post_computed = vec![ComputedSpec { name: "p".into(), expr: "s + 1".into() }];
        assert!(validate_dataset(&spec, &cache).is_ok());
        spec.aggregates = vec![];
        assert!(validate_dataset(&spec, &cache).is_err(), "无聚合时 post_computed 无意义");
    }

    #[test]
    fn validate_uses_schema_cache_when_spec_omits_columns() {
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![src("o", "mysql", "orders", &[])];
        spec.filters = vec!["amount > 0".into()];
        spec.fields = vec!["amount".into()];
        let cache = schemas(&[("o", &["id", "amount"])]);
        assert!(validate_dataset(&spec, &cache).is_ok());
        spec.fields = vec!["ghost".into()];
        assert!(validate_dataset(&spec, &cache).is_err());
    }

    #[tokio::test]
    async fn cross_database_dataset_runs_end_to_end() {
        let orders = tbl(&["id", "day", "amount", "cur"], &[
            vec![("id", Value::Int(1)), ("day", Value::Text("01".into())), ("amount", Value::Int(100)), ("cur", Value::Text("usd".into()))],
            vec![("id", Value::Int(2)), ("day", Value::Text("01".into())), ("amount", Value::Int(50)), ("cur", Value::Text("usd".into()))],
            vec![("id", Value::Int(3)), ("day", Value::Text("02".into())), ("amount", Value::Int(70)), ("cur", Value::Text("eur".into()))],
            // 汇率缺失的币种：LEFT 保留、net 变 NULL、SUM 忽略
            vec![("id", Value::Int(4)), ("day", Value::Text("02".into())), ("amount", Value::Int(9)), ("cur", Value::Text("xxx".into()))],
            // 被 WHERE 挡掉
            vec![("id", Value::Int(5)), ("day", Value::Text("03".into())), ("amount", Value::Int(0)), ("cur", Value::Text("usd".into()))],
        ]);
        let rates = tbl(&["cur", "rate"], &[
            vec![("cur", Value::Text("usd".into())), ("rate", Value::Float(7.2))],
            vec![("cur", Value::Text("eur".into())), ("rate", Value::Float(7.8))],
        ]);
        let source = Mock::new(vec![("orders", orders), ("rates", rates)]);
        let spec = cross_spec();
        let r = execute_dataset(&spec, &source).await.unwrap();

        assert_eq!(r.table.columns, vec!["day", "total", "n", "per_order"]);
        assert_eq!(r.table.len(), 2, "01 与 02 两组");
        // total 降序：01 组 150*7.2=1080
        assert_eq!(cell(&r.table, 0, "day"), Value::Text("01".into()));
        assert_eq!(cell(&r.table, 0, "n"), Value::Int(2));
        assert_eq!(cell(&r.table, 1, "day"), Value::Text("02".into()));
        assert_eq!(cell(&r.table, 1, "n"), Value::Int(2), "LEFT 未匹配的订单也计数");
        match cell(&r.table, 1, "total") {
            Value::Float(f) => assert!((f - 546.0).abs() < 1e-6, "只有 eur 那单可加，实际 {}", f),
            other => panic!("应为 Float，实际 {:?}", other),
        }
        // per_order 用 total / n，n 含未匹配行——这是 spec 显式表达的口径
        match cell(&r.table, 1, "per_order") {
            Value::Float(f) => assert!((f - 273.0).abs() < 1e-6, "实际 {}", f),
            other => panic!("应为 Float，实际 {:?}", other),
        }
        assert_eq!(r.generated_sql.len(), 2);
        assert_eq!(r.generated_sql[0].1, "SELECT `id`, `day`, `amount`, `cur` FROM `t_order` LIMIT 50001");
        assert_eq!(r.generated_sql[1].1, "SELECT \"cur\", \"rate\" FROM \"fx_rate\" LIMIT 50001");
        assert!(!r.is_partial());
    }

    #[tokio::test]
    async fn max_rows_gate_truncates_and_reports() {
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![src("o", "mysql", "big", &["k"])];
        spec.max_rows = Some(3);
        let rows: Vec<Vec<(&str, Value)>> = (0..10)
            .map(|i| vec![("k", Value::Int(i))])
            .collect();
        let source = Mock::new(vec![("o", tbl(&["k"], &rows))]);
        let r = execute_dataset(&spec, &source).await.unwrap();
        assert_eq!(r.table.len(), 3, "超过 cap 必须硬截断");
        assert_eq!(r.truncated, vec!["o".to_string()]);
        assert!(r.is_partial());
        assert_eq!(r.source_rows[0], ("o".to_string(), 10), "回报真实拉回数，不藏起来");
        assert!(r.generated_sql[0].1.ends_with("LIMIT 4"));
    }

    #[tokio::test]
    async fn where_runs_after_the_join_so_left_padded_rows_drop_out() {
        // 若先过滤再 join，order 4（cur=xxx）会被留在结果里并带 NULL rate；
        // 先 join 再过滤才是 SQL 的语义
        let orders = tbl(&["id", "cur"], &[
            vec![("id", Value::Int(1)), ("cur", Value::Text("usd".into()))],
            vec![("id", Value::Int(2)), ("cur", Value::Text("xxx".into()))],
        ]);
        let rates = tbl(&["cur", "rate"], &[vec![("cur", Value::Text("usd".into())), ("rate", Value::Float(7.0))]]);
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![
            src("o", "mysql", "t_order", &["id", "cur"]),
            src("r", "sqlite", "fx", &["cur", "rate"]),
        ];
        spec.joins = vec![JoinSpec {
            source: "r".into(),
            on: vec![JoinPair { left: "cur".into(), right: "cur".into() }],
            kind: JoinKind::Left,
        }];
        spec.filters = vec!["rate > 0".into()];
        let r = execute_dataset(&spec, &Mock::new(vec![("o", orders), ("r", rates)]))
            .await
            .unwrap();
        assert_eq!(r.table.len(), 1);
        assert_eq!(cell(&r.table, 0, "id"), Value::Int(1));
    }

    #[tokio::test]
    async fn execution_surfaces_missing_columns_and_sources() {
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![src("o", "mysql", "t", &["a"])];
        let source = Mock::new(vec![]);
        assert!(execute_dataset(&spec, &source).await.unwrap_err().contains("mock"));

        // 基表能取到数，但 join 目标不在 sources 里 → 计划期就定位到那个别名
        let ok = Mock::new(vec![("o", tbl(&["a"], &[vec![("a", Value::Int(1))]]))]);
        spec.sources.push(src("r", "sqlite", "u", &["b"]));
        spec.joins = vec![JoinSpec {
            source: "nope".into(),
            on: vec![JoinPair { left: "a".into(), right: "b".into() }],
            kind: JoinKind::Inner,
        }];
        let e = execute_dataset(&spec, &ok).await.unwrap_err();
        assert!(e.contains("nope"), "实际: {}", e);
    }

    #[tokio::test]
    async fn limit_and_projection_apply_last() {
        let rows: Vec<Vec<(&str, Value)>> = (1..=5)
            .map(|i| vec![("k", Value::Int(i)), ("v", Value::Text(format!("t{}", i)))])
            .collect();
        let mut spec = DatasetSpec::new("d", "n", "o");
        spec.sources = vec![src("o", "mysql", "t", &["k", "v"])];
        spec.sort = vec![SortDir { column: "k".into(), desc: true }];
        spec.fields = vec!["k".into()];
        spec.limit = Some(2);
        let r = execute_dataset(&spec, &Mock::new(vec![("o", tbl(&["k", "v"], &rows))]))
            .await
            .unwrap();
        assert_eq!(r.table.columns, vec!["k".to_string()]);
        assert_eq!(r.table.len(), 2);
        assert_eq!(cell(&r.table, 0, "k"), Value::Int(5));
        assert_eq!(cell(&r.table, 1, "k"), Value::Int(4));
    }

    #[test]
    fn tagged_cells_map_to_engine_scalars() {
        let mk = |k: &str, v: serde_json::Value| -> serde_json::Value {
            serde_json::json!({ "__kind": k, "value": v })
        };
        // 这些字面值就是 lib.rs::kind_to_str 的产物（小写）。之前这里按 PascalCase
        // 写测试并且通过了，真接上驱动后整列静默变 NULL —— 测试记了个错的契约，
        // 比没测试更糟。改大小写敏感的匹配前请先改这份字面量。
        assert_eq!(tagged_to_value(&mk("null", serde_json::Value::Null)), Value::Null);
        assert_eq!(tagged_to_value(&mk("integer", serde_json::json!(42))), Value::Int(42));
        assert_eq!(tagged_to_value(&mk("float", serde_json::json!(1.5))), Value::Float(1.5));
        // T-038：超过 2^53 的整数以字符串下发，必须还原成数字而不是 NULL
        assert_eq!(
            tagged_to_value(&mk("integer", serde_json::json!("9007199254740993"))),
            Value::Int(9007199254740993)
        );
        // BIGINT UNSIGNED 超出 i64：退化成 f64 也比丢成 NULL 好（聚合还能用）
        assert_eq!(
            tagged_to_value(&mk("integer", serde_json::json!("18446744073709551615"))),
            Value::Float(1.8446744073709552e19)
        );
        // Decimal 常以字符串下发，必须还原成数值而不是文本
        assert_eq!(tagged_to_value(&mk("decimal", serde_json::json!("12.500"))), Value::Float(12.5));
        assert_eq!(tagged_to_value(&mk("datetime", serde_json::json!("2026-01-01"))), Value::Text("2026-01-01".into()));
        // 解码失败与二进制不参与求值
        assert_eq!(tagged_to_value(&mk("decode_error", serde_json::json!("boom"))), Value::Null);
        assert_eq!(tagged_to_value(&mk("binary", serde_json::json!("AA=="))), Value::Null);
        // 大小写不敏感，容得下别的调用方沿用 serde 的 PascalCase
        assert_eq!(tagged_to_value(&mk("Integer", serde_json::json!(7))), Value::Int(7));
        // 裸值（v1 兼容路径）
        assert_eq!(tagged_to_value(&serde_json::json!(7)), Value::Int(7));
        assert_eq!(tagged_to_value(&serde_json::json!(true)), Value::Bool(true));
        assert_eq!(tagged_to_value(&serde_json::Value::Null), Value::Null);
    }

    #[test]
    fn table_from_tagged_aligns_by_ordinal_and_pads() {
        let cols: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let rows = vec![
            vec![
                serde_json::json!({"__kind": "integer", "value": 1}),
                serde_json::json!({"__kind": "text", "value": "x"}),
            ],
            vec![serde_json::json!({"__kind": "null", "value": null})],
        ];
        let t = table_from_tagged(&cols, &rows);
        assert_eq!(t.columns, cols);
        assert_eq!(cell(&t, 0, "a"), Value::Int(1));
        assert_eq!(cell(&t, 0, "b"), Value::Text("x".into()));
        assert_eq!(cell(&t, 0, "c"), Value::Null, "缺位补 NULL 而不是错位");
        assert_eq!(cell(&t, 1, "b"), Value::Null);
    }

    /// 计划推断出的输出列必须与真实执行结果逐列一致——视图校验、AI 回显都靠
    /// plan.columns 提前知道结果形状，一旦和实际错位，报错就会指向不存在的列。
    #[tokio::test]
    async fn plan_columns_match_the_executed_shape() {
        let spec = cross_spec();
        let sc = schemas(&[
            ("orders", &["id", "day", "amount", "cur"]),
            ("rates", &["cur", "rate"]),
        ]);
        let plan = plan_dataset(&spec, &sc).unwrap();
        assert_eq!(plan.columns, vec!["day", "total", "n", "per_order"]);
        assert_eq!(validate_dataset(&spec, &sc).unwrap(), plan.steps);

        let orders = tbl(
            &["id", "day", "amount", "cur"],
            &[
                vec![("id", Value::Int(1)), ("day", Value::Text("01".into())), ("amount", Value::Int(100)), ("cur", Value::Text("usd".into()))],
                vec![("id", Value::Int(2)), ("day", Value::Text("02".into())), ("amount", Value::Int(70)), ("cur", Value::Text("eur".into()))],
            ],
        );
        let rates = tbl(
            &["cur", "rate"],
            &[
                vec![("cur", Value::Text("usd".into())), ("rate", Value::Float(7.2))],
                vec![("cur", Value::Text("eur".into())), ("rate", Value::Float(7.8))],
            ],
        );
        let r = execute_dataset(&spec, &Mock::new(vec![("orders", orders), ("rates", rates)]))
            .await
            .unwrap();
        assert_eq!(r.table.columns, plan.columns);

        // fields 决定最终顺序，而不是"声明顺序"
        let mut proj = spec.clone();
        proj.fields = vec!["total".into(), "day".into()];
        let p2 = plan_dataset(&proj, &sc).unwrap();
        assert_eq!(p2.columns, vec!["total", "day"]);
        let r2 = execute_dataset(
            &proj,
            &Mock::new(vec![
                ("orders", tbl(&["id", "day", "amount", "cur"], &[vec![("id", Value::Int(1)), ("day", Value::Text("01".into())), ("amount", Value::Int(100)), ("cur", Value::Text("usd".into()))]])),
                ("rates", tbl(&["cur", "rate"], &[vec![("cur", Value::Text("usd".into())), ("rate", Value::Float(7.2))]])),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(r2.table.columns, p2.columns);
    }

    #[test]
    fn spec_roundtrips_through_json() {
        // 规格是持久化与 IPC 的唯一真相，必须能原样往返
        let spec = cross_spec();
        let j = serde_json::to_string(&spec).unwrap();
        let back: DatasetSpec = serde_json::from_str(&j).unwrap();
        assert_eq!(spec, back);
        // 最小 spec 只填必填字段即可解析
        let minimal: DatasetSpec =
            serde_json::from_str(r#"{"id":"a","name":"b","base":"c","sources":[]}"#).unwrap();
        assert!(minimal.joins.is_empty() && minimal.max_rows.is_none());
    }
}
