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
}

/// 模型产出的一整张报表草稿
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportDraft {
    pub datasets: Vec<DatasetSpec>,
    pub view: ViewSpec,
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

/// 去掉 ```...``` 包裹；没有围栏时原样返回
fn strip_fences(raw: &str) -> &str {
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
        lines.push(format!("    · {}({})", named, t.columns.join(", ")));
    }
    lines.join("\n")
}

/// 生成给模型的完整提示词。feedback 是上一稿被本地校验拒绝的原因。
pub fn prompt(question: &str, catalog: &[CatalogTable], feedback: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(
        "你是数据库报表设计器。只输出一段 JSON，不要解释、不要 markdown 代码块。\n\n\
         可用数据源与列（列名只能从这里取，禁止编造）：\n",
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
    if let Some(fb) = feedback {
        s.push_str("\n\n上一稿没有通过本机校验，原因：\n");
        s.push_str(fb.trim());
        s.push_str("\n请只修正这个问题，重新输出完整 JSON。");
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

/// 用目录覆盖模型声明的方言与列清单，并产出校验用的 SchemaCache。
/// 这一步之后，模型写下的任何标识符都不再影响能连哪个库、能读哪些列。
fn normalize_sources(
    ds: &DatasetSpec,
    catalog: &[CatalogTable],
) -> Result<(Vec<SourceRef>, SchemaCache), String> {
    let mut cache: SchemaCache = HashMap::new();
    let mut sources: Vec<SourceRef> = Vec::with_capacity(ds.sources.len());
    for src in &ds.sources {
        let hit = lookup(catalog, src).map_err(|e| format!("数据集 {}：{}", label_of(ds), e))?;
        cache.insert(src.alias.clone(), hit.columns.clone());
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
    Ok((sources, cache))
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

    for ds in draft.datasets {
        if ds.id.trim().is_empty() {
            return Err("数据集缺少 id".into());
        }
        if !ids.insert(ds.id.clone()) {
            return Err(format!("数据集 id {} 重复", ds.id));
        }
        let (sources, cache) = normalize_sources(&ds, catalog)?;
        let ds = DatasetSpec { sources, ..ds };
        let plan = super::dataset::plan_dataset(&ds, &cache)
            .map_err(|e| format!("数据集 {}：{}", label_of(&ds), e))?;
        steps.extend(plan.steps);
        columns.insert(ds.id.clone(), plan.columns.clone());
        datasets.push(ds);
    }

    let view_steps = super::view::validate_view(&draft.view, &columns)?;
    let used: HashSet<&str> = draft.view.widgets.iter().map(|w| w.dataset.as_str()).collect();
    let mut warnings: Vec<String> = Vec::new();
    for ds in &datasets {
        if !used.contains(ds.id.as_str()) {
            warnings.push(format!(
                "数据集 {}（{}）没有任何组件在用，执行时会白取一次数",
                ds.id, ds.name
            ));
        }
    }
    steps.extend(view_steps);
    Ok(DraftResult {
        datasets,
        view: draft.view,
        steps,
        columns,
        warnings,
        repairs: 0,
    })
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
pub async fn draft(
    model: &dyn Model,
    question: &str,
    catalog: &[CatalogTable],
    repairs: u8,
) -> Result<DraftResult, String> {
    if question.trim().is_empty() {
        return Err("请先描述你想要什么报表".into());
    }
    let rounds = repairs.min(MAX_REPAIRS);
    let mut feedback: Option<String> = None;
    let mut last = String::new();
    for round in 0..=rounds {
        let raw = model.complete(prompt(question, catalog, feedback.as_deref())).await?;
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
) -> Result<DraftResult, String> {
    let model = HttpModel::new(config)?;
    draft(&model, &question, &catalog, max_repairs.unwrap_or(2)).await
}

#[cfg(test)]
mod tests {
    use super::super::dataset::{JoinKind, JoinSpec};
    use super::super::table::AggFunc;
    use super::super::view::{AggType, ChartType};
    use super::*;

    fn catalog() -> Vec<CatalogTable> {
        vec![
            CatalogTable {
                connection_id: "shop".into(),
                connection_name: "商城库".into(),
                database_type: "sqlite".into(),
                schema: String::new(),
                table: "orders".into(),
                columns: vec!["id".into(), "user_id".into(), "amount".into(), "status".into()],
            },
            CatalogTable {
                connection_id: "shop".into(),
                connection_name: "商城库".into(),
                database_type: "sqlite".into(),
                schema: String::new(),
                table: "users".into(),
                columns: vec!["id".into(), "city".into(), "channel".into()],
            },
            CatalogTable {
                connection_id: "crm".into(),
                connection_name: "客户库".into(),
                database_type: "mysql".into(),
                schema: "crm".into(),
                table: "visits".into(),
                columns: vec!["user_id".into(), "day".into(), "pv".into()],
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

    #[test]
    fn prompt_lists_every_allowed_function_and_the_catalog() {
        let p = prompt("各城市成交额", &catalog(), None);
        for f in ALLOWED_FUNCTIONS {
            assert!(p.contains(f), "提示词漏了函数 {}", f);
        }
        assert!(p.contains("connection_id=shop"));
        assert!(p.contains("orders(id, user_id, amount, status)"));
        assert!(p.contains("crm.visits(user_id, day, pv)"));
        assert!(!p.contains("password"), "提示词不该带凭据");
    }

    #[test]
    fn prompt_feeds_the_previous_error_back() {
        let p = prompt("q", &catalog(), Some("列 total_fee 不存在"));
        assert!(p.contains("列 total_fee 不存在"));
        assert!(p.contains("重新输出完整 JSON"));
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
        let out = draft(&model, "各城市成交额", &catalog(), 3).await.unwrap();
        assert_eq!(out.repairs, 2);
        let prompts = model.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert!(prompts[1].contains("找不到 JSON 对象"), "{}", prompts[1]);
        assert!(prompts[2].contains("没有表 invoicez"), "{}", prompts[2]);
    }

    #[tokio::test]
    async fn draft_gives_up_with_the_last_local_error() {
        let model = scripted(vec![]);
        let err = draft(&model, "各城市成交额", &catalog(), 1).await.unwrap_err();
        assert!(err.contains("重试 2 次"), "{}", err);
        assert!(err.contains("找不到 JSON"), "{}", err);
        assert_eq!(model.prompts.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn draft_refuses_an_empty_question_without_calling_the_model() {
        let model = scripted(vec![draft_of(&[city_gmv()], &[widget("d1", "BAR")])]);
        let err = draft(&model, "   ", &catalog(), 2).await.unwrap_err();
        assert!(err.contains("描述"), "{}", err);
        assert!(model.prompts.lock().unwrap().is_empty());
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
