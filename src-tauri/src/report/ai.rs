// 自然语言 → 报表规格（"AI 加持"的落点）
//
// 关键取舍：模型不写 SQL，只产出 DatasetSpec / ViewSpec 的 JSON，
// 而且它在整条链路上拿不到任何数据库句柄——本模块只吃"目录"
// （连接 id + 表名 + 列名），连 DBConfig 都不进参数，密钥自然无从泄漏。
//
// 幻觉在哪一步被挡住（全部在联网之前 / 在取数之前）：
//   1. extract_json：模型爱写 ```json 围栏和前后废话，先剥干净
//   2. normalize_source：sources 里的 connection_id / table 必须命中目录，
//      database_type 与 columns 一律由目录覆盖——模型编的方言、编的列清单
//      根本进不了执行链
//   3. plan_dataset / validate_view：列名、表达式函数白名单、聚合结构、
//      组件编码全部本地校验，报错带 dataset / 组件名
//   4. 校验失败时把错误原文回喂给模型（自我修复重试），重试次数有上限

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use super::dataset::{DatasetSpec, SchemaCache, SourceRef};
use super::expr::ALLOWED_FUNCTIONS;
use super::table::plan_join_columns;
use super::view::ViewSpec;
use crate::AIConfig;

/// 单次请求超时：桌面端等 5 分钟不如等 60 秒后告诉用户换模型
pub const REQUEST_TIMEOUT_SECS: u64 = 60;
/// 自我修复重试上限，防止和模型无限拉锯
pub const MAX_REPAIRS: u8 = 3;

/// 一次会话里可参与报表的表。只描述结构，不含任何凭据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogTable {
    pub connection_id: String,
    #[serde(default)]
    pub connection_name: String,
    /// mysql | postgresql | sqlite —— 由本机连接配置给出，模型改不动
    pub database_type: String,
    #[serde(default)]
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    /// 列名 → 数据库类型。只进提示词，本机校验仍只比对 columns
    #[serde(default)]
    pub column_types: HashMap<String, String>,
}

/// 模型产出的一整张报表草稿
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportDraft {
    pub datasets: Vec<DatasetSpec>,
    pub view: ViewSpec,
}

/// 上一版报表：让"再加一个按月的折线图""把饼图换成表格"这种追问能在已有设计上改。
/// question 是那版设计对应的需求（手改过规格、或从报表簿打开的历史报表可能为空）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorReport {
    pub question: String,
    pub draft: ReportDraft,
}

/// 通过本地校验后的草稿：可以直接拿去 report_view_render
#[derive(Debug, Clone, Serialize)]
pub struct DraftResult {
    pub datasets: Vec<DatasetSpec>,
    pub view: ViewSpec,
    /// 数据集 + 视图的完整算子链，前端"执行计划"面板与 AI 回显共用
    pub steps: Vec<String>,
    /// dataset id → 输出列
    pub columns: HashMap<String, Vec<String>>,
    /// 不算错但值得看见的事，例如"某个数据集没有任何组件在用"
    pub warnings: Vec<String>,
    /// 模型被本地校验打回了几次
    pub repairs: u8,
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

// ==================== 从模型文本里挖 JSON ====================

/// 剥掉 ``` 围栏并取出第一个完整的 JSON 对象。
/// 模型经常写成"好的，这是你要的报表：\n```json\n{...}\n```"，
/// 而字符串里出现的花括号不能当成结构，所以要带状态扫一遍。
pub fn extract_json(raw: &str) -> Result<String, String> {
    let text: Vec<char> = strip_fences(raw).chars().collect();
    let starts: Vec<usize> = text
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == '{')
        .map(|(i, _)| i)
        .collect();
    if starts.is_empty() {
        return Err("回复里找不到 JSON 对象".into());
    }
    let mut unbalanced = false;
    for start in starts {
        match object_at(&text, start) {
            Some(candidate) => {
                if serde_json::from_str::<serde_json::Value>(&candidate).is_ok() {
                    return Ok(candidate);
                }
            }
            None => unbalanced = true,
        }
    }
    if unbalanced {
        return Err("JSON 花括号没有闭合（模型回复被截断了）".into());
    }
    Err("回复里的 JSON 无法解析（可能被截断或写坏了）".into())
}

/// 从 `start` 处的 '{' 起扫到配对的 '}'；字符串里的花括号不算结构。
/// 扫到结尾仍不闭合返回 None，让调用能把这段当成"模型被截断了"。
fn object_at(text: &[char], start: usize) -> Option<String> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in text.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if *c == '\\' {
                escaped = true;
            } else if *c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].iter().collect());
                }
            }
            _ => {}
        }
    }
    None
}

/// 去掉 ```...``` 包裹；没有围栏时原样返回。
/// ai_sql 那条链也要剥围栏，所以对 crate 内可见
pub(crate) fn strip_fences(raw: &str) -> &str {
    let t = raw.trim();
    let Some(open) = t.find("```") else { return t };
    let after = &t[open + 3..];
    // 语言标注（json / JSON）到首个换行为止
    let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &after[body_start..];
    match body.find("```") {
        Some(end) => body[..end].trim(),
        None => body.trim(),
    }
}

// ==================== 提示词 ====================

/// 列清单 → 给模型看的 `列名 类型` 形式。类型没探到就只写列名，
/// 不能因为某一库不给类型就在提示词里留下 `amount ` 这种悬空空格。
pub fn column_list(t: &CatalogTable) -> String {
    t.columns
        .iter()
        .map(|c| {
            let ty = t.column_types.get(c).map(|s| s.trim()).unwrap_or("");
            if ty.is_empty() {
                c.clone()
            } else {
                format!("{} {}", c, ty)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 目录 → 文本块。同一个连接 id 下的表聚在一起，模型更容易做跨库决策。
fn render_catalog(catalog: &[CatalogTable]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for t in catalog {
        if !seen.contains(&t.connection_id.as_str()) {
            seen.push(t.connection_id.as_str());
            let name = if t.connection_name.trim().is_empty() {
                t.connection_id.as_str()
            } else {
                t.connection_name.as_str()
            };
            lines.push(format!("- 连接 {}（connection_id={}，方言 {}）", name, t.connection_id, t.database_type));
        }
        let named = if t.schema.trim().is_empty() {
            t.table.clone()
        } else {
            format!("{}.{}", t.schema, t.table)
        };
        lines.push(format!("    · {}({})", named, column_list(t)));
    }
    lines.join("\n")
}

/// 生成给模型的完整提示词。prior 是上一版设计（追问式改稿时才带），
/// feedback 是上一稿被本地校验拒绝的原因。
pub fn prompt(
    question: &str,
    catalog: &[CatalogTable],
    prior: Option<&PriorReport>,
    feedback: Option<&str>,
) -> String {
    let mut s = String::new();
    s.push_str(
        "你是数据库报表设计器。只输出一段 JSON，不要解释、不要 markdown 代码块。\n\n\
         可用数据源与列（列名只能从这里取，禁止编造；括号里是「列名 类型」）：\n",
    );
    s.push_str(&render_catalog(catalog));
    s.push_str(
        "\n\nJSON 结构：\n\
         {\"datasets\":[<数据集>],\"view\":{\"id\":\"...\",\"name\":\"...\",\"widgets\":[\n\
         \x20 {\"id\":\"...\",\"type\":\"BAR\",\"title\":\"...\",\"dataset\":\"<数据集id>\",\n\
         \x20  \"encode\":{\"x\":\"city\",\"y\":\"gmv\",\"series\":\"channel\",\"columns\":[\"id\",\"amount\"]},\n\
         \x20  \"agg\":\"SUM\",\"filters\":[\"gmv > 100\"],\"limit\":50}],\"layout\":[]}}\n\n\
         数据集结构：\n\
         {\"id\":\"...\",\"name\":\"...\",\"base\":\"<主表别名>\",\n\
         \x20 \"sources\":[{\"alias\":\"o\",\"connection_id\":\"shop\",\"table\":\"orders\"}],\n\
         \x20 \"joins\":[{\"source\":\"u\",\"on\":[{\"left\":\"user_id\",\"right\":\"id\"}],\"kind\":\"LEFT\"}],\n\
         \x20 \"filters\":[\"status = 'paid'\"],\n\
         \x20 \"computed\":[{\"name\":\"net\",\"expr\":\"amount - fee\"}],\n\
         \x20 \"group_by\":[\"city\"],\n\
         \x20 \"aggregates\":[{\"output\":\"gmv\",\"func\":\"SUM\",\"column\":\"amount\"}],\n\
         \x20 \"post_computed\":[{\"name\":\"per_order\",\"expr\":\"gmv / cnt\"}],\n\
         \x20 \"fields\":[\"city\",\"gmv\"],\"sort\":[{\"column\":\"gmv\",\"desc\":true}],\n\
         \x20 \"limit\":100,\"max_rows\":20000}\n\n\
         规则：\n\
         1. sources 只需 alias / connection_id / table；方言和列清单由本机目录填充，写了也会被覆盖。\n\
         2. 跨库：把不同 connection_id 的表放进同一个数据集，join 在本机内存里做，不要写 SQL。\n\
         3. 引用列：本表列直接写名字，需要消歧时写 别名.列名（如 u.city）。\n\
         \x20  列名后面那个词是数据库真实类型：文本列别拿去 SUM。\n\
         \x20  joins[].on 只能写列名，不接受表达式或 CAST。内存 join 按规范化整数值比对：\n\
         \x20  bigint 1001 配得上 varchar「1001」，但带前导零的「007」配不上 7，\n\
         \x20  日期/时间戳/uuid/json 这类文本配不上数值——连接键两边要同写法。\n\
         4. 表达式支持 + - * /、比较、AND/OR/NOT、IS NULL、IN，以及函数 \
         ",
    );
    s.push_str(&ALLOWED_FUNCTIONS.join(" "));
    s.push_str(
        "。没有窗口函数、没有 DATE_TRUNC / NOW()；时间比较写成字符串比较，如 day >= '2026-01-01'。\n\
         5. aggregates.func ∈ COUNT SUM AVG MIN MAX COUNT_DISTINCT（COUNT 可省略 column）。\n\
         6. widgets[].type ∈ LINE BAR PIE TABLE KPI；agg ∈ RAW SUM COUNT COUNT_DISTINCT AVG MIN MAX。\n\
         \x20  饼图与 KPI 不写 agg 就是 SUM；折线 / 柱状 / 表格不写 agg 按数据集原样画，不会压成一个点。\n\
         7. 每个数据集至少要被一个组件用到；不确定的字段宁可不写，也不要编。\n",
    );
    s.push_str("\n需求：");
    s.push_str(question.trim());
    if let Some(p) = prior {
        let asked = if p.question.trim().is_empty() {
            String::new()
        } else {
            format!("（当时需求：{}）", p.question.trim())
        };
        let json = serde_json::to_string_pretty(&p.draft)
            .unwrap_or_else(|_| "{}".into());
        s.push_str(&format!(
            "\n\n上一版报表是这么设计的{asked}：\n{json}\n\
             请在它基础上按新需求改：新需求没提到的数据集与组件保持原样，\n\
             只回改好的完整 JSON，不要回增量、不要只回新加的那几个组件。\n"
        ));
    }
    if let Some(fb) = feedback {
        s.push_str("\n\n上一稿没有通过本机校验，原因：\n");
        s.push_str(fb.trim());
        // 校验一次列全，措辞就不能说"这个问题"：那等于叫模型只改第一条，剩下两轮照样撞
        s.push_str("\n请把上面每一处都改到，重新输出完整 JSON。");
    }
    s
}

// ==================== 本地校验（不联网、不碰库） ====================

/// 在目录里定位一张表。connection_id 必须精确命中；表名允许在 schema
/// 限定的情况下消歧，命中多张同名表时报错而不是随便挑一张。
fn lookup<'a>(
    catalog: &'a [CatalogTable],
    source: &SourceRef,
) -> Result<&'a CatalogTable, String> {
    let conns: Vec<&CatalogTable> = catalog
        .iter()
        .filter(|c| same(&c.connection_id, &source.connection_id))
        .collect();
    if conns.is_empty() {
        let mut ids: Vec<&str> = catalog.iter().map(|c| c.connection_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        return Err(format!(
            "源 {} 引用的连接 {} 不在本会话目录里（可用连接：{}）",
            source.alias,
            source.connection_id,
            if ids.is_empty() { "无".into() } else { ids.join(", ") }
        ));
    }
    let exact: Vec<&&CatalogTable> = conns
        .iter()
        .filter(|c| same(&c.table, &source.table) && same(&c.schema, &source.schema))
        .collect();
    let hits: Vec<&&CatalogTable> = if exact.is_empty() {
        conns.iter().filter(|c| same(&c.table, &source.table)).collect()
    } else {
        exact
    };
    match hits.len() {
        0 => {
            let mut tables: Vec<String> = conns
                .iter()
                .map(|c| {
                    if c.schema.trim().is_empty() {
                        c.table.clone()
                    } else {
                        format!("{}.{}", c.schema, c.table)
                    }
                })
                .collect();
            tables.sort();
            tables.dedup();
            Err(format!(
                "连接 {} 里没有表 {}（可用表：{}）",
                source.connection_id,
                source.table,
                tables.join(", ")
            ))
        }
        1 => Ok(hits[0]),
        n => Err(format!(
            "连接 {} 里有 {} 张同名表 {}，请在 source 里补上 schema",
            source.connection_id, n, source.table
        )),
    }
}

/// 用目录覆盖模型声明的方言与列清单，并产出校验用的 SchemaCache + 列类型索引。
/// 这一步之后，模型写下的任何标识符都不再影响能连哪个库、能读哪些列。
fn normalize_sources(
    ds: &DatasetSpec,
    catalog: &[CatalogTable],
) -> Result<(Vec<SourceRef>, SchemaCache, TypeIndex), String> {
    let mut cache: SchemaCache = HashMap::new();
    let mut types: TypeIndex = HashMap::new();
    let mut sources: Vec<SourceRef> = Vec::with_capacity(ds.sources.len());
    for src in &ds.sources {
        let hit = lookup(catalog, src).map_err(|e| format!("数据集 {}：{}", label_of(ds), e))?;
        cache.insert(src.alias.clone(), hit.columns.clone());
        types.insert(src.alias.clone(), lower_types(hit));
        sources.push(SourceRef {
            alias: src.alias.clone(),
            connection_id: hit.connection_id.clone(),
            database_type: hit.database_type.clone(),
            schema: hit.schema.clone(),
            table: hit.table.clone(),
            // 显式列清单而不是 SELECT *：桌面端宁可少拿一列大字段
            columns: hit.columns.clone(),
        });
    }
    Ok((sources, cache, types))
}

// ==================== 连接键的类型族 ====================

/// 列名（小写）→ 数据库类型。模型写的列名大小写不一定和目录一致，
/// 校验那边是忽略大小写比对的，这里同口径。
type TypeIndex = HashMap<String, HashMap<String, String>>;

fn lower_types(t: &CatalogTable) -> HashMap<String, String> {
    t.column_types
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect()
}

/// 连接键在数据库里属于哪个值族。join 比对走 `expr::join_key_of` 的规范化整数键，
/// 所以数值与文本之间只是"写法要一致"的风险；二进制/大字段则在取数阶段就被置成 NULL，
/// 而 NULL 连接键永不匹配，那是确定配不上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Numeric,
    Text,
    Binary,
    /// 目录没给类型，或类型名认不出：沉默，不猜
    Unknown,
}

/// 类型名开头的词。只看开头，所以 `decimal(12,2)`、`int(11)`、`double precision`、
/// `character varying(64)`、`timestamp without time zone` 都能落对。
fn type_head(ty: &str) -> String {
    ty.trim()
        .to_ascii_lowercase()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect()
}

/// 数据库自报类型 → 值族。
fn family_of(ty: &str) -> Family {
    match type_head(ty).as_str() {
        "int" | "integer" | "tinyint" | "smallint" | "mediumint" | "bigint" | "int2"
        | "int4" | "int8" | "serial" | "bigserial" | "smallserial" | "float" | "double"
        | "real" | "decimal" | "dec" | "numeric" | "fixed" | "money" | "smallmoney"
        | "bool" | "boolean" => Family::Numeric,
        "char" | "varchar" | "nchar" | "nvarchar" | "text" | "tinytext" | "mediumtext"
        | "longtext" | "character" | "citext" | "string" | "name" | "clob" | "uuid"
        | "json" | "jsonb" | "enum" | "set" | "date" | "datetime" | "smalldatetime"
        | "time" | "timestamp" | "timestamptz" | "year" => Family::Text,
        "binary" | "varbinary" | "blob" | "tinyblob" | "mediumblob" | "longblob"
        | "bytea" | "geometry" | "geography" | "image" | "vector" => Family::Binary,
        _ => Family::Unknown,
    }
}

/// 一列的类型与值桶。目录里没有这一列（或类型为空白）时返回 Unknown，
/// 让沉默成为默认——宁可漏报，也不能把本来能跑的草稿说成坏的。
fn column_family(types: &TypeIndex, alias: &str, col: &str) -> (String, Family) {
    let ty = types
        .get(alias)
        .and_then(|m| m.get(&col.to_ascii_lowercase()))
        .cloned()
        .unwrap_or_default();
    let f = if ty.is_empty() {
        Family::Unknown
    } else {
        family_of(&ty)
    };
    (ty, f)
}

fn show_ty(ty: &str) -> String {
    if ty.is_empty() {
        "类型未知".to_string()
    } else {
        ty.to_string()
    }
}

/// 文本族里有一类"永远写不成整数"的类型：日期、时间戳、uuid、json。
/// 它们与数值列做连接键时不存在规范化容忍的可能，只能按必然空表报；
/// varchar/char/text 那些则降级成"写法要一致"的提醒。
fn text_never_integer(ty: &str) -> bool {
    matches!(
        type_head(ty).as_str(),
        "date" | "datetime" | "smalldatetime" | "time" | "timestamp" | "timestamptz" | "uuid"
            | "json" | "jsonb"
    )
}

/// 两侧连接键的类型族差异。返回原因文本（拼到告警里）。
fn key_conflict(
    lf: Family,
    rf: Family,
    lcol: &str,
    lty: &str,
    rcol: &str,
    rty: &str,
) -> Option<String> {
    let why = match (lf, rf) {
        (Family::Binary, _) | (_, Family::Binary) => {
            "有一边是二进制/大字段，取数时会被置成 NULL，而 NULL 连接键不可能匹配——这一轮 join 一行都配不上，做出来的表会是空表；换一对两边同族的列".to_string()
        }
        (Family::Numeric, Family::Text) | (Family::Text, Family::Numeric) => {
            let text_ty = if lf == Family::Text { lty } else { rty };
            if text_never_integer(text_ty) {
                "一边数值一边文本，而日期、时间戳、uuid、json 这类值永远写不成整数键——这一轮 join 一行都配不上，做出来的表会是空表；换一对两边同族的列".to_string()
            } else {
                "一边数值一边文本：join 按规范化的整数值比对，所以 bigint 1001 配得上文本「1001」；\
                 但文本侧带前导零、小数点或空格就仍然配不上（「007」配不上 7），\
                 这类不一致会静默少行甚至整轮空表——确认两边写法一致最稳妥"
                    .to_string()
            }
        }
        _ => return None,
    };
    Some(format!(
        "{}（{}）与 {}（{}）做连接键，{}",
        lcol,
        show_ty(lty),
        rcol,
        show_ty(rty),
        why
    ))
}

/// 逐个 join 比对连接键的类型族。
///
/// 左侧是"到当前为止已累计的输出列"，命名规则与执行期共用 plan_join_columns，
/// 所以重命名成 `id_2` 的那一侧也能拿到正确的类型。列名本身已由 plan_dataset
/// 验过，这里只在能确定类型族时才开口。
fn join_key_warnings(ds: &DatasetSpec, types: &TypeIndex) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(base) = ds.source(&ds.base) else {
        return warnings;
    };
    // (输出列名, 别名.原始列名, 类型, 值桶)
    let mut acc: Vec<(String, String, String, Family)> = base
        .columns
        .iter()
        .map(|c| {
            let (ty, f) = column_family(types, &base.alias, c);
            (c.clone(), format!("{}.{}", base.alias, c), ty, f)
        })
        .collect();

    for j in &ds.joins {
        let Some(right) = ds.source(&j.source) else {
            continue;
        };
        for p in &j.on {
            let Some((_, lcol, lty, lf)) =
                acc.iter().find(|(n, _, _, _)| same(n, &p.left)).cloned()
            else {
                continue;
            };
            let (rty, rf) = column_family(types, &right.alias, &p.right);
            if let Some(why) = key_conflict(lf, rf, &lcol, &lty, &p.right, &rty) {
                warnings.push(format!("数据集 {}：{}", label_of(ds), why));
            }
        }
        let left: Vec<String> = acc.iter().map(|(n, _, _, _)| n.clone()).collect();
        let (_, mapping) = plan_join_columns(&left, &right.columns);
        for (orig, out) in mapping {
            let (ty, f) = column_family(types, &right.alias, &orig);
            acc.push((out, format!("{}.{}", right.alias, orig), ty, f));
        }
    }
    warnings
}

fn label_of(ds: &DatasetSpec) -> String {
    if ds.name.trim().is_empty() {
        ds.id.clone()
    } else {
        format!("{}（{}）", ds.id, ds.name)
    }
}

/// 把草稿规范化 + 全量校验。返回的 datasets 已按目录纠正方言与列清单，
/// 可以直接交给 report_view_render；任何幻觉都在这里以带 id 的错误挡下。
pub fn check_draft(draft: ReportDraft, catalog: &[CatalogTable]) -> Result<DraftResult, String> {
    if draft.datasets.is_empty() {
        return Err("草稿没有任何数据集".into());
    }
    if catalog.is_empty() {
        return Err("本会话没有可用表目录，先选择要参与报表的表".into());
    }
    let mut datasets = Vec::with_capacity(draft.datasets.len());
    let mut steps: Vec<String> = Vec::new();
    let mut columns: HashMap<String, Vec<String>> = HashMap::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    // 计划就没过的数据集：挂在它上面的组件不必再逐条喊"数据集不存在"
    let mut dead: HashSet<String> = HashSet::new();

    for ds in draft.datasets {
        // 这两条不编号往下走：id 空或重复时"哪一条坏了"根本说不清
        if ds.id.trim().is_empty() {
            return Err("数据集缺少 id".into());
        }
        if !ids.insert(ds.id.clone()) {
            return Err(format!("数据集 id {} 重复", ds.id));
        }
        let id = ds.id.clone();
        match plan_draft_dataset(ds, catalog) {
            Ok((ds, plan, types)) => {
                steps.extend(plan.steps);
                columns.insert(ds.id.clone(), plan.columns.clone());
                // 类型族只在结构已经合法之后才评：列名都不存在的草稿，报错比告警有用
                warnings.extend(join_key_warnings(&ds, &types));
                datasets.push(ds);
            }
            Err(e) => {
                problems.push(e);
                dead.insert(id);
            }
        }
    }

    // 一稿里几个数据集互不依赖，坏一个不该挡掉其余的校验；组件侧同理，
    // 所以两侧的问题并成一份清单一次回喂，模型一轮就能把整稿改干净。
    let view = strip_dead_widgets(&draft.view, &dead);
    let all_collateral = view.widgets.is_empty() && !draft.view.widgets.is_empty();
    if problems.is_empty() {
        steps.extend(super::view::validate_view(&view, &columns)?);
    } else {
        // 组件全挂在死集上时不必再问组件侧：那时"视图没有任何组件"说的是上面那条的连带
        if !all_collateral {
            problems.extend(super::view::view_problems(&view, &columns));
        }
        return Err(super::view::problem_list(&problems));
    }
    let used: HashSet<&str> = view.widgets.iter().map(|w| w.dataset.as_str()).collect();
    for ds in &datasets {
        if !used.contains(ds.id.as_str()) {
            warnings.push(format!(
                "数据集 {}（{}）没有任何组件在用，执行时会白取一次数",
                ds.id, ds.name
            ));
        }
    }
    Ok(DraftResult {
        datasets,
        view: draft.view,
        steps,
        columns,
        warnings,
        repairs: 0,
    })
}

/// 目录规范化 + 执行计划合成一步。错误文本已经带 "数据集 …" 前缀，调用方直接并入清单。
fn plan_draft_dataset(
    ds: DatasetSpec,
    catalog: &[CatalogTable],
) -> Result<(DatasetSpec, super::dataset::DatasetPlan, TypeIndex), String> {
    let (sources, cache, types) = normalize_sources(&ds, catalog)?;
    let ds = DatasetSpec { sources, ..ds };
    let label = label_of(&ds);
    let plan =
        super::dataset::plan_dataset(&ds, &cache).map_err(|e| format!("数据集 {}：{}", label, e))?;
    Ok((ds, plan, types))
}

/// 计划阶段就没过的数据集，挂在它上面的组件先摘掉：留着只会让清单上多出一串
/// "组件 x 绑定的数据集 y 不存在"，而 y 其实存在，只是这次没计划通。
fn strip_dead_widgets(view: &super::view::ViewSpec, dead: &HashSet<String>) -> super::view::ViewSpec {
    if dead.is_empty() {
        return view.clone();
    }
    let widgets: Vec<_> = view
        .widgets
        .iter()
        .filter(|w| !dead.contains(w.dataset.as_str()))
        .cloned()
        .collect();
    let layout = view
        .layout
        .iter()
        .filter(|l| widgets.iter().any(|w| w.id == l.widget))
        .cloned()
        .collect();
    super::view::ViewSpec {
        id: view.id.clone(),
        name: view.name.clone(),
        version: view.version,
        widgets,
        layout,
    }
}

// ==================== 模型调用 ====================

/// 唯一的大模型出口：标准 OpenAI 兼容 chat/completions。
/// 单条 user 消息即可覆盖本工具的用法，不引入会话状态。
pub async fn chat(cfg: &AIConfig, prompt: &str) -> Result<String, String> {
    if cfg.base_url.trim().is_empty() || cfg.api_key.trim().is_empty() {
        return Err("请先在设置中配置 AI 服务地址与密钥".into());
    }
    let base = cfg.base_url.trim().trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("HTTP 客户端构造失败: {}", e))?;
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [{ "role": "user", "content": prompt }],
        "temperature": 0,
        "stream": false,
    });
    let response = client
        .post(format!("{}/chat/completions", base))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("AI 服务请求失败: {}", e))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let brief: String = text.chars().take(200).collect();
        return Err(format!("AI 服务返回 {}：{}", status, brief));
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("解析 AI 响应失败: {}", e))?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "AI 响应缺少 choices[0].message.content".to_string())
}

/// 一次补全。测试用脚本化实现替身，不必真联网。
pub trait Model: Send + Sync {
    fn complete(&self, prompt: String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>>;
}

pub struct HttpModel {
    cfg: AIConfig,
}

impl HttpModel {
    pub fn new(cfg: AIConfig) -> Result<Self, String> {
        if cfg.model.trim().is_empty() {
            return Err("请先在设置中填写 AI 模型名".into());
        }
        Ok(Self { cfg })
    }
}

impl Model for HttpModel {
    fn complete(&self, prompt: String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        Box::pin(async move { chat(&self.cfg, &prompt).await })
    }
}

/// 起草：问一次 → 本地校验 → 不通过就把错误原文回喂，最多 repairs 次。
/// 校验全在本地跑，所以模型再怎么胡说也不会变成打到库上的查询。
/// prior 是上一版设计，追问式改稿时带上；改出来的设计照样过同一套本地校验。
pub async fn draft(
    model: &dyn Model,
    question: &str,
    catalog: &[CatalogTable],
    repairs: u8,
    prior: Option<&PriorReport>,
) -> Result<DraftResult, String> {
    if question.trim().is_empty() {
        return Err("请先描述你想要什么报表".into());
    }
    // 空白的上一版只会往提示词里塞噪音，当作没带
    let prior =
        prior.filter(|p| !p.draft.datasets.is_empty() || !p.draft.view.widgets.is_empty());
    let rounds = repairs.min(MAX_REPAIRS);
    let mut feedback: Option<String> = None;
    let mut last = String::new();
    for round in 0..=rounds {
        let raw = model
            .complete(prompt(question, catalog, prior, feedback.as_deref()))
            .await?;
        let parsed = match parse_draft(&raw) {
            Ok(d) => d,
            Err(e) => {
                last = e;
                feedback = Some(last.clone());
                continue;
            }
        };
        match check_draft(parsed, catalog) {
            Ok(mut out) => {
                out.repairs = round;
                return Ok(out);
            }
            Err(e) => {
                last = e;
                feedback = Some(last.clone());
            }
        }
    }
    Err(format!(
        "重试 {} 次后仍未通过本地校验：{}",
        rounds + 1,
        last
    ))
}

/// 从模型回复里挖出 JSON 并反序列化成草稿
pub fn parse_draft(raw: &str) -> Result<ReportDraft, String> {
    let json = extract_json(raw)?;
    serde_json::from_str::<ReportDraft>(&json).map_err(|e| format!("草稿 JSON 不合法: {}", e))
}

// ==================== Tauri 命令 ====================

/// 自然语言 → 一整张报表草稿（数据集 + 组件），已经过本地校验。
/// 出数仍要走 report_view_render，由用户带着连接配置点一次"执行"。
#[tauri::command]
pub async fn ai_report_draft(
    question: String,
    catalog: Vec<CatalogTable>,
    config: AIConfig,
    max_repairs: Option<u8>,
    prior: Option<PriorReport>,
) -> Result<DraftResult, String> {
    let model = HttpModel::new(config)?;
    draft(
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
    use super::super::dataset::{JoinKind, JoinSpec};
    use super::super::table::AggFunc;
    use super::super::view::{AggType, ChartType};
    use super::*;

    fn types(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn catalog() -> Vec<CatalogTable> {
        vec![
            CatalogTable {
                connection_id: "shop".into(),
                connection_name: "商城库".into(),
                database_type: "sqlite".into(),
                schema: String::new(),
                table: "orders".into(),
                columns: vec!["id".into(), "user_id".into(), "amount".into(), "status".into()],
                column_types: types(&[
                    ("id", "INTEGER"),
                    ("user_id", "INTEGER"),
                    ("amount", "REAL"),
                    ("status", "TEXT"),
                ]),
            },
            CatalogTable {
                connection_id: "shop".into(),
                connection_name: "商城库".into(),
                database_type: "sqlite".into(),
                schema: String::new(),
                table: "users".into(),
                columns: vec!["id".into(), "city".into(), "channel".into()],
                // 这张表故意没类型：模型要能只看列名也起草成功
                column_types: HashMap::new(),
            },
            CatalogTable {
                connection_id: "crm".into(),
                connection_name: "客户库".into(),
                database_type: "mysql".into(),
                schema: "crm".into(),
                table: "visits".into(),
                columns: vec!["user_id".into(), "day".into(), "pv".into()],
                column_types: types(&[("user_id", "bigint(20)"), ("day", "date"), ("pv", "int(11)")]),
            },
        ]
    }

    fn spec(id: &str, base: &str, sources: &str, rest: &str) -> String {
        format!(
            "{{\"id\":\"{id}\",\"name\":\"测试集 {id}\",\"base\":\"{base}\",\"sources\":{sources},{rest}}}"
        )
    }

    fn draft_of(datasets: &[String], widgets: &[serde_json::Value]) -> String {
        let ws: Vec<String> = widgets.iter().map(|w| w.to_string()).collect();
        format!(
            "{{\"datasets\":[{}],\"view\":{{\"id\":\"v1\",\"name\":\"看板\",\"widgets\":[{}],\"layout\":[]}}}}",
            datasets.join(","),
            ws.join(",")
        )
    }

    fn widget(dataset: &str, ty: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "w1", "type": ty, "title": "城市成交", "dataset": dataset,
            "encode": { "x": "city", "y": "gmv" }, "agg": "RAW"
        })
    }

    /// 一份能通过全部本地校验的跨库聚合集：SQLite 订单 join SQLite 客户
    fn city_gmv() -> String {
        spec(
            "d1",
            "o",
            r#"[{"alias":"o","connection_id":"shop","table":"orders"},{"alias":"u","connection_id":"shop","table":"users"}]"#,
            concat!(
                r#""joins":[{"source":"u","on":[{"left":"user_id","right":"id"}]}],"#,
                r#""filters":["status = 'paid'"],"group_by":["city"],"#,
                r#""aggregates":[{"output":"gmv","func":"SUM","column":"amount"}]"#,
            ),
        )
    }

    #[test]
    fn extract_json_survives_fences_and_prose() {
        let inner = r#"{"datasets":[],"view":{"id":"v"}}"#;
        assert_eq!(extract_json(inner).unwrap(), inner);
        assert_eq!(
            extract_json(&format!("好的：\n```json\n{}\n```\n希望有帮助", inner)).unwrap(),
            inner
        );
        assert_eq!(
            extract_json(&format!("前置废话 {{ 花括号\n{}", inner)).unwrap(),
            inner
        );
    }

    #[test]
    fn extract_json_respects_braces_inside_strings() {
        let raw = r#"{"a":"} not the end {","b":2}"#;
        assert_eq!(extract_json(raw).unwrap(), raw);
    }

    #[test]
    fn extract_json_blames_truncation_not_the_user() {
        let err = extract_json(r#"{"datasets":[{"name":"x"#).unwrap_err();
        assert!(err.contains("截断"), "{}", err);
        assert!(extract_json("这里什么对象都没有")
            .unwrap_err()
            .contains("找不到 JSON"));
    }

    /// 校验一次列全之后，提示词不能再叫模型"只修正这个问题"——那等于叫它只改第一条，
    /// 剩下几处照样要撞满两轮才收场。
    #[test]
    fn repair_prompt_asks_for_every_listed_problem() {
        let fb = "共 2 处问题，一次全改完再跑：\n1. 数据集 d1：没有表 user\n2. 组件 w2（数据集 d2）：y 列 uv 不在数据集输出里";
        let p = prompt("各城市成交额", &catalog(), None, Some(fb));
        assert!(p.contains("共 2 处问题"), "反馈原文要整份进提示词：{}", p);
        assert!(p.contains("没有表 user"), "{}", p);
        assert!(!p.contains("只修正这个问题"), "{}", p);
        assert!(p.contains("每一处"), "{}", p);
    }

    #[test]
    fn prompt_lists_every_allowed_function_and_the_catalog() {
        let p = prompt("各城市成交额", &catalog(), None, None);
        for f in ALLOWED_FUNCTIONS {
            assert!(p.contains(f), "提示词漏了函数 {}", f);
        }
        assert!(p.contains("connection_id=shop"));
        assert!(p.contains("orders(id INTEGER, user_id INTEGER, amount REAL, status TEXT)"));
        assert!(p.contains("crm.visits(user_id bigint(20), day date, pv int(11))"));
        assert!(!p.contains("password"), "提示词不该带凭据");
    }

    /// 类型只进提示词，不能渗进本机校验比对的列名清单。
    /// 探不到类型的表必须渲染成裸列名，不能留下 `city ` 这种悬空空格。
    #[test]
    fn prompt_carries_types_when_known_and_bare_names_otherwise() {
        let c = catalog();
        let p = prompt("各城市成交额", &c, None, None);
        // orders 带类型
        assert!(p.contains("amount REAL"), "{}", p);
        // users 没有类型 → 只有名字，且名字后面紧跟逗号或右括号
        assert!(
            p.contains("users(id, city, channel)"),
            "无类型的表该渲染成裸列名：{}",
            p
        );
        // 校验用的列名清单不含类型
        let hit = c.iter().find(|t| t.table == "orders").unwrap();
        assert_eq!(hit.columns, vec!["id", "user_id", "amount", "status"]);
    }

    #[test]
    fn prompt_feeds_the_previous_error_back() {
        let p = prompt("q", &catalog(), None, Some("列 total_fee 不存在"));
        assert!(p.contains("列 total_fee 不存在"));
        assert!(p.contains("重新输出完整 JSON"));
    }

    /// 提示词里连接键那段必须与引擎实际口径一致：说"两边必须同族"会挡掉
    /// 本来能跑的草稿（bigint 配 varchar 是跨库常态），说"随便配"又会换来静默空表。
    #[test]
    fn prompt_states_the_join_key_rule_the_engine_actually_applies() {
        let p = prompt("q", &catalog(), None, None);
        assert!(p.contains("只能写列名"), "表达式仍不接受：{}", p);
        assert!(p.contains("配得上 varchar「1001」"), "要承认整数写法能跨族配：{}", p);
        assert!(p.contains("「007」配不上 7"), "要说清前导零仍不配：{}", p);
        assert!(p.contains("配不上数值"), "日期/uuid 这类仍配不上：{}", p);
        assert!(!p.contains("必须同族"), "旧措辞已被引擎推翻：{}", p);
    }

    #[test]
    fn ai_written_enums_survive_case_and_aliases() {
        let ds: DatasetSpec = serde_json::from_str(&spec(
            "d1",
            "o",
            r#"[{"alias":"o","connection_id":"shop","table":"orders","database_type":"ORACLE"}]"#,
            r#""aggregates":[{"output":"gmv","func":"sum","column":"amount"},{"output":"n","func":"COUNTD"}]"#,
        ))
        .unwrap();
        assert_eq!(ds.aggregates[0].func, AggFunc::Sum);
        assert_eq!(ds.aggregates[1].func, AggFunc::CountDistinct);
        assert_eq!(ds.sources[0].database_type, "ORACLE", "反序列化能收，规范化会覆盖");

        let j: JoinSpec = serde_json::from_str(
            r#"{"source":"u","on":[{"left":"user_id","right":"id"}],"kind":"left outer"}"#,
        )
        .unwrap();
        assert_eq!(j.kind, JoinKind::Left);

        let v: ViewSpec = serde_json::from_str(
            r#"{"id":"v","name":"n","widgets":[{"id":"w","type":"kpi","dataset":"d","agg":"countdistinct"}]}"#,
        )
        .unwrap();
        assert_eq!(v.widgets[0].kind, ChartType::Kpi);
        assert_eq!(v.widgets[0].agg, AggType::CountDistinct);
    }

    #[test]
    fn check_normalizes_dialect_and_column_lists() {
        // 模型写了不存在的方言 oracle、编造的列清单，connection_id 大小写也不对
        let raw = draft_of(
            &[spec(
                "d1",
                "o",
                r#"[{"alias":"o","connection_id":"shop","table":"orders","database_type":"oracle","columns":["nope"]},{"alias":"u","connection_id":"SHOP","table":"USERS"}]"#,
                concat!(
                    r#""joins":[{"source":"u","on":[{"left":"user_id","right":"id"}],"kind":"left"}],"#,
                    r#""filters":["status = 'paid'"],"group_by":["city"],"#,
                    r#""aggregates":[{"output":"gmv","func":"SUM","column":"amount"}]"#,
                ),
            )],
            &[widget("d1", "bar")],
        );
        let out = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap();
        let src = &out.datasets[0].sources[0];
        assert_eq!(src.database_type, "sqlite", "模型写的 oracle 被目录覆盖");
        assert_eq!(src.columns, vec!["id", "user_id", "amount", "status"]);
        assert_eq!(out.datasets[0].sources[1].connection_id, "shop");
        assert_eq!(out.columns["d1"], vec!["city", "gmv"]);
        assert!(
            out.steps.iter().any(|s| s.starts_with("GROUP BY")),
            "{:?}",
            out.steps
        );
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn check_rejects_tables_and_connections_not_in_the_catalog() {
        let raw = draft_of(
            &[spec(
                "d1",
                "o",
                r#"[{"alias":"o","connection_id":"shop","table":"user"}]"#,
                r#""group_by":["city"]"#,
            )],
            &[widget("d1", "BAR")],
        );
        let err = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap_err();
        assert!(err.contains("没有表 user"), "{}", err);
        assert!(err.contains("orders, users"), "{}", err);
        assert!(err.contains("数据集 d1"), "{}", err);

        let raw2 = draft_of(
            &[spec(
                "d1",
                "o",
                r#"[{"alias":"o","connection_id":"warehouse","table":"orders"}]"#,
                r#""limit":10"#,
            )],
            &[widget("d1", "BAR")],
        );
        let err2 = check_draft(parse_draft(&raw2).unwrap(), &catalog()).unwrap_err();
        assert!(err2.contains("crm, shop"), "{}", err2);
    }

    #[test]
    fn check_catches_hallucinated_column_before_touching_a_database() {
        let raw = draft_of(
            &[spec(
                "d1",
                "o",
                r#"[{"alias":"o","connection_id":"shop","table":"orders"}]"#,
                r#""filters":["total_fee > 10"]"#,
            )],
            &[widget("d1", "BAR")],
        );
        let err = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap_err();
        assert!(err.contains("total_fee"), "{}", err);
        assert!(err.contains("d1"), "{}", err);
    }

    /// 自修复默认只有两轮预算：一稿里两个数据集各写错一处，逐条报就得三轮才改完，
    /// 于是本地直接把整份清单回喂给模型。
    #[test]
    fn check_lists_every_broken_dataset_in_one_pass() {
        let raw = draft_of(
            &[
                spec(
                    "d1",
                    "o",
                    r#"[{"alias":"o","connection_id":"shop","table":"user"}]"#,
                    r#""group_by":["city"]"#,
                ),
                spec(
                    "d2",
                    "v",
                    r#"[{"alias":"v","connection_id":"crm","table":"visits"}]"#,
                    r#""group_by":["hour"]"#,
                ),
            ],
            &[serde_json::json!({
                "id": "w1", "type": "BAR", "dataset": "d1",
                "encode": { "x": "city", "y": "gmv" }
            })],
        );
        let err = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap_err();
        assert!(err.contains("没有表 user"), "{}", err);
        assert!(err.contains("hour"), "{}", err);
        assert!(err.contains("共 2 处问题"), "{}", err);
        // d1 上的组件不再跟着喊"数据集 d1 不存在"——那是同一条错的连带，
        // 模型照着它改只会把组件挪到别处去
        assert!(!err.contains("组件 w1"), "{}", err);
    }

    #[test]
    fn check_pairs_a_dead_dataset_with_a_broken_widget() {
        let raw = draft_of(
            &[
                spec(
                    "d1",
                    "o",
                    r#"[{"alias":"o","connection_id":"shop","table":"user"}]"#,
                    r#""group_by":["city"]"#,
                ),
                spec(
                    "d2",
                    "v",
                    r#"[{"alias":"v","connection_id":"crm","table":"visits"}]"#,
                    concat!(
                        r#""group_by":["day"],"#,
                        r#""aggregates":[{"output":"pv","func":"SUM","column":"pv"}]"#,
                    ),
                ),
            ],
            &[
                serde_json::json!({
                    "id": "w1", "type": "BAR", "dataset": "d1",
                    "encode": { "x": "city", "y": "gmv" }
                }),
                serde_json::json!({
                    "id": "w2", "type": "BAR", "dataset": "d2",
                    "encode": { "x": "day", "y": "uv" }
                }),
            ],
        );
        let err = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap_err();
        // 数据集侧和组件侧的问题并成一份清单，一轮就都送到模型眼前
        assert!(err.contains("没有表 user"), "{}", err);
        assert!(err.contains("组件 w2"), "{}", err);
        assert!(err.contains("uv"), "{}", err);
        assert!(err.contains("共 2 处问题"), "{}", err);
        assert!(!err.contains("组件 w1"), "{}", err);
    }

    #[test]
    fn check_warns_about_a_dataset_nothing_renders() {
            let visits = spec(
            "d2",
            "v",
            r#"[{"alias":"v","connection_id":"crm","table":"visits"}]"#,
            r#""group_by":["day"],"aggregates":[{"output":"pv","func":"SUM","column":"pv"}]"#,
        );
        let raw = draft_of(&[city_gmv(), visits], &[widget("d1", "BAR")]);
        let out = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap();
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        assert!(out.warnings[0].contains("d2"), "{:?}", out.warnings);
    }

    /// 连接键的类型族决定告警准不准，所以按各方言真实写法逐条钉住
    #[test]
    fn family_of_covers_real_dialect_type_strings() {
        for ty in [
            "int(11)",
            "INT",
            "bigint(20)",
            "integer",
            "smallint",
            "decimal(12,2)",
            "numeric",
            "real",
            "double precision",
            "float",
            "boolean",
            "serial",
        ] {
            assert_eq!(family_of(ty), Family::Numeric, "{} 该算数值", ty);
        }
        for ty in [
            "varchar(64)",
            "char(8)",
            "text",
            "tinytext",
            "character varying(64)",
            "uuid",
            "json",
            "jsonb",
            "date",
            "datetime",
            "timestamp without time zone",
            "enum('a','b')",
        ] {
            assert_eq!(family_of(ty), Family::Text, "{} 该算文本", ty);
        }
        for ty in ["blob", "bytea", "varbinary(16)", "geometry"] {
            assert_eq!(family_of(ty), Family::Binary, "{} 该算二进制", ty);
        }
        // 认不出的一律沉默：宁可漏报，不能把本来能跑的草稿说成坏的
        for ty in ["", "   ", "widget", "未知类型", "array"] {
            assert_eq!(family_of(ty), Family::Unknown, "{} 不该乱猜", ty);
        }
    }

    /// 三张表专门用来验连接键：orders(bigint) / ext(bigint + int) / att(varchar + blob + datetime)
    fn typed_catalog() -> Vec<CatalogTable> {
        let t = |conn: &str, table: &str, cols: &[(&str, &str)]| CatalogTable {
            connection_id: conn.into(),
            connection_name: conn.into(),
            database_type: "mysql".into(),
            schema: String::new(),
            table: table.into(),
            columns: cols.iter().map(|(n, _)| n.to_string()).collect(),
            column_types: types(cols),
        };
        vec![
            t(
                "shop",
                "orders",
                &[
                    ("order_no", "bigint(20)"),
                    ("city", "varchar(32)"),
                    ("amount", "decimal(12,2)"),
                ],
            ),
            t(
                "crm",
                "ext",
                &[("order_no", "bigint(20)"), ("city", "int(11)")],
            ),
            t(
                "crm",
                "att",
                &[
                    ("zone", "varchar(64)"),
                    ("raw_key", "blob"),
                    ("created_at", "datetime"),
                ],
            ),
        ]
    }

    /// 把 sources + joins 拼成一个结构合法的数据集：聚合固定输出 city / gmv，配 widget()
    fn typed_ds(sources: &str, joins: &str) -> String {
        spec(
            "d1",
            "o",
            sources,
            &format!(
                concat!(
                    r#""joins":[{}],"group_by":["city"],"#,
                    r#""aggregates":[{{"output":"gmv","func":"SUM","column":"amount"}}]"#
                ),
                joins
            ),
        )
    }

    fn join_one(left: &str, src: &str, right: &str) -> String {
        format!(
            r#"{{"source":"{}","on":[{{"left":"{}","right":"{}"}}]}}"#,
            src, left, right
        )
    }

    /// join 已按规范化整数比对，所以跨族的措辞分三档：varchar 只提醒"写法要一致"，
    /// datetime/uuid 这类永远配不上的才说必然空表，二进制在取数阶段就被置成 NULL。
    /// 同族则必须沉默——否则这条告警会退化成噪音，模型和用户都学会忽略它。
    #[test]
    fn check_draft_scales_the_join_key_warning_to_the_text_type() {
        let c = typed_catalog();
        let warn = |right_col: &str| {
            let raw = typed_ds(
                r#"[{"alias":"o","connection_id":"shop","table":"orders"},{"alias":"a","connection_id":"crm","table":"att"}]"#,
                &join_one("order_no", "a", right_col),
            );
            let out =
                check_draft(parse_draft(&draft_of(&[raw], &[widget("d1", "BAR")])).unwrap(), &c)
                    .unwrap();
            assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
            out.warnings[0].clone()
        };

        // 文本有可能就是整数写法：说清残余风险，不能再断言"一行都配不上"
        let w = warn("zone");
        assert!(w.contains("d1"), "告警要带数据集 id：{}", w);
        assert!(w.contains("o.order_no"), "要指出左侧键：{}", w);
        assert!(w.contains("bigint(20)"), "要给出两侧真实类型：{}", w);
        assert!(w.contains("varchar(64)"), "要给出两侧真实类型：{}", w);
        assert!(w.contains("配得上"), "{} 要承认 1001 与「1001」已能配", w);
        assert!(w.contains("前导零"), "{} 要说清残余风险是写法不一致", w);
        assert!(
            !w.contains("一行都配不上"),
            "{} 不能再把可容忍的跨族说成必然空表",
            w
        );

        // datetime 永远写不成整数键：这一档仍然是必然空表
        let w = warn("created_at");
        assert!(w.contains("永远写不成整数"), "{}", w);
        assert!(w.contains("空表"), "{}", w);

        // 二进制键两边同族也配不上：取数阶段就被置成 NULL
        let w = warn("raw_key");
        assert!(w.contains("NULL"), "{}", w);
        assert!(w.contains("空表"), "{}", w);

        // 同族连接键不能报警
        let good = typed_ds(
            r#"[{"alias":"o","connection_id":"shop","table":"orders"},{"alias":"x","connection_id":"crm","table":"ext"}]"#,
            &join_one("order_no", "x", "order_no"),
        );
        let out3 =
            check_draft(parse_draft(&draft_of(&[good], &[widget("d1", "BAR")])).unwrap(), &c)
                .unwrap();
        assert!(out3.warnings.is_empty(), "{:?}", out3.warnings);
    }

    /// 重命名之后的连接键（ext.city 落成 city_2）也要能追到类型：
    /// 左侧累计列的命名规则必须和执行期 plan_join_columns 同一套。
    #[test]
    fn warning_follows_a_renamed_join_column() {
        let raw = draft_of(
            &[typed_ds(
                concat!(
                    r#"[{"alias":"o","connection_id":"shop","table":"orders"},"#,
                    r#"{"alias":"x","connection_id":"crm","table":"ext"},"#,
                    r#"{"alias":"a","connection_id":"crm","table":"att"}]"#
                ),
                &format!(
                    "{},{}",
                    join_one("order_no", "x", "order_no"),
                    join_one("city_2", "a", "zone")
                ),
            )],
            &[widget("d1", "BAR")],
        );
        let out = check_draft(parse_draft(&raw).unwrap(), &typed_catalog()).unwrap();
        assert_eq!(
            out.warnings.len(),
            1,
            "只有第二个 join 的连接键跨了族：{:?}",
            out.warnings
        );
        assert!(out.warnings[0].contains("x.city"), "{}", out.warnings[0]);
        assert!(out.warnings[0].contains("int(11)"), "{}", out.warnings[0]);
    }

    /// 目录没给类型时保持沉默——很多库就是读不出列类型，不能因此卡住起草
    #[test]
    fn no_join_warning_when_the_catalog_has_no_types() {
        let out = check_draft(
            parse_draft(&draft_of(&[city_gmv()], &[widget("d1", "BAR")])).unwrap(),
            &catalog(),
        )
        .unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn check_rejects_widgets_bound_to_an_unknown_dataset() {
        let raw = draft_of(&[city_gmv()], &[widget("ghost", "PIE")]);
        let err = check_draft(parse_draft(&raw).unwrap(), &catalog()).unwrap_err();
        assert!(err.contains("ghost"), "{}", err);
    }

    #[test]
    fn check_rejects_duplicate_or_missing_dataset_ids() {
        let dup = draft_of(&[city_gmv(), city_gmv()], &[widget("d1", "BAR")]);
        assert!(check_draft(parse_draft(&dup).unwrap(), &catalog())
            .unwrap_err()
            .contains("重复"));
        let no_id = draft_of(
            &[spec(
                "",
                "o",
                r#"[{"alias":"o","connection_id":"shop","table":"orders"}]"#,
                r#""limit":10"#,
            )],
            &[widget("", "BAR")],
        );
        assert!(check_draft(parse_draft(&no_id).unwrap(), &catalog())
            .unwrap_err()
            .contains("缺少 id"));
    }

    /// 脚本化模型：记录每次收到的提示词，按脚本回答
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
                .unwrap_or_else(|| "not a json at all".into());
            Box::pin(async move { Ok(answer) })
        }
    }

    fn scripted(answers: Vec<String>) -> Scripted {
        Scripted { answers, prompts: std::sync::Mutex::new(Vec::new()) }
    }

    #[tokio::test]
    async fn draft_repairs_itself_from_the_local_verdict() {
        let model = scripted(vec![
            "我说不出 JSON，只能给你一段 SQL：SELECT 1".into(),
            draft_of(
                &[spec(
                    "d1",
                    "o",
                    r#"[{"alias":"o","connection_id":"shop","table":"invoicez"}]"#,
                    r#""limit":10"#,
                )],
                &[widget("d1", "BAR")],
            ),
            format!("```json\n{}\n```", draft_of(&[city_gmv()], &[widget("d1", "BAR")])),
        ]);
        let out = draft(&model, "各城市成交额", &catalog(), 3, None).await.unwrap();
        assert_eq!(out.repairs, 2);
        let prompts = model.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert!(prompts[1].contains("找不到 JSON 对象"), "{}", prompts[1]);
        assert!(prompts[2].contains("没有表 invoicez"), "{}", prompts[2]);
    }

    #[tokio::test]
    async fn draft_gives_up_with_the_last_local_error() {
        let model = scripted(vec![]);
        let err = draft(&model, "各城市成交额", &catalog(), 1, None).await.unwrap_err();
        assert!(err.contains("重试 2 次"), "{}", err);
        assert!(err.contains("找不到 JSON"), "{}", err);
        assert_eq!(model.prompts.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn draft_refuses_an_empty_question_without_calling_the_model() {
        let model = scripted(vec![draft_of(&[city_gmv()], &[widget("d1", "BAR")])]);
        let err = draft(&model, "   ", &catalog(), 2, None).await.unwrap_err();
        assert!(err.contains("描述"), "{}", err);
        assert!(model.prompts.lock().unwrap().is_empty());
    }

    // ==================== 追问式改稿 ====================

    fn prior(question: &str, raw: &str) -> PriorReport {
        PriorReport { question: question.into(), draft: parse_draft(raw).unwrap() }
    }

    #[test]
    fn prompt_shows_the_previous_design_and_its_question() {
        let p = prior(
            "各城市成交额",
            &draft_of(&[city_gmv()], &[widget("d1", "BAR")]),
        );
        let text = prompt("再加一个按月的折线图", &catalog(), Some(&p), None);
        assert!(text.contains("上一版报表是这么设计的（当时需求：各城市成交额）"), "{text}");
        // 这两串只可能来自上一版设计：基础提示词的结构示例里既没有 d1 也没有 SUM/amount
        assert!(text.contains("\"id\": \"d1\""), "{text}");
        assert!(text.contains("\"output\": \"gmv\""), "{text}");
        assert!(text.contains("请在它基础上按新需求改"), "{text}");
        assert!(text.contains("不要回增量"), "{text}");
        assert!(text.contains("需求：再加一个按月的折线图"), "{text}");

        // 手改过规格、或从报表簿打开的老报表，当时需求可能压根没记下来
        let mute = prior("", &draft_of(&[city_gmv()], &[widget("d1", "BAR")]));
        let text2 = prompt("把饼图换成表格", &catalog(), Some(&mute), None);
        assert!(text2.contains("上一版报表是这么设计的："), "{text2}");
        assert!(!text2.contains("当时需求"), "{text2}");

        // 没带上一版时不能凭空冒出一段设计
        let text3 = prompt("各城市成交额", &catalog(), None, None);
        assert!(!text3.contains("上一版报表"), "{text3}");
    }

    #[tokio::test]
    async fn draft_ignores_an_empty_prior_design() {
        let model = scripted(vec![draft_of(&[city_gmv()], &[widget("d1", "BAR")])]);
        let empty = prior("上次的需求串", &draft_of(&[], &[]));
        let out = draft(&model, "各城市成交额", &catalog(), 1, Some(&empty))
            .await
            .unwrap();
        assert_eq!(out.datasets.len(), 1);
        let ps = model.prompts.lock().unwrap();
        assert_eq!(ps.len(), 1);
        assert!(!ps[0].contains("上一版报表"), "{}", ps[0]);
        assert!(!ps[0].contains("上次的需求串"), "{}", ps[0]);
    }

    /// 重试轮次也得带着上一版：否则第二轮模型是在凭空重画整张报表
    #[tokio::test]
    async fn draft_keeps_the_prior_through_the_repair_round() {
        let bad = draft_of(
            &[spec(
                "d1",
                "o",
                r#"[{"alias":"o","connection_id":"shop","table":"invoicez"}]"#,
                r#""limit":10"#,
            )],
            &[widget("d1", "BAR")],
        );
        let model = scripted(vec![bad.clone(), draft_of(&[city_gmv()], &[widget("d1", "BAR")])]);
        let p = prior("各城市成交额", &bad);
        let out = draft(&model, "把金额换成税前的", &catalog(), 3, Some(&p))
            .await
            .unwrap();
        assert_eq!(out.repairs, 1);
        let ps = model.prompts.lock().unwrap();
        assert_eq!(ps.len(), 2);
        for round in ps.iter() {
            assert!(round.contains("上一版报表是这么设计的"), "{round}");
            assert!(round.contains("invoicez"), "{round}");
            assert!(round.contains("把金额换成税前的"), "{round}");
        }
        // 改稿提示和本机拒因同时在场，模型才知道既要在旧设计上改、又要修哪个错
        assert!(ps[1].contains("上一稿没有通过本机校验"), "{}", ps[1]);
    }

    /// 上一版可能是用户手改的、也可能是连接改绑前存的：里面的表可能早就不在目录里。
    /// 模型照抄也不能放行，改出来的设计过的还是同一套本机校验。
    #[tokio::test]
    async fn draft_still_rejects_a_stale_prior_the_model_copies() {
        let stale = draft_of(
            &[spec(
                "d1",
                "o",
                r#"[{"alias":"o","connection_id":"shop","table":"invoicez"}]"#,
                r#""limit":10"#,
            )],
            &[widget("d1", "BAR")],
        );
        let model = scripted(vec![stale.clone(), stale.clone()]);
        let p = prior("各城市成交额", &stale);
        let e = draft(&model, "再加一个饼图", &catalog(), 1, Some(&p))
            .await
            .unwrap_err();
        assert!(e.contains("重试 2 次"), "{e}");
        assert!(e.contains("invoicez"), "{e}");
        assert_eq!(model.prompts.lock().unwrap().len(), 2);
    }

    #[test]
    fn prior_deserializes_from_the_frontend_shape() {
        let p: PriorReport = serde_json::from_str(
            r#"{"question":"按月份","draft":{"datasets":[],"view":{"id":"v","name":"n","widgets":[],"layout":[]}}}"#,
        )
        .unwrap();
        assert_eq!(p.question, "按月份");
        assert!(p.draft.datasets.is_empty());
        // 少带 draft 不能悄悄解成"空白上一版"
        assert!(serde_json::from_str::<PriorReport>(r#"{"question":"按月份"}"#).is_err());
    }

    fn cfg(base_url: &str, key: &str, model: &str) -> AIConfig {
        AIConfig { base_url: base_url.into(), api_key: key.into(), model: model.into() }
    }

    #[tokio::test]
    async fn chat_needs_url_key_and_model_before_any_request() {
        // 不传密钥就不该发出任何请求；错误文案要能指到设置页
        assert!(chat(&cfg("", "k", "m"), "hi").await.unwrap_err().contains("设置"));
        let e = match HttpModel::new(cfg("https://x/", "k", "  ")) {
            Ok(_) => panic!("空模型名不该被接受"),
            Err(e) => e,
        };
        assert!(e.contains("模型名"), "{}", e);
        // 地址留给 chat 统一报错：HttpModel 构造不该重复同一份校验
        assert!(HttpModel::new(cfg("", "k", "m")).is_ok());
    }
}
