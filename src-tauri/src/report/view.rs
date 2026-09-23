// 视图 / 图表规格：数据集出来的是"表"，视图决定"怎么看"
//
// 概念对齐 /Users/zifang/workplace/ceo_workplace/z-opc-foundation/z-report
// （ChartType / WidgetSpec / WidgetEncode / WidgetLayout / ViewSchema），
// 但有两处刻意不同，都是那边踩过的坑：
//   1. 组件级过滤用受限表达式，而不是 {field, op, value} 三元组。三元组表达不了
//      AND/OR/函数，还要为 value 再造一套转义；表达式语言已经校验过列名与函数白名单。
//   2. 聚合默认按图表类型推：饼图 / KPI 不写 agg 就是 SUM（一块饼只能一个数），
//      折线 / 柱状 / 表格不写 agg 就是原样画，不会悄悄把 12 个月的线压成一个点。
//
// 这里全是纯函数：不连库、不取数。取数在 dataset::execute_dataset，
// 视图只把"已经算好的表"编码成图表能直接画的结构。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::expr::{parse_expr, AllowedColumns, Value};
use super::table::{aggregate, AggFunc, AggSpec, Table};

/// 栅格宽度，与前端 12 栏布局一致
pub const GRID_COLUMNS: u8 = 12;
pub const DEFAULT_TABLE_ROWS: usize = 200;
pub const MAX_TABLE_ROWS: usize = 2_000;
/// 单视图组件数上限：桌面端渲染器不是无限画布
pub const MAX_WIDGETS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChartType {
    Line,
    Bar,
    Pie,
    Table,
    Kpi,
}

/// 图表类型字面量大小写不敏感，且接受 AI 爱写的别名。
impl std::str::FromStr for ChartType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm = s.trim().to_ascii_uppercase().replace([' ', '_'], "");
        match norm.as_str() {
            "LINE" | "LINECHART" => Ok(ChartType::Line),
            "BAR" | "BARCHART" | "COLUMN" => Ok(ChartType::Bar),
            "PIE" | "PIECHART" | "DONUT" => Ok(ChartType::Pie),
            "TABLE" | "GRID" | "DETAIL" => Ok(ChartType::Table),
            "KPI" | "KPICARD" | "NUMBER" => Ok(ChartType::Kpi),
            other => Err(format!(
                "不支持的图表类型 {}（可选 LINE / BAR / PIE / TABLE / KPI）",
                other
            )),
        }
    }
}

impl ChartType {
    /// 是否需要一个度量列（KPI 例外：COUNT 时可以没有列）
    pub fn needs_measure(self) -> bool {
        matches!(self, ChartType::Line | ChartType::Bar | ChartType::Pie)
    }

    /// 是否需要分类轴
    pub fn needs_category(self) -> bool {
        matches!(self, ChartType::Line | ChartType::Bar | ChartType::Pie | ChartType::Kpi)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AggType {
    /// 不聚合，按数据集返回的行原样画
    #[default]
    Raw,
    Sum,
    Count,
    // COUNT_DISTINCT 是可读的那份，但 AI 会写 COUNTDISTINCT / COUNTD / MEAN，
    // 全部收下——字面量不匹配时模型只会重试一次就放弃，容错比严格更值钱。
    #[serde(rename = "COUNT_DISTINCT", alias = "COUNTDISTINCT", alias = "COUNTD")]
    CountDistinct,
    #[serde(alias = "MEAN")]
    Avg,
    Min,
    Max,
}

/// 聚合字面量大小写不敏感，且接受 COUNTD / MEAN 这类 BI 工具写法。
impl std::str::FromStr for AggType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm = s.trim().to_ascii_uppercase().replace([' ', '_'], "");
        match norm.as_str() {
            "" | "RAW" | "NONE" | "NONE_AGG" => Ok(AggType::Raw),
            "SUM" => Ok(AggType::Sum),
            "COUNT" => Ok(AggType::Count),
            "COUNTDISTINCT" | "COUNTD" => Ok(AggType::CountDistinct),
            "AVG" | "MEAN" => Ok(AggType::Avg),
            "MIN" => Ok(AggType::Min),
            "MAX" => Ok(AggType::Max),
            other => Err(format!(
                "不支持的聚合 {}（可选 RAW / SUM / COUNT / COUNT_DISTINCT / AVG / MIN / MAX）",
                other
            )),
        }
    }
}

impl AggType {
    pub fn func(self) -> Option<AggFunc> {
        match self {
            AggType::Raw => None,
            AggType::Sum => Some(AggFunc::Sum),
            AggType::Count => Some(AggFunc::Count),
            AggType::CountDistinct => Some(AggFunc::CountDistinct),
            AggType::Avg => Some(AggFunc::Avg),
            AggType::Min => Some(AggFunc::Min),
            AggType::Max => Some(AggFunc::Max),
        }
    }

    /// 饼图 / KPI 缺省聚合：这两类图没有"逐行"这种画法，一块扇区只能一个数。
    /// 折线 / 柱状 / 表格保持 Raw，否则 AI 忘了写 agg 就会把趋势压成一个点。
    pub fn effective(self, kind: ChartType) -> Self {
        if self != AggType::Raw {
            return self;
        }
        match kind {
            ChartType::Pie | ChartType::Kpi => AggType::Sum,
            _ => AggType::Raw,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WidgetEncode {
    /// 分类轴（折线 / 柱状的 x，饼图的扇题名）
    #[serde(default)]
    pub x: Option<String>,
    /// 数值轴列；与 value 同义，两个都写时以 y 为准
    #[serde(default)]
    pub y: Option<String>,
    /// 拆分多条系列的列
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    /// 表格列顺序；空 = 数据集全部列
    #[serde(default)]
    pub columns: Vec<String>,
}

impl WidgetEncode {
    /// x 与 category 是一个东西的两种写法（ECharts 习惯 vs z-report 习惯），
    /// 让 AI 两种都写得出，但内部只认一个。
    pub fn category_col(&self) -> Option<&str> {
        self.x.as_deref().or(self.category.as_deref())
    }

    pub fn measure_col(&self) -> Option<&str> {
        self.y.as_deref().or(self.value.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetSpec {
    pub id: String,
    #[serde(rename = "type", deserialize_with = "super::de_ci")]
    pub kind: ChartType,
    #[serde(default)]
    pub title: String,
    /// 绑定的数据集 id
    pub dataset: String,
    #[serde(default)]
    pub encode: WidgetEncode,
    #[serde(default, deserialize_with = "super::de_ci")]
    pub agg: AggType,
    /// 组件级过滤：作用在数据集输出之上，语义与数据集过滤完全一致（三值逻辑）
    #[serde(default)]
    pub filters: Vec<String>,
    /// 表格 / 明细的展示行数上限
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetLayout {
    pub widget: String,
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewSpec {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: u32,
    pub widgets: Vec<WidgetSpec>,
    /// 为空时按 MAX 顺序自动纵向堆叠（AI 只写图表、不操心布局）
    #[serde(default)]
    pub layout: Vec<WidgetLayout>,
}

fn default_version() -> u32 {
    1
}

// ==================== 图表数据 ====================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartData {
    pub widget: String,
    pub kind: ChartType,
    pub title: String,
    /// 分类轴刻度；TABLE / KPI 可为空
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    /// TABLE：列与行
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// KPI：单个标量
    pub value: Option<Value>,
    pub row_count: usize,
    /// 不算错但值得让用户看见的事，例如"度量列有 3 行不是数值"
    pub warnings: Vec<String>,
}

impl ChartData {
    fn new(w: &WidgetSpec) -> ChartData {
        ChartData {
            widget: w.id.clone(),
            kind: w.kind,
            title: if w.title.trim().is_empty() {
                w.id.clone()
            } else {
                w.title.clone()
            },
            categories: Vec::new(),
            series: Vec::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            value: None,
            row_count: 0,
            warnings: Vec::new(),
        }
    }
}

// ==================== 校验 ====================

fn need_col(cols: &[String], name: Option<&str>, what: &str) -> Result<String, String> {
    let raw = name.ok_or_else(|| format!("缺少 {} 列", what))?;
    cols.iter()
        .find(|c| c.eq_ignore_ascii_case(raw))
        .cloned()
        .ok_or_else(|| {
            format!(
                "{} 列 {} 不在数据集输出里（可用列：{}）",
                what,
                raw,
                if cols.is_empty() { "-".to_string() } else { cols.join(", ") }
            )
        })
}

/// 布局解析：缺省时自动纵向堆叠；给了布局就必须整齐——每个组件恰好一条、
/// 不越界、不重叠。AI 生成布局最容易在这三处出错，报出来比画歪了强。
pub fn resolve_layout(view: &ViewSpec) -> Result<Vec<WidgetLayout>, String> {
    if view.widgets.len() > MAX_WIDGETS {
        return Err(format!("组件数 {} 超过上限 {}", view.widgets.len(), MAX_WIDGETS));
    }
    if view.layout.is_empty() {
        return Ok(view
            .widgets
            .iter()
            .enumerate()
            .map(|(i, w)| WidgetLayout {
                widget: w.id.clone(),
                x: 0,
                y: (i as u8) * 6,
                w: GRID_COLUMNS,
                h: 6,
            })
            .collect());
    }
    let mut out: Vec<WidgetLayout> = Vec::with_capacity(view.layout.len());
    for l in &view.layout {
        let w = view
            .widgets
            .iter()
            .find(|w| w.id == l.widget)
            .ok_or_else(|| format!("布局引用了不存在的组件 {}", l.widget))?;
        if out.iter().any(|o| o.widget == l.widget) {
            return Err(format!("组件 {} 有多条布局", l.widget));
        }
        if l.w == 0 || l.h == 0 {
            return Err(format!("组件 {} 的宽高必须大于 0", w.id));
        }
        if l.x.saturating_add(l.w) > GRID_COLUMNS {
            return Err(format!(
                "组件 {} 越过 {} 栏：x={} w={}",
                w.id, GRID_COLUMNS, l.x, l.w
            ));
        }
        out.push(l.clone());
    }
    for a in &out {
        for b in &out {
            if a.widget == b.widget {
                continue;
            }
            let overlap = a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
            if overlap {
                return Err(format!(
                    "组件 {} 与 {} 在栅格上重叠",
                    a.widget, b.widget
                ));
            }
        }
    }
    for w in &view.widgets {
        if !out.iter().any(|l| l.widget == w.id) {
            return Err(format!("组件 {} 缺少布局", w.id));
        }
    }
    Ok(out)
}

/// 静态校验。schemas：dataset id → 该数据集输出的列名。
/// 任何一处不成立都返回 Err，不会"先渲染看看"。
pub fn validate_view(
    view: &ViewSpec,
    schemas: &HashMap<String, Vec<String>>,
) -> Result<Vec<String>, String> {
    let (steps, problems) = check_view(view, schemas);
    if !problems.is_empty() {
        return Err(problem_list(&problems));
    }
    Ok(steps)
}

/// 只要"哪里不成立"，不要执行计划。起草那侧把它和数据集侧的问题并成一份清单，
/// 让模型一轮就把整稿改干净，而不是三轮各撞一条。
pub fn view_problems(view: &ViewSpec, schemas: &HashMap<String, Vec<String>>) -> Vec<String> {
    check_view(view, schemas).1
}

/// 返回（执行计划片段，不成立之处）。problems 非空时 steps 不再可信，调用方只看 problems。
///
/// 一次把不成立的地方列全，而不是撞到第一处就停：自修复默认只有两轮预算，
/// 逐条报等于让模型一轮只来得及改一个错；手改规格 JSON 的人更是改一处试一次。
fn check_view(
    view: &ViewSpec,
    schemas: &HashMap<String, Vec<String>>,
) -> (Vec<String>, Vec<String>) {
    let mut steps: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    if view.id.trim().is_empty() {
        problems.push("视图缺少 id".into());
    }
    if view.widgets.is_empty() {
        problems.push("视图没有任何组件".into());
    }
    let mut ids: Vec<String> = Vec::new();
    for w in &view.widgets {
        if w.id.trim().is_empty() {
            problems.push("存在没有 id 的组件".into());
        } else if ids.contains(&w.id) {
            problems.push(format!("组件 id {} 重复", w.id));
        } else {
            ids.push(w.id.clone());
        }
    }
    // 组件都指不清是谁，再逐条报"某列不存在"只是把真因埋起来
    if problems.is_empty() {
        for w in &view.widgets {
            let cols = match schemas.get(&w.dataset) {
                Some(c) => c,
                None => {
                    problems.push(format!("组件 {} 绑定的数据集 {} 不存在", w.id, w.dataset));
                    continue;
                }
            };
            if cols.is_empty() {
                problems.push(format!("数据集 {} 没有可输出的列", w.dataset));
                continue;
            }
            // 组件 id 由这里统一包上：一张 20 个组件的看板上，"total_fee 不存在"
            // 只有带着组件名才有人看得懂，AI 也才知道该改哪一条。
            let described = match validate_widget(w, cols) {
                Ok(d) => d,
                Err(e) => {
                    problems.push(format!("组件 {}（数据集 {}）：{}", w.id, w.dataset, e));
                    continue;
                }
            };
            let mut ok = true;
            for f in &w.filters {
                match parse_expr(f, &AllowedColumns(cols.clone())) {
                    Ok(_) => steps.push(format!("WHERE {}", f)),
                    Err(e) => {
                        problems.push(format!("组件 {} 过滤条件 [{}]: {}", w.id, f, e));
                        ok = false;
                    }
                }
            }
            if ok {
                steps.push(format!(
                    "WIDGET {} {} [{}] dataset={}",
                    w.id,
                    kind_name(w.kind),
                    described.join(" "),
                    w.dataset
                ));
            }
        }
        // 布局留到组件全对之后再评：一张列名就写错的稿子上再补一句"组件 X 缺少布局"，
        // 说的是同一处笔误的连带，不是第二个问题。
        if problems.is_empty() {
            match resolve_layout(view) {
                Ok(layout) => steps.push(format!("LAYOUT {} 个组件", layout.len())),
                Err(e) => problems.push(e),
            }
        }
    }
    (steps, problems)
}

/// 一处就照原样说清，多处才编号——前端的错误卡是 pre-wrap，一行一条正好是清单。
pub fn problem_list(problems: &[String]) -> String {
    if problems.len() == 1 {
        return problems[0].clone();
    }
    let mut out = format!("共 {} 处问题，一次全改完再跑：", problems.len());
    for (i, p) in problems.iter().enumerate() {
        out.push_str(&format!("\n{}. {}", i + 1, p));
    }
    out
}

/// 单组件校验，返回写进执行计划的描述片段。错误不带组件 id（由 validate_view 统一包）。
fn validate_widget(w: &WidgetSpec, cols: &[String]) -> Result<Vec<String>, String> {
    let agg = w.agg.effective(w.kind);
    let mut described: Vec<String> = Vec::new();
    if matches!(w.kind, ChartType::Line | ChartType::Bar) {
        let x = need_col(cols, Some(w.encode.category_col().unwrap_or("")), "x 轴")?;
        let y = need_col(cols, w.encode.measure_col(), "y 轴")?;
        described.push(format!("x={} y={} {}", x, y, agg_name(agg)));
        if let Some(s) = &w.encode.series {
            described.push(format!("series={}", need_col(cols, Some(s), "系列")?));
        }
    } else if w.kind == ChartType::Pie {
        let c = need_col(cols, Some(w.encode.category_col().unwrap_or("")), "扇区")?;
        let v = need_col(cols, w.encode.measure_col(), "数值")?;
        described.push(format!("category={} value={} {}", c, v, agg_name(agg)));
    } else if w.kind == ChartType::Kpi {
        match agg.func() {
            // COUNT(*) 不需要列，其它度量都要
            Some(f) if f == AggFunc::Count && w.encode.measure_col().is_none() => {
                described.push("COUNT(*)".into())
            }
            _ => {
                let v = need_col(cols, w.encode.measure_col(), "指标")?;
                described.push(format!("{}({})", agg_name(agg), v));
            }
        }
        if let Some(c) = w.encode.category_col() {
            described.push(format!("label={}", need_col(cols, Some(c), "标签")?));
        }
    } else {
        if w.encode.columns.is_empty() {
            described.push(format!("SELECT * ({} 列)", cols.len()));
        } else {
            let mut picked: Vec<String> = Vec::new();
            for c in &w.encode.columns {
                let real = need_col(cols, Some(c), "展示")?;
                if picked.contains(&real) {
                    return Err(format!("展示列 {} 重复", c));
                }
                picked.push(real);
            }
            described.push(format!("COLUMNS {}", picked.join(", ")));
        }
        if let Some(n) = w.limit {
            if n == 0 {
                return Err("limit 为 0，这张表永远画不出行".into());
            }
            described.push(format!("LIMIT {}", n));
        }
    }
    Ok(described)
}

fn agg_name(agg: AggType) -> &'static str {
    match agg {
        AggType::Raw => "RAW",
        AggType::Sum => "SUM",
        AggType::Count => "COUNT",
        AggType::CountDistinct => "COUNT DISTINCT",
        AggType::Avg => "AVG",
        AggType::Min => "MIN",
        AggType::Max => "MAX",
    }
}

fn kind_name(kind: ChartType) -> &'static str {
    match kind {
        ChartType::Line => "LINE",
        ChartType::Bar => "BAR",
        ChartType::Pie => "PIE",
        ChartType::Table => "TABLE",
        ChartType::Kpi => "KPI",
    }
}

// ==================== 渲染 ====================

fn apply_filters(w: &WidgetSpec, table: &Table) -> Result<Table, String> {
    let mut cur = table.clone();
    let cols = cur.columns.clone();
    for f in &w.filters {
        let e = parse_expr(f, &AllowedColumns(cols.clone()))
            .map_err(|err| format!("组件 {} 过滤条件 [{}]: {}", w.id, f, err))?;
        cur = cur.filter(&e)?;
    }
    Ok(cur)
}

fn label(v: &Value) -> String {
    v.as_text().unwrap_or_else(|| "-".to_string())
}

/// 数值化并统计掉到底有多少行不是数值：
/// 把"金额列里混了文本"这件事报出来，比画出一条少几个点的曲线诚实。
fn numericize(vals: &[Value], warnings: &mut Vec<String>, col: &str) -> Vec<Value> {
    let mut out = Vec::with_capacity(vals.len());
    let mut lost = 0usize;
    for v in vals {
        out.push(match v.as_f64() {
            Some(f) => Value::Float(f),
            None => {
                if !v.is_null() {
                    lost += 1;
                }
                Value::Null
            }
        });
    }
    if lost > 0 {
        warnings.push(format!(
            "度量列 {} 有 {} 行不是数值，已按空值处理",
            col, lost
        ));
    }
    out
}


/// 把 (分类, 系列, 度量) 三元组摊成 categories + 多条 series。
/// 同一格出现多次时后写的覆盖前写的，并明确告警——静默平均或静默丢弃都会骗人。
/// `no_series_label` 是"没有系列列"时那条系列的显示名。
/// 内部仍然用 "-" 占一个系列桶，但系列名是给 tooltip / 图例看的，
/// 写 "-" 等于把 "SUM(gmv)" 这个口径信息丢掉。
fn pivot(rows: &Table, x: &str, series: Option<&str>, measure: &str, no_series_label: &str, out: &mut ChartData) {
    let xs: Vec<String> = rows.column_values(x).iter().map(label).collect();
    let sy: Vec<String> = match series {
        Some(s) => rows.column_values(s).iter().map(label).collect(),
        None => vec!["-".to_string(); xs.len()],
    };
    let mv = numericize(&rows.column_values(measure), &mut out.warnings, measure);

    let mut xi: HashMap<String, usize> = HashMap::with_capacity(xs.len());
    for k in &xs {
        if !xi.contains_key(k) {
            let i = out.categories.len();
            out.categories.push(k.clone());
            xi.insert(k.clone(), i);
        }
    }
    let mut ni: HashMap<String, usize> = HashMap::new();
    for k in &sy {
        if !ni.contains_key(k) {
            let i = ni.len();
            ni.insert(k.clone(), i);
        }
    }
    let names: Vec<String> = {
        let mut v: Vec<(usize, String)> = ni.iter().map(|(k, i)| (*i, k.clone())).collect();
        v.sort_by_key(|(i, _)| *i);
        v.into_iter().map(|(_, k)| k).collect()
    };
    let mut grid: Vec<Vec<Value>> = names
        .iter()
        .map(|_| vec![Value::Null; out.categories.len()])
        .collect();
    let mut collided = 0usize;
    for i in 0..xs.len() {
        let cell = &mut grid[ni[&sy[i]]][xi[&xs[i]]];
        if *cell != Value::Null {
            collided += 1;
        }
        *cell = mv[i].clone();
    }
    if collided > 0 {
        out.warnings.push(format!(
            "有 {} 个 (分类, 系列) 组合重复，取最后一行；需要合计请先在数据集里聚合",
            collided
        ));
    }
    out.series = names
        .into_iter()
        .map(|n| {
            if series.is_none() {
                no_series_label.to_string()
            } else {
                n
            }
        })
        .zip(grid)
        .map(|(name, values)| Series { name, values })
        .collect();
}

/// 单组件渲染。table 必须是该组件绑定数据集已执行出的结果。
pub fn render_widget(w: &WidgetSpec, table: &Table) -> Result<ChartData, String> {
    let mut out = ChartData::new(w);
    let filtered = apply_filters(w, table)?;
    let cols = filtered.columns.clone();
    let agg = w.agg.effective(w.kind);

    match w.kind {
        ChartType::Table => {
            let picked: Vec<String> = if w.encode.columns.is_empty() {
                cols.clone()
            } else {
                w.encode
                    .columns
                    .iter()
                    .map(|c| {
                        cols.iter()
                            .find(|x| x.eq_ignore_ascii_case(c))
                            .cloned()
                            .ok_or_else(|| format!("展示列 {} 不在数据集输出里", c))
                    })
                    .collect::<Result<_, String>>()?
            };
            let cap = w.limit.unwrap_or(DEFAULT_TABLE_ROWS).min(MAX_TABLE_ROWS);
            let shown = filtered.project(&picked)?.limit(cap);
            out.row_count = filtered.len();
            out.columns = picked.clone();
            out.rows = super::source::to_records(&shown);
            if shown.len() < filtered.len() {
                out.warnings.push(format!(
                    "只显示前 {} 行（共 {} 行），需要更多请调大 limit",
                    shown.len(),
                    filtered.len()
                ));
            }
            return Ok(out);
        }
        ChartType::Kpi => {
            let measure = w.encode.measure_col().and_then(|m| {
                cols.iter().find(|c| c.eq_ignore_ascii_case(m)).cloned()
            });
            let func = match agg.func() {
                Some(f) => f,
                None => AggFunc::Sum,
            };
            if func != AggFunc::Count && measure.is_none() {
                return Err(format!("KPI {} 没有度量列，无法算 {}", w.id, agg_name(agg)));
            }
            let one = aggregate(&filtered, &[], &[AggSpec::new("_k", func, measure.as_deref())])?;
            out.value = one.column_values("_k").first().cloned();
            out.row_count = filtered.len();
            if let Some(c) = w.encode.category_col() {
                let real = need_col(&cols, Some(c), "标签")?;
                out.categories = vec![label(one.column_values(&real).first().unwrap_or(&Value::Null))];
            }
            return Ok(out);
        }
        _ => {}
    }

    let x = need_col(&cols, w.encode.category_col(), "分类轴")?;
    let measure = need_col(&cols, w.encode.measure_col(), "度量")?;
    let series = match &w.encode.series {
        Some(s) => Some(need_col(&cols, Some(s), "系列")?),
        None => None,
    };
    // 写了 agg 才聚合：折线/柱状没写就逐行原样画，绝不压成一个点。
    // 饼图按 effective 一定是 SUM，扇区天然互斥，所以只按分类分组。
    let rows = match agg.func() {
        Some(func) => {
            let mut group = vec![x.clone()];
            if w.kind != ChartType::Pie {
                if let Some(s) = &series {
                    if s != &x {
                        group.push(s.clone());
                    }
                }
            }
            aggregate(&filtered, &group, &[AggSpec::new("_m", func, Some(&measure))])?
        }
        None => filtered,
    };
    let measure_col = if agg.func().is_some() {
        "_m".to_string()
    } else {
        measure.clone()
    };
    out.row_count = rows.len();
    // 系列名 = 口径名，图例/tooltip 直接可读；聚合过就写成 SUM(gmv)
    let measure_label = match agg {
        AggType::Raw => measure.clone(),
        other => format!("{}({})", agg_name(other), measure),
    };
    if w.kind == ChartType::Pie {
        out.categories = rows.column_values(&x).iter().map(label).collect();
        let vals = numericize(&rows.column_values(&measure_col), &mut out.warnings, &measure);
        out.series = vec![Series {
            name: measure_label.clone(),
            values: vals,
        }];
    } else {
        pivot(
            &rows,
            &x,
            series.as_deref(),
            &measure_col,
            &measure_label,
            &mut out,
        );
    }
    Ok(out)
}

/// 整视图渲染：tables 提供每个 dataset id 的结果。缺表直接报错而不是画空图。
pub fn render_view(
    view: &ViewSpec,
    tables: &HashMap<String, Table>,
) -> Result<Vec<ChartData>, String> {
    let mut out = Vec::with_capacity(view.widgets.len());
    for w in &view.widgets {
        let t = tables
            .get(&w.dataset)
            .ok_or_else(|| format!("数据集 {} 尚未执行，组件 {} 无法渲染", w.dataset, w.id))?;
        out.push(render_widget(w, t)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tbl(columns: &[&str], rows: &[Vec<(usize, Value)>]) -> Table {
        let cols: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
        let owned: Vec<HashMap<String, Value>> = rows
            .iter()
            .map(|r| {
                let mut m = HashMap::new();
                for (i, v) in r {
                    m.insert(cols[*i].clone(), v.clone());
                }
                m
            })
            .collect();
        Table::new(cols, owned)
    }

    fn monthly() -> Table {
        tbl(
            &["month", "channel", "gmv"],
            &[
                vec![(0, Value::Text("1月".into())), (1, Value::Text("直营".into())), (2, Value::Float(100.0))],
                vec![(0, Value::Text("1月".into())), (1, Value::Text("加盟".into())), (2, Value::Float(30.0))],
                vec![(0, Value::Text("2月".into())), (1, Value::Text("直营".into())), (2, Value::Float(120.0))],
                vec![(0, Value::Text("2月".into())), (1, Value::Text("加盟".into())), (2, Value::Float(40.0))],
            ],
        )
    }

    fn widget(kind: ChartType) -> WidgetSpec {
        WidgetSpec {
            id: "w1".into(),
            kind,
            title: "标题".into(),
            dataset: "m".into(),
            encode: WidgetEncode {
                x: Some("month".into()),
                y: Some("gmv".into()),
                ..Default::default()
            },
            agg: AggType::default(),
            filters: Vec::default(),
            limit: None,
        }
    }

    fn schemas() -> HashMap<String, Vec<String>> {
        let mut m = HashMap::new();
        m.insert(
            "m".to_string(),
            vec!["month".to_string(), "channel".to_string(), "gmv".to_string()],
        );
        m
    }

    fn view_of(widgets: Vec<WidgetSpec>) -> ViewSpec {
        ViewSpec {
            id: "v".into(),
            name: "月度看板".into(),
            version: 1,
            widgets,
            layout: Vec::default(),
        }
    }

    #[test]
    fn json_literals_match_z_report_schema() {
        // 前端与 AI 提示词都按大写字面量写，serde 改名会静默打断这条链路
        let j = serde_json::to_string(&ChartType::Kpi).unwrap();
        assert_eq!(j, "\"KPI\"");
        assert_eq!(serde_json::to_string(&AggType::CountDistinct).unwrap(), "\"COUNT_DISTINCT\"");
        for spell in ["\"COUNT_DISTINCT\"", "\"COUNTDISTINCT\"", "\"COUNTD\""] {
            assert_eq!(serde_json::from_str::<AggType>(spell).unwrap(), AggType::CountDistinct, "{}", spell);
        }
        assert_eq!(serde_json::from_str::<AggType>("\"MEAN\"").unwrap(), AggType::Avg);
        let w: WidgetSpec = serde_json::from_str(
            r#"{"id":"a","type":"PIE","dataset":"m","encode":{"category":"month","value":"gmv"}}"#,
        )
        .unwrap();
        assert_eq!(w.encode.category_col(), Some("month"));
        assert_eq!(w.encode.measure_col(), Some("gmv"));
        assert_eq!(w.agg, AggType::Raw, "缺省必须是 Raw，由 effective() 按图型推");
    }

    #[test]
    fn pie_collapses_but_line_keeps_rows() {
        assert_eq!(AggType::Raw.effective(ChartType::Pie), AggType::Sum);
        assert_eq!(AggType::Raw.effective(ChartType::Kpi), AggType::Sum);
        // 折线若被压成 SUM，12 个月就只剩一个点
        assert_eq!(AggType::Raw.effective(ChartType::Line), AggType::Raw);
        assert_eq!(AggType::Avg.effective(ChartType::Pie), AggType::Avg);
    }

    #[test]
    fn validate_rejects_columns_that_are_not_in_the_dataset() {
        let mut w = widget(ChartType::Bar);
        w.encode.y = Some("total_fee".into());
        let err = validate_view(&view_of(vec![w]), &schemas()).unwrap_err();
        assert!(err.contains("total_fee"), "{}", err);
        assert!(err.contains("month, channel, gmv"), "{}", err);
    }

    #[test]
    fn validate_rejects_unknown_dataset_and_empty_view() {
        let mut w = widget(ChartType::Bar);
        w.dataset = "ghost".into();
        assert!(validate_view(&view_of(vec![w.clone()]), &schemas())
            .unwrap_err()
            .contains("ghost"));
        assert!(validate_view(&view_of(vec![]), &schemas()).is_err());
        let dup = view_of(vec![widget(ChartType::Bar), widget(ChartType::Bar)]);
        assert!(validate_view(&dup, &schemas()).unwrap_err().contains("重复"));
    }

    /// 撞到第一处就停的校验，等于让模型用两轮自修复预算去撞三个错，手改 JSON 的人
    /// 更是改一处试一次。一次列全，每条各带自己的组件 id。
    #[test]
    fn validate_lists_every_broken_widget_at_once() {
        let mut bad_col = widget(ChartType::Bar);
        bad_col.encode.y = Some("total_fee".into());
        let mut ghost = widget(ChartType::Pie);
        ghost.id = "w2".into();
        ghost.dataset = "ghost".into();
        let mut bad_filter = widget(ChartType::Line);
        bad_filter.id = "w3".into();
        bad_filter.filters = vec!["nope > 1".into()];
        let err = validate_view(
            &view_of(vec![bad_col, ghost, bad_filter]),
            &schemas(),
        )
        .unwrap_err();
        assert!(err.contains("共 3 处问题"), "{}", err);
        // 三处各自说清是什么：列不在数据集、数据集不存在、过滤条件里的列不存在
        assert!(err.contains("total_fee"), "{}", err);
        assert!(err.contains("ghost"), "{}", err);
        assert!(err.contains("nope > 1"), "{}", err);
        for id in ["w1", "w2", "w3"] {
            assert!(err.contains(id), "{} 没被点名：{}", id, err);
        }
        // 编号成清单：错误卡是 pre-wrap，一行一条才看得完
        assert!(err.contains("\n1. "), "{}", err);
        assert!(err.contains("\n3. "), "{}", err);
    }

    #[test]
    fn a_single_problem_is_not_wrapped_in_a_list() {
        let mut w = widget(ChartType::Bar);
        w.encode.y = Some("total_fee".into());
        let err = validate_view(&view_of(vec![w]), &schemas()).unwrap_err();
        assert!(!err.contains("共 "), "{}", err);
        assert!(!err.contains("1. "), "{}", err);
    }

    /// 列名就写错着的稿子，"组件 X 缺少布局"说的多半是同一处笔误的连带
    #[test]
    fn layout_is_blamed_only_after_the_widgets_are_right() {
        let mut broken = widget(ChartType::Bar);
        broken.encode.y = Some("total_fee".into());
        let bad_layout = vec![WidgetLayout {
            widget: "nope".into(),
            x: 0,
            y: 0,
            w: 12,
            h: 6,
        }];
        let v = ViewSpec {
            layout: bad_layout.clone(),
            ..view_of(vec![broken])
        };
        let err = validate_view(&v, &schemas()).unwrap_err();
        assert!(err.contains("total_fee"), "{}", err);
        assert!(!err.contains("布局"), "{}", err);
        // 组件改对了，才轮到布局这条真问题开口
        let ok = ViewSpec {
            layout: bad_layout,
            ..view_of(vec![widget(ChartType::Bar)])
        };
        assert!(validate_view(&ok, &schemas())
            .unwrap_err()
            .contains("布局"));
    }

    #[test]
    fn layout_is_auto_stacked_or_checked() {
        let a = widget(ChartType::Bar);
        let mut b = widget(ChartType::Pie);
        b.id = "w2".into();
        let auto = resolve_layout(&view_of(vec![a.clone(), b.clone()])).unwrap();
        assert_eq!(auto[1].y, 6);
        assert_eq!(auto[0].w, GRID_COLUMNS);

        let v = ViewSpec {
            layout: vec![
                WidgetLayout { widget: "w1".into(), x: 0, y: 0, w: 8, h: 6 },
                WidgetLayout { widget: "w2".into(), x: 6, y: 0, w: 6, h: 6 },
            ],
            widgets: vec![a.clone(), b.clone()],
            ..view_of(vec![a, b])
        };
        assert!(resolve_layout(&v).unwrap_err().contains("重叠"));
        let over = ViewSpec {
            layout: vec![WidgetLayout { widget: "w1".into(), x: 6, y: 0, w: 8, h: 6 }],
            ..v
        };
        assert!(resolve_layout(&over).unwrap_err().contains("越过"));
    }

    #[test]
    fn missing_layout_entry_for_a_widget_is_an_error() {
        let a = widget(ChartType::Bar);
        let mut b = widget(ChartType::Kpi);
        b.id = "w2".into();
        let v = ViewSpec {
            layout: vec![WidgetLayout { widget: "w1".into(), x: 0, y: 0, w: 6, h: 4 }],
            widgets: vec![a, b],
            ..view_of(vec![])
        };
        assert!(resolve_layout(&v).unwrap_err().contains("缺少布局"));
    }

    #[test]
    fn widget_filters_use_three_valued_sql_logic() {
        // gmv 为 NULL 的行必须被丢掉（NULL > 0 是 NULL，不是 FALSE）
        let t = tbl(
            &["month", "gmv"],
            &[
                vec![(0, Value::Text("1月".into())), (1, Value::Float(10.0))],
                vec![(0, Value::Text("2月".into())), (1, Value::Null)],
            ],
        );
        let mut w = widget(ChartType::Line);
        w.encode = WidgetEncode {
            x: Some("month".into()),
            y: Some("gmv".into()),
            ..Default::default()
        };
        w.filters = vec!["gmv > 0".into()];
        let c = render_widget(&w, &t).unwrap();
        assert_eq!(c.categories, vec!["1月"]);
        assert_eq!(c.row_count, 1);

        // 未知列在渲染期也要拦住
        w.filters = vec!["nope > 0".into()];
        assert!(render_widget(&w, &t).unwrap_err().contains("nope"));
    }

    #[test]
    fn line_with_series_becomes_a_matrix() {
        let mut w = widget(ChartType::Line);
        w.encode.series = Some("channel".into());
        let c = render_widget(&w, &monthly()).unwrap();
        assert_eq!(c.categories, vec!["1月", "2月"]);
        assert_eq!(c.series.len(), 2);
        let direct = c.series.iter().find(|s| s.name == "直营").unwrap();
        assert_eq!(direct.values, vec![Value::Float(100.0), Value::Float(120.0)]);
        let franchise = c.series.iter().find(|s| s.name == "加盟").unwrap();
        assert_eq!(franchise.values, vec![Value::Float(30.0), Value::Float(40.0)]);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn raw_duplicate_cell_warns_instead_of_silently_averaging() {
        let t = tbl(
            &["month", "gmv"],
            &[
                vec![(0, Value::Text("1月".into())), (1, Value::Float(10.0))],
                vec![(0, Value::Text("1月".into())), (1, Value::Float(20.0))],
            ],
        );
        let c = render_widget(&widget(ChartType::Line), &t).unwrap();
        assert_eq!(c.categories, vec!["1月"]);
        assert_eq!(c.series[0].values, vec![Value::Float(20.0)], "后写覆盖，且不静默求和");
        assert!(c
            .warnings
            .iter()
            .any(|w| w.contains("重复") && w.contains("请先在数据集里聚合")), "{:?}", c.warnings);
    }

    #[test]
    fn pie_always_groups_by_category() {
        let mut w = widget(ChartType::Pie);
        w.encode = WidgetEncode {
            category: Some("month".into()),
            value: Some("gmv".into()),
            ..Default::default()
        };
        let c = render_widget(&w, &monthly()).unwrap();
        assert_eq!(c.categories, vec!["1月", "2月"]);
        assert_eq!(c.series[0].values, vec![Value::Float(130.0), Value::Float(160.0)]);
    }

    #[test]
    fn series_names_say_which_measure_they_plot() {
        // 没有系列列时系列名曾是占位符 "-"，tooltip 会读成 "-: 100"。
        // 系列名是图例/tooltip 唯一的口径线索，必须写成列名或 SUM(列名)。
        let raw = render_widget(&widget(ChartType::Bar), &monthly()).unwrap();
        assert_eq!(raw.series.len(), 1);
        assert_eq!(raw.series[0].name, "gmv", "RAW 就是列名");

        let mut summed = widget(ChartType::Bar);
        summed.agg = AggType::Sum;
        let c = render_widget(&summed, &monthly()).unwrap();
        assert_eq!(c.series[0].name, "SUM(gmv)");

        // 写了系列列就按列值命名，别把口径名盖到系列上
        let mut split = widget(ChartType::Bar);
        split.encode.series = Some("channel".into());
        split.agg = AggType::Sum;
        let c = render_widget(&split, &monthly()).unwrap();
        let names: Vec<&str> = c.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["直营", "加盟"]);

        let mut pie = widget(ChartType::Pie);
        pie.encode = WidgetEncode {
            category: Some("month".into()),
            value: Some("gmv".into()),
            ..Default::default()
        };
        let c = render_widget(&pie, &monthly()).unwrap();
        assert_eq!(c.series[0].name, "SUM(gmv)", "饼图缺省 SUM");
    }

    #[test]
    fn bar_with_explicit_agg_aggregates_over_series_groups() {
        let mut w = widget(ChartType::Bar);
        w.encode.series = Some("channel".into());
        w.agg = AggType::Sum;
        let c = render_widget(&w, &monthly()).unwrap();
        assert_eq!(c.categories, vec!["1月", "2月"]);
        assert_eq!(c.series.len(), 2);
        assert_eq!(c.series[0].values, vec![Value::Float(100.0), Value::Float(120.0)]);
    }

    #[test]
    fn kpi_supports_sum_and_row_count() {
        let c = render_widget(&widget(ChartType::Kpi), &monthly()).unwrap();
        assert_eq!(c.value, Some(Value::Float(290.0)), "100+30+120+40");
        let mut w = widget(ChartType::Kpi);
        w.agg = AggType::Count;
        w.encode = WidgetEncode::default();
        let c = render_widget(&w, &monthly()).unwrap();
        assert_eq!(c.value, Some(Value::Int(4)));
        let mut bad = widget(ChartType::Kpi);
        bad.encode = WidgetEncode::default();
        assert!(render_widget(&bad, &monthly()).unwrap_err().contains("没有度量列"));
    }

    #[test]
    fn non_numeric_measure_is_reported_not_faked_as_zero() {
        let t = tbl(
            &["month", "gmv"],
            &[
                vec![(0, Value::Text("1月".into())), (1, Value::Float(10.0))],
                vec![(0, Value::Text("2月".into())), (1, Value::Text("待定".into()))],
            ],
        );
        let c = render_widget(&widget(ChartType::Bar), &t).unwrap();
        assert_eq!(c.series[0].values, vec![Value::Float(10.0), Value::Null]);
        assert!(c.warnings.iter().any(|w| w.contains("不是数值")), "{:?}", c.warnings);
    }

    #[test]
    fn table_widget_caps_rows_and_says_so() {
        let mut w = widget(ChartType::Table);
        w.encode = WidgetEncode {
            columns: vec!["month".into(), "gmv".into()],
            ..Default::default()
        };
        w.limit = Some(3);
        let c = render_widget(&w, &monthly()).unwrap();
        assert_eq!(c.columns, vec!["month", "gmv"]);
        assert_eq!(c.rows.len(), 3);
        assert_eq!(c.row_count, 4);
        assert!(c.warnings[0].contains("只显示前 3 行"), "{:?}", c.warnings);
        assert_eq!(c.rows[0][0], serde_json::json!("1月"));
        // 列缺失要报错，不能画一列空白
        w.encode.columns = vec!["nope".into()];
        assert!(render_widget(&w, &monthly()).unwrap_err().contains("nope"));
    }

    #[test]
    fn render_view_requires_every_dataset_to_have_been_executed() {
        let mut tables = HashMap::new();
        tables.insert("m".to_string(), monthly());
        let charts = render_view(&view_of(vec![widget(ChartType::Bar)]), &tables).unwrap();
        assert_eq!(charts.len(), 1);
        let mut w = widget(ChartType::Bar);
        w.id = "w9".into();
        w.dataset = "other".into();
        let err = render_view(&view_of(vec![w]), &tables).unwrap_err();
        assert!(err.contains("other"), "{}", err);
    }

    #[test]
    fn validate_then_render_is_consistent_for_every_chart() {
        let kinds = [ChartType::Line, ChartType::Bar, ChartType::Pie, ChartType::Table, ChartType::Kpi];
        for k in kinds {
            let w = widget(k);
            let steps = validate_view(&view_of(vec![w.clone()]), &schemas()).unwrap();
            assert!(steps.iter().any(|s| s.starts_with("WIDGET w1 ")), "{:?} {:?}", k, steps);
            let c = render_widget(&w, &monthly()).unwrap();
            assert_eq!(c.kind, k);
        }
    }

    #[test]
    fn view_roundtrips_through_json() {
        let v = view_of(vec![widget(ChartType::Line), widget(ChartType::Kpi)]);
        let j = serde_json::to_string(&v).unwrap();
        let back: ViewSpec = serde_json::from_str(&j).unwrap();
        assert_eq!(v, back);
    }
}
